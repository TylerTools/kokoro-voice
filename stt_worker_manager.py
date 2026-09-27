"""Lifecycle manager for the disposable speech-recognition worker."""

from __future__ import annotations

import json
import os
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import BinaryIO, Callable


MAX_FRAME_BYTES = 64 * 1024
SHUTDOWN_TIMEOUT_SECONDS = 5


class SttWorkerError(RuntimeError):
    pass


class SttWorkerProtocolError(SttWorkerError):
    pass


def _read_exact(stream: BinaryIO, count: int) -> bytes:
    chunks: list[bytes] = []
    remaining = count
    while remaining:
        chunk = stream.read(remaining)
        if not chunk:
            raise SttWorkerProtocolError("speech worker closed its response stream")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


class SttWorkerManager:
    """Own one lazy worker and retire it only while no request is active."""

    def __init__(
        self,
        worker_script: str | os.PathLike[str],
        *,
        idle_seconds: float = 90,
        repeat_idle_seconds: float | None = None,
        process_factory: Callable[..., subprocess.Popen] = subprocess.Popen,
    ) -> None:
        if idle_seconds < 0:
            raise ValueError("idle_seconds must be zero or positive")
        if repeat_idle_seconds is not None and repeat_idle_seconds < 0:
            raise ValueError("repeat_idle_seconds must be zero or positive")
        self.worker_script = str(Path(worker_script))
        self.idle_seconds = idle_seconds
        self.repeat_idle_seconds = (
            idle_seconds if repeat_idle_seconds is None else repeat_idle_seconds
        )
        self._current_idle_seconds = idle_seconds
        self._process_factory = process_factory
        self._lock = threading.Lock()
        self._process: subprocess.Popen | None = None
        self._timer: threading.Timer | None = None
        self._timer_generation = 0
        self._state = "cold"
        self._active_requests = 0
        self._last_cold_start_seconds: float | None = None
        self._last_cold_request_seconds: float | None = None
        self._last_error: str | None = None
        self._idle_shutdowns = 0
        self._backend: str | None = None

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
            raise SttWorkerError("speech worker pipes were not created")
        self._state = "loading"
        return self._process, True

    def _cancel_timer_locked(self) -> None:
        self._timer_generation += 1
        if self._timer is not None:
            self._timer.cancel()
            self._timer = None

    def _schedule_idle_locked(self) -> None:
        self._cancel_timer_locked()
        if not self._current_idle_seconds or self._process is None:
            return
        generation = self._timer_generation
        timer = threading.Timer(
            self._current_idle_seconds, self._idle_expired, (generation,)
        )
        timer.daemon = True
        self._timer = timer
        timer.start()

    def _idle_expired(self, generation: int) -> None:
        with self._lock:
            if generation != self._timer_generation or self._active_requests:
                return
            self._timer = None
            if self._process is not None:
                self._terminate_locked("cold")
                self._idle_shutdowns += 1

    @staticmethod
    def _write_header(process: subprocess.Popen, value: dict) -> None:
        if process.stdin is None:
            raise SttWorkerProtocolError("speech worker input pipe is unavailable")
        encoded = json.dumps(value, separators=(",", ":")).encode("utf-8")
        if len(encoded) > MAX_FRAME_BYTES:
            raise SttWorkerProtocolError("speech worker request header is too large")
        process.stdin.write(struct.pack("!I", len(encoded)))
        process.stdin.write(encoded)
        process.stdin.flush()

    @classmethod
    def _stop_process(cls, process: subprocess.Popen) -> None:
        """Ask an idle worker to exit cleanly, then enforce a bounded fallback."""
        if process.poll() is None:
            try:
                cls._write_header(
                    process, {"command": "shutdown", "audio_bytes": 0}
                )
                process.wait(timeout=SHUTDOWN_TIMEOUT_SECONDS)
            except (BrokenPipeError, OSError, SttWorkerError, subprocess.TimeoutExpired):
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
        self._current_idle_seconds = self.idle_seconds

    @staticmethod
    def _exchange(process: subprocess.Popen, audio: bytes) -> dict:
        if process.stdin is None or process.stdout is None:
            raise SttWorkerProtocolError("speech worker pipes are unavailable")
        try:
            SttWorkerManager._write_header(
                process,
                {"command": "transcribe", "audio_bytes": len(audio)},
            )
            process.stdin.write(audio)
            process.stdin.flush()
            size = struct.unpack("!I", _read_exact(process.stdout, 4))[0]
            if size > MAX_FRAME_BYTES:
                raise SttWorkerProtocolError("speech worker response is too large")
            response = json.loads(_read_exact(process.stdout, size))
        except (BrokenPipeError, OSError, ValueError, json.JSONDecodeError) as error:
            raise SttWorkerProtocolError(f"speech worker protocol failed: {error}") from error
        if not isinstance(response, dict):
            raise SttWorkerProtocolError("speech worker returned a non-object response")
        if not response.get("ok"):
            raise SttWorkerError(response.get("error") or "speech recognition failed")
        return response

    def transcribe(self, audio_float32: bytes) -> dict:
        """Transcribe one float32 mono 16 kHz buffer.

        The manager lock is held for the complete exchange. The idle callback
        uses the same lock, so it cannot terminate a worker during a request.
        """
        with self._lock:
            self._cancel_timer_locked()
            self._active_requests = 1
            started_at = time.monotonic()
            try:
                process, cold = self._start_locked()
                self._state = "loading" if cold else "busy"
                response = self._exchange(process, audio_float32)
                if cold:
                    request_seconds = time.monotonic() - started_at
                    model_load_seconds = response.get("model_load_seconds")
                    self._last_cold_start_seconds = (
                        float(model_load_seconds)
                        if isinstance(model_load_seconds, (int, float))
                        else request_seconds
                    )
                    self._last_cold_request_seconds = request_seconds
                    self._current_idle_seconds = self.idle_seconds
                    response = {
                        **response,
                        "worker_cold_start_seconds": round(
                            self._last_cold_start_seconds, 3
                        ),
                    }
                else:
                    self._current_idle_seconds = self.repeat_idle_seconds
                if isinstance(response.get("backend"), str):
                    self._backend = response["backend"]
                self._last_error = None
                self._state = "warm"
                return response
            except Exception as error:
                self._last_error = f"{type(error).__name__}: {error}"
                self._terminate_locked("error")
                if isinstance(error, SttWorkerError):
                    raise
                raise SttWorkerError(f"could not run speech worker: {error}") from error
            finally:
                self._active_requests = 0
                if self._state == "warm":
                    self._schedule_idle_locked()

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
            finally:
                self._lock.release()
        else:
            pid = self._process.pid if self._process is not None else None
            state = "busy"
            active = max(1, self._active_requests)
        return {
            "state": state,
            "worker_pid": pid,
            "active_requests": active,
            "idle_seconds": self._current_idle_seconds,
            "base_idle_seconds": self.idle_seconds,
            "repeat_idle_seconds": self.repeat_idle_seconds,
            "last_cold_start_seconds": self._last_cold_start_seconds,
            "last_cold_request_seconds": self._last_cold_request_seconds,
            "idle_shutdowns": self._idle_shutdowns,
            "backend": self._backend,
            "last_error": self._last_error,
        }

    def shutdown(self) -> None:
        with self._lock:
            self._cancel_timer_locked()
            self._terminate_locked("stopped")
