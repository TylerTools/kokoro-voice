"""Disposable Kokoro process with a framed private parent protocol.

Standard output is protocol-only. Process exit is the authoritative way to
release ONNX allocations after a large read-aloud session.
"""

from __future__ import annotations

import io
import json
import os
import struct
import sys
import time


MAX_HEADER_BYTES = 256 * 1024
MAX_TEXT_CHARS = 20_000
_PROTOCOL_OUTPUT = sys.stdout.buffer


def _read_exact(count: int) -> bytes | None:
    chunks: list[bytes] = []
    remaining = count
    while remaining:
        chunk = sys.stdin.buffer.read(remaining)
        if not chunk:
            return None
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _send(header: dict, audio: bytes = b"") -> None:
    value = {**header, "audio_bytes": len(audio)}
    encoded = json.dumps(value, separators=(",", ":")).encode("utf-8")
    _PROTOCOL_OUTPUT.write(struct.pack("!I", len(encoded)))
    _PROTOCOL_OUTPUT.write(encoded)
    if audio:
        _PROTOCOL_OUTPUT.write(audio)
    _PROTOCOL_OUTPUT.flush()


def serve() -> int:
    sys.stdout = sys.stderr
    import soundfile as sf

    from tts_engine import TTS_CPU_MEM_ARENA_ENABLED, TtsEngine

    engine: TtsEngine | None = None
    while True:
        size_bytes = _read_exact(4)
        if size_bytes is None:
            return 0
        header_size = struct.unpack("!I", size_bytes)[0]
        if header_size > MAX_HEADER_BYTES:
            return 2
        raw_header = _read_exact(header_size)
        if raw_header is None:
            return 2
        try:
            header = json.loads(raw_header)
            command = header["command"]
        except (KeyError, TypeError, json.JSONDecodeError):
            return 2
        if command == "shutdown":
            return 0
        try:
            load_started = time.monotonic()
            cold = engine is None
            if engine is None:
                engine = TtsEngine()
            load_seconds = time.monotonic() - load_started if cold else 0.0
            if command in {"warm", "voices"}:
                _send(
                    {
                        "ok": True,
                        "voices": engine.voices(),
                        "cold_start": cold,
                        "model_load_seconds": round(load_seconds, 4),
                        "cpu_mem_arena": TTS_CPU_MEM_ARENA_ENABLED,
                        "worker_pid": os.getpid(),
                    }
                )
                continue
            if command != "synthesize":
                return 2
            text = header["text"]
            voice = header["voice"]
            speed = float(header["speed"])
            lang = header["lang"]
            if not isinstance(text, str) or not 1 <= len(text) <= MAX_TEXT_CHARS:
                return 2
            started = time.monotonic()
            samples, sample_rate = engine.create(text, voice, speed, lang)
            synth_seconds = time.monotonic() - started + load_seconds
            wav = io.BytesIO()
            sf.write(wav, samples, sample_rate, format="WAV", subtype="PCM_16")
            _send(
                {
                    "ok": True,
                    "sample_rate": sample_rate,
                    "sample_count": len(samples),
                    "synth_seconds": round(synth_seconds, 4),
                    "cold_start": cold,
                    "model_load_seconds": round(load_seconds, 4),
                    "cpu_mem_arena": TTS_CPU_MEM_ARENA_ENABLED,
                    "worker_pid": os.getpid(),
                },
                wav.getvalue(),
            )
        except Exception as error:
            print(
                f"[tts-worker] {type(error).__name__}: {error}",
                file=sys.stderr,
                flush=True,
            )
            _send({"ok": False, "error": f"{type(error).__name__}: {error}"})


if __name__ == "__main__":
    raise SystemExit(serve())
