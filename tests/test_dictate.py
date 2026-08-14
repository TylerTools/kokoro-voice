import importlib.util
import os
from pathlib import Path
import tempfile
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

    def test_preview_wav_is_bounded_to_recent_window(self):
        import io
        import numpy as np
        import soundfile as sf

        frames = [np.zeros((self.dictate.SAMPLE_RATE, 1), dtype="float32") for _ in range(3)]
        wav = self.dictate._wav_bytes(frames, np, sf, max_seconds=1.0)
        audio, rate = sf.read(io.BytesIO(wav), dtype="float32")
        self.assertEqual(rate, self.dictate.SAMPLE_RATE)
        self.assertEqual(len(audio), self.dictate.SAMPLE_RATE)


if __name__ == "__main__":
    unittest.main()
"""Contract tests for session-safe dictation controls and bounded previews."""
