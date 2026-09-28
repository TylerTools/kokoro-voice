"""
kokoro-voice service — local text-to-speech and speech-to-text.

Owns recyclable TTS and STT worker processes. Thin per-platform clients POST
text here and play the returned audio, or POST audio and receive a transcript.

Fully local: no API keys, no outbound network calls at runtime.
"""
import hmac
import io
import os
import time

import soundfile as sf
from fastapi import FastAPI, Header, HTTPException, Request
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel, Field
from starlette.concurrency import run_in_threadpool

from stt_worker_manager import SttWorkerError, SttWorkerManager
from tts_worker_manager import TtsWorkerError, TtsWorkerManager

HERE = os.path.dirname(os.path.abspath(__file__))
# fp16 measured faster than fp32 on this M4 (0.50s vs 0.59s first-chunk) and is
# 141MB smaller. int8 was benchmarked too and is much SLOWER (1.27s) — ARM lacks
# good int8 kernels for this graph. Do not "optimize" by switching to int8.
TTS_MODEL_NAME = os.environ.get("KOKORO_MODEL", "kokoro-v1.0.fp16-2026-08.onnx")
TTS_VOICES_NAME = "voices-v1.0.bin"

DEFAULT_VOICE = os.environ.get("KOKORO_VOICE", "af_heart")
DEFAULT_LANG = os.environ.get("KOKORO_LANG", "en-us")

