import unittest

import numpy as np
from pydantic import ValidationError

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

    def test_speak_speed_matches_the_engine_contract(self):
        for speed in (0.49, 2.01):
            with self.subTest(speed=speed), self.assertRaises(ValidationError):
                server.SpeakRequest(text="hello", speed=speed)
        self.assertEqual(server.SpeakRequest(text="hello", speed=0.5).speed, 0.5)
        self.assertEqual(server.SpeakRequest(text="hello", speed=2.0).speed, 2.0)

    def test_tts_recovers_from_kokoro_token_overflow(self):
        class FakeKokoro:
            def create(self, text, **_kwargs):
                if len(text) > 40:
                    raise IndexError("index 510 is out of bounds for axis 0 with size 510")
                return np.ones(len(text), dtype="float32"), 24000

        text = "This deliberately long sentence exercises recursive splitting without losing words."
        audio, rate = server._create_tts(FakeKokoro(), text, "voice", 1.0, "en-us")
        self.assertEqual(rate, 24000)
        self.assertGreater(len(audio), 0)


class ServiceConcurrencyTests(unittest.IsolatedAsyncioTestCase):
    async def test_transcribe_offloads_whisper_from_the_event_loop(self):
        class FakeRequest:
            @staticmethod
            async def body():
                return b"wav"

        previous_worker = server._transcribe_audio
        previous_runner = server.run_in_threadpool
        previous_token = server.AUTH_TOKEN
        calls = []

        def fake_worker(raw):
            calls.append(("worker", raw))
            return {"text": "ok"}

        async def fake_runner(function, *args):
            calls.append(("runner", function, args))
            return function(*args)

        server._transcribe_audio = fake_worker
        server.run_in_threadpool = fake_runner
        server.AUTH_TOKEN = None
        try:
            result = await server.transcribe(FakeRequest(), authorization=None)
        finally:
            server._transcribe_audio = previous_worker
            server.run_in_threadpool = previous_runner
            server.AUTH_TOKEN = previous_token

        self.assertEqual(result, {"text": "ok"})
        self.assertEqual(calls[0][0], "runner")
        self.assertEqual(calls[1], ("worker", b"wav"))


if __name__ == "__main__":
    unittest.main()
"""Security, readiness, concurrency, and request-boundary service tests."""
