#!/usr/bin/env python3
"""
Kokoro read-aloud client — one codebase, runs on macOS and Windows.

Grabs text (explicit, stdin, clipboard, or selection), sends it to the local
kokoro-voice service, and plays the audio.

Playback is PIPELINED: the text is split into chunks, and chunk N+1 is
synthesized while chunk N is still playing. Speech therefore starts after the
first short chunk (~0.3s) instead of after the whole passage, and because
synthesis runs several times faster than realtime, the queue stays ahead of
playback and it sounds continuous.

Usage:
    speak.py --text "hello"     speak a literal string
    speak.py --stdin            read text from stdin
    speak.py --clipboard        speak whatever is on the clipboard
    speak.py --selection        copy the current selection, then speak it
    speak.py --stop             stop playback immediately
    speak.py --voices           list available voices

Only stdlib — no pip install needed on the client side.
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import platform
import queue
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

# Loopback default, matching the service's 127.0.0.1 bind. When the Windows
# client lands, point THAT client at the service machine via KOKORO_HOST and give
# the service the same address — don't reopen it to 0.0.0.0.
HOST = os.environ.get("KOKORO_HOST", "127.0.0.1:8125")


def _load_token() -> str | None:
    """Env first, then the shared 0600 token file the server reads.

    The file exists so desktop and explicit legacy launch paths do not each
    need the secret plumbed into their environment.
    """
    tok = (os.environ.get("KOKORO_TOKEN") or "").strip()
    if tok:
        return tok
    path = os.environ.get(
        "KOKORO_TOKEN_FILE", os.path.expanduser("~/.config/kokoro-voice-2-1/token")
    )
    try:
        with open(path) as fh:
            return fh.read().strip() or None
    except OSError:
        return None


TOKEN = _load_token()
VOICE = os.environ.get("KOKORO_VOICE")
SPEED = float(os.environ.get("KOKORO_SPEED", "1.0"))
TIMEOUT = float(os.environ.get("KOKORO_TIMEOUT", "120"))

# Chunk sizes RAMP UP. Measured on the M4: synthesis runs ~4-5x realtime, so a
# chunk yielding D seconds of audio covers the synthesis of a following chunk up
# to ~4x its length. Starting small gets the first word out fast; growing from
# there keeps the producer comfortably ahead of playback without ever letting a
# later chunk take longer to synthesize than the previous chunk plays.
#   60 chars  -> ~0.8s synth, ~3.5s audio   (first sound fast)
#  200 chars  -> ~2.5s synth  < 3.5s cover  (no gap)
#  400 chars  -> steady state
CHUNK_RAMP = [
    int(os.environ.get("KOKORO_FIRST_CHUNK", "60")),
    200,
]
CHUNK_CHARS = int(os.environ.get("KOKORO_CHUNK", "400"))
PREFETCH = int(os.environ.get("KOKORO_PREFETCH", "3"))


def _budget_for(index: int) -> int:
    return CHUNK_RAMP[index] if index < len(CHUNK_RAMP) else CHUNK_CHARS

IS_MAC = platform.system() == "Darwin"
IS_WIN = platform.system() == "Windows"

def _state_dir() -> str:
    """A private 0700 scratch dir for the state file and the WAV chunks.

    On macOS gettempdir() is already per-user ($TMPDIR under /var/folders), but
    on Linux and Windows it is the shared /tmp. stop_playback() SIGKILLs every
    PID it reads out of the state file, so a world-writable location would let
    any local user aim that at processes of ours. Own the directory instead.
    """
    override = os.environ.get("KOKORO_STATE_DIR")
    if override:
        base = override
    elif IS_MAC:
        # The desktop host explicitly uses this directory. A standalone
        # client must share its playback lock or both can speak at once.
        base = os.path.expanduser("~/.config/kokoro-voice-2-1/runtime")
    else:
        try:
            who = str(os.getuid())
        except AttributeError:  # Windows
            who = os.environ.get("USERNAME", "user")
        base = os.path.join(tempfile.gettempdir(), f"kokoro-voice-2-1-{who}")

    os.makedirs(base, mode=0o700, exist_ok=True)
    # makedirs(exist_ok=True) accepts a pre-existing dir whatever its owner or
    # mode, so an attacker who won the race to create it would still win. Check.
    st = os.lstat(base)
    if hasattr(os, "getuid"):
        if os.path.islink(base) or st.st_uid != os.getuid():
            raise SystemExit(f"refusing to use {base}: not a directory we own")
        os.chmod(base, 0o700)
    return base


STATE_DIR = _state_dir()
STATEFILE = os.path.join(STATE_DIR, "playback.state")
LOCKFILE = os.path.join(STATE_DIR, "playback.lock")
_CANCELLED = threading.Event()
_OUTPUT_STREAM = None
_OUTPUT_FORMAT = None


class PlaybackCancelled(Exception):
    """Cooperative terminal state used to unwind playback cleanup."""


class PlaybackLock:
    """Cross-platform process lock held for one complete playback session."""

    def __init__(self) -> None:
        self._file = None

    def try_acquire(self) -> bool:
        if self._file is not None:
            return True
        handle = open(LOCKFILE, "a+b")
        handle.seek(0, os.SEEK_END)
        if handle.tell() == 0:
            handle.write(b"0")
            handle.flush()
        handle.seek(0)
        try:
            if IS_WIN:
                import msvcrt
                msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except (BlockingIOError, OSError):
            handle.close()
            return False
        self._file = handle
        return True

    def release(self) -> None:
        if self._file is None:
            return
        try:
            if IS_WIN:
                import msvcrt
                self._file.seek(0)
                msvcrt.locking(self._file.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                import fcntl
                fcntl.flock(self._file.fileno(), fcntl.LOCK_UN)
        finally:
            self._file.close()
            self._file = None


def _url(path: str) -> str:
    host = HOST if "://" in HOST else f"http://{HOST}"
    return f"{host.rstrip('/')}{path}"


# ── text acquisition ────────────────────────────────────────────────────────
def get_clipboard() -> str:
    if IS_MAC:
        return subprocess.run(["pbpaste"], capture_output=True, text=True).stdout
    if IS_WIN:
        return subprocess.run(
            ["powershell.exe", "-NoProfile", "-Command", "Get-Clipboard"],
            capture_output=True, text=True,
        ).stdout
    return subprocess.run(["xclip", "-o", "-selection", "clipboard"],
                          capture_output=True, text=True).stdout


# Every modifier a configurable shortcut can hold down: Ctrl, Alt, Shift, and
# both Windows keys. A Windows/Super key arriving through Deskflow is stored as
# Command, so a read gesture really can be holding LWIN when the action fires.
_MODIFIER_VKS = (0x11, 0x12, 0x10, 0x5B, 0x5C)


def modifiers_held() -> bool:
    """True while any modifier key is physically down. Windows only."""
    if not IS_WIN:
        return False
    try:
        import ctypes

        user32 = ctypes.windll.user32
        return any(user32.GetAsyncKeyState(vk) & 0x8000 for vk in _MODIFIER_VKS)
    except Exception:  # noqa: BLE001 - a probe failure must not block reading
        return False


def wait_for_modifier_release(timeout: float = 1.5) -> bool:
    """Let the shortcut's own keys come up before a fallback Ctrl+C."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not modifiers_held():
            return True
        time.sleep(0.02)
    return not modifiers_held()


