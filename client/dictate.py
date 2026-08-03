#!/usr/bin/env python3
"""
Dictation client — record from the mic, transcribe locally, print the text.

Runs in the service venv (needs sounddevice + soundfile), unlike speak.py which
is deliberately stdlib-only.

Recording stops when a stop-file appears. That is used instead of signals
because the caller (Hammerspoon) terminates tasks with SIGTERM, and we need to
keep running *after* the stop in order to transcribe — a signal would race with
that work. The stop-file makes the handoff explicit.

    dictate.py --record        record until stopped, then print the transcript
    dictate.py --stop          create the stop-file (ends an in-flight recording)
    dictate.py --devices       list input devices
"""
from __future__ import annotations

import argparse
import io
import json
import os
import sys
import tempfile
import time
import urllib.error
import urllib.request

HOST = os.environ.get("KOKORO_HOST", "127.0.0.1:8123")
TOKEN = os.environ.get("KOKORO_TOKEN")
TOKEN_FILE = os.environ.get(
    "KOKORO_TOKEN_FILE", os.path.expanduser("~/.config/kokoro/token")
)

SAMPLE_RATE = 16000          # whisper's native rate — no resampling needed
MAX_SECONDS = float(os.environ.get("DICTATE_MAX_SECONDS", "120"))


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
        base = os.path.join(tempfile.gettempdir(), f"kokoro-{who}")

    os.makedirs(base, mode=0o700, exist_ok=True)
    st = os.lstat(base)
    if hasattr(os, "getuid"):
        if os.path.islink(base) or st.st_uid != os.getuid():
            raise SystemExit(f"refusing to use {base}: not a directory we own")
        os.chmod(base, 0o700)
    return base


STOPFILE = os.path.join(_state_dir(), "dictate.stop")


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


def transcribe(wav_bytes: bytes) -> dict:
    req = urllib.request.Request(
        _url("/transcribe"),
        data=wav_bytes,
        headers={"Content-Type": "audio/wav", "Content-Length": str(len(wav_bytes))},
        method="POST",
    )
    tok = _token()
    if tok:
        req.add_header("Authorization", f"Bearer {tok}")
    with urllib.request.urlopen(req, timeout=120) as resp:
        return json.loads(resp.read())


def record_until_stopped() -> bytes:
    import numpy as np
    import sounddevice as sd
    import soundfile as sf

    if os.path.exists(STOPFILE):
        os.remove(STOPFILE)

    frames: list = []

    def cb(indata, _frames, _time, status):
        if status:
            print(f"STATUS {status}", file=sys.stderr, flush=True)
        frames.append(indata.copy())

    with sd.InputStream(samplerate=SAMPLE_RATE, channels=1,
                        dtype="float32", callback=cb):
        print("RECORDING", flush=True)   # the UI waits for this
        t0 = time.time()
        while not os.path.exists(STOPFILE):
            time.sleep(0.05)
            if time.time() - t0 > MAX_SECONDS:
                print("MAXLEN", flush=True)
                break

    try:
        os.remove(STOPFILE)
    except OSError:
        pass

    if not frames:
        return b""

    audio = np.concatenate(frames, axis=0)
    buf = io.BytesIO()
    sf.write(buf, audio, SAMPLE_RATE, format="WAV", subtype="PCM_16")
    return buf.getvalue()


def main() -> int:
    ap = argparse.ArgumentParser(description="Kokoro dictation client")
    ap.add_argument("--record", action="store_true")
    ap.add_argument("--stop", action="store_true")
    ap.add_argument("--devices", action="store_true")
    args = ap.parse_args()

    if args.stop:
        open(STOPFILE, "w").close()
        return 0

    if args.devices:
        import sounddevice as sd
        print(sd.query_devices())
        return 0

    if not args.record:
        ap.print_help()
        return 1

    wav = record_until_stopped()
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
    except urllib.error.URLError as e:
        print(f"ERROR service unreachable at {HOST}: {e.reason}", flush=True)
        return 2

    text = (result.get("text") or "").strip()
    if not text:
        print("ERROR nothing recognized", flush=True)
        return 1

    # Single line so the caller can read it straight off stdout. Strip ALL
    # control characters, not just \n: Hammerspoon replays this through
    # hs.eventtap.keyStrokes into whatever app has focus, so a stray \r or \t
    # would be typed as Return/Tab — submitting a form or a shell line rather
    # than inserting text. Whisper is very unlikely to emit them; this costs
    # nothing and removes the question.
    clean = "".join(c if c.isprintable() else " " for c in text)
    print("TEXT " + " ".join(clean.split()), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
