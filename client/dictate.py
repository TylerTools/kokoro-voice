#!/usr/bin/env python3
"""
Dictation client — record from the mic, transcribe locally, print the text.

Runs in the service venv (needs sounddevice + soundfile), unlike speak.py which
is deliberately stdlib-only.

The desktop host owns one recorder process and sends session-scoped stop/cancel
commands over its stdin. Session-specific control files remain available for
manual CLI use and compatibility. Neither mechanism terminates the recorder,
because it must stay alive after capture ends to transcribe the in-memory WAV.

    dictate.py --record --session ID  record until that session is stopped
    dictate.py --stop --session ID    stop only that recording session
    dictate.py --devices       list input devices
"""
from __future__ import annotations

import argparse
import io
import json
import os
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

HOST = os.environ.get("KOKORO_HOST", "127.0.0.1:8125")
TOKEN = os.environ.get("KOKORO_TOKEN")
TOKEN_FILE = os.environ.get(
    "KOKORO_TOKEN_FILE", os.path.expanduser("~/.config/kokoro-voice-2-1/token")
)

MAX_SECONDS = float(os.environ.get("DICTATE_MAX_SECONDS", "120"))
PREVIEW_INTERVAL = float(os.environ.get("DICTATE_PREVIEW_INTERVAL", "0.55"))
PREVIEW_MAX_SECONDS = float(os.environ.get("DICTATE_PREVIEW_WINDOW", "18"))
PREVIEW_START_SECONDS = 3.0


def _state_dir() -> str:
    """Same private 0700 dir speak.py uses — see the rationale there.

    The stop-file is a control channel for the microphone: whoever can create
    it can cut a recording short. macOS gettempdir() is already per-user, but
    on Linux/Windows it is the shared /tmp, so own the directory explicitly
    rather than inheriting whatever /tmp allows.
    """
    override = os.environ.get("KOKORO_STATE_DIR")
    if override:
        base = override
    else:
        try:
            who = str(os.getuid())
        except AttributeError:  # Windows
            who = os.environ.get("USERNAME", "user")
        base = os.path.join(tempfile.gettempdir(), f"kokoro-voice-2-1-{who}")

    os.makedirs(base, mode=0o700, exist_ok=True)
    st = os.lstat(base)
    if hasattr(os, "getuid"):
        if os.path.islink(base) or st.st_uid != os.getuid():
            raise SystemExit(f"refusing to use {base}: not a directory we own")
        os.chmod(base, 0o700)
    return base


STATE_DIR = _state_dir()


def _safe_session(value: str) -> str:
    """Accept UUID-like opaque IDs without allowing path traversal."""
    if not value or len(value) > 80 or not all(c.isalnum() or c in "-_" for c in value):
        raise ValueError("invalid session id")
    return value


def stopfile(session: str) -> str:
    return os.path.join(STATE_DIR, f"dictate-{_safe_session(session)}.stop")


def cancelfile(session: str) -> str:
    return os.path.join(STATE_DIR, f"dictate-{_safe_session(session)}.cancel")


def apply_control_line(
    line: str,
    session: str,
    stop_event: threading.Event,
    cancel_event: threading.Event,
) -> bool:
    """Apply one exact, session-scoped control record from the desktop host."""
    try:
        message = json.loads(line)
    except (TypeError, json.JSONDecodeError):
        return False
    if not isinstance(message, dict) or message.get("session") != session:
        return False
    command = message.get("command")
    if command == "stop":
        stop_event.set()
        return True
    if command == "cancel":
        cancel_event.set()
        return True
    return False


def listen_for_controls(
    session: str,
    stop_event: threading.Event,
    cancel_event: threading.Event,
) -> None:
    """Read host controls until stdin closes or the session is commanded."""
    for line in sys.stdin:
        if apply_control_line(line, session, stop_event, cancel_event):
            return


def _token() -> str | None:
    if TOKEN:
        return TOKEN
    try:
        with open(TOKEN_FILE) as fh:
            return fh.read().strip() or None
    except OSError:
        return None


def _url(path: str) -> str:
    host = HOST if "://" in HOST else f"http://{HOST}"
    return f"{host.rstrip('/')}{path}"


