import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def load(name: str):
    path = ROOT / "scripts/release" / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


writer = load("write_artifact_manifest")
pairing = load("verify_paired_manifests")


class ReleaseManifestTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

    def tearDown(self):
        self.temporary.cleanup()

    def manifest(self, platform: str, architecture: str, revision: str = "a" * 40):
        artifact = self.root / f"HereWord-{platform}.bin"
        artifact.write_bytes(platform.encode())
        value = writer.build_manifest(
            platform=platform,
            architecture=architecture,
            tag="v2.1.1-beta.10",
            revision=revision,
            signing_identity=f"signed-{platform}",
            artifacts=[artifact],
        )
        path = self.root / f"{platform}.json"
        path.write_text(json.dumps(value))
        return path

    def test_accepts_matching_signed_platform_pair(self):
        result = pairing.verify_pair(
            [self.manifest("macos", "aarch64"), self.manifest("windows", "x86_64")]
        )
        self.assertEqual(set(result), {"macos", "windows"})

    def test_rejects_a_single_platform_release(self):
        with self.assertRaisesRegex(ValueError, "missing release manifests"):
            pairing.verify_pair([self.manifest("macos", "aarch64")])

    def test_rejects_mixed_revisions(self):
        with self.assertRaisesRegex(ValueError, "mismatch for revision"):
            pairing.verify_pair(
                [
                    self.manifest("macos", "aarch64"),
                    self.manifest("windows", "x86_64", "b" * 40),
                ]
            )


if __name__ == "__main__":
    unittest.main()

