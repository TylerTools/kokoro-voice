import json
import pathlib
import plistlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class VariantIsolationTests(unittest.TestCase):
    def test_stable_bundle_identity_remains_the_installed_2_1_identity(self):
        config = json.loads((ROOT / "app/src-tauri/tauri.conf.json").read_text())
        cargo = (ROOT / "app/src-tauri/Cargo.toml").read_text()
        package = json.loads((ROOT / "app/package.json").read_text())

        self.assertEqual(config["productName"], "HereWord")
        self.assertEqual(config["identifier"], "com.tylertools.kokoro-voice-2-1")
        self.assertRegex(cargo, r'(?m)^name = "kokoro-voice-2-1"$')
        self.assertIn(f'version = "{config["version"]}"', cargo)
        self.assertEqual(package["name"], "kokoro-voice-2-1-ui")
        self.assertEqual(package["version"], config["version"])

    def test_public_brand_changes_without_resetting_runtime_identity(self):
        config = json.loads((ROOT / "app/src-tauri/tauri.conf.json").read_text())
        candidate = json.loads(
            (ROOT / "app/src-tauri/tauri.candidate.conf.json").read_text()
        )
        index = (ROOT / "app/index.html").read_text()
        info = (ROOT / "app/src-tauri/Info.plist").read_text()
        variant = (ROOT / "app/src-tauri/src/variant.rs").read_text()
        stable_build = (ROOT / "scripts/release/build-stable.sh").read_text()
        candidate_build = (ROOT / "scripts/release/build-candidate.sh").read_text()

        self.assertEqual(config["productName"], "HereWord")
        self.assertEqual(candidate["productName"], "HereWord Candidate")
        self.assertIn("Your words stay here.", index)
        self.assertNotIn("Kokoro", index)
        self.assertNotIn("Kokoro", info)
        self.assertIn('None => "HereWord"', variant)
        self.assertIn('None => "Kokoro Voice 2.1"', variant)
        self.assertIn("bundle/macos/HereWord.app", stable_build)
        self.assertIn('KOKORO_DISPLAY_NAME="HereWord Candidate"', candidate_build)

    def test_candidate_bundle_has_one_reusable_identity(self):
        config = json.loads(
            (ROOT / "app/src-tauri/tauri.candidate.conf.json").read_text()
        )
        self.assertEqual(config["productName"], "HereWord Candidate")
        self.assertEqual(
            config["identifier"], "com.tylertools.kokoro-voice-candidate"
        )
        self.assertIn("127.0.0.1:8126", config["app"]["security"]["csp"])
        self.assertEqual(config["bundle"]["macOS"]["infoPlist"], "Info.candidate.plist")

    def test_runtime_namespace_does_not_overlap_version_one(self):
        variant = (ROOT / "app/src-tauri/src/variant.rs").read_text()
        stable_fallbacks = {
            "APP_SUPPORT_DIR": "Kokoro Voice 2.1",
            "CONFIG_DIR_NAME": "kokoro-voice-2-1",
            "DEFAULT_PORT": "8125",
            "CLIENT_HOST": "127.0.0.1:8125",
        }
        for name, value in stable_fallbacks.items():
            self.assertIn(f'None => "{value}"', variant)
            self.assertIn(f'option_env!("KOKORO_{name}")', variant)

        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text()
        runtime = (ROOT / "app/src-tauri/src/runtime.rs").read_text()
        chords = (ROOT / "app/src-tauri/src/chords.rs").read_text()
        self.assertIn("variant::APP_SUPPORT_DIR", runtime)
        self.assertIn("variant::CONFIG_DIR_NAME", runtime)
        self.assertIn("variant::DEFAULT_PORT", runtime)
        self.assertIn("crate::variant::CONFIG_DIR_NAME", chords)
        self.assertIn(
            'KOKORO_SERVICE_VERSION", env!("CARGO_PKG_VERSION")', runtime
        )
        self.assertIn("mod runtime;", lib)
        self.assertIn("mod runtime_hygiene;", lib)

    def test_candidate_build_exports_an_isolated_runtime_and_disables_autostart(self):
        script = (ROOT / "scripts/release/build-candidate.sh").read_text()
        for setting in (
            'KOKORO_BUILD_CHANNEL=candidate',
            'KOKORO_APP_SUPPORT_DIR="Kokoro Voice Candidate"',
            'KOKORO_CONFIG_DIR_NAME="kokoro-voice-candidate"',
            'KOKORO_DEFAULT_PORT="8126"',
            'KOKORO_CLIENT_HOST="127.0.0.1:8126"',
            'KOKORO_TTS_CPU_MEM_ARENA="1"',
        ):
            self.assertIn(setting, script)
        variant = (ROOT / "app/src-tauri/src/variant.rs").read_text()
        self.assertIn('option_env!("KOKORO_BUILD_CHANNEL").is_none()', variant)
        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text()
        self.assertIn("variant::DEFAULT_AUTOSTART", lib)
        self.assertIn("variant::INPUT_CONTROLLER_ENABLED", lib)
        self.assertIn("passive Candidate build", lib)
        self.assertIn("LEGACY_CONFIG_DIR_NAME", variant)
        self.assertIn('Some("kokoro-voice-2")', variant)
        runtime = (ROOT / "app/src-tauri/src/runtime.rs").read_text()
        self.assertIn('var_os("KOKORO_ONNX_CPU_MEM_ARENA")', runtime)
        self.assertIn("variant::TTS_CPU_MEM_ARENA", runtime)

    def test_stable_keeps_the_default_onnx_cpu_arena(self):
        variant = (ROOT / "app/src-tauri/src/variant.rs").read_text()
        stable = (ROOT / "scripts/release/build-stable.sh").read_text()
        self.assertIn('option_env!("KOKORO_TTS_CPU_MEM_ARENA")', variant)
        self.assertIn('None => "1"', variant)
        self.assertIn("unset KOKORO_TTS_CPU_MEM_ARENA", stable)

    def test_release_builds_seal_the_complete_bundle(self):
        for name in ("build-candidate.sh", "build-stable.sh"):
            script = (ROOT / "scripts/release" / name).read_text()
            self.assertIn("codesign --force --deep", script)
            self.assertIn("codesign --verify --deep --strict", script)

    def test_hardened_macos_bundle_can_request_audio_input(self):
        config = json.loads((ROOT / "app/src-tauri/tauri.conf.json").read_text())
        entitlements_path = ROOT / "app/src-tauri/Entitlements.plist"
        entitlements = plistlib.loads(entitlements_path.read_bytes())
        capabilities = json.loads(
            (ROOT / "app/src-tauri/capabilities/default.json").read_text()
        )

        self.assertEqual(
            config["bundle"]["macOS"]["entitlements"], "Entitlements.plist"
        )
        self.assertIs(entitlements["com.apple.security.device.audio-input"], True)
        opener = next(
            item
            for item in capabilities["permissions"]
            if isinstance(item, dict)
            and item.get("identifier") == "opener:allow-open-url"
        )
        self.assertIn({"url": "x-apple.systempreferences:*"}, opener["allow"])
        for name in ("build-candidate.sh", "build-stable.sh"):
            script = (ROOT / "scripts/release" / name).read_text()
            self.assertIn("--entitlements", script)
        release_workflow = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertIn("'com\\.apple\\.security\\.device\\.audio-input'", release_workflow)

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

    def test_disposable_stt_worker_is_bundled_and_not_started_during_warmup(self):
        config = (ROOT / "app/src-tauri/tauri.conf.json").read_text()
        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text()
        server = (ROOT / "server.py").read_text()
        for name in ("stt_worker.py", "stt_worker_manager.py"):
            self.assertIn(name, config)
            self.assertIn(name, lib)
        self.assertNotIn("_warm_whisper", server)
        self.assertIn('KOKORO_STT_BASE_IDLE_SECONDS", "90"', server)
        self.assertIn('KOKORO_STT_REPEAT_IDLE_SECONDS", "180"', server)
        self.assertIn('os.environ.get("KOKORO_STT_IDLE_SECONDS")', server)

    def test_macos_install_does_not_download_then_uninstall_torch(self):
        lock = (ROOT / "requirements-macos.lock").read_text()
        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text()
        self.assertNotRegex(lock, r"(?m)^torch==")
        self.assertIn('"--require-hashes", "--no-deps"', lib)
        self.assertNotIn('args(["pip", "uninstall", "torch"])', lib)

    def test_recyclable_tts_worker_is_bundled_and_parent_does_not_import_onnx(self):
        config = (ROOT / "app/src-tauri/tauri.conf.json").read_text()
        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text()
        server = (ROOT / "server.py").read_text()
        for name in ("tts_engine.py", "tts_worker.py", "tts_worker_manager.py"):
            self.assertIn(name, config)
            self.assertIn(name, lib)
        self.assertNotIn("import onnxruntime", server)
        self.assertNotIn("from kokoro_onnx", server)
        self.assertIn('KOKORO_TTS_WORKER_RETIRE_CHARS", "2000"', server)
        speak = (ROOT / "client/speak.py").read_text()
        self.assertIn('session_end=i == len(chunks) - 1', speak)

    def test_every_app_exit_path_stops_the_managed_engine(self):
        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text()
        runtime = (ROOT / "app/src-tauri/src/runtime.rs").read_text()
        self.assertIn("tauri::RunEvent::ExitRequested", lib)
        self.assertIn("tauri::RunEvent::Exit", lib)
        self.assertIn("stop_managed_child(&mut child)", runtime)
        self.assertIn("libc::SIGTERM", lib)
        self.assertIn("libc::SIGTERM", runtime)

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

    def test_macos_read_capture_runs_after_the_quartz_callback_returns(self):
        lib = (ROOT / "app/src-tauri/src/lib.rs").read_text()
        backend = (ROOT / "app/src-tauri/src/text_backend.rs").read_text()
        cargo = (ROOT / "app/src-tauri/Cargo.toml").read_text()
        self.assertIn("fn dispatch_read_from_hotkey(app: AppHandle)", lib)
        self.assertIn("from_millis(25)", lib)
        self.assertIn("dispatch_read_from_hotkey(read_app.clone())", lib)
        self.assertNotIn("read_selection(read_app.clone())", lib)
        self.assertIn("NSWorkspace::sharedWorkspace()", backend)
        self.assertIn("frontmostApplication()", backend)
        self.assertIn('"NSWorkspace"', cargo)

    def test_candidate_default_shortcuts_do_not_dispatch_stable(self):
        hotkeys = (ROOT / "app/src-tauri/src/hotkeys.rs").read_text()
        self.assertIn('option_env!("KOKORO_BUILD_CHANNEL")', hotkeys)
        for shortcut in (
            "Control+Alt+Command+Shift+KeyU",
            "Control+Alt+Command+Shift+KeyI",
            "Control+Alt+Command+Shift+KeyP",
        ):
            self.assertIn(shortcut, hotkeys)


if __name__ == "__main__":
    unittest.main()
