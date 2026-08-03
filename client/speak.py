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
HOST = os.environ.get("KOKORO_HOST", "127.0.0.1:8123")


def _load_token() -> str | None:
    """Env first, then the shared 0600 token file the server reads.

    The file exists so the hotkey paths (Hammerspoon, Automator) don't each
    need the secret plumbed into their environment.
    """
    tok = (os.environ.get("KOKORO_TOKEN") or "").strip()
    if tok:
        return tok
    path = os.environ.get(
        "KOKORO_TOKEN_FILE", os.path.expanduser("~/.config/kokoro/token")
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
    else:
        try:
            who = str(os.getuid())
        except AttributeError:  # Windows
            who = os.environ.get("USERNAME", "user")
        base = os.path.join(tempfile.gettempdir(), f"kokoro-{who}")

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


def _url(path: str) -> str:
    host = HOST if "://" in HOST else f"http://{HOST}"
    return f"{host.rstrip('/')}{path}"


# ── text acquisition ────────────────────────────────────────────────────────
def get_clipboard() -> str:
    if IS_MAC:
        return subprocess.run(["pbpaste"], capture_output=True, text=True).stdout
    if IS_WIN:
        return subprocess.run(
            ["powershell", "-NoProfile", "-Command", "Get-Clipboard"],
            capture_output=True, text=True,
        ).stdout
    return subprocess.run(["xclip", "-o", "-selection", "clipboard"],
                          capture_output=True, text=True).stdout


def copy_selection() -> str:
    previous = get_clipboard()
    if IS_MAC:
        subprocess.run(
            ["osascript", "-e",
             'tell application "System Events" to keystroke "c" using command down'],
            capture_output=True,
        )
    elif IS_WIN:
        subprocess.run(
            ["powershell", "-NoProfile", "-Command",
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
def synthesize(text: str, voice: str | None, speed: float) -> bytes:
    payload = {"text": text, "speed": speed}
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


# ── playback control ────────────────────────────────────────────────────────
def _write_state(player_pid: int | None) -> None:
    try:
        with open(STATEFILE, "w") as fh:
            fh.write(f"{os.getpid()}\n{player_pid or ''}\n")
    except OSError:
        pass


def stop_playback() -> None:
    """Kill both the player and any pipeline process driving it."""
    try:
        with open(STATEFILE) as fh:
            lines = fh.read().splitlines()
    except OSError:
        return

    for raw in reversed(lines):  # player first, then the pipeline
        raw = raw.strip()
        if not raw:
            continue
        try:
            pid = int(raw)
        except ValueError:
            continue
        if pid == os.getpid():
            continue
        try:
            os.kill(pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass

    try:
        os.remove(STATEFILE)
    except OSError:
        pass


def _player_pid() -> int | None:
    """PID of the process currently producing sound (line 2 of the state file)."""
    try:
        with open(STATEFILE) as fh:
            lines = fh.read().splitlines()
        return int(lines[1].strip()) if len(lines) > 1 and lines[1].strip() else None
    except (OSError, ValueError, IndexError):
        return None


def is_paused() -> bool:
    """True if the player process is in the stopped (T) state."""
    pid = _player_pid()
    if not pid:
        return False
    out = subprocess.run(["ps", "-p", str(pid), "-o", "stat="],
                         capture_output=True, text=True).stdout.strip()
    return out.startswith("T")


def pause_playback() -> bool:
    """SIGSTOP the player — a real pause; audio resumes exactly where it left off."""
    pid = _player_pid()
    if not pid:
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
    try:
        os.kill(pid, signal.SIGCONT)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def toggle_playback() -> str:
    if not _player_pid():
        return "idle"
    if is_paused():
        resume_playback()
        return "playing"
    pause_playback()
    return "paused"


def _play_file(path: str) -> None:
    if IS_MAC:
        proc = subprocess.Popen(["afplay", path])
    elif IS_WIN:
        proc = subprocess.Popen([
            "powershell", "-NoProfile", "-Command",
            f"(New-Object Media.SoundPlayer '{path}').PlaySync()",
        ])
    else:
        proc = subprocess.Popen(["aplay", path])
    _write_state(proc.pid)
    proc.wait()


def notify(message: str) -> None:
    sys.stderr.write(message + "\n")
    if IS_MAC:
        subprocess.run(
            ["osascript", "-e",
             f'display notification {json.dumps(message)} with title "Kokoro TTS"'],
            capture_output=True,
        )


def emit_paths(text: str, voice: str | None, speed: float) -> int:
    """Synthesize chunks and print each WAV path as it becomes ready.

    Used by the Hammerspoon front-end, which plays the clips IN-PROCESS via
    hs.sound. That matters: spawning `afplay` per chunk costs ~0.93s each
    (measured), while in-process playback costs ~0.21s and can be eliminated
    entirely by preloading the next clip while the current one plays.

    The caller owns the files and is responsible for deleting them.
    """
    chunks = split_chunks(text)
    if not chunks:
        print("ERROR Nothing to speak.", flush=True)
        return 1

    print(f"COUNT {len(chunks)}", flush=True)
    for i, chunk in enumerate(chunks):
        try:
            audio = synthesize(chunk, voice, speed)
        except urllib.error.HTTPError as e:
            print(f"ERROR Kokoro error {e.code}", flush=True)
            return 2
        except urllib.error.URLError as e:
            print(f"ERROR Kokoro unreachable at {HOST}: {e.reason}", flush=True)
            return 2

        path = os.path.join(STATE_DIR, f"kokoro-{os.getpid()}-{i:03d}.wav")
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

    stop_playback()  # a second press interrupts rather than overlapping
    _write_state(None)

    audio_q: "queue.Queue[tuple[int, str | None, str | None]]" = queue.Queue(maxsize=PREFETCH)
    t_start = time.time()

    def producer() -> None:
        for i, chunk in enumerate(chunks):
            try:
                audio = synthesize(chunk, voice, speed)
            except urllib.error.HTTPError as e:
                audio_q.put((i, None, f"Kokoro error {e.code}"))
                return
            except urllib.error.URLError as e:
                audio_q.put((i, None, f"Kokoro service unreachable at {HOST}: {e.reason}"))
                return
            except Exception as e:  # noqa: BLE001 - producer must never hang the consumer
                audio_q.put((i, None, str(e)))
                return

            path = os.path.join(STATE_DIR, f"kokoro-{os.getpid()}-{i:03d}.wav")
            try:
                with open(path, "wb") as fh:
                    fh.write(audio)
            except OSError as e:
                audio_q.put((i, None, str(e)))
                return
            audio_q.put((i, path, None))
        audio_q.put((-1, None, None))  # sentinel

    threading.Thread(target=producer, daemon=True).start()

    first = True
    played = 0
    while True:
        idx, path, err = audio_q.get()
        if err:
            notify(err)
            return 2
        if idx == -1:
            break
        if first:
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
        finally:
            try:
                os.remove(path)
            except OSError:
                pass

    try:
        os.remove(STATEFILE)
    except OSError:
        pass
    if verbose:
        print(f"played {played} chunks in {time.time() - t_start:.2f}s")
    return 0


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
        notify(f"Kokoro service unreachable at {HOST}: {e.reason}")
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
        notify("Nothing to speak (no selection or empty clipboard).")
        return 1

    if args.emit_paths:
        return emit_paths(text, args.voice, args.speed)
    return speak_streaming(text, args.voice, args.speed, verbose=args.verbose)


if __name__ == "__main__":
    sys.exit(main())
