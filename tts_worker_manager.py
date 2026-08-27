"""Lifecycle and framed protocol for the recyclable Kokoro worker.

The manager serializes synthesis and retires the worker only between requests,
after a configurable amount of document text has passed through it.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import threading
import time
from typing import BinaryIO, Callable


MAX_HEADER_BYTES = 256 * 1024
MAX_AUDIO_BYTES = 128 * 1024 * 1024
SHUTDOWN_TIMEOUT_SECONDS = 5


class TtsWorkerError(RuntimeError):
    pass


class TtsWorkerProtocolError(TtsWorkerError):
    pass


def _read_exact(stream: BinaryIO, count: int) -> bytes:
    chunks: list[bytes] = []
    remaining = count
    while remaining:
        chunk = stream.read(remaining)
        if not chunk:
            raise TtsWorkerProtocolError("TTS worker closed its response stream")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


class TtsWorkerManager:
    def __init__(
        self,
        worker_script: str | os.PathLike[str],
        *,
        retire_chars: int = 2000,
        process_factory: Callable[..., subprocess.Popen] = subprocess.Popen,
    ) -> None:
        if retire_chars < 0:
            raise ValueError("retire_chars must be zero or positive")
        self.worker_script = str(Path(worker_script))
        self.retire_chars = retire_chars
        self._process_factory = process_factory
        self._lock = threading.Lock()
        self._process: subprocess.Popen | None = None
        self._state = "cold"
        self._active_requests = 0
        self._characters_since_start = 0
        self._session_characters = 0
        self._large_read_retirements = 0
        self._explicit_retirements = 0
        self._last_cold_start_seconds: float | None = None
        self._last_cold_request_seconds: float | None = None
        self._last_error: str | None = None
        self._voices: list[str] | None = None
        self._cpu_mem_arena: bool | None = None

    def _start_locked(self) -> tuple[subprocess.Popen, bool]:
        if self._process is not None and self._process.poll() is None:
            return self._process, False
        self._process = self._process_factory(
            [sys.executable, self.worker_script],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=None,
            cwd=str(Path(self.worker_script).parent),
            env=os.environ.copy(),
            bufsize=0,
        )
        if self._process.stdin is None or self._process.stdout is None:
            self._terminate_locked("error")
            raise TtsWorkerError("TTS worker pipes were not created")
        self._characters_since_start = 0
        self._state = "loading"
        return self._process, True

    @staticmethod
    def _write_header(process: subprocess.Popen, value: dict) -> None:
        if process.stdin is None:
            raise TtsWorkerProtocolError("TTS worker input pipe is unavailable")
        encoded = json.dumps(value, separators=(",", ":")).encode("utf-8")
        if len(encoded) > MAX_HEADER_BYTES:
            raise TtsWorkerProtocolError("TTS worker request header is too large")
        process.stdin.write(struct.pack("!I", len(encoded)))
        process.stdin.write(encoded)
        process.stdin.flush()

    @classmethod
    def _stop_process(cls, process: subprocess.Popen) -> None:
        if process.poll() is None:
            try:
                cls._write_header(process, {"command": "shutdown"})
                process.wait(timeout=SHUTDOWN_TIMEOUT_SECONDS)
            except (BrokenPipeError, OSError, TtsWorkerError, subprocess.TimeoutExpired):
                process.terminate()
                try:
                    process.wait(timeout=SHUTDOWN_TIMEOUT_SECONDS)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=SHUTDOWN_TIMEOUT_SECONDS)
        for stream in (process.stdin, process.stdout):
            try:
                if stream:
                    stream.close()
            except OSError:
                pass

    def _terminate_locked(self, state: str) -> None:
        process = self._process
        self._process = None
        if process is not None:
            self._stop_process(process)
        self._state = state

    @staticmethod
    def _exchange(process: subprocess.Popen, request: dict) -> dict:
        if process.stdout is None:
            raise TtsWorkerProtocolError("TTS worker output pipe is unavailable")
        try:
            TtsWorkerManager._write_header(process, request)
            size = struct.unpack("!I", _read_exact(process.stdout, 4))[0]
            if size > MAX_HEADER_BYTES:
                raise TtsWorkerProtocolError("TTS worker response header is too large")
            response = json.loads(_read_exact(process.stdout, size))
            if not isinstance(response, dict):
                raise TtsWorkerProtocolError("TTS worker returned a non-object response")
            audio_bytes = int(response.get("audio_bytes", 0))
            if audio_bytes < 0 or audio_bytes > MAX_AUDIO_BYTES:
                raise TtsWorkerProtocolError("TTS worker audio response is too large")
            audio = _read_exact(process.stdout, audio_bytes) if audio_bytes else b""
        except (BrokenPipeError, OSError, ValueError, json.JSONDecodeError) as error:
            raise TtsWorkerProtocolError(f"TTS worker protocol failed: {error}") from error
        if not response.get("ok"):
            raise TtsWorkerError(response.get("error") or "speech synthesis failed")
        response["audio"] = audio
        return response

    def _record_response_locked(self, response: dict, cold: bool, started: float) -> None:
        if cold:
            request_seconds = time.monotonic() - started
            model_load_seconds = response.get("model_load_seconds")
            self._last_cold_start_seconds = (
                float(model_load_seconds)
                if isinstance(model_load_seconds, (int, float))
                else request_seconds
            )
            self._last_cold_request_seconds = request_seconds
        if isinstance(response.get("voices"), list):
            self._voices = [str(voice) for voice in response["voices"]]
        if isinstance(response.get("cpu_mem_arena"), bool):
            self._cpu_mem_arena = response["cpu_mem_arena"]
        self._last_error = None

    def warm(self) -> dict:
        with self._lock:
            started = time.monotonic()
            process, cold = self._start_locked()
            try:
                response = self._exchange(process, {"command": "warm"})
                self._record_response_locked(response, cold, started)
                self._state = "warm"
                return response
            except Exception as error:
                self._last_error = f"{type(error).__name__}: {error}"
                self._terminate_locked("error")
                if isinstance(error, TtsWorkerError):
                    raise
                raise TtsWorkerError(f"could not warm TTS worker: {error}") from error

    def voices(self) -> list[str]:
        if self._voices is None:
            self.warm()
        return list(self._voices or [])

    def synthesize(
        self,
        text: str,
        voice: str,
        speed: float,
        lang: str,
        *,
        session_end: bool = True,
    ) -> dict:
        with self._lock:
            self._active_requests = 1
            started = time.monotonic()
            try:
                process, cold = self._start_locked()
                self._state = "loading" if cold else "busy"
                response = self._exchange(
                    process,
                    {
                        "command": "synthesize",
                        "text": text,
                        "voice": voice,
                        "speed": speed,
                        "lang": lang,
                    },
                )
                self._record_response_locked(response, cold, started)
                self._characters_since_start += len(text)
                self._session_characters += len(text)
                worker_limit = (
                    self.retire_chars
                    and self._characters_since_start >= self.retire_chars
                )
                completed_large_session = (
                    self.retire_chars
                    and session_end
                    and self._session_characters >= self.retire_chars
                )
                if session_end:
                    self._session_characters = 0
                if worker_limit or completed_large_session:
                    self._terminate_locked("cold")
                    self._large_read_retirements += 1
                else:
                    self._state = "warm"
                return response
            except Exception as error:
                self._last_error = f"{type(error).__name__}: {error}"
                self._terminate_locked("error")
                if isinstance(error, TtsWorkerError):
                    raise
                raise TtsWorkerError(f"could not run TTS worker: {error}") from error
            finally:
                self._active_requests = 0

    def status(self) -> dict:
        if self._lock.acquire(blocking=False):
            try:
                if self._process is not None and self._process.poll() is not None:
                    self._process = None
                    if self._state not in {"error", "stopped"}:
                        self._state = "cold"
                pid = self._process.pid if self._process is not None else None
                state = self._state
                active = self._active_requests
                characters = self._characters_since_start
                session_characters = self._session_characters
            finally:
                self._lock.release()
        else:
            pid = self._process.pid if self._process is not None else None
            state = "busy"
            active = max(1, self._active_requests)
            characters = self._characters_since_start
            session_characters = self._session_characters
        return {
            "state": state,
            "worker_pid": pid,
            "active_requests": active,
            "retire_chars": self.retire_chars,
            "characters_since_start": characters,
            "session_characters": session_characters,
            "large_read_retirements": self._large_read_retirements,
            "explicit_retirements": self._explicit_retirements,
            "last_cold_start_seconds": self._last_cold_start_seconds,
            "last_cold_request_seconds": self._last_cold_request_seconds,
            "cpu_mem_arena": self._cpu_mem_arena,
            "voice_count": len(self._voices or []),
            "last_error": self._last_error,
        }

    def retire(self) -> bool:
        """Release a warm worker after cancellation; never interrupt inference."""
        with self._lock:
            if self._process is None:
                self._session_characters = 0
                return False
            self._terminate_locked("cold")
            self._session_characters = 0
            self._explicit_retirements += 1
            return True

    def shutdown(self) -> None:
        with self._lock:
            self._terminate_locked("stopped")
