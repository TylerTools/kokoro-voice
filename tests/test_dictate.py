import importlib.util
import os
from pathlib import Path
import tempfile
import threading
import unittest


class DictationControlTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        os.environ["KOKORO_STATE_DIR"] = cls.tmp.name
        path = Path(__file__).parents[1] / "client" / "dictate.py"
        spec = importlib.util.spec_from_file_location("dictate_under_test", path)
        cls.dictate = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.dictate)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_sessions_have_independent_stop_files(self):
        one = self.dictate.stopfile("one")
        two = self.dictate.stopfile("two")
        self.assertNotEqual(one, two)
        Path(one).touch()
        self.assertTrue(Path(one).exists())
        self.assertFalse(Path(two).exists())

    def test_cancel_is_session_specific(self):
        one = self.dictate.cancelfile("one")
        two = self.dictate.cancelfile("two")
        Path(one).touch()
        self.assertTrue(Path(one).exists())
        self.assertFalse(Path(two).exists())

    def test_session_rejects_path_traversal(self):
        for value in ("../escape", "a/b", "", "x" * 81):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.dictate.stopfile(value)

    def test_stdin_stop_control_is_exact_and_session_scoped(self):
        stop = threading.Event()
        cancel = threading.Event()
        self.assertTrue(self.dictate.apply_control_line(
            '{"command":"stop","session":"one"}', "one", stop, cancel
        ))
        self.assertTrue(stop.is_set())
        self.assertFalse(cancel.is_set())

    def test_stdin_control_rejects_malformed_wrong_or_unknown_records(self):
        for line in (
            "not-json",
            '[]',
            '{"command":"cancel","session":"two"}',
            '{"command":"erase","session":"one"}',
        ):
            with self.subTest(line=line):
                stop = threading.Event()
                cancel = threading.Event()
                self.assertFalse(
                    self.dictate.apply_control_line(line, "one", stop, cancel)
                )
                self.assertFalse(stop.is_set())
                self.assertFalse(cancel.is_set())

    def test_stdin_cancel_control_is_exact_and_session_scoped(self):
        stop = threading.Event()
        cancel = threading.Event()
        self.assertTrue(self.dictate.apply_control_line(
            '{"command":"cancel","session":"one"}', "one", stop, cancel
        ))
        self.assertFalse(stop.is_set())
        self.assertTrue(cancel.is_set())

    def test_preview_wav_is_bounded_to_recent_window(self):
        import io
        import numpy as np
        import soundfile as sf

        sample_rate = 24000
        frames = [np.zeros((sample_rate, 1), dtype="float32") for _ in range(3)]
        wav = self.dictate._wav_bytes(frames, np, sf, sample_rate,
                                      max_seconds=1.0)
        audio, rate = sf.read(io.BytesIO(wav), dtype="float32")
        self.assertEqual(rate, sample_rate)
        self.assertEqual(len(audio), sample_rate)

    def test_preferred_microphone_falls_back_only_when_unavailable(self):
        devices = [
            {"name": "MacBook Air Microphone", "max_input_channels": 1},
            {"name": "AirPods", "max_input_channels": 1},
            {"name": "AirPods", "max_input_channels": 0},
        ]
        self.assertEqual(self.dictate.available_input_device("AirPods", devices), "AirPods")
        self.assertEqual(self.dictate.available_input_device(1, devices), 1)
        self.assertIsNone(self.dictate.available_input_device("AirPods", devices[:1]))
        self.assertIsNone(self.dictate.available_input_device(2, devices))


if __name__ == "__main__":
    unittest.main()
"""Contract tests for session-safe dictation controls and bounded previews."""