def transcribe(wav_bytes: bytes, timeout: float = 120) -> dict:
    req = urllib.request.Request(
        _url("/transcribe"),
        data=wav_bytes,
        headers={"Content-Type": "audio/wav", "Content-Length": str(len(wav_bytes))},
        method="POST",
    )
    tok = _token()
    if tok:
        req.add_header("Authorization", f"Bearer {tok}")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read())


def _wav_bytes(frames, np, sf, sample_rate: int,
               max_seconds: float | None = None) -> bytes:
    audio = np.concatenate(frames, axis=0)
    if max_seconds is not None:
        audio = audio[-int(sample_rate * max_seconds):]
    buf = io.BytesIO()
    sf.write(buf, audio, sample_rate, format="WAV", subtype="PCM_16")
    return buf.getvalue()


def available_input_device(requested: int | str | None, devices) -> int | str | None:
    """Keep a preferred mic only while it is present as an input device."""
    if requested is None:
        return None
    if isinstance(requested, int):
        if 0 <= requested < len(devices) and devices[requested].get("max_input_channels", 0) > 0:
            return requested
        return None
    if any(
        item.get("name") == requested and item.get("max_input_channels", 0) > 0
        for item in devices
    ):
        return requested
    return None


class MacNativeInputStream:
    """Capture the macOS default microphone through AVAudioEngine.

    PortAudio can block indefinitely in AudioDeviceStart even while the system
    microphone meter is receiving sound. AVAudioEngine uses the working native
    capture path and lets macOS own the input route.
    """

    def __init__(self, callback, np):
        import ctypes
        import objc

        # Quartz/Vision already installs PyObjC. Load AVFAudio directly so an
        # app update needs no new Python package or network access. The tap
        # selector's block signature must be registered for this one call.
        ctypes.CDLL("/System/Library/Frameworks/AVFAudio.framework/AVFAudio")
        objc.registerMetaDataForSelector(
            b"AVAudioNode",
            b"installTapOnBus:bufferSize:format:block:",
            {"arguments": {5: {"callable": {
                "retval": {"type": b"v"},
                "arguments": (
                    {"type": b"^v", "null_accepted": True},
                    {"type": b"@"},
                    {"type": b"@"},
                ),
            }}}},
        )
        self.engine = objc.lookUpClass("AVAudioEngine").alloc().init()
        self.node = self.engine.inputNode()
        self.format = self.node.inputFormatForBus_(0)
        self.sample_rate = int(round(self.format.sampleRate()))
        self.started = False

        def tap(buffer, _when):
            count = int(buffer.frameLength())
            if count:
                # Copy inside the tap: AVAudioEngine reuses its buffer as soon
                # as this callback returns.
                channel = ctypes.cast(
                    buffer.floatChannelData().pointerAsInteger,
                    ctypes.POINTER(ctypes.POINTER(ctypes.c_float)),
                )[0]
                samples = np.ctypeslib.as_array(channel, shape=(count,)).reshape(-1, 1)
                callback(samples, count, None, None)

        self.tap = tap
        self.node.installTapOnBus_bufferSize_format_block_(
            0, 1024, self.format, self.tap
        )

    def start(self):
        result = self.engine.startAndReturnError_(None)
        ok, error = result if isinstance(result, tuple) else (result, None)
        if not ok:
            raise RuntimeError(f"native microphone start failed: {error}")
        self.started = True

    def __enter__(self):
        return self

    def __exit__(self, _type, _value, _traceback):
        if self.started:
            self.engine.stop()
        self.node.removeTapOnBus_(0)
        # AVAudioEngine releases tap blocks on its own queue. Let that finish
        # before Python finalizes the callback and its Objective-C bridge.
        time.sleep(0.3)


