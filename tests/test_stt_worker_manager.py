import io
import json
import struct
import threading
import time
import unittest

from stt_worker_manager import SttWorkerManager


def framed_response() -> bytes:
    payload = json.dumps(
        {
            "ok": True,
            "text": "hello",
            "backend": "fake",
            "cold_start": True,
            "model_load_seconds": 1.25,
            "transcribe_seconds": 0.5,
        }
    ).encode()
    return struct.pack("!I", len(payload)) + payload


class FakeProcess:
    next_pid = 9000

    def __init__(self, stdout=None):
        type(self).next_pid += 1
        self.pid = type(self).next_pid
        self.stdin = io.BytesIO()
        self.stdout = stdout or io.BytesIO(framed_response())
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


class BlockingOutput:
    def __init__(self):
        self.release = threading.Event()
        self.data = io.BytesIO(framed_response())

    def read(self, count):
        self.release.wait(timeout=2)
        return self.data.read(count)

    def close(self):
        pass


class SttWorkerManagerTests(unittest.TestCase):
    def test_warm_repeat_earns_the_longer_burst_lease(self):
        process = FakeProcess(stdout=io.BytesIO(framed_response() * 2))
        manager = SttWorkerManager(
            "/tmp/stt_worker.py",
            idle_seconds=0.04,
            repeat_idle_seconds=0.12,
            process_factory=lambda *_args, **_kwargs: process,
        )
        first = manager.transcribe(b"\0\0\0\0")
        self.assertEqual(first["worker_cold_start_seconds"], 1.25)
        self.assertEqual(manager.status()["idle_seconds"], 0.04)
        time.sleep(0.02)
        manager.transcribe(b"\0\0\0\0")
        self.assertEqual(manager.status()["idle_seconds"], 0.12)
        time.sleep(0.06)
        self.assertFalse(process.shutdown_requested)
        deadline = time.monotonic() + 0.5
        while time.monotonic() < deadline and not process.shutdown_requested:
            time.sleep(0.01)
        self.assertTrue(process.shutdown_requested)

    def test_worker_is_lazy_and_exits_after_idle(self):
        created = []

        def factory(*_args, **_kwargs):
            process = FakeProcess()
            created.append(process)
            return process

        manager = SttWorkerManager(
            "/tmp/stt_worker.py", idle_seconds=0.05, process_factory=factory
        )
        self.assertEqual(manager.status()["state"], "cold")
        self.assertEqual(created, [])

        result = manager.transcribe(b"\0\0\0\0")
        self.assertEqual(result["text"], "hello")
        self.assertIn("worker_cold_start_seconds", result)
        self.assertEqual(manager.status()["state"], "warm")

        deadline = time.monotonic() + 1
        while time.monotonic() < deadline and not created[0].shutdown_requested:
            time.sleep(0.01)
        self.assertTrue(created[0].shutdown_requested)
        self.assertFalse(created[0].terminated)
        status = manager.status()
        self.assertEqual(status["state"], "cold")
        self.assertEqual(status["idle_shutdowns"], 1)
        self.assertEqual(status["backend"], "fake")
        self.assertEqual(status["last_cold_start_seconds"], 1.25)

    def test_idle_shutdown_cannot_interrupt_an_active_request(self):
        output = BlockingOutput()
        process = FakeProcess(stdout=output)
        manager = SttWorkerManager(
            "/tmp/stt_worker.py",
            idle_seconds=0.02,
            process_factory=lambda *_args, **_kwargs: process,
        )
        results = []
        thread = threading.Thread(
            target=lambda: results.append(manager.transcribe(b"\0\0\0\0"))
        )
        thread.start()
        time.sleep(0.08)
        self.assertFalse(process.terminated)
        self.assertEqual(manager.status()["state"], "busy")

        output.release.set()
        thread.join(timeout=1)
        self.assertFalse(thread.is_alive())
        self.assertEqual(results[0]["text"], "hello")
        manager.shutdown()

    def test_zero_idle_seconds_keeps_the_worker_until_shutdown(self):
        process = FakeProcess()
        manager = SttWorkerManager(
            "/tmp/stt_worker.py",
            idle_seconds=0,
            process_factory=lambda *_args, **_kwargs: process,
        )
        manager.transcribe(b"\0\0\0\0")
        time.sleep(0.05)
        self.assertFalse(process.terminated)
        manager.shutdown()
        self.assertTrue(process.shutdown_requested)
        self.assertFalse(process.terminated)


if __name__ == "__main__":
    unittest.main()
