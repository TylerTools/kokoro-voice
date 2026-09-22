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

    def test_windows_tray_opens_settings_without_changing_the_mac_menu(self):
        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text(encoding="utf-8")
        self.assertIn('.show_menu_on_left_click(true)', lib)
        self.assertRegex(
            lib,
            r'#\[cfg\(target_os = "windows"\)\]\s+let tray = tray\s+'
            r'\.tooltip\(format!\("\{\} — Settings", variant::DISPLAY_NAME\)\)\s+'
            r'\.show_menu_on_left_click\(false\)',
        )
        self.assertIn('button: MouseButton::Left', lib)
        self.assertIn('button_state: MouseButtonState::Up', lib)
        self.assertIn('show_settings(tray.app_handle());', lib)
        self.assertIn('"open" => show_settings(app)', lib)

    def test_read_asks_accessibility_before_touching_the_clipboard(self):
        """Reading the selection must not be the clipboard's job by default.

        Copying overwrites whatever the user had on the clipboard, and the
        synthetic Ctrl+C it relies on is corrupted by the modifiers a
        modifier-only Read shortcut is still holding. The accessibility route
        has neither problem, so it has to be tried first -- but the copy has to
        survive as a fallback, because many controls expose no text pattern.
        """
        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text(encoding="utf-8")
        body = lib.split("fn read_selection(app: AppHandle) {", 1)[1].split(
            "\n}", 1
        )[0]
        accessibility = body.index("selection::focused_selection()")
        clipboard = body.index('"--selection"')
        self.assertLess(accessibility, clipboard)
        self.assertIn('args.insert(0, "--stdin".to_string());', body)
        self.assertIn("run_client_monitored_input(&app, \"speak.py\", args, Some(text));", body)

    def test_selected_text_is_never_read_out_of_a_password_field(self):
        source = (ROOT / "app/src-tauri/src/selection.rs").read_text(encoding="utf-8")
        self.assertIn("CurrentIsPassword()", source)
        # The password check must precede any attempt to pull text out.
        self.assertLess(
            source.index("CurrentIsPassword()"), source.index("GetSelection()")
        )

    def test_piped_text_closes_the_pipe_so_the_client_can_start(self):
        """A client reading stdin to EOF hangs forever if the pipe stays open."""
        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text(encoding="utf-8")
        self.assertIn("fn run_client_monitored_input(", lib)
        self.assertIn("child.stdin.take()", lib)
        self.assertIn("wait_with_output()", lib)

    def test_settings_restores_the_existing_window(self):
        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text(encoding="utf-8")
        helper = lib.split('fn show_settings(app: &AppHandle) {', 1)[1].split(
            '#[cfg_attr(mobile', 1
        )[0]
        self.assertIn('app.get_webview_window("main")', helper)
        self.assertIn('window.unminimize()', helper)
        self.assertIn('window.show()', helper)
        self.assertIn('window.set_focus()', helper)
        self.assertNotIn('WebviewWindowBuilder', helper)

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
