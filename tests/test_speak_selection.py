import importlib.util
import os
from pathlib import Path
import tempfile
import unittest


class SelectionCaptureTests(unittest.TestCase):
    """Read-aloud copies the selection by synthesizing Ctrl+C.

    A modifier-only shortcut is still held when the action fires, and the held
    keys turn that Ctrl+C into something else entirely, so the copy must wait
    for them to come up first.
    """

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        os.environ["KOKORO_STATE_DIR"] = cls.tmp.name
        path = Path(__file__).parents[1] / "client" / "speak.py"
        spec = importlib.util.spec_from_file_location("speak_under_test", path)
        cls.speak = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.speak)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_release_is_awaited_before_the_keystroke_is_sent(self):
        order = []
        original_held = self.speak.modifiers_held
        original_run = self.speak.subprocess.run
        # Held for the first two probes, then released.
        probes = iter([True, True, False, False, False, False])

        def fake_held():
            order.append("probe")
            return next(probes, False)

        def fake_run(*args, **kwargs):
            order.append("sendkeys")

            class Result:
                stdout = ""
                returncode = 0

            return Result()

        self.speak.modifiers_held = fake_held
        self.speak.subprocess.run = fake_run
        try:
            self.speak.wait_for_modifier_release(timeout=1.0)
        finally:
            self.speak.modifiers_held = original_held
            self.speak.subprocess.run = original_run

        self.assertIn("probe", order)
        self.assertNotIn("sendkeys", order)

    def test_waiting_reports_success_once_the_keys_come_up(self):
        original = self.speak.modifiers_held
        probes = iter([True, False])
        self.speak.modifiers_held = lambda: next(probes, False)
        try:
            self.assertTrue(self.speak.wait_for_modifier_release(timeout=1.0))
        finally:
            self.speak.modifiers_held = original

    def test_waiting_gives_up_rather_than_refusing_to_read(self):
        original = self.speak.modifiers_held
        self.speak.modifiers_held = lambda: True
        try:
            # Still held at the deadline: report it, but do not block forever.
            self.assertFalse(self.speak.wait_for_modifier_release(timeout=0.1))
        finally:
            self.speak.modifiers_held = original

    def test_no_modifier_probe_off_windows(self):
        if self.speak.IS_WIN:
            self.skipTest("probe is only meaningful on Windows")
        self.assertFalse(self.speak.modifiers_held())
        self.assertTrue(self.speak.wait_for_modifier_release(timeout=0.1))


if __name__ == "__main__":
    unittest.main()
