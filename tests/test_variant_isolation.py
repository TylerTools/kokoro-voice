import json
import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class VariantIsolationTests(unittest.TestCase):
    def test_bundle_identity_and_display_name_are_distinct(self):
        config = json.loads((ROOT / "app/src-tauri/tauri.conf.json").read_text())
        cargo = (ROOT / "app/src-tauri/Cargo.toml").read_text()
        package = json.loads((ROOT / "app/package.json").read_text())

        self.assertEqual(config["productName"], "Kokoro Voice 2.1")
        self.assertEqual(config["identifier"], "com.tylertools.kokoro-voice-2-1")
        self.assertRegex(cargo, r'(?m)^name = "kokoro-voice-2-1"$')
        self.assertIn(f'version = "{config["version"]}"', cargo)
        self.assertEqual(package["name"], "kokoro-voice-2-1-ui")
        self.assertEqual(package["version"], config["version"])

    def test_runtime_namespace_does_not_overlap_version_one(self):
        variant = (ROOT / "app/src-tauri/src/variant.rs").read_text()
        expected = {
            "APP_SUPPORT_DIR": "Kokoro Voice 2.1",
            "CONFIG_DIR_NAME": "kokoro-voice-2-1",
            "DEFAULT_PORT": "8125",
            "CLIENT_HOST": "127.0.0.1:8125",
        }
        for name, value in expected.items():
            self.assertRegex(
                variant,
                rf'pub const {name}: &str = "{re.escape(value)}";',
            )

        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text()
        chords = (ROOT / "app/src-tauri/src/chords.rs").read_text()
        self.assertIn("variant::APP_SUPPORT_DIR", lib)
        self.assertIn("variant::CONFIG_DIR_NAME", lib)
        self.assertIn("variant::DEFAULT_PORT", lib)
        self.assertIn("crate::variant::CONFIG_DIR_NAME", chords)

    def test_python_children_use_the_version_two_port_token_and_state(self):
        server = (ROOT / "server.py").read_text()
        speak = (ROOT / "client/speak.py").read_text()
        dictate = (ROOT / "client/dictate.py").read_text()
        snip = (ROOT / "client/snip.py").read_text()

        self.assertIn("~/.config/kokoro-voice-2-1/token", server)
        for client in (speak, dictate):
            self.assertIn('127.0.0.1:8125', client)
            self.assertIn("~/.config/kokoro-voice-2-1/token", client)
            self.assertIn('f"kokoro-voice-2-1-{who}"', client)
        self.assertIn('f"kokoro-voice-2-1-{who}"', snip)

    def test_version_two_does_not_implicitly_create_a_second_tray(self):
        config = json.loads((ROOT / "app/src-tauri/tauri.conf.json").read_text())
        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text()

        self.assertNotIn("trayIcon", config["app"])
        self.assertEqual(lib.count("TrayIconBuilder::new()"), 1)

    def test_status_window_is_created_for_all_workspaces(self):
        config = json.loads((ROOT / "app/src-tauri/tauri.conf.json").read_text())
        player = next(
            window for window in config["app"]["windows"] if window["label"] == "player"
        )
        self.assertTrue(player["visibleOnAllWorkspaces"])
        self.assertFalse(player["focusable"])

    def test_default_complete_shortcuts_do_not_match_version_one(self):
        hotkeys = (ROOT / "app/src-tauri/src/hotkeys.rs").read_text()
        for shortcut in (
            "Control+Alt+Command+KeyU",
            "Control+Alt+Command+KeyI",
            "Control+Alt+Command+KeyP",
        ):
            self.assertIn(shortcut, hotkeys)
        for version_one_shortcut in (
            'DEFAULT_READ: &str = "Control+Alt+R"',
            'DEFAULT_DICTATE: &str = "Control+Alt+W"',
        ):
            self.assertNotIn(version_one_shortcut, hotkeys)
        self.assertNotIn("MAC_READ_GESTURE", hotkeys)
        self.assertNotIn("MAC_DICTATE_GESTURE", hotkeys)


if __name__ == "__main__":
    unittest.main()