def record_until_stopped(
    session: str, device: int | str | None = None, live_preview: bool = False
) -> bytes:
    print("MIC_PHASE imports", flush=True)
    import numpy as np
    import soundfile as sf

    control = stopfile(session)
    cancel = cancelfile(session)
    stop_event = threading.Event()
    cancel_event = threading.Event()
    threading.Thread(
        target=listen_for_controls,
        args=(session, stop_event, cancel_event),
        daemon=True,
    ).start()

    frames: list = []
    preview_stop = threading.Event()
    last_voice = [time.monotonic()]

    def cb(indata, _frames, _time, status):
        if status:
            print(f"STATUS {status}", file=sys.stderr, flush=True)
        frames.append(indata.copy())
        if float(abs(indata).max()) > 0.012:
            last_voice[0] = time.monotonic()

    # Opening the microphone can BLOCK INDEFINITELY when the host app has no
    # microphone permission — no error, no prompt, just a hang that looks like
    # the app has frozen. Bound it, so a permissions problem reports itself.
    try:
        print("MIC_PHASE device", flush=True)
        if sys.platform == "darwin" and device is None:
            print("MIC_PHASE open", flush=True)
            stream = MacNativeInputStream(cb, np)
            sample_rate = stream.sample_rate
        else:
            import sounddevice as sd
            if device is not None:
                available = available_input_device(device, sd.query_devices())
                if available is None:
                    print("STATUS preferred microphone unavailable; using system default",
                          file=sys.stderr, flush=True)
                device = available
            sample_rate = int(round(sd.query_devices(device, "input")["default_samplerate"]))
            print("MIC_PHASE open", flush=True)
            stream = sd.InputStream(device=device, samplerate=sample_rate, channels=1,
                                    dtype="float32", callback=cb)
        print("MIC_PHASE start", flush=True)
        stream.start()
    except Exception as e:  # noqa: BLE001
        print(f"ERROR microphone unavailable ({e}) — grant Microphone access",
              flush=True)
        return b""

    with stream:
        print("RECORDING", flush=True)   # the UI waits for this
        if live_preview:
            def preview_worker():
                last_samples = 0
                # A short take needs one authoritative pass. Starting a preview
                # first makes its final pass wait behind redundant model work.
                next_preview = time.monotonic() + max(
                    PREVIEW_START_SECONDS, PREVIEW_INTERVAL
                )
                while not preview_stop.wait(max(0, next_preview - time.monotonic())):
                    snapshot = list(frames)
                    samples = sum(len(frame) for frame in snapshot)
                    if samples - last_samples < sample_rate * 0.6:
                        continue
                    last_samples = samples
                    try:
                        result = transcribe(
                            _wav_bytes(snapshot, np, sf, sample_rate,
                                       PREVIEW_MAX_SECONDS), timeout=15
                        )
                        text = " ".join((result.get("text") or "").split())
                        clean = "".join(c if c.isprintable() else " " for c in text)
                        if clean:
                            kind = "PREVIEW_FULL" if samples <= sample_rate * PREVIEW_MAX_SECONDS else "PREVIEW_ROLLING"
                            print(kind + " " + clean, flush=True)
                    except Exception:
                        # Preview is advisory. The authoritative final pass
                        # below still owns success/failure and must stay quiet.
                        pass
                    # Start another pass as soon as the interval permits. This
                    # deliberately does not add a second full delay after a
                    # slow inference.
                    next_preview = max(next_preview + PREVIEW_INTERVAL,
                                       time.monotonic())

            threading.Thread(target=preview_worker, daemon=True).start()
        t0 = time.time()
        inactivity_warned = False
        while (
            not stop_event.is_set()
            and not cancel_event.is_set()
            and not os.path.exists(control)
            and not os.path.exists(cancel)
        ):
            time.sleep(0.05)
            silent_for = time.monotonic() - last_voice[0]
            if silent_for < 3:
                inactivity_warned = False
            elif silent_for > 20 and not inactivity_warned:
                print("INACTIVITY_WARNING", flush=True)
                inactivity_warned = True
            if time.time() - t0 > MAX_SECONDS:
                print("MAXLEN", flush=True)
                break

    preview_stop.set()

    if cancel_event.is_set() or os.path.exists(cancel):
        try:
            os.remove(cancel)
        except OSError:
            pass
        print("CANCELLED", flush=True)
        return b""

    try:
        os.remove(control)
    except OSError:
        pass

    if not frames:
        return b""

    return _wav_bytes(frames, np, sf, sample_rate)


