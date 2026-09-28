"""Kokoro model ownership and synthesis helpers for the TTS worker.

This module may allocate ONNX model memory. Import it only inside the disposable
worker process; importing it in the FastAPI parent defeats process recycling.
"""

from __future__ import annotations

import os

import numpy as np
import onnxruntime as ort
from kokoro_onnx import Kokoro


HERE = os.path.dirname(os.path.abspath(__file__))
MODEL_DIR = os.environ.get("KOKORO_MODEL_DIR", os.path.join(HERE, "models"))
# The August 2026 re-export (upstream model-files-v1.1) of the same v1.0
# weights; desktop setup stores it under this name. Keep it in step with
# KOKORO_MODEL_FILE in runtime.rs.
MODEL = os.path.join(
    MODEL_DIR, os.environ.get("KOKORO_MODEL", "kokoro-v1.0.fp16-2026-08.onnx")
)
VOICES = os.path.join(MODEL_DIR, "voices-v1.0.bin")
TTS_SEGMENT_CHARS = int(os.environ.get("KOKORO_TTS_SEGMENT_CHARS", "350"))


def _env_bool(name: str, default: bool) -> bool:
    raw = os.environ.get(name)
    if raw is None:
        return default
    normalized = raw.strip().lower()
    if normalized in {"1", "true", "yes", "on"}:
        return True
    if normalized in {"0", "false", "no", "off"}:
        return False
    raise RuntimeError(f"{name} must be a boolean value")


TTS_CPU_MEM_ARENA_ENABLED = _env_bool("KOKORO_ONNX_CPU_MEM_ARENA", True)


def _new_kokoro() -> Kokoro:
    if TTS_CPU_MEM_ARENA_ENABLED:
        return Kokoro(MODEL, VOICES)
    options = ort.SessionOptions()
    options.enable_cpu_mem_arena = False
    provider = os.environ.get("ONNX_PROVIDER", "CPUExecutionProvider")
    session = ort.InferenceSession(
        MODEL,
        sess_options=options,
        providers=[provider],
    )
    return Kokoro.from_session(session, VOICES)


def _split_text_once(text: str) -> tuple[str, str]:
    midpoint = max(1, len(text) // 2)
    candidates = [
        text.rfind(mark, 0, midpoint + 1)
        for mark in (". ", "! ", "? ", "; ", ", ", " ")
    ]
    cut = max(candidates, default=-1)
    if cut < max(1, midpoint // 2):
        cut = midpoint
    elif text[cut : cut + 2] in (". ", "! ", "? ", "; ", ", "):
        cut += 1
    left = text[:cut].strip()
    right = text[cut:].strip()
    if not left or not right:
        left, right = text[:midpoint], text[midpoint:]
    return left, right


def _tts_context_error(error: Exception) -> bool:
    message = str(error).lower()
    return (
        (isinstance(error, IndexError) and "out of bounds" in message)
        or (isinstance(error, AssertionError) and "context length" in message)
        or (isinstance(error, ValueError) and "too long" in message)
    )


def _create_tts_segment(k, text: str, voice: str, speed: float, lang: str):
    """Create one segment, splitting it again if Kokoro rejects its length.

    kokoro-onnx 0.5.0 could overflow its voice-style table (IndexError); 0.6.x
    reports an oversized batch as ValueError. Both stay handled so a manual
    environment not yet reinstalled from the current lock still reads long text.
    """
    try:
        return k.create(text, voice=voice, speed=speed, lang=lang)
    except (IndexError, AssertionError, ValueError) as error:
        if len(text) <= 1 or not _tts_context_error(error):
            raise
        left, right = _split_text_once(text)
        left_audio, left_rate = _create_tts_segment(k, left, voice, speed, lang)
        right_audio, right_rate = _create_tts_segment(k, right, voice, speed, lang)
        if left_rate != right_rate:
            raise RuntimeError("Kokoro returned inconsistent sample rates")
        return np.concatenate([left_audio, right_audio]), left_rate


def _create_tts(k, text: str, voice: str, speed: float, lang: str):
    pieces: list[str] = []
    remaining = text.strip()
    while len(remaining) > TTS_SEGMENT_CHARS:
        window = remaining[:TTS_SEGMENT_CHARS]
        cut = max(window.rfind(mark) for mark in (". ", "! ", "? ", "; ", ", ", " "))
        if cut < TTS_SEGMENT_CHARS // 2:
            cut = TTS_SEGMENT_CHARS
        else:
            cut += 1
        pieces.append(remaining[:cut].strip())
        remaining = remaining[cut:].strip()
    if remaining:
        pieces.append(remaining)

    rendered = [_create_tts_segment(k, piece, voice, speed, lang) for piece in pieces]
    sample_rates = {rate for _, rate in rendered}
    if len(sample_rates) != 1:
        raise RuntimeError("Kokoro returned inconsistent sample rates")
    return np.concatenate([audio for audio, _ in rendered]), sample_rates.pop()


class TtsEngine:
    def __init__(self) -> None:
        self.kokoro = _new_kokoro()

    def voices(self) -> list[str]:
        return sorted(self.kokoro.get_voices())

    def create(self, text: str, voice: str, speed: float, lang: str):
        return _create_tts(self.kokoro, text, voice, speed, lang)
