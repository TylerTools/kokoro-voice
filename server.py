"""
kokoro-voice service — local text-to-speech and speech-to-text.

Owns both models and exposes them over HTTP. Thin per-platform clients POST text
here and play the returned audio, or POST audio and receive a transcript.

Fully local: no API keys, no outbound network calls at runtime.
"""
import hmac
import io
import os
import threading
import time

import soundfile as sf
from fastapi import FastAPI, Header, HTTPException, Request
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel, Field

from kokoro_onnx import Kokoro

HERE = os.path.dirname(os.path.abspath(__file__))
# fp16 measured faster than fp32 on this M4 (0.50s vs 0.59s first-chunk) and is
# 141MB smaller. int8 was benchmarked too and is much SLOWER (1.27s) — ARM lacks
# good int8 kernels for this graph. Do not "optimize" by switching to int8.
MODEL = os.path.join(HERE, "models", os.environ.get("KOKORO_MODEL", "kokoro-v1.0.fp16.onnx"))
VOICES = os.path.join(HERE, "models", "voices-v1.0.bin")

DEFAULT_VOICE = os.environ.get("KOKORO_VOICE", "af_heart")
DEFAULT_LANG = os.environ.get("KOKORO_LANG", "en-us")

# Shared secret. Read from KOKORO_TOKEN, else from a 0600 file that the client
# reads too — a file avoids plumbing the secret through launchd *and*
# Hammerspoon *and* the Automator workflow, and keeps it out of `ps` and logs.
TOKEN_FILE = os.environ.get(
    "KOKORO_TOKEN_FILE", os.path.expanduser("~/.config/kokoro/token")
)


def _load_token() -> str | None:
    tok = (os.environ.get("KOKORO_TOKEN") or "").strip()
    if tok:
        return tok
    try:
        with open(TOKEN_FILE) as fh:
            return fh.read().strip() or None
    except OSError:
        return None


AUTH_TOKEN = _load_token()
# Guard against a pasted-whole-document request pinning the box.
MAX_CHARS = int(os.environ.get("KOKORO_MAX_CHARS", "20000"))
# Reject oversized bodies BEFORE Starlette buffers and Pydantic parses them.
# MAX_CHARS alone is enforced too late: a 10MB body is fully read into memory
# and only then rejected. 20k chars is <=256KB even worst-case JSON-escaped.
MAX_BODY_BYTES = int(os.environ.get("KOKORO_MAX_BODY", str(256 * 1024)))
# Dictation audio: 16kHz mono PCM16 is 32KB/s, so 8MB ~= 4 minutes of speech.
MAX_AUDIO_BYTES = int(os.environ.get("KOKORO_MAX_AUDIO", str(8 * 1024 * 1024)))

# Speech-to-text. mlx-whisper large-v3-turbo on the Apple GPU measured 0.86s for
# 6.85s of audio (7.9x realtime) vs 2.48s for faster-whisper large-v3-turbo
# float32 on CPU — large-model accuracy at small-model latency. int8 was slower
# again (4.30s), the same ARM int8 trap seen with Kokoro. Do not "optimize" to
# int8 or move this to CPU.
#
# PINNED, and the plist sets HF_HUB_OFFLINE=1. Left unpinned, huggingface_hub
# contacts the CDN on every load to resolve "latest" — verified: an outbound
# HTTPS socket to the HF CDN was open from this service. That both breaks the
# "fully local" promise above and means an upstream repo change silently swaps
# the weights that transcribe your microphone. Cached revision below; to move
# it deliberately, unset HF_HUB_OFFLINE, pull, then re-pin to the new hash.
WHISPER_REVISION = "a4aaeec0636e6fef84abdcbe3544cb2bf7e9f6fb"
WHISPER_REPO = os.environ.get("WHISPER_REPO", "mlx-community/whisper-large-v3-turbo")
WHISPER_LANG = os.environ.get("WHISPER_LANG", "en")

app = FastAPI(title="Kokoro TTS", version="1.0")

# onnxruntime sessions are not safe to drive concurrently from multiple
# threads; serialize synthesis so two hotkey presses can't corrupt each other.
_lock = threading.Lock()
_kokoro: Kokoro | None = None


def get_kokoro() -> Kokoro:
    global _kokoro
    if _kokoro is None:
        with _lock:
            if _kokoro is None:
                _kokoro = Kokoro(MODEL, VOICES)
    return _kokoro


class SpeakRequest(BaseModel):
    text: str = Field(..., min_length=1)
    voice: str | None = None
    speed: float = Field(1.0, gt=0.1, le=3.0)
    lang: str | None = None


def _check_auth(authorization: str | None) -> None:
    if not AUTH_TOKEN:
        return
    expected = f"Bearer {AUTH_TOKEN}"
    # compare_digest, not `!=`: a plain string compare short-circuits on the
    # first differing byte and leaks the token a character at a time to anyone
    # who can time the response.
    if authorization is None or not hmac.compare_digest(authorization, expected):
        raise HTTPException(status_code=401, detail="unauthorized")


