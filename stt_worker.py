"""Disposable Whisper process. Standard output is a framed private protocol."""

from __future__ import annotations

import json
import os
import platform
import struct
import sys
import time

from stt_config import (
    FASTER_WHISPER_REPO,
    FASTER_WHISPER_REVISION,
    candidates as stt_candidates,
    load_cpu_compute,
)


WHISPER_REVISION = "a4aaeec0636e6fef84abdcbe3544cb2bf7e9f6fb"
WHISPER_REPO = os.environ.get("WHISPER_REPO", "mlx-community/whisper-large-v3-turbo")
WHISPER_LANG = os.environ.get("WHISPER_LANG", "en")
TOKEN_FILE = os.environ.get(
    "KOKORO_TOKEN_FILE", os.path.expanduser("~/.config/kokoro-voice-2-1/token")
)
MAX_AUDIO_BYTES = int(
    os.environ.get("KOKORO_STT_WORKER_MAX_AUDIO", str(32 * 1024 * 1024))
)
MAX_HEADER_BYTES = 64 * 1024
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


def _send(value: dict) -> None:
    encoded = json.dumps(value, separators=(",", ":")).encode("utf-8")
    _PROTOCOL_OUTPUT.write(struct.pack("!I", len(encoded)))
    _PROTOCOL_OUTPUT.write(encoded)
    _PROTOCOL_OUTPUT.flush()


class Transcriber:
    def __init__(self) -> None:
        self.model = None
        self.backend = "mlx" if platform.system() == "Darwin" else "faster-whisper"
        self.model_path: str | None = None

    def _mlx_model_path(self) -> str:
        if self.model_path is None:
            from huggingface_hub import snapshot_download

            self.model_path = snapshot_download(
                WHISPER_REPO, revision=WHISPER_REVISION, local_files_only=True
            )
        return self.model_path

    def _load_faster_whisper(self) -> None:
        from faster_whisper import WhisperModel

        options = stt_candidates(
            platform.system(),
            os.environ.get("KOKORO_STT_DEVICE", "auto"),
            os.environ.get("KOKORO_STT_CPU_COMPUTE")
            or load_cpu_compute(os.path.join(os.path.dirname(TOKEN_FILE), "stt-backend.json")),
        )
        last_error = None
        for _backend, device, compute in options:
            try:
                self.model = WhisperModel(
                    FASTER_WHISPER_REPO,
                    revision=FASTER_WHISPER_REVISION,
                    local_files_only=True,
                    device=device,
                    compute_type=compute,
                )
                self.backend = f"faster-whisper-{device}-{compute}"
                return
            except Exception as error:  # CUDA absence is an expected fallback
                last_error = error
        raise RuntimeError("no usable speech-recognition backend") from last_error

    def transcribe(self, audio) -> tuple[str, float, float, bool]:
        cold = self.model is None and platform.system() != "Darwin"
        load_started = time.monotonic()
        if platform.system() == "Darwin":
            # mlx-whisper owns its model through ModelHolder. This worker is the
            # ownership boundary; exiting it releases the weights and Metal heap.
            import mlx_whisper

            cold = self.model_path is None
            model_path = self._mlx_model_path()
            loaded_in = time.monotonic() - load_started
            started = time.monotonic()
            result = mlx_whisper.transcribe(
                audio,
                path_or_hf_repo=model_path,
                language=WHISPER_LANG,
                condition_on_previous_text=False,
            )
            decoded = result.get("text") or ""
        else:
            if self.model is None:
                self._load_faster_whisper()
            loaded_in = time.monotonic() - load_started
            started = time.monotonic()
            segments, _info = self.model.transcribe(
                audio, language=WHISPER_LANG, condition_on_previous_text=False
            )
            decoded = " ".join(segment.text.strip() for segment in segments)
        return decoded, loaded_in, time.monotonic() - started, cold


def serve() -> int:
    # Third-party model libraries are free to print. Keep their output away
    # from stdout because stdout is the framed parent/worker protocol.
    sys.stdout = sys.stderr
    import numpy as np

    transcriber = Transcriber()
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
            command = header.get("command", "transcribe")
            audio_bytes = int(header["audio_bytes"])
        except (KeyError, TypeError, ValueError, json.JSONDecodeError):
            return 2
        if command == "shutdown":
            return 0 if audio_bytes == 0 else 2
        if command != "transcribe":
            return 2
        if audio_bytes < 0 or audio_bytes > MAX_AUDIO_BYTES or audio_bytes % 4:
            return 2
        payload = _read_exact(audio_bytes)
        if payload is None:
            return 2
        try:
            audio = np.frombuffer(payload, dtype="float32")
            text, load_seconds, transcribe_seconds, cold = transcriber.transcribe(audio)
            _send(
                {
                    "ok": True,
                    "text": text,
                    "backend": transcriber.backend,
                    "cold_start": cold,
                    "model_load_seconds": round(load_seconds, 3),
                    "transcribe_seconds": round(transcribe_seconds, 3),
                    "worker_pid": os.getpid(),
                }
            )
        except Exception as error:  # keep protocol output free of tracebacks
            print(
                f"[whisper-worker] {type(error).__name__}: {error}",
                file=sys.stderr,
                flush=True,
            )
            _send({"ok": False, "error": f"{type(error).__name__}: {error}"})


if __name__ == "__main__":
    raise SystemExit(serve())
