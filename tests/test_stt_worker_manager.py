import io
import json
import struct
import threading
import time
import unittest
from unittest import mock

from stt_worker_manager import SttWorkerManager, SttWorkerTimeoutError


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


class HungProcess(FakeProcess):
    def __init__(self):
        self.released = threading.Event()
        self.read_started = threading.Event()
        super().__init__(stdout=self)

    def read(self, count):
        self.read_started.set()
        self.released.wait()
        return b""

    def kill(self):
        super().kill()
        self.released.set()

    def close(self):
        pass


class SttWorkerManagerTests(unittest.TestCase):
    def test_final_preempts_an_active_preview(self):
        processes = []
        created = threading.Event()

        def factory(*_args, **_kwargs):
            process = HungProcess() if not processes else FakeProcess()
            processes.append(process)
            created.set()
            return process

        manager = SttWorkerManager(
            "/tmp/stt_worker.py", idle_seconds=0,
            exchange_timeout_seconds=5, process_factory=factory,
        )
        preview_errors = []

        def preview():
            try:
                manager.transcribe(b"\0\0\0\0", preview=True)
            except Exception as error:
                preview_errors.append(error)

        thread = threading.Thread(target=preview)
        thread.start()
        self.assertTrue(created.wait(1))
        self.assertTrue(processes[0].read_started.wait(1))
        started = time.monotonic()
        self.assertEqual(manager.transcribe(b"\0\0\0\0")["text"], "hello")
        self.assertLess(time.monotonic() - started, 1)
        thread.join(1)
        self.assertFalse(thread.is_alive())
        self.assertTrue(preview_errors)
        self.assertEqual(processes[0].returncode, -9)
        manager.shutdown()

    def test_hung_worker_is_killed_and_next_request_starts_fresh(self):
        processes = []

        def factory(*_args, **_kwargs):
            process = HungProcess() if not processes else FakeProcess()
            processes.append(process)
            return process

        manager = SttWorkerManager(
            "/tmp/stt_worker.py",
            idle_seconds=0,
            exchange_timeout_seconds=0.05,
            process_factory=factory,
        )
        started = time.monotonic()
        with self.assertRaises(SttWorkerTimeoutError):
            manager.transcribe(b"\0\0\0\0")
        self.assertLess(time.monotonic() - started, 1)
        self.assertEqual(processes[0].returncode, -9)
        self.assertEqual(manager.transcribe(b"\0\0\0\0")["text"], "hello")
        manager.shutdown()

    def test_slow_cold_start_inside_deadline_is_preserved(self):
        output = BlockingOutput()
        process = FakeProcess(stdout=output)
        manager = SttWorkerManager(
            "/tmp/stt_worker.py",
            idle_seconds=0,
            exchange_timeout_seconds=0.5,
            process_factory=lambda *_args, **_kwargs: process,
        )
        timer = threading.Timer(0.1, output.release.set)
        timer.start()
        self.assertEqual(manager.transcribe(b"\0\0\0\0")["text"], "hello")
        self.assertFalse(process.terminated)
        manager.shutdown()

    def test_warm_repeat_earns_the_longer_burst_lease(self):
        timers = []

        class ManualTimer:
            def __init__(self, interval, function, args=None, kwargs=None):
                self.interval = interval
                self.function = function
                self.args = args or ()
                self.kwargs = kwargs or {}
                self.daemon = False
                self.cancelled = False
                timers.append(self)

            def start(self):
                pass

            def cancel(self):
                self.cancelled = True

            def fire(self, *, even_if_cancelled=False):
                if even_if_cancelled or not self.cancelled:
                    self.function(*self.args, **self.kwargs)

        process = FakeProcess(stdout=io.BytesIO(framed_response() * 2))
        with mock.patch("stt_worker_manager.threading.Timer", ManualTimer):
            manager = SttWorkerManager(
                "/tmp/stt_worker.py",
                idle_seconds=0.04,
                repeat_idle_seconds=0.12,
                process_factory=lambda *_args, **_kwargs: process,
            )
            first = manager.transcribe(b"\0\0\0\0")
            self.assertEqual(first["worker_cold_start_seconds"], 1.25)
            self.assertEqual(manager.status()["idle_seconds"], 0.04)
            self.assertEqual(timers[0].interval, 0.04)

            manager.transcribe(b"\0\0\0\0")
            self.assertEqual(manager.status()["idle_seconds"], 0.12)
            self.assertEqual(timers[1].interval, 0.12)

            # Even if a cancelled callback arrives late, its stale generation
            # cannot retire the worker renewed by the warm repeat.
            timers[0].fire(even_if_cancelled=True)
            self.assertFalse(process.shutdown_requested)
            timers[1].fire()
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