@app.middleware("http")
async def _limit_body(request: Request, call_next):
    """Cap request size at the door.

    Chunked requests carry no Content-Length; our own clients always send one,
    so an absent length on a write is rejected rather than waved through.
    """
    if request.method in ("POST", "PUT", "PATCH"):
        raw = request.headers.get("content-length")
        if raw is None:
            return JSONResponse({"detail": "content-length required"}, status_code=411)
        try:
            declared = int(raw)
        except ValueError:
            return JSONResponse({"detail": "bad content-length"}, status_code=400)
        # Audio uploads are legitimately far larger than text; 256KB would
        # reject even a few seconds of speech.
        cap = MAX_AUDIO_BYTES if request.url.path == "/transcribe" else MAX_BODY_BYTES
        if declared > cap:
            return JSONResponse(
                {"detail": f"body too large: {declared} bytes (max {cap})"},
                status_code=413,
            )
    return await call_next(request)


@app.on_event("startup")
def _warm() -> None:
    if not AUTH_TOKEN:
        print(
            "[kokoro] WARNING: no auth token — every reachable host can synthesize. "
            f"Write one to {TOKEN_FILE} (chmod 600) or set KOKORO_TOKEN.",
            flush=True,
        )
    t0 = time.time()
    get_kokoro()
    print(f"[kokoro] model loaded in {time.time() - t0:.2f}s", flush=True)
    # Background so TTS is usable immediately; STT is warm a few seconds later.
    threading.Thread(target=_warm_whisper, daemon=True).start()


@app.get("/health")
def health() -> dict:
    k = get_kokoro()
    return {
        "status": "ok",
        "voices": len(k.get_voices()),
        "default_voice": DEFAULT_VOICE,
        "auth_required": bool(AUTH_TOKEN),
        "stt_ready": _whisper_ready,
    }


@app.get("/voices")
def voices() -> dict:
    return {"voices": sorted(get_kokoro().get_voices())}


@app.post("/speak")
def speak(req: SpeakRequest, authorization: str | None = Header(default=None)) -> Response:
    _check_auth(authorization)

    text = req.text.strip()
    if not text:
        raise HTTPException(status_code=400, detail="empty text")
    if len(text) > MAX_CHARS:
        raise HTTPException(
            status_code=413,
            detail=f"text too long: {len(text)} chars (max {MAX_CHARS})",
        )

    k = get_kokoro()
    voice = req.voice or DEFAULT_VOICE
    if voice not in k.get_voices():
        raise HTTPException(status_code=400, detail=f"unknown voice: {voice}")

    t0 = time.time()
    with _lock:
        samples, sample_rate = k.create(
            text, voice=voice, speed=req.speed, lang=req.lang or DEFAULT_LANG
        )
    synth_s = time.time() - t0

    buf = io.BytesIO()
    sf.write(buf, samples, sample_rate, format="WAV", subtype="PCM_16")
    audio = buf.getvalue()

    duration = len(samples) / sample_rate
    print(
        f"[kokoro] {len(text)} chars -> {duration:.2f}s audio in {synth_s:.2f}s "
        f"({duration / synth_s:.1f}x realtime, voice={voice})",
        flush=True,
    )

    return Response(
        content=audio,
        media_type="audio/wav",
        headers={
            "X-Audio-Duration": f"{duration:.3f}",
            "X-Synth-Seconds": f"{synth_s:.3f}",
            "X-Voice": voice,
        },
    )


# ── speech to text ──────────────────────────────────────────────────────────
_whisper_lock = threading.Lock()
_whisper_ready = False

# Stock Whisper filler, emitted when it is handed silence. These come from the
# YouTube-caption data it was trained on.
_HALLUCINATION_ARTIFACTS = {
    "thank you.",
    "thank you",
    "thanks for watching.",
    "thanks for watching!",
    "thank you for watching.",
    "thank you for watching!",
    "please subscribe.",
    "please subscribe to my channel.",
    "subtitles by the amara.org community",
    "subtitles by the amara.org community.",
    "you",
    "you.",
    "bye.",
    "bye bye.",
    ".",
}


def _trim_silence(audio, sr: int = 16000, floor_db: float = -45.0,
                  pad_s: float = 0.25):
    """Trim leading/trailing silence before Whisper ever sees it.

    This is the real fix for dictation hallucination. Push-to-talk always
    captures dead air — you stop speaking a beat before you release the key —
    and silence is precisely what makes Whisper emit training-set filler.
    Measured: 10s of speech + 75s of trailing silence decoded to the correct
    106 chars followed by "sent" repeated ~180 times. Removing the silence
    removes the trigger, and decoding less audio is faster too.

    The threshold is relative to the loudest frame, so it survives whatever the
    mic gain happens to be, with an absolute floor so an all-silence take is
    reported as empty rather than "trimmed" down to its own noise.
    """
    import numpy as np

    if len(audio) < sr // 4:
        return audio

    frame = int(sr * 0.02)                       # 20ms frames
    n = len(audio) // frame
    if n == 0:
        return audio
    rms = np.sqrt((audio[:n * frame].reshape(n, frame) ** 2).mean(axis=1) + 1e-12)

    # Absolute floor: real speech sits around 0.01-0.1 RMS, so 1e-3 (~-60dBFS)
    # is comfortably below anything voiced but above a quiet room.
    if float(rms.max()) < 1e-3:
        return audio[:0]

    voiced = np.where(rms > float(rms.max()) * (10.0 ** (floor_db / 20.0)))[0]
    if len(voiced) == 0:
        return audio[:0]

    pad = int(pad_s * sr)
    start = max(0, int(voiced[0]) * frame - pad)
    end = min(len(audio), (int(voiced[-1]) + 1) * frame + pad)
    return audio[start:end]


