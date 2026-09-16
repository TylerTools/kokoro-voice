import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class CrossPlatformReleaseContractTests(unittest.TestCase):
    def test_release_builds_both_platforms_and_never_auto_publishes(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertIn("runs-on: macos-14", workflow)
        self.assertIn("runs-on: windows-2022", workflow)
        self.assertIn("releaseDraft: true", workflow)
        self.assertNotIn("releaseDraft: false", workflow)
        self.assertIn("verify_paired_manifests.py", workflow)
        self.assertIn("Release must remain a draft until physical acceptance", workflow)

    def test_both_builds_are_bound_to_the_same_revision(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertGreaterEqual(
            workflow.count("ref: ${{ needs.prepare.outputs.revision }}"), 3
        )
        self.assertIn("HEREWORD_BUILD_REVISION", workflow)
        self.assertIn("verify_version_sync.py", workflow)

    def test_windows_release_fails_closed_on_signing(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        config = (ROOT / "app/src-tauri/tauri.windows-release.conf.json").read_text()
        signer = (ROOT / "scripts/release/sign-windows.ps1").read_text()
        self.assertIn("environment: windows-release-signing", workflow)
        self.assertIn("signCommand", config)
        self.assertIn("Get-AuthenticodeSignature", signer)
        self.assertIn("WINDOWS_EXPECTED_PUBLISHER", signer)

    def test_bootstrap_never_embeds_private_repository_credentials(self):
        bootstrap = (ROOT / "install.ps1").read_text()
        self.assertIn("HEREWORD_RELEASE_REPOSITORY", bootstrap)
        self.assertIn("x64-setup", bootstrap)
        self.assertNotIn("Tyler-Tools/kokoro-voice-2", bootstrap)
        self.assertNotIn("Authorization", bootstrap)


if __name__ == "__main__":
    unittest.main()

