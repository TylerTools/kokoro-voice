"""Lifecycle and framing tests for the recyclable TTS worker manager."""

import io
import json
import struct
import unittest

from tts_worker_manager import TtsWorkerManager


def framed_response(*, voices: bool = False) -> bytes:
    audio = b"" if voices else b"wav"
    payload = {
        "ok": True,
        "audio_bytes": len(audio),
        "voices": ["one", "two"] if voices else None,
        "sample_rate": 24000,
        "sample_count": 72000,
        "synth_seconds": 0.5,
        "cold_start": True,
        "model_load_seconds": 0.125,
        "cpu_mem_arena": True,
    }
    encoded = json.dumps(payload).encode()
    return struct.pack("!I", len(encoded)) + encoded + audio


class FakeProcess:
    next_pid = 12000

    def __init__(self, responses: bytes):
        type(self).next_pid += 1
        self.pid = type(self).next_pid
        self.stdin = io.BytesIO()
        self.stdout = io.BytesIO(responses)
        self.returncode = None
        self.terminated = False
        self.shutdown_requested = False

    def poll(self):
        return self.returncode

    def terminate(self):
        self.terminated = True
        self.returncode = 0

    def kill(self):
        self.returncode = -9

    def wait(self, timeout=None):
        if self.returncode is None and b'"command":"shutdown"' in self.stdin.getvalue():
            self.shutdown_requested = True
            self.returncode = 0
        return self.returncode


class TtsWorkerManagerTests(unittest.TestCase):
    def test_cold_start_metric_reports_model_load_not_the_whole_request(self):
        process = FakeProcess(framed_response())
        manager = TtsWorkerManager(
            "/tmp/tts_worker.py",
            process_factory=lambda *_args, **_kwargs: process,
        )
        manager.synthesize("hello", "one", 1.0, "en-us")
        status = manager.status()
        self.assertEqual(status["last_cold_start_seconds"], 0.125)
        self.assertGreaterEqual(status["last_cold_request_seconds"], 0)
        manager.shutdown()

    def test_worker_retires_after_cumulative_long_read(self):
        created = []

        def factory(*_args, **_kwargs):
            process = FakeProcess(framed_response() * 2)
            created.append(process)
            return process

        manager = TtsWorkerManager(
            "/tmp/tts_worker.py", retire_chars=8, process_factory=factory
        )
        first = manager.synthesize(
            "four", "one", 1.0, "en-us", session_end=False
        )
        self.assertEqual(first["audio"], b"wav")
        self.assertEqual(manager.status()["state"], "warm")

        manager.synthesize("more", "one", 1.0, "en-us", session_end=True)
        self.assertTrue(created[0].shutdown_requested)
        status = manager.status()
        self.assertEqual(status["state"], "cold")
        self.assertEqual(status["large_read_retirements"], 1)

        manager.synthesize("new", "one", 1.0, "en-us")
        self.assertEqual(len(created), 2)
        manager.shutdown()

    def test_long_session_end_retires_a_partially_filled_replacement_worker(self):
        created = []

        def factory(*_args, **_kwargs):
            process = FakeProcess(framed_response() * 3)
            created.append(process)
            return process

        manager = TtsWorkerManager(
            "/tmp/tts_worker.py", retire_chars=5, process_factory=factory
        )
        manager.synthesize("aaa", "one", 1.0, "en-us", session_end=False)
        manager.synthesize("bbb", "one", 1.0, "en-us", session_end=False)
        self.assertEqual(manager.status()["state"], "cold")
        manager.synthesize("c", "one", 1.0, "en-us", session_end=True)
        self.assertEqual(manager.status()["state"], "cold")
        self.assertEqual(manager.status()["large_read_retirements"], 2)

    def test_zero_threshold_keeps_the_warm_worker(self):
        process = FakeProcess(framed_response() * 2)
        manager = TtsWorkerManager(
            "/tmp/tts_worker.py",
            retire_chars=0,
            process_factory=lambda *_args, **_kwargs: process,
        )
        manager.synthesize("long text", "one", 1.0, "en-us")
        self.assertEqual(manager.status()["state"], "warm")
        self.assertFalse(process.shutdown_requested)
        manager.shutdown()
        self.assertTrue(process.shutdown_requested)

    def test_explicit_retirement_releases_a_partial_cancelled_session(self):
        process = FakeProcess(framed_response())
        manager = TtsWorkerManager(
            "/tmp/tts_worker.py",
            retire_chars=2000,
            process_factory=lambda *_args, **_kwargs: process,
        )
        manager.synthesize("partial", "one", 1.0, "en-us", session_end=False)
        self.assertTrue(manager.retire())
        self.assertTrue(process.shutdown_requested)
        self.assertEqual(manager.status()["explicit_retirements"], 1)

    def test_voice_warmup_is_cached_after_worker_retirement(self):
        process = FakeProcess(framed_response(voices=True) + framed_response())
        manager = TtsWorkerManager(
            "/tmp/tts_worker.py",
            retire_chars=1,
            process_factory=lambda *_args, **_kwargs: process,
        )
        self.assertEqual(manager.voices(), ["one", "two"])
        manager.synthesize("x", "one", 1.0, "en-us")
        self.assertEqual(manager.status()["state"], "cold")
        self.assertEqual(manager.voices(), ["one", "two"])


if __name__ == "__main__":
    unittest.main()
