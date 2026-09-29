"""Owns update-feed contract tests, never contacts GitHub or uses signing keys."""

import importlib.util
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("update_manifest", Path(__file__).resolve().parents[1] / "scripts/release/write_update_manifest.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class UpdateManifestTests(unittest.TestCase):
    def test_feed_contains_both_signed_platforms_and_escaped_urls(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            for name in ("HereWord.app.tar.gz", "HereWord Setup.exe"):
                (directory / name).write_bytes(b"artifact")
                (directory / (name + ".sig")).write_text("signed-test-data\n")
            feed = MODULE.manifest("2.1.1-beta.13", "TylerTools/kokoro-voice", directory)
            self.assertEqual(set(feed["platforms"]), {"darwin-aarch64", "windows-x86_64"})
            self.assertIn("HereWord%20Setup.exe", feed["platforms"]["windows-x86_64"]["url"])
            self.assertEqual(feed["platforms"]["darwin-aarch64"]["signature"], "signed-test-data")

    def test_unsigned_or_ambiguous_artifacts_cannot_enter_feed(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / "HereWord.app.tar.gz").write_bytes(b"artifact")
            with self.assertRaises(FileNotFoundError):
                MODULE.manifest("2.1.1", "TylerTools/kokoro-voice", directory)
            (directory / "Other.app.tar.gz").write_bytes(b"artifact")
            with self.assertRaises(ValueError):
                MODULE.manifest("2.1.1", "TylerTools/kokoro-voice", directory)