def copy_selection() -> str:
    previous = get_clipboard()
    if IS_MAC:
        subprocess.run(
            ["osascript", "-e",
             'tell application "System Events" to keystroke "c" using command down'],
            capture_output=True,
        )
    elif IS_WIN:
        wait_for_modifier_release()
        subprocess.run(
            ["powershell.exe", "-NoProfile", "-Command",
             "Add-Type -AssemblyName System.Windows.Forms;"
             "[System.Windows.Forms.SendKeys]::SendWait('^c')"],
            capture_output=True,
        )
    time.sleep(0.25)
    return get_clipboard()


# ── chunking ────────────────────────────────────────────────────────────────
_SENTENCE = re.compile(r"(?<=[.!?])\s+|\n{2,}")


def split_chunks(text: str) -> list[str]:
    """Split into speakable chunks on sentence boundaries.

    Chunk boundaries land where a speaker would pause anyway, so the seams
    between separately-synthesized clips are inaudible.
    """
    text = " ".join(text.split())
    if not text:
        return []

    pieces = [p.strip() for p in _SENTENCE.split(text) if p and p.strip()]
    if not pieces:
        pieces = [text]

    chunks: list[str] = []
    budget = _budget_for(0)
    current = ""

    for piece in pieces:
        # A single sentence longer than the budget gets split on commas so the
        # first sound still arrives quickly.
        while len(piece) > budget * 2:
            cut = piece.rfind(", ", 0, budget)
            if cut <= 0:
                cut = piece.rfind(" ", 0, budget)
            if cut <= 0:
                break
            head, piece = piece[:cut + 1].strip(), piece[cut + 1:].strip()
            if current:
                chunks.append(current)
                current = ""
            chunks.append(head)
            budget = _budget_for(len(chunks))

        if not current:
            current = piece
        elif len(current) + len(piece) + 1 <= budget:
            current = f"{current} {piece}"
        else:
            chunks.append(current)
            current = piece
            budget = _budget_for(len(chunks))

    if current:
        chunks.append(current)
    return chunks


