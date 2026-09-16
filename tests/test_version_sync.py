import json
import tempfile
import unittest
from pathlib import Path

from scripts.release.verify_version_sync import verify


class VersionSyncTests(unittest.TestCase):
    def make_repo(self, version: str = "2.1.1-beta.9") -> Path:
        root = Path(self.temporary.name)
        (root / "app/src-tauri").mkdir(parents=True)
        (root / "app/src-tauri/tauri.conf.json").write_text(
            json.dumps({"version": version})
        )
        (root / "app/package.json").write_text(json.dumps({"version": version}))
        (root / "app/src-tauri/Cargo.toml").write_text(
            f'[package]\nname = "hereword"\nversion = "{version}"\n'
        )
        return root

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.temporary.cleanup()

    def test_accepts_one_version_and_matching_tag(self):
        root = self.make_repo()
        self.assertEqual(verify("v2.1.1-beta.9", root), "2.1.1-beta.9")

    def test_rejects_metadata_drift(self):
        root = self.make_repo()
        (root / "app/package.json").write_text(json.dumps({"version": "2.1.0"}))
        with self.assertRaisesRegex(ValueError, "do not match"):
            verify(root=root)

    def test_rejects_a_tag_for_another_build(self):
        root = self.make_repo()
        with self.assertRaisesRegex(ValueError, "must equal"):
            verify("v2.1.1-beta.8", root)


if __name__ == "__main__":
    unittest.main()

