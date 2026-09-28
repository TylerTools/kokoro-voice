//! Tauri build-script entrypoint; bundle metadata lives in `tauri.conf.json`.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let source = "src/media_focus/quiet_macos.m";
        println!("cargo:rerun-if-changed={source}");
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
        let object = out.join("quiet_macos.o");
        let archive = out.join("libhereword_quiet.a");
        let status = std::process::Command::new("clang")
            .args([
                "-fobjc-arc",
                "-Wall",
                "-Werror",
                "-mmacosx-version-min=13.0",
                "-c",
                source,
                "-o",
            ])
            .arg(&object)
            .status()
            .expect("compile Core Audio quieting adapter");
        assert!(
            status.success(),
            "Core Audio quieting adapter failed to compile"
        );
        let status = std::process::Command::new("ar")
            .arg("crs")
            .arg(&archive)
            .arg(&object)
            .status()
            .expect("archive quieting adapter");
        assert!(status.success(), "quieting adapter archive failed");
        println!("cargo:rustc-link-search=native={}", out.display());
        println!("cargo:rustc-link-lib=static=hereword_quiet");
        println!("cargo:rustc-link-lib=framework=CoreAudio");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
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
