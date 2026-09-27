import importlib.util
import os
import pathlib
import signal
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[1]


class SpeakPlaybackTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.previous_state_dir = os.environ.get("KOKORO_STATE_DIR")
        os.environ["KOKORO_STATE_DIR"] = cls.temp.name
        spec = importlib.util.spec_from_file_location(
            "kokoro_speak_playback_test", ROOT / "client/speak.py"
        )
        cls.speak = importlib.util.module_from_spec(spec)
        assert spec.loader is not None
        spec.loader.exec_module(cls.speak)

    @classmethod
    def tearDownClass(cls):
        if cls.previous_state_dir is None:
            os.environ.pop("KOKORO_STATE_DIR", None)
        else:
            os.environ["KOKORO_STATE_DIR"] = cls.previous_state_dir
        cls.temp.cleanup()

    def setUp(self):
        self.speak._CANCELLED.clear()
        for path in pathlib.Path(self.temp.name).iterdir():
            if path.name != "playback.lock":
                path.unlink()

    def runtime_audio(self):
        return sorted(pathlib.Path(self.temp.name).glob("kokoro-*.wav*"))

    def test_completed_playback_deletes_every_generated_chunk(self):
        played = []
        with (
            mock.patch.object(self.speak, "split_chunks", return_value=["one", "two", "three"]),
            mock.patch.object(self.speak, "synthesize", return_value=b"wav"),
            mock.patch.object(self.speak, "retire_tts") as retire,
            mock.patch.object(self.speak, "_play_file", side_effect=played.append),
        ):
            result = self.speak.speak_streaming("text", None, 1.0)

        self.assertEqual(result, 0)
        self.assertEqual(len(played), 3)
        self.assertEqual(self.runtime_audio(), [])
        self.assertFalse(pathlib.Path(self.speak.STATEFILE).exists())
        retire.assert_not_called()

    def test_cancelled_playback_deletes_playing_and_prefetched_chunks(self):
        def cancel(_path):
            self.speak._CANCELLED.set()
            raise self.speak.PlaybackCancelled

        with (
            mock.patch.object(
                self.speak,
                "split_chunks",
                return_value=["one", "two", "three", "four"],
            ),
            mock.patch.object(self.speak, "synthesize", return_value=b"wav"),
            mock.patch.object(self.speak, "retire_tts") as retire,
            mock.patch.object(self.speak, "_play_file", side_effect=cancel),
        ):
            result = self.speak.speak_streaming("text", None, 1.0)

        self.assertEqual(result, 0)
        self.assertEqual(self.runtime_audio(), [])
        self.assertFalse(pathlib.Path(self.speak.STATEFILE).exists())
        retire.assert_called_once_with()

    def test_orphan_scavenger_removes_only_managed_playback_files(self):
        orphan = pathlib.Path(self.temp.name, "kokoro-999999-000.wav")
        partial = pathlib.Path(self.temp.name, "kokoro-999999-001.wav.part")
        exported = pathlib.Path(self.temp.name, "kokoro-export-999999-000.wav")
        for path in (orphan, partial, exported):
            path.write_bytes(b"wav")

        with mock.patch.object(self.speak, "_pid_is_our_speaker", return_value=False):
            self.speak._cleanup_orphan_files()

        self.assertFalse(orphan.exists())
        self.assertFalse(partial.exists())
        self.assertTrue(exported.exists())

    def test_state_is_cleared_only_by_its_owner(self):
        self.speak._write_state(None)
        state = pathlib.Path(self.speak.STATEFILE)
        self.assertTrue(state.exists())

        self.speak._clear_state_if_owned(os.getpid() + 1)
        self.assertTrue(state.exists())
        self.speak._clear_state_if_owned(os.getpid())
        self.assertFalse(state.exists())

    def test_process_lock_allows_only_one_playback_owner(self):
        first = self.speak.PlaybackLock()
        second = self.speak.PlaybackLock()
        self.assertTrue(first.try_acquire())
        self.assertFalse(second.try_acquire())
        first.release()
        self.assertTrue(second.try_acquire())
        second.release()

    @unittest.skipIf(os.name == "nt", "SIGCONT is a POSIX behavior")
    def test_paused_speaker_is_resumed_before_graceful_termination(self):
        with (
            mock.patch.object(self.speak, "_pid_is_our_speaker", return_value=True),
            mock.patch.object(self.speak, "_wait_for_speaker_exit", return_value=True),
            mock.patch.object(self.speak.os, "kill") as kill,
        ):
            self.speak._terminate_speaker(43210)

        self.assertEqual(
            kill.call_args_list,
            [mock.call(43210, signal.SIGCONT), mock.call(43210, signal.SIGTERM)],
        )


if __name__ == "__main__":
    unittest.main()
