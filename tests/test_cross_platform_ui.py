import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class CrossPlatformUiContractTests(unittest.TestCase):
    def test_shared_markup_has_no_apple_only_setup_claim(self):
        index = (ROOT / "app/index.html").read_text()
        self.assertNotIn("required macOS access", index)
        self.assertIn('id="app-version"', index)

    def test_platform_policy_owns_native_labels(self):
        policy = (ROOT / "app/src/platform_ui.ts").read_text()
        main = (ROOT / "app/src/main.ts").read_text()
        self.assertIn('platform === "windows"', policy)
        self.assertIn('paste_shortcut', policy)
        self.assertNotIn('Press Command+V', main)
        self.assertNotIn('required macOS access', main)

    def test_backend_reports_build_and_platform_identity(self):
        backend = (ROOT / "app/src-tauri/src/lib.rs").read_text()
        build = (ROOT / "app/src-tauri/build.rs").read_text()
        for field in (
            '"app_version"',
            '"build_revision"',
            '"platform"',
            '"architecture"',
            '"paste_shortcut"',
        ):
            self.assertIn(field, backend)
        self.assertIn("HEREWORD_BUILD_REVISION", build)


if __name__ == "__main__":
    unittest.main()

