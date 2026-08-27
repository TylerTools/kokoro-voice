"""Pure contract tests for the passive Candidate runtime QC harness."""

import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "runtime_qc", ROOT / "scripts/quality/runtime_qc.py"
)
runtime_qc = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(runtime_qc)


class RuntimeQcTests(unittest.TestCase):
    def test_tts_stress_fixture_has_a_bounded_exact_size(self):
        self.assertEqual(len(runtime_qc.tts_stress_text(4000)), 4000)
        for invalid in (0, 20_001):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                runtime_qc.tts_stress_text(invalid)

    def test_tts_stress_matches_the_real_chunked_request_shape(self):
        client = runtime_qc.CandidateClient.__new__(runtime_qc.CandidateClient)

        session_ends = []

        def speak_text(text, *, session_end=True):
            session_ends.append(session_end)
            return text.encode(), {
                "request_seconds": 1.0,
                "synth_seconds": 0.8,
                "audio_seconds": 2.0,
                "audio_bytes": len(text),
            }

        client.speak_text = speak_text
        result = client.speak_stress("abcdefghij", 4)
        self.assertEqual(result["request_chunks"], 3)
        self.assertEqual(result["input_characters"], 10)
        self.assertEqual(result["request_seconds"], 3.0)
        self.assertEqual(session_ends, [False, False, True])

    def test_footprint_units_are_normalized_to_megabytes(self):
        self.assertEqual(runtime_qc.parse_footprint("Footprint: 339 MB"), 339)
        self.assertEqual(runtime_qc.parse_footprint("Footprint: 2.5 GB"), 2560)

    def test_transcript_comparison_ignores_case_and_punctuation_only(self):
        self.assertEqual(
            runtime_qc.normalized_text("Hello, PRIVATE world!"),
            "hello private world",
        )
        self.assertNotEqual(
            runtime_qc.normalized_text("hello private world"),
            runtime_qc.normalized_text("hello public world"),
        )

    def test_candidate_client_rejects_stable_or_non_loopback_urls(self):
        for url in ("http://127.0.0.1:8125", "http://192.168.1.20:8126"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                runtime_qc.CandidateClient(url, "token")

    def test_idle_wait_uses_the_current_adaptive_lease(self):
        health = {"stt_status": {"idle_seconds": 180, "base_idle_seconds": 90}}
        self.assertEqual(runtime_qc.idle_wait_timeout(health, None), 210)
        self.assertEqual(runtime_qc.idle_wait_timeout(health, 12), 12)

    def test_idle_memory_and_relative_regressions_are_gated(self):
        baseline = {
            "tts": {"request_seconds": 1.0},
            "tts_stress_runs": [
                {"request_seconds": 10.0},
                {"request_seconds": 11.0},
                {"request_seconds": 12.0},
            ],
            "stt_runs": [
                {"request_seconds": 3.0, "stt_cold_start": True},
                {"request_seconds": 1.0, "stt_cold_start": False},
            ],
        }
        report = {
            "health_initial": {
                "status": "ok",
                "auth_required": True,
                "stt_cache_mode": "owned",
                "tts_cpu_mem_arena": True,
            },
            "tts": {"request_seconds": 1.05},
            "tts_stress_runs": [
                {"request_seconds": 10.5},
                {"request_seconds": 11.5},
                {"request_seconds": 12.5},
            ],
            "stt_runs": [
                {
                    "request_seconds": 3.0,
                    "stt_cold_start": True,
                    "transcript_matches_fixture": True,
                },
                {
                    "request_seconds": 1.05,
                    "stt_cold_start": False,
                    "transcript_matches_fixture": True,
                },
            ],
            "memory_idle": {"combined_mb": 480},
        }
        checks = runtime_qc.baseline_checks(report, baseline, 500, True)
        self.assertTrue(all(check["passed"] for check in checks))

        report["memory_idle"]["combined_mb"] = 501
        checks = runtime_qc.baseline_checks(report, baseline, 500, True)
        self.assertFalse(next(c for c in checks if c["name"] == "idle-memory")["passed"])

        report["memory_idle"]["combined_mb"] = 480
        report["tts_stress_runs"][1]["request_seconds"] = 12.2
        checks = runtime_qc.baseline_checks(report, baseline, 500, True)
        self.assertFalse(
            next(c for c in checks if c["name"] == "tts-stress-regression")["passed"]
        )


if __name__ == "__main__":
    unittest.main()
