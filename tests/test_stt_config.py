import unittest

from stt_config import candidates, load_cpu_compute
import json
import tempfile


class SttSelectionTests(unittest.TestCase):
    def test_mac_uses_mlx(self):
        self.assertEqual(candidates("Darwin"), [("mlx", "gpu", "float16")])

    def test_windows_auto_falls_back_from_cuda_to_cpu(self):
        self.assertEqual(candidates("Windows"), [
            ("faster-whisper", "cuda", "float16"),
            ("faster-whisper", "cpu", "int8"),
        ])

    def test_explicit_cpu_skips_cuda(self):
        self.assertEqual(candidates("Windows", "cpu", "float32"), [
            ("faster-whisper", "cpu", "float32"),
        ])

    def test_invalid_device_is_rejected(self):
        with self.assertRaises(ValueError):
            candidates("Windows", "magic")

    def test_persisted_cpu_choice_is_validated(self):
        with tempfile.NamedTemporaryFile("w+", suffix=".json") as handle:
            json.dump({"cpu_compute": "float32"}, handle)
            handle.flush()
            self.assertEqual(load_cpu_compute(handle.name), "float32")


if __name__ == "__main__":
    unittest.main()
"""Cross-platform STT backend-selection contract tests."""