# ── service ─────────────────────────────────────────────────────────────────
def synthesize(
    text: str, voice: str | None, speed: float, *, session_end: bool = True
) -> bytes:
    payload = {"text": text, "speed": speed, "session_end": session_end}
    if voice:
        payload["voice"] = voice
    req = urllib.request.Request(
        _url("/speak"),
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    if TOKEN:
        req.add_header("Authorization", f"Bearer {TOKEN}")
    with urllib.request.urlopen(req, timeout=TIMEOUT) as resp:
        return resp.read()


def retire_tts() -> None:
    req = urllib.request.Request(
        _url("/tts/retire"),
        data=b"{}",
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    if TOKEN:
        req.add_header("Authorization", f"Bearer {TOKEN}")
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT) as response:
            response.read()
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError):
        pass


# ── playback control ────────────────────────────────────────────────────────
def _write_state(player_pid: int | None) -> None:
    temporary = f"{STATEFILE}.{os.getpid()}.tmp"
    try:
        with open(temporary, "w") as fh:
            fh.write(f"{os.getpid()}\n{player_pid or ''}\n")
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(temporary, STATEFILE)
    except OSError:
        try:
            os.remove(temporary)
        except OSError:
            pass


def _state_pids() -> tuple[int | None, int | None]:
    try:
        with open(STATEFILE) as fh:
            lines = fh.read().splitlines()
    except OSError:
        return None, None

    def parse(index: int) -> int | None:
        try:
            return int(lines[index].strip()) if lines[index].strip() else None
        except (IndexError, ValueError):
            return None

    return parse(0), parse(1)


def _pipeline_pid() -> int | None:
    return _state_pids()[0]


def _pid_is_our_speaker(pid: int) -> bool:
    if pid == os.getpid():
        return True
    try:
        if IS_WIN:
            command = subprocess.run(
                [
                    "powershell.exe",
                    "-NoProfile",
                    "-Command",
                    f"(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine",
                ],
                capture_output=True,
                text=True,
                timeout=2,
            ).stdout
        else:
            command = subprocess.run(
                ["ps", "-p", str(pid), "-o", "command="],
                capture_output=True,
                text=True,
                timeout=2,
            ).stdout
    except (OSError, subprocess.SubprocessError):
        return False
    return "speak.py" in command


def _cleanup_session_files(pid: int) -> None:
    for pattern in (f"kokoro-{pid}-*.wav", f"kokoro-{pid}-*.part"):
        for path in glob.glob(os.path.join(STATE_DIR, pattern)):
            try:
                os.remove(path)
            except OSError:
                pass


def _cleanup_orphan_files() -> None:
    pattern = re.compile(r"^kokoro-(\d+)-\d+\.wav(?:\.part)?$")
    live_speakers: dict[int, bool] = {}
    for path in glob.glob(os.path.join(STATE_DIR, "kokoro-*")):
        match = pattern.match(os.path.basename(path))
        if not match:
            continue
        pid = int(match.group(1))
        if pid not in live_speakers:
            live_speakers[pid] = _pid_is_our_speaker(pid)
        if live_speakers[pid]:
            continue
        try:
            os.remove(path)
        except OSError:
            pass


def _clear_state_if_owned(pid: int) -> None:
    if _pipeline_pid() != pid:
        return
    try:
        os.remove(STATEFILE)
    except OSError:
        pass