def main() -> int:
    ap = argparse.ArgumentParser(description="Kokoro dictation client")
    ap.add_argument("--record", action="store_true")
    ap.add_argument("--stop", action="store_true")
    ap.add_argument("--cancel", action="store_true")
    ap.add_argument("--devices", action="store_true")
    ap.add_argument("--probe-device", action="store_true")
    ap.add_argument("--session")
    ap.add_argument("--device")
    ap.add_argument("--live-preview", action="store_true")
    args = ap.parse_args()

    if args.stop:
        if not args.session:
            ap.error("--stop requires --session")
        open(stopfile(args.session), "w").close()
        return 0

    if args.cancel:
        if not args.session:
            ap.error("--cancel requires --session")
        open(cancelfile(args.session), "w").close()
        return 0

    if args.devices:
        import sounddevice as sd
        default = sd.default.device[0]
        devices = []
        for index, item in enumerate(sd.query_devices()):
            if item.get("max_input_channels", 0) > 0:
                devices.append({
                    "id": index,
                    "name": item.get("name", f"Input {index}"),
                    "default": index == default,
                    "channels": item.get("max_input_channels", 0),
                })
        print(json.dumps(devices))
        return 0

    if args.probe_device:
        device = int(args.device) if args.device and args.device.isdigit() else args.device
        try:
            if sys.platform == "darwin" and device is None:
                import numpy as np
                with MacNativeInputStream(lambda *_: None, np) as stream:
                    stream.start()
                    time.sleep(0.12)
            else:
                import sounddevice as sd
                sample_rate = int(round(sd.query_devices(device, "input")["default_samplerate"]))
                with sd.InputStream(device=device, samplerate=sample_rate, channels=1,
                                    dtype="float32"):
                    time.sleep(0.12)
            print(json.dumps({"ok": True}))
            return 0
        except Exception as error:  # noqa: BLE001
            print(json.dumps({"ok": False, "code": "microphone-unavailable",
                              "message": str(error)[:160]}))
            return 2

    if not args.record:
        ap.print_help()
        return 1

    if not args.session:
        ap.error("--record requires --session")

    device = None
    if args.device is not None:
        device = int(args.device) if args.device.isdigit() else args.device
    wav = record_until_stopped(args.session, device, args.live_preview)
    if not wav:
        print("ERROR no audio captured", flush=True)
        return 1

    import soundfile as sf
    seconds = sf.info(io.BytesIO(wav)).duration
    print(f"TRANSCRIBING {seconds:.1f}", flush=True)   # UI switches to spinner

    try:
        result = transcribe(wav)
    except urllib.error.HTTPError as e:
        print(f"ERROR service {e.code}: {e.read().decode(errors='replace')[:120]}",
              flush=True)
        return 2
    except urllib.error.URLError as first_error:
        # Keep WAV bytes in memory and give the parent one chance to restart
        # the local engine. Audio is never written to disk.
        print("RETRYING engine", flush=True)
        time.sleep(2.0)
        try:
            result = transcribe(wav)
        except (urllib.error.URLError, urllib.error.HTTPError) as error:
            print(f"ERROR service retry failed: {error or first_error}", flush=True)
            return 2

    text = (result.get("text") or "").strip()
    if not text:
        print("ERROR nothing recognized", flush=True)
        return 1

    audio_seconds = float(result.get("audio_seconds") or seconds)
    transcribe_seconds = float(result.get("transcribe_seconds") or 0)
    print("METRICS " + json.dumps({
        "audio_seconds": round(audio_seconds, 3),
        "transcribe_seconds": round(transcribe_seconds, 3),
        "realtime_ratio": round(transcribe_seconds / max(audio_seconds, 0.001), 4),
        "preview_interval": PREVIEW_INTERVAL,
        "preview_start_seconds": PREVIEW_START_SECONDS,
    }, separators=(",", ":")), flush=True)

    # Single line so the desktop parser can treat stdout as one-record-per-line.
    # Strip ALL control characters, not just \n: the host injects this text into
    # the focused control, so a stray \r or \t could submit a form or shell line
    # instead of inserting text. Whisper is unlikely to emit them; the boundary
    # still enforces the invariant rather than relying on that assumption.
    clean = "".join(c if c.isprintable() else " " for c in text)
    print("TEXT " + " ".join(clean.split()), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
