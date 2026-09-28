import io
from unittest import mock
import unittest

import numpy as np
import soundfile as sf
from pydantic import ValidationError

from fastapi import HTTPException
import server
import tts_engine


class ServiceGuardTests(unittest.TestCase):
    def test_tts_cpu_arena_can_be_disabled_without_replacing_kokoro(self):
        options = mock.Mock()
        session = mock.Mock()
        expected = mock.Mock()
        previous = tts_engine.TTS_CPU_MEM_ARENA_ENABLED
        tts_engine.TTS_CPU_MEM_ARENA_ENABLED = False
        try:
            with (
                mock.patch.object(tts_engine.ort, "SessionOptions", return_value=options),
                mock.patch.object(
                    tts_engine.ort, "InferenceSession", return_value=session
                ) as create,
                mock.patch.object(
                    tts_engine.Kokoro, "from_session", return_value=expected
                ) as wrap,
            ):
                result = tts_engine._new_kokoro()
        finally:
            tts_engine.TTS_CPU_MEM_ARENA_ENABLED = previous

        self.assertIs(result, expected)
        self.assertFalse(options.enable_cpu_mem_arena)
        create.assert_called_once_with(
            tts_engine.MODEL,
            sess_options=options,
            providers=["CPUExecutionProvider"],
        )
        wrap.assert_called_once_with(session, tts_engine.VOICES)

    def test_tts_cpu_arena_defaults_to_library_constructor(self):
        expected = mock.Mock()
        previous = tts_engine.TTS_CPU_MEM_ARENA_ENABLED
        tts_engine.TTS_CPU_MEM_ARENA_ENABLED = True
        try:
            with mock.patch.object(
                tts_engine, "Kokoro", return_value=expected
            ) as create:
                result = tts_engine._new_kokoro()
        finally:
            tts_engine.TTS_CPU_MEM_ARENA_ENABLED = previous

        self.assertIs(result, expected)
        create.assert_called_once_with(tts_engine.MODEL, tts_engine.VOICES)

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

    def test_health_treats_cold_stt_as_ready_without_claiming_it_is_warm(self):
        previous_tts = server._tts_worker
        previous_worker = server._stt_worker

        class FakeTtsWorker:
            @staticmethod
            def status():
                return {
                    "state": "warm",
                    "worker_pid": 123,
                    "active_requests": 0,
                    "retire_chars": 2000,
                    "characters_since_start": 0,
                    "large_read_retirements": 0,
                    "last_cold_start_seconds": 0.5,
                    "cpu_mem_arena": True,
                    "voice_count": 2,
                    "last_error": None,
                }

        class FakeWorker:
            state = "cold"

            @staticmethod
            def status():
                return {
                    "state": FakeWorker.state,
                    "worker_pid": None,
                    "active_requests": 0,
                    "idle_seconds": 300,
                    "last_cold_start_seconds": 2.5,
                    "idle_shutdowns": 1,
                    "backend": "mlx",
                    "last_error": None,
                }

        try:
            server._tts_worker = FakeTtsWorker()
            server._stt_worker = FakeWorker()
            cold = server.health()
            FakeWorker.state = "warm"
            warm = server.health()
        finally:
            server._tts_worker = previous_tts
            server._stt_worker = previous_worker

        self.assertEqual(cold["status"], "ok")
        self.assertTrue(cold["tts_ready"])
        self.assertTrue(cold["tts_cpu_mem_arena"])
        self.assertEqual(cold["tts_status"]["retire_chars"], 2000)
        self.assertTrue(cold["stt_ready"])
        self.assertFalse(cold["stt_warm"])
        self.assertEqual(cold["stt_backend"], "mlx")
        self.assertEqual(cold["stt_status"]["state"], "cold")
        self.assertEqual(cold["stt_status"]["idle_seconds"], 300)
        self.assertEqual(cold["voices"], 2)
        self.assertIn("service_version", cold)
        self.assertNotIn("token", cold)
        self.assertTrue(warm["stt_ready"])
        self.assertTrue(warm["stt_warm"])

    def test_speak_speed_matches_the_engine_contract(self):
        for speed in (0.49, 2.01):
            with self.subTest(speed=speed), self.assertRaises(ValidationError):
                server.SpeakRequest(text="hello", speed=speed)
        self.assertEqual(server.SpeakRequest(text="hello", speed=0.5).speed, 0.5)
        self.assertEqual(server.SpeakRequest(text="hello", speed=2.0).speed, 2.0)

    def test_tts_recovers_from_kokoro_token_overflow(self):
        # 0.5.0 overflowed its style table; 0.6.x rejects the batch instead.
        overflows = (
            IndexError("index 510 is out of bounds for axis 0 with size 510"),
            ValueError("text is too long, must be less than 510 phonemes"),
        )
        for overflow in overflows:
            class FakeKokoro:
                def create(self, text, **_kwargs):
                    if len(text) > 40:
                        raise overflow
                    return np.ones(len(text), dtype="float32"), 24000

            text = "This deliberately long sentence exercises recursive splitting without losing words."
            with self.subTest(error=type(overflow).__name__):
                audio, rate = tts_engine._create_tts(
                    FakeKokoro(), text, "voice", 1.0, "en-us"
                )
                self.assertEqual(rate, 24000)
                self.assertGreater(len(audio), 0)

    def test_tts_does_not_split_unrelated_value_errors(self):
        class FakeKokoro:
            def create(self, text, **_kwargs):
                raise ValueError("Voice nobody not found in available voices")

        with self.assertRaises(ValueError):
            tts_engine._create_tts(FakeKokoro(), "hello there", "nobody", 1.0, "en-us")

    def test_transcription_is_delegated_and_reports_cold_start(self):
        class FakeWorker:
            calls = []

            def transcribe(self, audio):
                self.calls.append(audio)
                return {
                    "text": "delegated",
                    "backend": "fake",
                    "cold_start": True,
                    "worker_cold_start_seconds": 2.75,
                    "transcribe_seconds": 0.5,
                }

        samples = np.full(16000, 0.1, dtype="float32")
        wav = io.BytesIO()
        sf.write(wav, samples, 16000, format="WAV", subtype="PCM_16")
        previous_worker = server._stt_worker
        worker = FakeWorker()
        server._stt_worker = worker
        try:
            result = server._transcribe_audio(wav.getvalue())
        finally:
            server._stt_worker = previous_worker

        self.assertEqual(result["text"], "delegated")
        self.assertEqual(result["stt_backend"], "fake")
        self.assertTrue(result["stt_cold_start"])
        self.assertEqual(result["stt_cold_start_seconds"], 2.75)
        self.assertEqual(len(worker.calls), 1)


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
