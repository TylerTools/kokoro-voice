//! Tauri build-script entrypoint; bundle metadata lives in `tauri.conf.json`.

fn main() {
    for name in [
        "KOKORO_BUILD_CHANNEL",
        "KOKORO_DISPLAY_NAME",
        "KOKORO_APP_SUPPORT_DIR",
        "KOKORO_CONFIG_DIR_NAME",
        "KOKORO_DEFAULT_PORT",
        "KOKORO_CLIENT_HOST",
        "KOKORO_DIAGNOSTICS_FILE",
        "KOKORO_TTS_CPU_MEM_ARENA",
        "HEREWORD_BUILD_REVISION",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    tauri_build::build()
}
