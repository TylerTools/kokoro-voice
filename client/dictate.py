#!/usr/bin/env python3
"""
Dictation client — record from the mic, transcribe locally, print the text.

Runs in the service venv (needs sounddevice + soundfile), unlike speak.py which
is deliberately stdlib-only.

Recording stops when a session-specific stop-file appears. That is used instead
of signals because the desktop host must keep this child alive *after* capture
ends so it can transcribe; terminating the recorder would race with that work.
The stop-file makes the ownership handoff explicit and session-safe.

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

SAMPLE_RATE = 16000          # whisper's native rate — no resampling needed
MAX_SECONDS = float(os.environ.get("DICTATE_MAX_SECONDS", "120"))
PREVIEW_INTERVAL = float(os.environ.get("DICTATE_PREVIEW_INTERVAL", "0.55"))
PREVIEW_MAX_SECONDS = float(os.environ.get("DICTATE_PREVIEW_WINDOW", "18"))


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


def _wav_bytes(frames, np, sf, max_seconds: float | None = None) -> bytes:
    audio = np.concatenate(frames, axis=0)
    if max_seconds is not None:
        audio = audio[-int(SAMPLE_RATE * max_seconds):]
    buf = io.BytesIO()
    sf.write(buf, audio, SAMPLE_RATE, format="WAV", subtype="PCM_16")
    return buf.getvalue()


def record_until_stopped(
    session: str, device: int | str | None = None, live_preview: bool = False
) -> bytes:
    import numpy as np
    import sounddevice as sd
    import soundfile as sf

    control = stopfile(session)
    cancel = cancelfile(session)

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
        stream = sd.InputStream(device=device, samplerate=SAMPLE_RATE, channels=1,
                                dtype="float32", callback=cb)
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
                next_preview = time.monotonic() + PREVIEW_INTERVAL
                while not preview_stop.wait(max(0, next_preview - time.monotonic())):
                    snapshot = list(frames)
                    samples = sum(len(frame) for frame in snapshot)
                    if samples - last_samples < SAMPLE_RATE * 0.6:
                        continue
                    last_samples = samples
                    try:
                        result = transcribe(
                            _wav_bytes(snapshot, np, sf, PREVIEW_MAX_SECONDS), timeout=15
                        )
                        text = " ".join((result.get("text") or "").split())
                        clean = "".join(c if c.isprintable() else " " for c in text)
                        if clean:
                            kind = "PREVIEW_FULL" if samples <= SAMPLE_RATE * PREVIEW_MAX_SECONDS else "PREVIEW_ROLLING"
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
        while not os.path.exists(control) and not os.path.exists(cancel):
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

    if os.path.exists(cancel):
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

    return _wav_bytes(frames, np, sf)


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
        import sounddevice as sd
        device = int(args.device) if args.device and args.device.isdigit() else args.device
        try:
            with sd.InputStream(device=device, samplerate=SAMPLE_RATE, channels=1,
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

    seconds = len(wav) / (SAMPLE_RATE * 2)
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