# Shared secret. Read from KOKORO_TOKEN, else from a 0600 file that the desktop
# host and clients share. The file avoids duplicating secret plumbing across
# process launch paths and keeps the token out of `ps` and logs.
TOKEN_FILE = os.environ.get(
    "KOKORO_TOKEN_FILE", os.path.expanduser("~/.config/kokoro-voice-2-1/token")
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
# mlx-whisper retains its model and Metal heap in a process singleton. A single
# dictation gets a short lease; a warm repeat indicates a real burst and earns
# a longer lease. The legacy override keeps a one-value operational rollback.
_stt_idle_override = os.environ.get("KOKORO_STT_IDLE_SECONDS")
if _stt_idle_override is not None:
    STT_IDLE_SECONDS = STT_REPEAT_IDLE_SECONDS = float(_stt_idle_override)
else:
    STT_IDLE_SECONDS = float(os.environ.get("KOKORO_STT_BASE_IDLE_SECONDS", "90"))
    STT_REPEAT_IDLE_SECONDS = float(
        os.environ.get("KOKORO_STT_REPEAT_IDLE_SECONDS", "180")
    )
_stt_worker = SttWorkerManager(
    os.path.join(HERE, "stt_worker.py"),
    idle_seconds=STT_IDLE_SECONDS,
    repeat_idle_seconds=STT_REPEAT_IDLE_SECONDS,
)
# The read-aloud client sends a sequence of <=400-character chunks. Retire only
# after enough cumulative text indicates a long document, then let process exit
# return ONNX's arena and libc allocations to macOS between requests.
TTS_RETIRE_CHARS = int(os.environ.get("KOKORO_TTS_WORKER_RETIRE_CHARS", "2000"))
_tts_worker = TtsWorkerManager(
    os.path.join(HERE, "tts_worker.py"), retire_chars=TTS_RETIRE_CHARS
)

SERVICE_VERSION = os.environ.get("KOKORO_SERVICE_VERSION", "development")
app = FastAPI(title="Kokoro TTS", version=SERVICE_VERSION)


class SpeakRequest(BaseModel):
    text: str = Field(..., min_length=1)
    voice: str | None = None
    # kokoro-onnx asserts this exact range internally. Reject invalid input as a
    # 422 contract error instead of surfacing an AssertionError as HTTP 500.
    speed: float = Field(1.0, ge=0.5, le=2.0)
    lang: str | None = None
    # The read-aloud client marks only its final chunk. This lets the worker
    # retire after a completed long document without penalizing short reads.
    session_end: bool = True


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
    _tts_worker.warm()
    print(f"[kokoro] TTS worker loaded in {time.time() - t0:.2f}s", flush=True)


@app.on_event("shutdown")
def _shutdown_workers() -> None:
    _tts_worker.shutdown()
    _stt_worker.shutdown()


@app.get("/health")
def health() -> dict:
    tts = _tts_worker.status()
    stt = _stt_worker.status()
    tts_ready = tts["state"] not in {"error", "stopped"}
    stt_ready = stt["state"] not in {"error", "stopped"}
    return {
        "status": "ok",
        "service_version": app.version,
        "voices": tts["voice_count"],
        "default_voice": DEFAULT_VOICE,
        "auth_required": bool(AUTH_TOKEN),
        "tts_ready": tts_ready,
        "tts_warm": tts["state"] == "warm",
        "tts_cpu_mem_arena": tts.get("cpu_mem_arena"),
        "tts_status": tts,
        # A cold worker is intentional and ready on demand. Keep operational
        # readiness separate from residency so the UI does not report a healthy
        # memory-saving state as an incomplete startup.
        "stt_ready": stt_ready,
        "stt_warm": stt["state"] == "warm",
        "stt_backend": stt.get("backend"),
        "stt_cache_mode": os.environ.get("KOKORO_STT_CACHE_MODE", "unknown"),
        "stt_status": stt,
        "models": {
            "tts": TTS_MODEL_NAME,
            "voices": TTS_VOICES_NAME,
            "stt": "large-v3-turbo",
        },
    }


@app.get("/voices")
def voices() -> dict:
    return {"voices": _tts_worker.voices()}


@app.post("/tts/retire")
def retire_tts(authorization: str | None = Header(default=None)) -> dict:
    _check_auth(authorization)
    return {"retired": _tts_worker.retire()}


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

    voice = req.voice or DEFAULT_VOICE
    if voice not in _tts_worker.voices():
        raise HTTPException(status_code=400, detail=f"unknown voice: {voice}")

    try:
        result = _tts_worker.synthesize(
            text,
            voice=voice,
            speed=req.speed,
            lang=req.lang or DEFAULT_LANG,
            session_end=req.session_end,
        )
    except TtsWorkerError as error:
        raise HTTPException(status_code=503, detail=str(error)) from error
    audio = result.pop("audio")
    sample_rate = int(result["sample_rate"])
    duration = int(result["sample_count"]) / sample_rate
    synth_s = float(result["synth_seconds"])
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


@app.post("/transcribe")
async def transcribe(request: Request,
                     authorization: str | None = Header(default=None)) -> dict:
    """Raw WAV bytes in, text out.

    Audio is sent as a WAV body rather than multipart to keep the client
    dependency-free — the Windows client has to do this with stdlib too.
    """
    _check_auth(authorization)
    raw = await request.body()
    if not raw:
        raise HTTPException(status_code=400, detail="empty body")

    # Whisper is synchronous and can take tens of seconds on a long Windows
    # dictation. Running it on the ASGI event loop made /health unreachable;
    # the desktop watchdog then killed the healthy engine mid-transcription.
    return await run_in_threadpool(_transcribe_audio, raw)


def _transcribe_audio(raw: bytes) -> dict:
    import numpy as np

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

    try:
        result = _stt_worker.transcribe(audio.astype("float32", copy=False).tobytes())
    except SttWorkerError as error:
        raise HTTPException(
            status_code=503, detail=f"speech recognition worker failed: {error}"
        ) from error
    elapsed = float(result["transcribe_seconds"])
    decoded = result.get("text") or ""
    text = _drop_hallucinated(decoded.strip())

    rate = len(text) / max(duration, 1e-6)
    print(
        f"[whisper] {duration:.2f}s audio -> {len(text)} chars in {elapsed:.2f}s "
        f"({duration / max(elapsed, 1e-6):.1f}x realtime, {rate:.1f} c/s)"
        # Surfaced as a number, not the text, so the log stays free of dictated
        # content while still making a runaway visible after the fact.
        + ("  <-- ANOMALOUS RATE, possible hallucination" if rate > 25 else ""),
        flush=True,
    )
    return {
        "text": text,
        "audio_seconds": round(duration, 3),
        "transcribe_seconds": round(elapsed, 3),
        "stt_backend": result.get("backend"),
        "stt_cold_start": bool(result.get("cold_start")),
        "stt_cold_start_seconds": result.get("worker_cold_start_seconds"),
    }