def _wait_for_speaker_exit(pid: int, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not _pid_is_our_speaker(pid):
            return True
        time.sleep(0.05)
    return not _pid_is_our_speaker(pid)


def _terminate_speaker(pid: int) -> None:
    """Cooperatively stop a verified speaker, then force only as a fallback."""
    if pid == os.getpid() or not _pid_is_our_speaker(pid):
        return
    try:
        if not IS_WIN:
            # A paused process cannot handle SIGTERM until it is resumed.
            os.kill(pid, signal.SIGCONT)
        os.kill(pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        return
    if _wait_for_speaker_exit(pid, 1.5):
        return
    try:
        os.kill(pid, signal.SIGKILL if not IS_WIN else signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        pass
    _wait_for_speaker_exit(pid, 0.5)


def stop_playback() -> None:
    """Stop the active pipeline without bypassing its cleanup path."""
    pipeline_pid = _pipeline_pid()
    if pipeline_pid and pipeline_pid != os.getpid():
        _terminate_speaker(pipeline_pid)
        _cleanup_session_files(pipeline_pid)
        _clear_state_if_owned(pipeline_pid)
    _cleanup_orphan_files()


def _player_pid() -> int | None:
    """PID of the process currently producing sound (line 2 of the state file)."""
    pipeline_pid, player_pid = _state_pids()
    if not pipeline_pid or not _pid_is_our_speaker(pipeline_pid):
        return None
    return player_pid


def _claim_playback() -> PlaybackLock:
    """Cancel any predecessor and become the only process allowed to play."""
    lock = PlaybackLock()
    deadline = time.monotonic() + 5.0
    while not lock.try_acquire():
        predecessor = _pipeline_pid()
        if predecessor and predecessor != os.getpid():
            _terminate_speaker(predecessor)
            _cleanup_session_files(predecessor)
            _clear_state_if_owned(predecessor)
        if time.monotonic() >= deadline:
            raise RuntimeError("another Kokoro playback process would not stop")
        time.sleep(0.05)
    _cleanup_orphan_files()
    _write_state(None)
    return lock


def _install_cancel_handler():
    if threading.current_thread() is not threading.main_thread():
        return None
    previous = signal.getsignal(signal.SIGTERM)

    def cancel(_signum, _frame) -> None:
        _CANCELLED.set()
        _close_output_stream(abort=True)
        try:
            import sounddevice as sd
            sd.stop()
        except (ImportError, RuntimeError):
            pass

    signal.signal(signal.SIGTERM, cancel)
    return previous


def is_paused() -> bool:
    """True if the player process is in the stopped (T) state."""
    pid = _player_pid()
    if not pid:
        return False
    if IS_WIN:
        return False
    out = subprocess.run(["ps", "-p", str(pid), "-o", "stat="],
                         capture_output=True, text=True).stdout.strip()
    return out.startswith("T")


def pause_playback() -> bool:
    """SIGSTOP the player — a real pause; audio resumes exactly where it left off."""
    pid = _player_pid()
    if not pid:
        return False
    if IS_WIN:
        return False
    try:
        os.kill(pid, signal.SIGSTOP)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def resume_playback() -> bool:
    pid = _player_pid()
    if not pid:
        return False
    if IS_WIN:
        return False
    try:
        os.kill(pid, signal.SIGCONT)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def toggle_playback() -> str:
    if not _player_pid():
        return "idle"
    if is_paused():
        return "playing" if resume_playback() else "paused"
    return "paused" if pause_playback() else "playing"


def _close_output_stream(*, abort: bool = False) -> None:
    """Drain completed speech, but discard queued audio on cancellation."""
    global _OUTPUT_STREAM, _OUTPUT_FORMAT
    stream, _OUTPUT_STREAM = _OUTPUT_STREAM, None
    _OUTPUT_FORMAT = None
    if stream is not None:
        try:
            if abort:
                stream.abort()
            else:
                stream.stop()
        finally:
            stream.close()


def _play_file(path: str) -> None:
    global _OUTPUT_STREAM, _OUTPUT_FORMAT
    if _CANCELLED.is_set():
        raise PlaybackCancelled
    if IS_MAC or IS_WIN:
        # Let the OS choose the output when a playback stream opens.
        # Reuse it across speech chunks to avoid gaps between chunks.
        import sounddevice as sd
        import soundfile as sf
        samples, sample_rate = sf.read(path, dtype="float32", always_2d=True)
        audio_format = (sample_rate, samples.shape[1])
        _write_state(os.getpid())
        for offset in range(0, len(samples), 2048):
            if _CANCELLED.is_set():
                raise PlaybackCancelled
            if _OUTPUT_STREAM is None or _OUTPUT_FORMAT != audio_format:
                _close_output_stream()
                _OUTPUT_STREAM = sd.OutputStream(
                    samplerate=sample_rate,
                    channels=audio_format[1], dtype="float32", latency="high",
                )
                _OUTPUT_FORMAT = audio_format
                _OUTPUT_STREAM.start()
            try:
                _OUTPUT_STREAM.write(samples[offset:offset + 2048])
            except Exception:
                if _CANCELLED.is_set():
                    raise PlaybackCancelled from None
                raise
        return
    else:
        proc = subprocess.Popen(["aplay", path])
    _write_state(proc.pid)
    while proc.poll() is None:
        if _CANCELLED.wait(0.05):
            proc.terminate()
            proc.wait()
            raise PlaybackCancelled


def notify(message: str) -> None:
    # The desktop host renders this inside its own status pill. Avoid macOS
    # notifications, which expose the Python/AppleScript implementation and
    # visually detach the failure from the control that triggered it.
    print(f"NOTICE {message}", flush=True)


def emit_paths(text: str, voice: str | None, speed: float) -> int:
    """Legacy-host adapter: print each synthesized WAV path as it becomes ready.

    Retained for the explicitly legacy Hammerspoon host. The supported Tauri
    app does not call this mode. Hammerspoon plays clips in-process because
    spawning `afplay` per chunk costs ~0.93s each (measured), while in-process
    playback costs ~0.21s and can be hidden by preloading the next clip.

    The caller owns the files and is responsible for deleting them.
    """
    chunks = split_chunks(text)
    if not chunks:
        print("ERROR Nothing to speak.", flush=True)
        return 1

    print(f"COUNT {len(chunks)}", flush=True)
    for i, chunk in enumerate(chunks):
        try:
            audio = synthesize(
                chunk, voice, speed, session_end=i == len(chunks) - 1
            )
        except urllib.error.HTTPError as e:
            print(f"ERROR Speech engine error {e.code}", flush=True)
            return 2
        except urllib.error.URLError as e:
            print(f"ERROR Kokoro unreachable at {HOST}: {e.reason}", flush=True)
            return 2

        path = os.path.join(STATE_DIR, f"kokoro-export-{os.getpid()}-{i:03d}.wav")
        with open(path, "wb") as fh:
            fh.write(audio)
        print(f"CHUNK {path}", flush=True)

    print("DONE", flush=True)
    return 0


def speak_streaming(text: str, voice: str | None, speed: float, verbose: bool = False) -> int:
    """Synthesize ahead of playback so speech starts fast and never gaps."""
    chunks = split_chunks(text)
    if not chunks:
        notify("Nothing to speak.")
        return 1

    try:
        playback_lock = _claim_playback()
    except RuntimeError as error:
        print(f"ERROR {error}", file=sys.stderr, flush=True)
        notify("Speech is already playing.")
        return 2
    _CANCELLED.clear()
    previous_handler = _install_cancel_handler()

    audio_q: "queue.Queue[tuple[int, str | None, str | None]]" = queue.Queue(maxsize=PREFETCH)
    initial_buffer_ready = threading.Event()
    t_start = time.time()

    def deliver(item: tuple[int, str | None, str | None]) -> bool:
        while not _CANCELLED.is_set():
            try:
                audio_q.put(item, timeout=0.1)
                return True
            except queue.Full:
                continue
        return False

    def producer() -> None:
        for i, chunk in enumerate(chunks):
            if _CANCELLED.is_set():
                return
            try:
                audio = synthesize(
                    chunk, voice, speed, session_end=i == len(chunks) - 1
                )
            except urllib.error.HTTPError as e:
                initial_buffer_ready.set()
                deliver((i, None, f"Speech engine error {e.code}"))
                return
            except urllib.error.URLError as e:
                initial_buffer_ready.set()
                deliver((i, None, f"Kokoro service unreachable at {HOST}: {e.reason}"))
                return
            except Exception as e:  # noqa: BLE001 - producer must never hang the consumer
                initial_buffer_ready.set()
                deliver((i, None, str(e)))
                return

            path = os.path.join(STATE_DIR, f"kokoro-{os.getpid()}-{i:03d}.wav")
            partial = f"{path}.part"
            try:
                with open(partial, "wb") as fh:
                    fh.write(audio)
                if _CANCELLED.is_set():
                    os.remove(partial)
                    return
                os.replace(partial, path)
            except OSError as e:
                initial_buffer_ready.set()
                deliver((i, None, str(e)))
                return
            if not deliver((i, path, None)):
                return
            if i >= 1:
                initial_buffer_ready.set()
        initial_buffer_ready.set()
        deliver((-1, None, None))  # sentinel

    producer_thread = threading.Thread(target=producer, daemon=True)
    producer_thread.start()

    first = True
    played = 0
    result = 0
    try:
        while not _CANCELLED.is_set():
            try:
                idx, path, err = audio_q.get(timeout=0.1)
            except queue.Empty:
                continue
            if err:
                print(f"ERROR speech synthesis failed: {err}", file=sys.stderr, flush=True)
                notify("Speech failed. Open Settings.")
                result = 2
                break
            if idx == -1:
                break
            if first:
                # The small first chunk cannot cover a cold synthesis of the
                # next larger chunk. Buffer two before opening the audio route.
                while not initial_buffer_ready.wait(0.1):
                    if _CANCELLED.is_set():
                        break
                if _CANCELLED.is_set():
                    break
                # Signal the UI that synthesis is done and sound is starting, so it
                # can swap its spinner for the pause control.
                print("PLAYING", flush=True)
                if verbose:
                    print(f"time to first audio: {time.time() - t_start:.2f}s "
                          f"({len(chunks)} chunks)", flush=True)
                first = False
            try:
                _play_file(path)
                played += 1
            except PlaybackCancelled:
                break
            finally:
                try:
                    os.remove(path)
                except OSError:
                    pass
        if verbose and not _CANCELLED.is_set():
            print(f"played {played} chunks in {time.time() - t_start:.2f}s")
        return result
    finally:
        was_cancelled = _CANCELLED.is_set()
        _CANCELLED.set()
        _close_output_stream(abort=was_cancelled)
        producer_thread.join(timeout=0.5)
        if was_cancelled:
            retire_tts()
        _cleanup_session_files(os.getpid())
        _clear_state_if_owned(os.getpid())
        playback_lock.release()
        if previous_handler is not None:
            signal.signal(signal.SIGTERM, previous_handler)


def main() -> int:
    ap = argparse.ArgumentParser(description="Kokoro read-aloud client")
    src = ap.add_mutually_exclusive_group()
    src.add_argument("--text")
    src.add_argument("--stdin", action="store_true",
                     help="read text from stdin (used by the hotkey)")
    src.add_argument("--clipboard", action="store_true")
    src.add_argument("--selection", action="store_true")
    src.add_argument("--stop", action="store_true")
    src.add_argument("--pause", action="store_true")
    src.add_argument("--resume", action="store_true")
    src.add_argument("--toggle", action="store_true",
                     help="pause if playing, resume if paused")
    src.add_argument("--status", action="store_true",
                     help="prints idle | playing | paused")
    src.add_argument("--voices", action="store_true")
    src.add_argument("--health", action="store_true")
    ap.add_argument("--voice", default=VOICE)
    ap.add_argument("--speed", type=float, default=SPEED)
    ap.add_argument("--verbose", action="store_true", help="print timing")
    ap.add_argument("--emit-paths", action="store_true",
                    help="synthesize only; print each WAV path for an external player")
    args = ap.parse_args()

    if args.stop:
        stop_playback()
        return 0
    if args.pause:
        return 0 if pause_playback() else 1
    if args.resume:
        return 0 if resume_playback() else 1
    if args.toggle:
        print(toggle_playback())
        return 0
    if args.status:
        if not _player_pid():
            print("idle")
        else:
            print("paused" if is_paused() else "playing")
        return 0

    try:
        if args.health:
            with urllib.request.urlopen(_url("/health"), timeout=10) as r:
                print(r.read().decode())
            return 0
        if args.voices:
            with urllib.request.urlopen(_url("/voices"), timeout=10) as r:
                print(", ".join(json.loads(r.read())["voices"]))
            return 0
    except urllib.error.URLError as e:
        print(f"ERROR Kokoro unreachable at {HOST}: {e.reason}", file=sys.stderr, flush=True)
        notify("Kokoro is not running. Open Settings.")
        return 2

    if args.text:
        text = args.text
    elif args.stdin:
        text = sys.stdin.read()
    elif args.selection:
        text = copy_selection()
    else:
        text = get_clipboard()

    text = (text or "").strip()
    if not text:
        notify("Select text, then press Read.")
        return 1

    if args.emit_paths:
        return emit_paths(text, args.voice, args.speed)
    return speak_streaming(text, args.voice, args.speed, verbose=args.verbose)


if __name__ == "__main__":
    sys.exit(main())