def _drop_hallucinated(text: str) -> str:
    """Drop a transcript that is ONLY a known silence artifact.

    Deliberately an exact whole-string match, not substring removal: people do
    genuinely say "thank you", and silently editing the middle of a real
    transcript would be a worse failure than leaving filler in. This only fires
    when the entire result is filler, which is the silence case.
    """
    if text.strip().lower() in _HALLUCINATION_ARTIFACTS:
        print(f"[whisper] dropped silence artifact ({len(text)} chars)", flush=True)
        return ""
    return text


def _warm_whisper() -> None:
    """Load the Whisper weights once, off the request path.

    First use otherwise pays ~2.6s of model load on top of transcription, which
    is the difference between dictation feeling instant and feeling broken.
    """
    global _whisper_ready
    try:
        import numpy as np
        import mlx_whisper

        with _whisper_lock:
            mlx_whisper.transcribe(
                np.zeros(16000, dtype="float32"),
                path_or_hf_repo=WHISPER_REPO,
                language=WHISPER_LANG,
            )
            _whisper_ready = True
        print("[whisper] model warm", flush=True)
    except Exception as e:  # noqa: BLE001 - never let STT break the TTS service
        print(f"[whisper] warm-up failed: {type(e).__name__}: {e}", flush=True)


@app.post("/transcribe")
async def transcribe(request: Request,
                     authorization: str | None = Header(default=None)) -> dict:
    """Raw WAV bytes in, text out.

    Audio is sent as a WAV body rather than multipart to keep the client
    dependency-free — the Windows client has to do this with stdlib too.
    """
    _check_auth(authorization)

    import numpy as np
    import mlx_whisper

    raw = await request.body()
    if not raw:
        raise HTTPException(status_code=400, detail="empty body")

    try:
        audio, sr = sf.read(io.BytesIO(raw), dtype="float32")
    except Exception as e:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"undecodable audio: {e}") from e

    if audio.ndim > 1:                      # whisper wants mono
        audio = audio.mean(axis=1)
    if sr != 16000:                         # and 16kHz
        n = int(len(audio) * 16000 / sr)
        audio = np.interp(
            np.linspace(0, len(audio) - 1, n), np.arange(len(audio)), audio
        ).astype("float32")

    raw_duration = len(audio) / 16000
    audio = _trim_silence(audio)
    duration = len(audio) / 16000
    if duration < 0.2:                      # nothing voiced in the take
        print(f"[whisper] {raw_duration:.2f}s audio -> silent, skipped", flush=True)
        return {"text": "", "audio_seconds": round(raw_duration, 3),
                "transcribe_seconds": 0.0}

    t0 = time.time()
    with _whisper_lock:                     # one GPU decode at a time
        result = mlx_whisper.transcribe(
            audio,
            path_or_hf_repo=WHISPER_REPO,
            language=WHISPER_LANG,
            # Whisper decodes in 30s windows and by default primes each window
            # with the text decoded from the previous one, so a single bad
            # window seeds the next and the model snowballs. Dictation is a
            # sequence of INDEPENDENT utterances, not continuous narration, so
            # the carry-over buys nothing. Measured on the 85s repro: 1232
            # chars of runaway with it on, 128 with it off — and 4.6x faster.
            #
            # Deliberately NOT setting word_timestamps/hallucination_silence_
            # threshold: benchmarked here, they doubled latency (1.1s -> 2.0s)
            # and did not suppress the silence artifact at all. _trim_silence
            # above is what actually removes the trigger.
            condition_on_previous_text=False,
        )
    elapsed = time.time() - t0
    text = _drop_hallucinated((result.get("text") or "").strip())

    rate = len(text) / max(duration, 1e-6)
    print(
        f"[whisper] {duration:.2f}s audio -> {len(text)} chars in {elapsed:.2f}s "
        f"({duration / max(elapsed, 1e-6):.1f}x realtime, {rate:.1f} c/s)"
        # Surfaced as a number, not the text, so the log stays free of dictated
        # content while still making a runaway visible after the fact.
        + ("  <-- ANOMALOUS RATE, possible hallucination" if rate > 25 else ""),
        flush=True,
    )
    return {"text": text, "audio_seconds": round(duration, 3),
            "transcribe_seconds": round(elapsed, 3)}
