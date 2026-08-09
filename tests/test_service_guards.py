import unittest

from fastapi import HTTPException
import server


class ServiceGuardTests(unittest.TestCase):
    def test_auth_rejects_missing_and_wrong_tokens(self):
        previous = server.AUTH_TOKEN
        server.AUTH_TOKEN = "expected"
        try:
            for value in (None, "Bearer wrong", "wrong"):
                with self.subTest(value=value), self.assertRaises(HTTPException) as caught:
                    server._check_auth(value)
                self.assertEqual(caught.exception.status_code, 401)
            server._check_auth("Bearer expected")
        finally:
            server.AUTH_TOKEN = previous

    def test_only_whole_silence_artifacts_are_removed(self):
        self.assertEqual(server._drop_hallucinated("Thank you."), "")
        self.assertEqual(server._drop_hallucinated("Thank you for helping."), "Thank you for helping.")

    def test_health_reports_real_tts_and_stt_readiness_without_secrets(self):
        class FakeKokoro:
            @staticmethod
            def get_voices():
                return ["one", "two"]

        previous_get = server.get_kokoro
        previous_ready = server._whisper_ready
        previous_backend = server._stt_backend
        try:
            server.get_kokoro = lambda: FakeKokoro()
            server._whisper_ready = True
            server._stt_backend = "mlx"
            result = server.health()
        finally:
            server.get_kokoro = previous_get
            server._whisper_ready = previous_ready
            server._stt_backend = previous_backend

        self.assertEqual(result["status"], "ok")
        self.assertTrue(result["tts_ready"])
        self.assertTrue(result["stt_ready"])
        self.assertEqual(result["stt_backend"], "mlx")
        self.assertEqual(result["voices"], 2)
        self.assertIn("service_version", result)
        self.assertNotIn("token", result)


if __name__ == "__main__":
    unittest.main()
