//! HereWord desktop composition root.
//!
//! This module owns first-run setup, engine lifecycle, Tauri commands, action
//! orchestration, tray/UI composition, and shutdown. Domain logic belongs in
//! the sibling modules below; do not add another composition root.
//!
//! Small Python sources ship in the bundle, while the private environment and
//! models live in Application Support so app replacement preserves downloaded
//! data and never writes through the code signature.

mod app_updates;
#[cfg(target_os = "macos")]
mod chords;
mod dictation_protocol;
mod hotkeys;
#[cfg(target_os = "macos")]
mod media_controls;
mod media_focus;

#[cfg(target_os = "windows")]
pub fn run_audio_quiet_worker(root: u32, duck: bool, level: f64) {
    media_focus::run_quiet_worker(root, duck, level);
}
#[cfg(target_os = "macos")]
mod microphone_permission;
mod playback;
mod preferences;
#[cfg(any(target_os = "macos", test))]
mod read_action;
mod runtime;
mod runtime_hygiene;
#[cfg(target_os = "windows")]
mod selection;
mod text_backend;
mod variant;
#[cfg(target_os = "windows")]
mod windows_chords;

use std::io::Write;
use std::process::{ChildStdin, Command, Stdio};
#[cfg(unix)]
use std::sync::atomic::AtomicI32;
#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(target_os = "macos")]
use std::sync::Condvar;
#[cfg(target_os = "macos")]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

#[cfg(test)]
use runtime::active_source_root;
use runtime::{
    active_source_file, client_command, config_dir, engine_root, ensure_auth_token, is_installed,
    legacy_config_dir, local_json, python_path, replace_file, spawn_engine_and_record,
    start_watchdog, stop_engine, structured_log, Engine, Paths, KOKORO_MODEL_FILE,
    KOKORO_VOICES_FILE, QUITTING,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};
#[cfg(target_os = "windows")]
use tauri_plugin_global_shortcut::ShortcutState;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

#[derive(Clone, Debug, serde::Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum DictationStatus {
    Starting,
    Recording,
    Transcribing,
    /// The authoritative final transcript was verified in the captured target.
    Completed,
    Cancelled,
    PermissionDenied,
    DeviceUnavailable,
    TimedOut,
    LiveTyping,
    /// The final transcript was copied but was not verified in the target.
    ClipboardFallback,
    CancelledByUser,
}

#[derive(Clone, Debug)]
struct DictationSession {
    id: String,
    status: DictationStatus,
    started: std::time::Instant,
    child_pid: Option<u32>,
    target: Option<text_backend::TargetSnapshot>,
    inserted_text: String,
    fallback_reason: Option<String>,
    stop_requested: bool,
    cancel_requested: bool,
    control: Option<Arc<Mutex<ChildStdin>>>,
}

struct Dictation(Mutex<Option<DictationSession>>);
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);
static SETUP_CANCELLED: AtomicBool = AtomicBool::new(false);
static HOTKEYS_REGISTERED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static CHORDS_STARTED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static MICROPHONE_PERMISSION_REQUEST_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static STATUS_PANEL: AtomicPtr<objc2_app_kit::NSPanel> = AtomicPtr::new(std::ptr::null_mut());
#[cfg(target_os = "macos")]
static STATUS_PANEL_POSITION: OnceLock<Mutex<Option<(f64, f64)>>> = OnceLock::new();
#[cfg(target_os = "macos")]
static STATUS_WINDOW_REQUESTED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static STATUS_WINDOW_WIDTH_BITS: AtomicU64 = AtomicU64::new(0);
#[cfg(target_os = "macos")]
static STATUS_WINDOW_HEIGHT_BITS: AtomicU64 = AtomicU64::new(0);
#[cfg(target_os = "macos")]
static STATUS_SPACE_WATCHER_STARTED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static STATUS_SPACE_RECOVERY_ARMED: AtomicBool = AtomicBool::new(true);
#[cfg(unix)]
static SIGNAL_WRITE_FD: AtomicI32 = AtomicI32::new(-1);
#[cfg(target_os = "macos")]
static STATUS_SPACE_STATE: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
// ── first-run setup ──────────────────────────────────────────────────────────

fn emit_step(app: &AppHandle, pct: u32, message: &str) {
    let state = SetupState {
        schema_version: 1,
        stage: message.to_string(),
        pct,
        completed: pct == 100,
        error: None,
    };
    let _ = write_json_atomic(&setup_state_file(), &state);
    let _ = app.emit(
        "setup-progress",
        serde_json::json!({ "pct": pct, "message": message }),
    );
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct SetupState {
    schema_version: u32,
    stage: String,
    pct: u32,
    completed: bool,
    error: Option<String>,
}

fn setup_state_file() -> std::path::PathBuf {
    config_dir().join("setup-state.json")
}

fn performance_profile_file() -> std::path::PathBuf {
    config_dir().join("performance-profile.json")
}

fn write_json_atomic<T: serde::Serialize>(path: &std::path::Path, value: &T) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}

#[tauri::command]
fn setup_status() -> serde_json::Value {
    std::fs::read_to_string(setup_state_file())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| {
            serde_json::json!({
                "schema_version": 1, "stage": "not-started", "pct": 0,
                "completed": false, "error": null
            })
        })
}

fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut digest = sha2::Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn download_verified(
    url: &str,
    destination: &std::path::Path,
    expected_size: Option<u64>,
    expected_sha256: &str,
) -> Result<(), String> {
    use reqwest::header::RANGE;
    use std::io::{Read, Write};
    let partial = destination.with_extension("part");
    let verify = |path: &std::path::Path| -> bool {
        if expected_size.is_some_and(|size| path.metadata().map(|m| m.len()).ok() != Some(size)) {
            return false;
        }
        sha256_file(path).is_ok_and(|actual| actual == expected_sha256)
    };
    if verify(destination) {
        return Ok(());
    }
    if verify(&partial) {
        return std::fs::rename(partial, destination).map_err(|e| e.to_string());
    }
    let existing = partial.metadata().map(|m| m.len()).unwrap_or(0);
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(900))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.get(url);
    if existing > 0 {
        request = request.header(RANGE, format!("bytes={existing}-"));
    }
    let mut response = request
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("download failed: {e}"))?;
    let append = existing > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true);
    if append {
        options.append(true);
    } else {
        options.truncate(true);
    }
    let mut file = options.open(&partial).map_err(|e| e.to_string())?;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        if SETUP_CANCELLED.load(Ordering::SeqCst) {
            return Err("installation cancelled; partial download retained".into());
        }
        let count = response.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    if expected_size.is_some_and(|size| partial.metadata().map(|m| m.len()).ok() != Some(size)) {
        return Err("download size verification failed".into());
    }
    let actual = sha256_file(&partial)?;
    if actual != expected_sha256 {
        let _ = std::fs::remove_file(&partial);
        return Err("download SHA-256 verification failed".into());
    }
    std::fs::rename(partial, destination).map_err(|e| e.to_string())
}

fn platform_requirements() -> &'static str {
    if cfg!(target_os = "windows") {
        "requirements-windows.txt"
    } else {
        "requirements-macos.txt"
    }
}

fn platform_lockfile() -> &'static str {
    if cfg!(target_os = "windows") {
        "requirements-windows.lock"
    } else {
        "requirements-macos.lock"
    }
}

fn sync_engine_sources(
    app: &AppHandle,
    root: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    use sha2::Digest;

    let src = app
        .path()
        .resource_dir()
        .map_err(|e| format!("no resource dir: {e}"))?
        .join("engine-src");
    if !src.exists() {
        return Err(format!(
            "engine source missing from the app bundle ({src:?})"
        ));
    }
    let files = [
        "server.py",
        "stt_config.py",
        "stt_worker.py",
        "stt_worker_manager.py",
        "tts_engine.py",
        "tts_worker.py",
        "tts_worker_manager.py",
        "benchmark_stt.py",
        "requirements.txt",
        platform_requirements(),
        "requirements-macos.lock",
        "requirements-windows.lock",
    ];
    let clients = ["speak.py", "dictate.py", "snip.py"];
    let mut digest = sha2::Sha256::new();
    for name in files {
        let from = src.join(name);
        if from.exists() {
            digest.update(name.as_bytes());
            digest.update(std::fs::read(&from).map_err(|e| format!("read {name}: {e}"))?);
        }
    }
    for name in clients {
        let from = src.join("client").join(name);
        digest.update(format!("client/{name}").as_bytes());
        digest.update(std::fs::read(&from).map_err(|e| format!("read {name}: {e}"))?);
    }
    let hash = format!("{:x}", digest.finalize());
    let source_name = format!("{}-{}", env!("CARGO_PKG_VERSION"), &hash[..16]);
    let sources = root.join("sources");
    let destination = sources.join(&source_name);
    std::fs::create_dir_all(&sources).map_err(|e| format!("cannot create source store: {e}"))?;

    if !destination.exists() {
        let staging = sources.join(format!(".{source_name}.staging-{}", std::process::id()));
        if staging.exists() {
            std::fs::remove_dir_all(&staging)
                .map_err(|e| format!("cannot clear stale source staging: {e}"))?;
        }
        std::fs::create_dir_all(staging.join("client"))
            .map_err(|e| format!("cannot create source staging: {e}"))?;
        for name in files {
            let from = src.join(name);
            if from.exists() {
                std::fs::copy(&from, staging.join(name))
                    .map_err(|e| format!("copy {name}: {e}"))?;
            }
        }
        for name in clients {
            std::fs::copy(
                src.join("client").join(name),
                staging.join("client").join(name),
            )
            .map_err(|e| format!("copy {name}: {e}"))?;
        }
        std::fs::rename(&staging, &destination)
            .map_err(|e| format!("activate staged source directory: {e}"))?;
    }

    let pointer = active_source_file(root);
    let temporary = root.join(format!("active-source.{}.tmp", std::process::id()));
    std::fs::write(&temporary, format!("{source_name}\n"))
        .map_err(|e| format!("write source pointer: {e}"))?;
    replace_file(&temporary, &pointer).map_err(|e| format!("activate source pointer: {e}"))?;

    Ok(destination)
}

fn find_or_install_uv(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let tools = engine_root().join("tools");
    std::fs::create_dir_all(&tools).map_err(|e| e.to_string())?;
    let executable = tools.join(if cfg!(target_os = "windows") {
        "uv.exe"
    } else {
        "uv"
    });
    if executable.exists() {
        return Ok(executable);
    }
    emit_step(app, 18, "Downloading the Python manager…");
    #[cfg(target_os = "windows")]
    let (url, sha, archive) = (
        "https://github.com/astral-sh/uv/releases/download/0.12.2/uv-x86_64-pc-windows-msvc.zip",
        "01442d8ce5c7124151a73e697c836d252c6da853c18c73206d3cc4c2378a91d2",
        tools.join("uv.zip"),
    );
    #[cfg(not(target_os = "windows"))]
    let (url, sha, archive) = (
        "https://github.com/astral-sh/uv/releases/download/0.12.2/uv-aarch64-apple-darwin.tar.gz",
        "fa909fea3bc06f460db79017030a221fdbc43ec4478f089cb554d8335c090817",
        tools.join("uv.tar.gz"),
    );
    download_verified(url, &archive, None, sha)?;
    #[cfg(target_os = "windows")]
    {
        let file = std::fs::File::open(&archive).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        let flat = zip.by_name("uv.exe").is_ok();
        let prefixed = zip.by_name("uv-x86_64-pc-windows-msvc/uv.exe").is_ok();
        let entry = windows_uv_archive_entry(flat, prefixed)?;
        let mut source = zip.by_name(entry).map_err(|e| e.to_string())?;
        let mut output = std::fs::File::create(&executable).map_err(|e| e.to_string())?;
        std::io::copy(&mut source, &mut output).map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let file = std::fs::File::open(&archive).map_err(|e| e.to_string())?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(decoder);
        for entry in tar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            if entry.path().map_err(|e| e.to_string())?.ends_with("uv") {
                entry.unpack(&executable).map_err(|e| e.to_string())?;
                break;
            }
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(archive);
    executable
        .exists()
        .then_some(executable)
        .ok_or_else(|| "verified Python manager archive did not contain uv".into())
}

#[cfg(target_os = "windows")]
fn windows_uv_archive_entry(
    has_flat_entry: bool,
    has_prefixed_entry: bool,
) -> Result<&'static str, String> {
    if has_flat_entry {
        Ok("uv.exe")
    } else if has_prefixed_entry {
        Ok("uv-x86_64-pc-windows-msvc/uv.exe")
    } else {
        Err("verified Python manager archive did not contain uv.exe".into())
    }
}

/// Build the engine into Application Support.
///
/// Every step is idempotent so an interrupted run can simply be retried — a
/// half-built environment is the most likely failure and the least forgivable
/// one to strand someone in.
fn setup_engine_inner(app: &AppHandle) -> Result<String, String> {
    SETUP_CANCELLED.store(false, Ordering::SeqCst);
    let root = engine_root();
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {root:?}: {e}"))?;

    // 1. Copy the engine source out of the app bundle.
    emit_step(app, 5, "Unpacking…");
    let source_root = sync_engine_sources(app, &root)?;

    // 2. uv — manages Python without touching the system install.
    emit_step(app, 15, "Setting up Python…");
    let uv = find_or_install_uv(app)?;

    // 3. Environment. Setup only runs while the engine is not installed, so no
    //    engine holds this environment. uv refuses to reuse an existing one, and
    //    `--no-deps` below would never remove a package a newer lock dropped, so
    //    rebuild it from the lock every time.
    let venv_ok = Command::new(&uv)
        .args(["venv", "--clear", "--python", "3.12"])
        .arg(root.join(".venv"))
        .status()
        .map_err(|e| format!("uv venv: {e}"))?;
    if !venv_ok.success() {
        return Err("could not create the Python environment".into());
    }

    emit_step(app, 30, "Installing components… (a minute or two)");
    let lockfile = platform_lockfile();
    let lock = source_root.join(lockfile);
    if !lock.exists() {
        return Err(format!("locked dependency set is missing: {lockfile}"));
    }
    let st = Command::new(&uv)
        .args(["pip", "install", "--require-hashes", "--no-deps", "-r"])
        .arg(&lock)
        .env("VIRTUAL_ENV", root.join(".venv"))
        .status()
        .map_err(|e| format!("uv pip install: {e}"))?;
    if !st.success() {
        return Err(format!(
            "could not install verified dependencies from {lockfile}"
        ));
    }
    // 4. Models — not redistributed, fetched from upstream. Sizes are verified
    //    because a truncated download fails much later and far less obviously.
    std::fs::create_dir_all(root.join("models")).ok();
    // The superseded model-files-v1.0 export; nothing loads it after this run.
    let _ = std::fs::remove_file(root.join("models/kokoro-v1.0.fp16.onnx"));
    let base = "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.1";
    for (upstream, name, expected, sha256, pct) in [
        (
            "kokoro-v1.0.fp16.onnx",
            KOKORO_MODEL_FILE,
            163_527_961u64,
            "f3a290d384fbb27966d462905c71a46cef9e5fd00516b40df32a0b4afe77ac96",
            55u32,
        ),
        (
            KOKORO_VOICES_FILE,
            KOKORO_VOICES_FILE,
            28_214_398u64,
            "bca610b8308e8d99f32e6fe4197e7ec01679264efed0cac9140fe9c29f1fbf7d",
            80u32,
        ),
    ] {
        let dest = root.join("models").join(name);
        if dest.metadata().map(|m| m.len()).ok() == Some(expected) {
            if sha256_file(&dest).is_ok_and(|actual| actual == sha256) {
                continue;
            }
            let _ = std::fs::remove_file(&dest);
        }
        emit_step(app, pct, &format!("Downloading voices ({name})…"));
        download_verified(&format!("{base}/{upstream}"), &dest, Some(expected), sha256)?;
    }

    // Prime the platform STT model while networking is deliberately available.
    // The engine itself starts with HF_HUB_OFFLINE=1, so a clean install must
    // populate the cache here rather than failing on its first dictation.
    emit_step(app, 88, "Downloading speech recognition…");
    #[cfg(target_os = "macos")]
    let stt_prime = Command::new(python_path(&root)).env_remove("HF_HUB_OFFLINE").env("HF_HOME", runtime::stt_cache_home(&root)).args(["-c",
        "import numpy as np, mlx_whisper; from huggingface_hub import snapshot_download; p=snapshot_download('mlx-community/whisper-large-v3-turbo',revision='a4aaeec0636e6fef84abdcbe3544cb2bf7e9f6fb'); mlx_whisper.transcribe(np.zeros(16000,dtype='float32'), path_or_hf_repo=p, language='en')"
    ]).status();
    #[cfg(target_os = "windows")]
    let stt_prime = Command::new(python_path(&root)).env_remove("HF_HUB_OFFLINE").env("HF_HOME", runtime::stt_cache_home(&root)).args(["-c",
        "from faster_whisper import WhisperModel; WhisperModel('dropbox-dash/faster-whisper-large-v3-turbo', revision='0a363e9161cbc7ed1431c9597a8ceaf0c4f78fcf', device='cpu', compute_type='int8')"
    ]).status();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let stt_prime = Ok(std::process::ExitStatus::default());
    if !stt_prime.map(|s| s.success()).unwrap_or(false) {
        return Err("could not install the speech-recognition model".into());
    }
    #[cfg(target_os = "windows")]
    {
        emit_step(app, 90, "Optimizing speech recognition for this PC…");
        let benchmark = Command::new(python_path(&root))
            .arg(source_root.join("benchmark_stt.py"))
            .arg("--output")
            .arg(config_dir().join("stt-backend.json"))
            .env_remove("HF_HUB_OFFLINE")
            .status();
        if !benchmark.map(|s| s.success()).unwrap_or(false) {
            return Err("could not benchmark the Windows speech-recognition backend".into());
        }
    }

    // 5. Auth token. Runtime repeats this gate on every launch so a missing
    // token can never silently downgrade the loopback service.
    emit_step(app, 92, "Finishing…");
    ensure_auth_token(&python_path(&root))?;

    emit_step(app, 100, "Ready");
    spawn_engine_and_record(app)?;
    Ok("installed".into())
}

#[tauri::command]
async fn setup_engine(app: AppHandle) -> Result<String, String> {
    let result = setup_engine_inner(&app);
    if let Err(error) = &result {
        let state = SetupState {
            schema_version: 1,
            stage: "failed".into(),
            pct: setup_status()
                .get("pct")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            completed: false,
            error: Some(error.clone()),
        };
        let _ = write_json_atomic(&setup_state_file(), &state);
        structured_log(
            "setup-failed",
            serde_json::json!({ "code": "setup-failed" }),
        );
    }
    result
}

#[tauri::command]
async fn resume_setup(app: AppHandle) -> Result<String, String> {
    setup_engine(app).await
}

#[tauri::command]
fn cancel_setup() {
    SETUP_CANCELLED.store(true, Ordering::SeqCst);
}

// ── preferences ──────────────────────────────────────────────────────────────

fn prefs_file() -> std::path::PathBuf {
    config_dir().join("prefs.json")
}

fn load_prefs() -> serde_json::Value {
    preferences::Preferences::load(&prefs_file()).into_value()
}

#[tauri::command]
fn get_prefs() -> serde_json::Value {
    load_prefs()
}

#[tauri::command]
fn set_prefs(
    voice: Option<String>,
    speed: Option<f64>,
    cue_enabled: Option<bool>,
    cue_volume: Option<f64>,
    live_preview: Option<bool>,
    media_mode: Option<String>,
    media_duck_level: Option<f64>,
) -> Result<serde_json::Value, String> {
    let mut preferences = preferences::Preferences::load(&prefs_file());
    preferences.set_general(voice, speed, cue_enabled, cue_volume, live_preview);
    if let Some(mode) = media_mode {
        let mode = preferences::MediaMode::parse(&mode)
            .ok_or_else(|| "Choose a valid other-audio setting.".to_string())?;
        preferences.set_media_mode(mode);
    }
    if let Some(level) = media_duck_level {
        preferences.set_media_duck_level(level);
    }
    let value = preferences.into_value();
    write_json_atomic(&prefs_file(), &value)?;
    Ok(value)
}

#[tauri::command]
fn list_voices() -> Vec<String> {
    local_json("/voices", std::time::Duration::from_secs(5))
        .and_then(|v| {
            v.get("voices")?.as_array().map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// Voice and speed as CLI arguments for the speak client.
fn voice_args() -> Vec<String> {
    let preferences = preferences::Preferences::load(&prefs_file());
    vec![
        "--voice".into(),
        preferences.voice().to_string(),
        "--speed".into(),
        format!("{}", preferences.speed()),
    ]
}

fn microphone_arg() -> Option<String> {
    preferences::Preferences::load(&prefs_file())
        .microphone_device()
        .map(String::from)
}

#[tauri::command]
fn microphone_devices() -> serde_json::Value {
    let Some(paths) = Paths::current() else {
        return serde_json::json!([]);
    };
    client_command(&paths, "dictate.py")
        .arg("--devices")
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or_else(|| serde_json::json!([]))
}

#[tauri::command]
fn set_microphone(device: Option<String>) -> serde_json::Value {
    let mut preferences = preferences::Preferences::load(&prefs_file());
    preferences.set(
        "microphone_device",
        device
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    let value = preferences.into_value();
    let _ = write_json_atomic(&prefs_file(), &value);
    value
}

#[tauri::command]
fn dictation_status(app: AppHandle) -> serde_json::Value {
    app.try_state::<Dictation>()
        .and_then(|d| {
            d.0.lock().ok().and_then(|s| {
                s.as_ref().map(|x| {
                    serde_json::json!({
                        "session": x.id,
                        "state": x.status,
                        "elapsed_ms": x.started.elapsed().as_millis(),
                        "child_pid": x.child_pid,
                        "target_verified": x.target.is_some(),
                        "live_characters": x.inserted_text.chars().count(),
                        "fallback_reason": x.fallback_reason,
                    })
                })
            })
        })
        .unwrap_or_else(|| serde_json::json!({ "state": "idle" }))
}

fn app_info() -> serde_json::Value {
    let platform = std::env::consts::OS;
    serde_json::json!({
        "app_version": env!("CARGO_PKG_VERSION"),
        "build_revision": option_env!("HEREWORD_BUILD_REVISION").unwrap_or("development"),
        "platform": platform,
        "architecture": std::env::consts::ARCH,
        "paste_shortcut": if platform == "windows" { "Ctrl+V" } else { "Command+V" },
    })
}

#[tauri::command]
fn export_diagnostics(app: AppHandle) -> Result<String, String> {
    use sha2::Digest;
    let events = config_dir().join("events.jsonl");
    let event_bytes = std::fs::read(&events).unwrap_or_default();
    let performance: serde_json::Value = std::fs::read_to_string(performance_profile_file())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(serde_json::Value::Null);
    let capabilities: serde_json::Value =
        std::fs::read_to_string(config_dir().join("capabilities.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(serde_json::Value::Null);
    let document = serde_json::json!({
        "app": app_info(),
        "app_version": env!("CARGO_PKG_VERSION"),
        "build_revision": option_env!("HEREWORD_BUILD_REVISION").unwrap_or("development"),
        "platform": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "installed": is_installed(),
        "engine": engine_status(),
        "hotkeys": hotkeys(),
        "dictation": dictation_status(app.clone()),
        "permissions": permission_status(),
        "setup": setup_status(),
        "storage": storage_status(),
        "performance": performance,
        "capabilities": capabilities,
        "log_integrity": {
            "bytes": event_bytes.len(),
            "sha256": format!("{:x}", sha2::Sha256::digest(&event_bytes)),
        },
        // Deliberately excludes token values, transcripts, clipboard content,
        // environment variables, and full process command lines.
    });
    let dir = app
        .path()
        .download_dir()
        .unwrap_or_else(|_| std::env::temp_dir());
    let path = dir.join(variant::DIAGNOSTICS_FILE);
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&document).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("cannot write diagnostics: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

fn directory_size(path: &std::path::Path) -> u64 {
    std::fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| {
            let Ok(file_type) = entry.file_type() else {
                return 0;
            };
            if file_type.is_symlink() {
                return 0;
            }
            if file_type.is_dir() {
                directory_size(&entry.path())
            } else {
                entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
            }
        })
        .sum()
}

#[tauri::command]
fn storage_status() -> serde_json::Value {
    let shared_stt_cache = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("huggingface")
        .join("hub")
        .join("models--mlx-community--whisper-large-v3-turbo");
    let legacy_runtime = legacy_config_dir().map(|directory| directory.join("runtime"));
    serde_json::json!({
        "engine_bytes": directory_size(&engine_root()),
        "config_bytes": directory_size(&config_dir()),
        "current_runtime_bytes": runtime_hygiene::managed_runtime_bytes(&config_dir().join("runtime")),
        "legacy_runtime_bytes": legacy_runtime
            .as_deref()
            .map(runtime_hygiene::managed_runtime_bytes)
            .unwrap_or(0),
        "shared_stt_cache_bytes": directory_size(&shared_stt_cache),
        "preferences_present": prefs_file().exists(),
    })
}

#[tauri::command]
fn permission_status() -> serde_json::Value {
    #[cfg(target_os = "macos")]
    {
        let accessibility = macos_accessibility_client::accessibility::application_is_trusted();
        let input_monitoring = objc2_core_graphics::CGPreflightListenEventAccess();
        let microphone = microphone_permission::status();
        serde_json::json!({
            "accessibility": if accessibility { "available" } else { "required" },
            "input_monitoring": if input_monitoring { "available" } else { "required" },
            "microphone": microphone.as_permission_state(),
            "screen_capture": "checked-on-use",
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        serde_json::json!({
            "accessibility": "not-required",
            "input_monitoring": "not-required",
            "microphone": "checked-on-use",
            "screen_capture": "available",
        })
    }
}

#[tauri::command]
fn run_capability_test(capability: String) -> serde_json::Value {
    match capability.as_str() {
        "engine" => engine_status(),
        "permissions" => permission_status(),
        "microphone" => {
            #[cfg(target_os = "macos")]
            if !microphone_permission::status().is_authorized() {
                return serde_json::json!({
                    "ok": false,
                    "code": "microphone-permission-required"
                });
            }
            let Some(paths) = Paths::current() else {
                return serde_json::json!({ "ok": false, "code": "engine-missing" });
            };
            client_command(&paths, "dictate.py")
                .arg("--probe-device")
                .output()
                .ok()
                .and_then(|out| serde_json::from_slice(&out.stdout).ok())
                .unwrap_or_else(|| serde_json::json!({ "ok": false, "code": "probe-failed" }))
        }
        _ => serde_json::json!({ "ok": false, "code": "unknown-capability" }),
    }
}

#[tauri::command]
fn record_capability(capability: String, passed: bool) -> Result<(), String> {
    let path = config_dir().join("capabilities.json");
    let mut report: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({ "schema_version": 1, "results": {} }));
    report["results"][&capability] = serde_json::json!({
        "passed": passed,
        "checked_at_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()
    });
    write_json_atomic(&path, &report)
}

#[cfg(target_os = "macos")]
fn request_microphone_permission(app: &AppHandle) -> bool {
    if microphone_permission::status() != microphone_permission::Status::NotDetermined {
        return false;
    }
    if MICROPHONE_PERMISSION_REQUEST_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return true;
    }

    let app = app.clone();
    microphone_permission::request_access(move |granted| {
        MICROPHONE_PERMISSION_REQUEST_IN_FLIGHT.store(false, Ordering::SeqCst);
        structured_log(
            "microphone-permission-result",
            serde_json::json!({ "granted": granted }),
        );
        let _ = app.emit("microphone-permission-changed", granted);
        let _ = log_runtime_readiness();
    });
    true
}

#[tauri::command]
fn retry_permission(app: AppHandle, capability: String) -> serde_json::Value {
    #[cfg(target_os = "macos")]
    if capability == "accessibility" {
        let available =
            macos_accessibility_client::accessibility::application_is_trusted_with_prompt();
        return serde_json::json!({ "capability": capability, "available": available });
    }
    #[cfg(target_os = "macos")]
    if capability == "input-monitoring" {
        let available = objc2_core_graphics::CGRequestListenEventAccess();
        return serde_json::json!({ "capability": capability, "available": available });
    }
    #[cfg(target_os = "macos")]
    if capability == "microphone" {
        let requested = request_microphone_permission(&app);
        let status = microphone_permission::status();
        return serde_json::json!({
            "capability": capability,
            "available": status.is_authorized(),
            "requested": requested,
            "state": status.as_code(),
        });
    }

    #[cfg(not(target_os = "macos"))]
    let _ = (app, capability);

    permission_status()
}

#[tauri::command]
fn system_check(app: AppHandle) -> serde_json::Value {
    #[cfg(target_os = "macos")]
    if objc2_core_graphics::CGPreflightListenEventAccess()
        && !HOTKEYS_REGISTERED.load(Ordering::SeqCst)
    {
        let _ = register_hotkeys(&app);
    }

    #[cfg(not(target_os = "macos"))]
    let _ = &app;

    serde_json::json!({
        "app": app_info(),
        "engine": engine_status(),
        "permissions": permission_status(),
        "hotkeys": hotkeys(),
        "microphones": microphone_devices(),
        "setup": setup_status(),
        "offline_ready": is_installed(),
    })
}

#[tauri::command]
fn launch_at_login_status(app: AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

fn launch_at_login_preference(prefs: &serde_json::Value) -> bool {
    preferences::Preferences::from_value(prefs.clone()).launch_at_login(variant::DEFAULT_AUTOSTART)
}

#[tauri::command]
fn set_launch_at_login(app: AppHandle, enabled: bool) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())?;
    } else {
        manager.disable().map_err(|e| e.to_string())?;
    }
    let active = manager.is_enabled().unwrap_or(false);
    let mut preferences = preferences::Preferences::load(&prefs_file());
    preferences.set("launch_at_login", serde_json::Value::Bool(active));
    write_json_atomic(&prefs_file(), preferences.as_value())?;
    structured_log(
        "autostart-changed",
        serde_json::json!({ "enabled": active }),
    );
    Ok(active)
}

#[tauri::command]
fn remove_local_data(app: AppHandle, remove_preferences: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    stop_engine(&app);
    let result = (|| {
        if engine_root().exists() {
            std::fs::remove_dir_all(engine_root())
                .map_err(|e| format!("cannot remove engine data: {e}"))?;
        }
        if remove_preferences && config_dir().exists() {
            let _ = app.autolaunch().disable();
            std::fs::remove_dir_all(config_dir())
                .map_err(|e| format!("cannot remove preferences: {e}"))?;
        }
        Ok(())
    })();
    QUITTING.store(false, Ordering::SeqCst);
    result
}

// ── commands ─────────────────────────────────────────────────────────────────

#[tauri::command]
fn engine_status() -> serde_json::Value {
    if !is_installed() {
        return serde_json::json!({ "status": "not-installed" });
    }
    local_json("/health", std::time::Duration::from_secs(3))
        .unwrap_or_else(|| serde_json::json!({ "status": "down" }))
}

fn send_dictation_command(control: &Arc<Mutex<ChildStdin>>, session: &str, command: &str) -> bool {
    let Ok(record) = serde_json::to_string(&serde_json::json!({
        "command": command,
        "session": session,
    })) else {
        return false;
    };
    let Ok(mut stdin) = control.lock() else {
        return false;
    };
    writeln!(stdin, "{record}")
        .and_then(|_| stdin.flush())
        .is_ok()
}

fn is_dictating(app: &AppHandle) -> bool {
    app.try_state::<Dictation>()
        .and_then(|d| d.0.lock().ok().map(|s| s.is_some()))
        .unwrap_or(false)
}

fn set_dictation_status(app: &AppHandle, id: &str, status: DictationStatus) {
    if let Some(d) = app.try_state::<Dictation>() {
        if let Ok(mut guard) = d.0.lock() {
            if let Some(session) = guard.as_mut().filter(|s| s.id == id) {
                session.status = status.clone();
            }
        }
    }
    let _ = app.emit(
        "dictation-state",
        serde_json::json!({
            "session": id,
            "state": status.clone(),
        }),
    );
    structured_log(
        "dictation-state",
        serde_json::json!({ "session": id, "state": status }),
    );
}

#[cfg(target_os = "macos")]
fn status_window_collection_behavior() -> objc2_app_kit::NSWindowCollectionBehavior {
    use objc2_app_kit::NSWindowCollectionBehavior as Behavior;

    // This is a transport overlay, not one of HereWord's normal application
    // windows. It must remain eligible while another app owns the active Space
    // or Stage Manager set, including when that app is full-screen.
    Behavior::CanJoinAllSpaces
        | Behavior::CanJoinAllApplications
        | Behavior::FullScreenAuxiliary
        | Behavior::Stationary
        | Behavior::IgnoresCycle
}

#[cfg(target_os = "macos")]
fn status_window_level() -> objc2_app_kit::NSWindowLevel {
    // Floating/status levels remain below another application's full-screen
    // content. The transport is visible only while HereWord is actively playing,
    // recording, transcribing, or reporting a short notice, so the screen-saver
    // overlay level is both necessary and tightly bounded.
    objc2_app_kit::NSScreenSaverWindowLevel
}

#[cfg(target_os = "macos")]
fn status_window_style_mask() -> objc2_app_kit::NSWindowStyleMask {
    use objc2_app_kit::NSWindowStyleMask as Style;

    Style::Borderless | Style::NonactivatingPanel
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatusWindowRecovery {
    None,
    Reassert,
    Recreate,
}

#[cfg(target_os = "macos")]
fn status_window_recovery(
    requested: bool,
    visible: bool,
    on_active_space: bool,
    space_recovery_armed: bool,
) -> StatusWindowRecovery {
    if !requested {
        return StatusWindowRecovery::None;
    }
    if !on_active_space {
        return if space_recovery_armed {
            StatusWindowRecovery::Recreate
        } else {
            StatusWindowRecovery::None
        };
    }
    if !visible {
        return StatusWindowRecovery::Reassert;
    }
    StatusWindowRecovery::None
}

#[cfg(target_os = "macos")]
fn set_status_window_requested(requested: bool) {
    STATUS_WINDOW_REQUESTED.store(requested, Ordering::SeqCst);
    STATUS_SPACE_RECOVERY_ARMED.store(requested, Ordering::SeqCst);
    let (lock, changed) = STATUS_SPACE_STATE.get_or_init(|| (Mutex::new(false), Condvar::new()));
    if let Ok(mut active) = lock.lock() {
        *active = requested;
        changed.notify_all();
    }
}

const PLAYER_WIDTH: f64 = 205.0;
const PLAYER_HEIGHT: f64 = 50.0;
const NOTICE_WIDTH: f64 = 360.0;
const NOTICE_HEIGHT: f64 = 66.0;

#[cfg(target_os = "macos")]
fn place_status_panel(panel: &objc2_app_kit::NSPanel, width: f64, height: f64) {
    let Some(main_thread) = objc2::MainThreadMarker::new() else {
        return;
    };
    let mut frame = objc2_app_kit::NSScreen::mainScreen(main_thread)
        .map(|screen| screen.visibleFrame())
        .unwrap_or_else(|| panel.frame());
    let saved = STATUS_PANEL_POSITION
        .get_or_init(|| Mutex::new(load_status_panel_position()))
        .lock()
        .ok()
        .and_then(|position| *position);
    if let Some((x, y)) = saved {
        if let Some(screen) = objc2_app_kit::NSScreen::screens(main_thread)
            .iter()
            .find(|screen| {
                let area = screen.visibleFrame();
                x >= area.origin.x
                    && x < area.origin.x + area.size.width
                    && y >= area.origin.y
                    && y < area.origin.y + area.size.height
            })
        {
            frame = screen.visibleFrame();
        }
    }
    let default_x = frame.origin.x + frame.size.width - width - 18.0;
    let default_y = frame.origin.y + frame.size.height - height - 14.0;
    let (x, y) = saved.unwrap_or((default_x, default_y));
    frame.origin.x = x.clamp(
        frame.origin.x,
        (frame.origin.x + frame.size.width - width).max(frame.origin.x),
    );
    frame.origin.y = y.clamp(
        frame.origin.y,
        (frame.origin.y + frame.size.height - height).max(frame.origin.y),
    );
    frame.size.width = width;
    frame.size.height = height;
    panel.setFrame_display(frame, true);
}

#[cfg(target_os = "macos")]
fn status_panel_position_file() -> std::path::PathBuf {
    config_dir().join("player-position.json")
}

#[cfg(target_os = "macos")]
fn load_status_panel_position() -> Option<(f64, f64)> {
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(status_panel_position_file()).ok()?).ok()?;
    let x = value.get("x")?.as_f64()?;
    let y = value.get("y")?.as_f64()?;
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

#[cfg(target_os = "macos")]
fn configure_status_panel(panel: &objc2_app_kit::NSPanel, width: f64, height: f64) {
    place_status_panel(panel, width, height);
    panel.setCollectionBehavior(status_window_collection_behavior());
    panel.setHidesOnDeactivate(false);
    panel.setCanHide(false);
    panel.setLevel(status_window_level());
}

#[cfg(target_os = "macos")]
fn new_status_panel(
    main_thread: objc2::MainThreadMarker,
    frame: objc2_foundation::NSRect,
) -> objc2::rc::Retained<objc2_app_kit::NSPanel> {
    let panel = objc2_app_kit::NSPanel::initWithContentRect_styleMask_backing_defer(
        main_thread.alloc(),
        frame,
        status_window_style_mask(),
        objc2_app_kit::NSBackingStoreType::Buffered,
        false,
    );
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setOpaque(false);
    panel.setHasShadow(false);
    unsafe { panel.setReleasedWhenClosed(false) };
    panel
}

/// Replace a panel that AppKit left bound to an inactive Space. Reordering the
/// same NSPanel cannot change that binding; a new panel is born on the current
/// Space, then receives the existing webview without activating HereWord.
#[cfg(target_os = "macos")]
fn recreate_status_panel_for_active_space(
    panel_pointer: *mut objc2_app_kit::NSPanel,
    width: f64,
    height: f64,
) -> Option<*mut objc2_app_kit::NSPanel> {
    let main_thread = objc2::MainThreadMarker::new()?;
    let previous = unsafe { &*panel_pointer };
    let content_view = previous.contentView()?;
    let replacement = new_status_panel(main_thread, previous.frame());
    replacement.setBackgroundColor(Some(&previous.backgroundColor()));

    // Retain the webview while its old panel receives a harmless placeholder.
    // AppKit views may belong to only one window at a time.
    let placeholder = objc2_app_kit::NSView::initWithFrame(main_thread.alloc(), previous.frame());
    previous.setContentView(Some(&placeholder));
    replacement.setContentView(Some(&content_view));
    configure_status_panel(&replacement, width, height);

    previous.orderOut(None);
    replacement.orderFrontRegardless();
    let replacement_pointer = objc2::rc::Retained::into_raw(replacement);
    STATUS_PANEL.store(replacement_pointer, Ordering::SeqCst);

    // STATUS_PANEL owned the retain created by into_raw when the old panel was
    // installed. The atomic now owns the replacement, so release the old one.
    drop(unsafe { objc2::rc::Retained::from_raw(panel_pointer) });
    Some(replacement_pointer)
}

/// macOS can leave an already-visible all-Spaces panel assigned to the Space
/// it was first ordered on when the user moves into another app's full-screen
/// Space. The audio client remains alive, so hiding the only controls is an
/// invalid state. Recreate the panel once when it falls off the active Space;
/// repeatedly ordering the same panel cannot change its Space assignment and
/// previously produced thousands of retries without restoring the controls.
#[cfg(target_os = "macos")]
fn start_status_space_watcher(app: AppHandle) {
    if STATUS_SPACE_WATCHER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || loop {
        let (lock, changed) =
            STATUS_SPACE_STATE.get_or_init(|| (Mutex::new(false), Condvar::new()));
        let Ok(mut active) = lock.lock() else {
            return;
        };
        while !*active {
            if QUITTING.load(Ordering::Relaxed) {
                return;
            }
            let Ok(next) = changed.wait(active) else {
                return;
            };
            active = next;
        }
        let Ok((active, _)) = changed.wait_timeout(active, std::time::Duration::from_millis(150))
        else {
            return;
        };
        if !*active {
            continue;
        }
        drop(active);
        if QUITTING.load(Ordering::Relaxed) {
            return;
        }
        if !STATUS_WINDOW_REQUESTED.load(Ordering::SeqCst) {
            continue;
        }
        let Some(window) = app.get_webview_window("player") else {
            continue;
        };
        let _ = window.run_on_main_thread(move || {
            let panel_pointer = STATUS_PANEL.load(Ordering::SeqCst);
            if panel_pointer.is_null() || !STATUS_WINDOW_REQUESTED.load(Ordering::SeqCst) {
                return;
            }
            let panel = unsafe { &*panel_pointer };
            let visible = panel.isVisible();
            let on_active_space = panel.isOnActiveSpace();
            if on_active_space {
                STATUS_SPACE_RECOVERY_ARMED.store(true, Ordering::SeqCst);
            }
            let width = f64::from_bits(STATUS_WINDOW_WIDTH_BITS.load(Ordering::SeqCst));
            let height = f64::from_bits(STATUS_WINDOW_HEIGHT_BITS.load(Ordering::SeqCst));
            match status_window_recovery(
                true,
                visible,
                on_active_space,
                STATUS_SPACE_RECOVERY_ARMED.load(Ordering::SeqCst),
            ) {
                StatusWindowRecovery::None => {}
                StatusWindowRecovery::Reassert => {
                    configure_status_panel(panel, width, height);
                    panel.orderFrontRegardless();
                    structured_log(
                        "status-window-reasserted",
                        serde_json::json!({
                            "visible": panel.isVisible(),
                            "active_space": panel.isOnActiveSpace(),
                            "level": panel.level(),
                        }),
                    );
                }
                StatusWindowRecovery::Recreate => {
                    if STATUS_SPACE_RECOVERY_ARMED.swap(false, Ordering::SeqCst) {
                        if let Some(pointer) =
                            recreate_status_panel_for_active_space(panel_pointer, width, height)
                        {
                            let replacement = unsafe { &*pointer };
                            structured_log(
                                "status-window-recreated",
                                serde_json::json!({
                                    "visible": replacement.isVisible(),
                                    "active_space": replacement.isOnActiveSpace(),
                                    "level": replacement.level(),
                                }),
                            );
                        } else {
                            structured_log(
                                "status-window-recovery-failed",
                                serde_json::json!({ "reason": "panel-recreation-unavailable" }),
                            );
                        }
                    }
                }
            }
        });
    });
}

#[cfg(target_os = "macos")]
fn show_status_window_without_activation(window: &tauri::WebviewWindow, width: f64, height: f64) {
    // WebviewWindow::show can activate a regular macOS application even when
    // the window itself is non-focusable. That steals the Accessibility target
    // between capture and the first live preview. AppKit's
    // orderFrontRegardless makes an inactive window visible without activating
    // its owning application.
    let window = window.clone();
    let _ = window.clone().run_on_main_thread(move || {
        let Ok(pointer) = window.ns_window() else {
            return;
        };
        let native = unsafe { &*pointer.cast::<objc2_app_kit::NSWindow>() };
        let Some(main_thread) = objc2::MainThreadMarker::new() else {
            return;
        };

        let panel_pointer = STATUS_PANEL.load(Ordering::SeqCst);
        let panel = if panel_pointer.is_null() {
            let mut frame = objc2_app_kit::NSScreen::mainScreen(main_thread)
                .map(|screen| screen.visibleFrame())
                .unwrap_or_else(|| native.frame());
            frame.origin.x += frame.size.width - width - 18.0;
            frame.origin.y += frame.size.height - height - 14.0;
            frame.size.width = width;
            frame.size.height = height;
            let panel = new_status_panel(main_thread, frame);
            panel.setBackgroundColor(Some(&native.backgroundColor()));
            if let Some(content_view) = native.contentView() {
                panel.setContentView(Some(&content_view));
                // Tao's resize delegate assumes its NSWindow always has a
                // content view. Keep that invariant after transferring the
                // actual player webview into the overlay panel.
                let placeholder =
                    objc2_app_kit::NSView::initWithFrame(main_thread.alloc(), native.frame());
                native.setContentView(Some(&placeholder));
            }
            native.orderOut(None);
            let panel_pointer = objc2::rc::Retained::into_raw(panel);
            STATUS_PANEL.store(panel_pointer, Ordering::SeqCst);
            unsafe { &*panel_pointer }
        } else {
            unsafe { &*panel_pointer }
        };

        STATUS_WINDOW_WIDTH_BITS.store(width.to_bits(), Ordering::SeqCst);
        STATUS_WINDOW_HEIGHT_BITS.store(height.to_bits(), Ordering::SeqCst);
        set_status_window_requested(true);
        configure_status_panel(panel, width, height);
        // Do this synchronously in the same main-thread turn as ordering the
        // window. Tauri's set_always_on_top queues an asynchronous floating-
        // level update that can race this order operation and leave the player
        // under a full-screen app or on another Space.
        panel.orderFrontRegardless();
        structured_log(
            "status-window-shown",
            serde_json::json!({
                "visible": panel.isVisible(),
                "active_space": panel.isOnActiveSpace(),
                "level": panel.level(),
                "native_panel": true,
                "joins_all_spaces": true,
                "joins_all_applications": true,
            }),
        );
    });
}

#[cfg(not(target_os = "macos"))]
fn show_status_window_without_activation(window: &tauri::WebviewWindow, _width: f64, _height: f64) {
    let _ = window.show();
}

#[cfg(target_os = "macos")]
fn hide_status_window(window: &tauri::WebviewWindow) {
    set_status_window_requested(false);
    let window = window.clone();
    let _ = window.run_on_main_thread(move || {
        let panel_pointer = STATUS_PANEL.load(Ordering::SeqCst);
        if !panel_pointer.is_null() {
            unsafe { &*panel_pointer }.orderOut(None);
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn hide_status_window(window: &tauri::WebviewWindow) {
    let _ = window.hide();
}

/// Park the transport in the upper-right of the work area and show it.
fn show_player(app: &AppHandle) {
    let Some(w) = app.get_webview_window("player") else {
        return;
    };
    // This status bubble must never become the Accessibility-focused element;
    // dictation owns and validates the editor that was focused before it opens.
    let _ = w.set_focusable(false);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = w.set_size(tauri::LogicalSize::new(PLAYER_WIDTH, PLAYER_HEIGHT));
        if let Ok(Some(mon)) = w.primary_monitor() {
            let scale = mon.scale_factor();
            let work_area = mon.work_area();
            let size = work_area.size.to_logical::<f64>(scale);
            let pos = work_area.position.to_logical::<f64>(scale);
            let _ = w.set_position(tauri::LogicalPosition::new(
                pos.x + size.width - 170.0,
                pos.y + 14.0,
            ));
        }
    }
    show_status_window_without_activation(&w, PLAYER_WIDTH, PLAYER_HEIGHT);
}

fn show_player_notice(app: &AppHandle, message: &str) {
    let Some(w) = app.get_webview_window("player") else {
        return;
    };
    let _ = w.set_focusable(false);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = w.set_size(tauri::LogicalSize::new(NOTICE_WIDTH, NOTICE_HEIGHT));
        if let Ok(Some(mon)) = w.primary_monitor() {
            let scale = mon.scale_factor();
            let work_area = mon.work_area();
            let size = work_area.size.to_logical::<f64>(scale);
            let pos = work_area.position.to_logical::<f64>(scale);
            let _ = w.set_position(tauri::LogicalPosition::new(
                pos.x + size.width - NOTICE_WIDTH - 18.0,
                pos.y + 14.0,
            ));
        }
    }
    let encoded = serde_json::to_string(message).unwrap_or_else(|_| "\"HereWord error\"".into());
    let _ = w.eval(format!(
        "window.__kokoroShowNotice && window.__kokoroShowNotice({encoded})"
    ));
    show_status_window_without_activation(&w, NOTICE_WIDTH, NOTICE_HEIGHT);
}

/// Tell the transport what it is representing: "playing" or "recording".
fn set_player_mode(app: &AppHandle, mode: &str) {
    // A global event can be emitted in the narrow gap before the player
    // webview's async listener is registered. Target the actual window too so
    // its visuals cannot remain in the default playback state during capture.
    if matches!(mode, "playing" | "starting" | "recording" | "transcribing") {
        if let Some(window) = app.get_webview_window("player") {
            let _ = window.eval(format!(
                "window.__kokoroSetPlayerMode && window.__kokoroSetPlayerMode({mode:?})"
            ));
        }
    }
    let _ = app.emit("player-mode", mode);
}

fn hide_player(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("player") {
        hide_status_window(&w);
    }
}

/// A speech/notice completion may arrive after Dictate has claimed the panel.
/// Check on the macOS UI thread as well as here, since ordering the panel is
/// asynchronous and the recording can begin while a hide is queued.
fn hide_player_unless_dictating(app: &AppHandle) {
    if is_dictating(app) {
        return;
    }
    #[cfg(target_os = "macos")]
    if let Some(window) = app.get_webview_window("player") {
        let app = app.clone();
        let _ = window.run_on_main_thread(move || {
            if is_dictating(&app) {
                return;
            }
            set_status_window_requested(false);
            let pointer = STATUS_PANEL.load(Ordering::SeqCst);
            if !pointer.is_null() {
                unsafe { &*pointer }.orderOut(None);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    hide_player(app);
}

/// Move only the native transport panel. Moving Tauri's hidden owner would
/// break the non-activating, all-Spaces overlay used for dictation targets.
#[tauri::command]
fn move_player_by(app: AppHandle, dx: f64, dy: f64) {
    if !dx.is_finite() || !dy.is_finite() || dx.abs() > 2000.0 || dy.abs() > 2000.0 {
        return;
    }
    #[cfg(target_os = "macos")]
    if let Some(window) = app.get_webview_window("player") {
        let _ = window.run_on_main_thread(move || {
            let pointer = STATUS_PANEL.load(Ordering::SeqCst);
            if pointer.is_null() {
                return;
            }
            let panel = unsafe { &*pointer };
            let mut frame = panel.frame();
            frame.origin.x += dx;
            frame.origin.y -= dy;
            if let Some(main_thread) = objc2::MainThreadMarker::new() {
                let center_x = frame.origin.x + frame.size.width / 2.0;
                let center_y = frame.origin.y + frame.size.height / 2.0;
                let screens = objc2_app_kit::NSScreen::screens(main_thread);
                let screen = screens
                    .iter()
                    .find(|screen| {
                        let area = screen.visibleFrame();
                        center_x >= area.origin.x
                            && center_x < area.origin.x + area.size.width
                            && center_y >= area.origin.y
                            && center_y < area.origin.y + area.size.height
                    })
                    .or_else(|| panel.screen());
                if let Some(screen) = screen {
                    let area = screen.visibleFrame();
                    frame.origin.x = frame.origin.x.clamp(
                        area.origin.x,
                        (area.origin.x + area.size.width - frame.size.width).max(area.origin.x),
                    );
                    frame.origin.y = frame.origin.y.clamp(
                        area.origin.y,
                        (area.origin.y + area.size.height - frame.size.height).max(area.origin.y),
                    );
                }
            }
            panel.setFrame_display(frame, true);
            if let Ok(mut saved) = STATUS_PANEL_POSITION
                .get_or_init(|| Mutex::new(load_status_panel_position()))
                .lock()
            {
                *saved = Some((frame.origin.x, frame.origin.y));
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(window) = app.get_webview_window("player") {
        if let Ok(position) = window.outer_position() {
            let _ = window.set_position(tauri::PhysicalPosition::new(
                position.x + dx.round() as i32,
                position.y + dy.round() as i32,
            ));
        }
    }
}

#[tauri::command]
fn save_player_position(app: AppHandle) {
    #[cfg(target_os = "macos")]
    if let Some(window) = app.get_webview_window("player") {
        let _ = window.run_on_main_thread(move || {
            let pointer = STATUS_PANEL.load(Ordering::SeqCst);
            if pointer.is_null() {
                return;
            }
            let frame = unsafe { &*pointer }.frame();
            let x = frame.origin.x;
            let y = frame.origin.y;
            if let Ok(mut position) = STATUS_PANEL_POSITION
                .get_or_init(|| Mutex::new(load_status_panel_position()))
                .lock()
            {
                *position = Some((x, y));
            }
            let _ = std::fs::create_dir_all(config_dir());
            let _ = std::fs::write(
                status_panel_position_file(),
                format!("{{\"x\":{x},\"y\":{y}}}"),
            );
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

#[tauri::command]
fn close_player_popup(app: AppHandle, notice: bool) {
    if !notice {
        if is_dictating(&app) {
            stop_managed_dictation(&app);
        } else {
            stop_managed_playback(&app);
        }
    }
    hide_player(&app);
}

/// Spawn a client and keep the transport visible for exactly as long as it
/// runs. Fire-and-forget left the user with no way to stop audio: the mini
/// player was a host feature that did not survive the move off Hammerspoon.
fn run_client_monitored(
    app: &AppHandle,
    script: &str,
    args: Vec<String>,
    stdin_payload: Option<String>,
) {
    if is_dictating(app) {
        return;
    }
    let Some(paths) = Paths::current() else {
        let _ = app.emit("engine-missing", ());
        return;
    };
    // Own the script name because the monitoring thread outlives this call.
    let script = script.to_string();
    let app2 = app.clone();
    std::thread::spawn(move || {
        let media_lease = acquire_media_focus(&app2);
        // A delayed Read or Snip result must not start speech after Dictate
        // claimed the shared player. Hold this lock through spawn/registration
        // so Dictate can always pause a child that started just before it.
        let dictation = app2.try_state::<Dictation>();
        let dictation_guard = dictation.as_ref().and_then(|state| state.0.lock().ok());
        if dictation_guard
            .as_ref()
            .is_some_and(|session| session.is_some())
        {
            return;
        }
        if let Some(manager) = app2.try_state::<playback::PlaybackManager>() {
            let _ = manager.stop();
        }
        show_player(&app2);
        set_player_mode(&app2, "playing");
        let mut command = client_command(&paths, &script);
        command
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if stdin_payload.is_some() {
            command.stdin(Stdio::piped());
        }
        let child = command.spawn();
        let generation = child.as_ref().ok().and_then(|child| {
            app2.try_state::<playback::PlaybackManager>()
                .map(|manager| {
                    let generation = manager.begin(child.id());
                    manager.mark_playing(generation);
                    generation
                })
        });
        drop(dictation_guard);
        // A paused Read can remain alive indefinitely. Keep its music fade only
        // while speech is actually playing, including accessory pause/resume.
        let focus_stop = Arc::new(AtomicBool::new(false));
        let focus_watcher = generation.map(|current_generation| {
            let app = app2.clone();
            let stop = focus_stop.clone();
            std::thread::spawn(move || {
                let mut lease = media_lease;
                while !stop.load(Ordering::SeqCst) {
                    let Some(manager) = app.try_state::<playback::PlaybackManager>() else {
                        break;
                    };
                    if !manager.is_current(current_generation) {
                        break;
                    }
                    match manager.state() {
                        "playing" if lease.is_none() => lease = acquire_media_focus(&app),
                        "paused" | "stopping" => drop(lease.take()),
                        _ => {}
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                drop(lease);
            })
        });
        let (generation, output) = match child {
            Ok(mut child) => {
                let input_result = stdin_payload.as_deref().map(|text| {
                    let mut stdin = child
                        .stdin
                        .take()
                        .ok_or_else(|| "speech client input pipe was unavailable".to_string())?;
                    stdin
                        .write_all(text.as_bytes())
                        .map_err(|error| format!("could not send speech text: {error}"))
                });
                let output = match input_result {
                    Some(Err(error)) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        Err(error)
                    }
                    _ => child.wait_with_output().map_err(|error| error.to_string()),
                };
                (generation, output)
            }
            Err(error) => (generation, Err(error.to_string())),
        };
        // Audio has ended even if a notice remains visible afterwards.
        focus_stop.store(true, Ordering::SeqCst);
        if let Some(watcher) = focus_watcher {
            let _ = watcher.join();
        }
        let notice = match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                for line in stdout.lines() {
                    if let Some(stats) = line.strip_prefix("AUDIO_STATS ") {
                        let fields = stats
                            .split_whitespace()
                            .filter_map(|part| part.split_once('='))
                            .filter_map(|(key, value)| {
                                matches!(key, "underflows" | "reopens")
                                    .then(|| value.parse::<u32>().ok().map(|value| (key, value)))
                                    .flatten()
                            })
                            .collect::<std::collections::HashMap<_, _>>();
                        structured_log("speech-audio-stats", serde_json::json!(fields));
                    }
                }
                stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("NOTICE ").map(str::to_owned))
            }
            Err(error) => {
                eprintln!("could not start HereWord playback: {error}");
                Some("HereWord couldn't start. Open Settings.".into())
            }
        };
        let current = generation
            .and_then(|generation| {
                app2.try_state::<playback::PlaybackManager>()
                    .map(|manager| manager.is_current(generation))
            })
            .unwrap_or(true);
        if current && !is_dictating(&app2) {
            if let Some(message) = notice {
                show_player_notice(&app2, &message);
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        }
        let cleared = generation
            .and_then(|generation| {
                app2.try_state::<playback::PlaybackManager>()
                    .map(|manager| manager.finish(generation))
            })
            .unwrap_or(true);
        if cleared {
            hide_player_unless_dictating(&app2);
        }
    });
}

#[tauri::command]
fn read_selection(app: AppHandle) {
    // Read never competes with dictation. On macOS it uses Accessibility text
    // APIs directly, so selected text never passes through the pasteboard.
    if is_dictating(&app) {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        use text_backend::TextBackend;

        let playback_state = app
            .try_state::<playback::PlaybackManager>()
            .map(|manager| manager.state())
            .unwrap_or("idle");
        let selection = text_backend::PlatformTextBackend::selected_text();
        // Accessibility selection can yield while the user starts Dictate.
        // The late Read result must not replace or hide its Listening panel.
        if is_dictating(&app) {
            return;
        }
        match read_action::decide(selection, playback_state) {
            read_action::Decision::Speak(text) => {
                structured_log(
                    "read-selection-acquired",
                    serde_json::json!({ "characters": text.chars().count() }),
                );
                let mut args = vec!["--stdin".to_string()];
                args.extend(voice_args());
                run_client_monitored(&app, "speak.py", args, Some(text));
            }
            read_action::Decision::Toggle => {
                let state = app
                    .try_state::<playback::PlaybackManager>()
                    .map(|manager| manager.toggle())
                    .unwrap_or("idle");
                structured_log(
                    "read-playback-toggled",
                    serde_json::json!({ "state": state }),
                );
                set_player_mode(&app, state);
            }
            read_action::Decision::Notice { code, message } => {
                if is_dictating(&app) {
                    return;
                }
                structured_log("read-rejected", serde_json::json!({ "code": code }));
                show_player_notice(&app, message);
                let app2 = app.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    hide_player_unless_dictating(&app2);
                });
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        match selection::focused_selection() {
            selection::Selection::Selected(text) => {
                structured_log(
                    "read-selection-acquired",
                    serde_json::json!({ "characters": text.chars().count() }),
                );
                let mut args = vec!["--stdin".to_string()];
                args.extend(voice_args());
                run_client_monitored(&app, "speak.py", args, Some(text));
            }
            selection::Selection::Secure => {
                structured_log(
                    "read-rejected",
                    serde_json::json!({ "code": "secure-field" }),
                );
                show_player_notice(&app, "Protected text can't be read aloud.");
                let app2 = app.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    hide_player(&app2);
                });
            }
            selection::Selection::Unavailable => {
                let mut args = vec!["--selection".to_string()];
                args.extend(voice_args());
                run_client_monitored(&app, "speak.py", args, None);
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut args = vec!["--selection".to_string()];
        args.extend(voice_args());
        run_client_monitored(&app, "speak.py", args, None);
    }
}

/// The Quartz event tap must return before Accessibility asks the foreground
/// app for its selection. Chromium-family apps can reject that synchronous
/// cross-process query while they are still completing the modifier release.
#[cfg(target_os = "macos")]
fn dispatch_read_from_hotkey(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(25));
        read_selection(app);
    });
}

/// Pause or resume the desktop-owned playback child.
#[tauri::command]
fn toggle_playback(app: AppHandle) -> String {
    app.try_state::<playback::PlaybackManager>()
        .map(|manager| manager.toggle().to_string())
        .unwrap_or_else(|| "idle".into())
}

#[tauri::command]
fn stop_speaking(app: AppHandle) {
    if is_dictating(&app) {
        dictation_stop(&app);
    }
    if let Some(manager) = app.try_state::<playback::PlaybackManager>() {
        let _ = manager.stop();
    }
    hide_player(&app);
}

fn snip_result_code(success: bool, stdout: &str, stderr: &str) -> Option<&'static str> {
    let output = stdout.trim();
    if success && !output.is_empty() && !output.starts_with("ERROR") {
        return None;
    }
    if output.starts_with("CANCELLED") {
        return Some(if stderr.trim().is_empty() {
            "cancelled"
        } else {
            "capture-failed"
        });
    }
    if output.contains("no text found") {
        Some("no-text")
    } else if output.starts_with("ERROR") || !stderr.trim().is_empty() {
        Some("ocr-failed")
    } else {
        Some("empty-result")
    }
}

fn snip_failure_notice(code: &str) -> Option<&'static str> {
    match code {
        "cancelled" => None,
        "capture-failed" => Some("Screen capture blocked. Allow Screen Recording."),
        "no-text" => Some("No readable text in that area."),
        "ocr-failed" => Some("Text recognition failed. Try again."),
        _ => Some("Snip couldn't start. Open Settings."),
    }
}

/// Snip, then read what was captured.
///
/// The app orchestrates the two halves rather than letting snip.py invoke the
/// speak client itself. That fixes two things at once:
///   - the transport is no longer shown during the crosshair, where it covered
///     the very region being selected and could be captured inside the snip;
///   - the chosen voice and speed reach the playback, which they never did when
///     snip.py spawned speak.py internally with no arguments.
#[tauri::command]
fn snip_and_read(app: AppHandle) {
    if is_dictating(&app) {
        structured_log(
            "snip-rejected",
            serde_json::json!({ "code": "dictation-active" }),
        );
        return;
    }
    let Some(paths) = Paths::current() else {
        structured_log(
            "snip-failed",
            serde_json::json!({ "code": "engine-missing" }),
        );
        let _ = app.emit("engine-missing", ());
        return;
    };
    structured_log("snip-trigger", serde_json::json!({ "action": "start" }));
    let app2 = app.clone();
    std::thread::spawn(move || {
        // No player yet: the crosshair IS the feedback, and anything floating
        // on screen would be in the way.
        let out = client_command(&paths, "snip.py").output();
        if is_dictating(&app2) {
            return;
        }
        let Ok(out) = out else {
            structured_log("snip-failed", serde_json::json!({ "code": "spawn-failed" }));
            show_player_notice(&app2, "Snip couldn't start. Open Settings.");
            std::thread::sleep(std::time::Duration::from_secs(3));
            hide_player_unless_dictating(&app2);
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if let Some(code) = snip_result_code(out.status.success(), &text, &stderr) {
            structured_log(
                if code == "cancelled" {
                    "snip-cancelled"
                } else {
                    "snip-failed"
                },
                serde_json::json!({ "code": code }),
            );
            if let Some(message) = snip_failure_notice(code) {
                show_player_notice(&app2, message);
                std::thread::sleep(std::time::Duration::from_secs(3));
                hide_player_unless_dictating(&app2);
            }
            return;
        }
        structured_log(
            "snip-ocr-completed",
            serde_json::json!({ "characters": text.chars().count() }),
        );
        let mut args = vec!["--stdin".to_string()];
        args.extend(voice_args());
        run_client_monitored(&app2, "speak.py", args, Some(text));
    });
}

#[tauri::command]
fn speak_text(app: AppHandle, text: String) {
    let mut args = vec!["--stdin".to_string()];
    args.extend(voice_args());
    run_client_monitored(&app, "speak.py", args, Some(text));
}

#[cfg(target_os = "macos")]
fn set_clipboard(text: &str) {
    use std::io::Write;
    if let Ok(mut copy) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = copy.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = copy.wait();
    }
}

#[cfg(target_os = "windows")]
fn set_clipboard(text: &str) {
    use std::io::Write;
    // Clipboard is the durable fallback; SendKeys performs the immediate paste
    // without interpolating dictated text into PowerShell source.
    let mut copy = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            "Set-Clipboard -Value ([Console]::In.ReadToEnd())",
        ])
        .stdin(Stdio::piped())
        .spawn()
        .ok();
    if let Some(mut stdin) = copy.as_mut().and_then(|c| c.stdin.take()) {
        let _ = stdin.write_all(text.as_bytes());
    }
    if let Some(mut child) = copy {
        let _ = child.wait();
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn set_clipboard(_text: &str) {}

#[cfg(any(target_os = "macos", test))]
fn edit_delta(old: &str, new: &str) -> (usize, String) {
    let old_chars: Vec<char> = old.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();
    let mut prefix = old_chars
        .iter()
        .zip(&new_chars)
        .take_while(|(a, b)| a == b)
        .count();
    // If Whisper revised a word, rewrite that whole word. It looks natural in
    // the target editor and avoids leaving a partially corrected token.
    if prefix < old_chars.len() && prefix < new_chars.len() {
        while prefix > 0 && !old_chars[prefix - 1].is_whitespace() {
            prefix -= 1;
        }
    }
    (
        old_chars.len().saturating_sub(prefix),
        new_chars[prefix..].iter().collect(),
    )
}

fn normalized_word(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn merge_rolling_text(previous: &str, rolling: &str) -> String {
    let previous_words: Vec<&str> = previous.split_whitespace().collect();
    let rolling_words: Vec<&str> = rolling.split_whitespace().collect();
    let max_overlap = previous_words.len().min(rolling_words.len());
    let overlap = (2..=max_overlap).rev().find(|&size| {
        previous_words[previous_words.len() - size..]
            .iter()
            .map(|word| normalized_word(word))
            .eq(rolling_words[..size]
                .iter()
                .map(|word| normalized_word(word)))
    });
    let Some(overlap) = overlap else {
        return previous.to_string();
    };
    let tail = rolling_words[overlap..].join(" ");
    if tail.is_empty() {
        previous.to_string()
    } else {
        format!("{} {}", previous.trim_end(), tail)
    }
}

fn record_verified_insertion(
    inserted_text: &mut String,
    desired: &str,
    outcome: &text_backend::ApplyOutcome,
) -> bool {
    if outcome == &text_backend::ApplyOutcome::Applied {
        *inserted_text = desired.to_string();
        true
    } else {
        false
    }
}

fn successful_dictation_status(final_inserted: bool) -> DictationStatus {
    if final_inserted {
        DictationStatus::Completed
    } else {
        DictationStatus::ClipboardFallback
    }
}

/// Start recording. The transcript is typed when the recorder exits.
fn dictation_start(app: &AppHandle) {
    let id = format!(
        "{}-{}",
        std::process::id(),
        SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    use text_backend::{ApplyOutcome, PlatformTextBackend, TextBackend};
    structured_log(
        "dictation-trigger",
        serde_json::json!({ "action": "start" }),
    );
    #[cfg(target_os = "macos")]
    {
        let microphone = microphone_permission::status();
        if !microphone.is_authorized() {
            let requested = request_microphone_permission(app);
            structured_log(
                "dictation-start-rejected",
                serde_json::json!({
                    "code": "microphone-permission-required",
                    "permission": microphone.as_code(),
                    "requested": requested,
                }),
            );
            show_player_notice(
                app,
                if requested {
                    "Allow Microphone access, then dictate again."
                } else {
                    "Microphone access is off. Open HereWord Settings."
                },
            );
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
            return;
        }
    }
    let target = match PlatformTextBackend::capture_target() {
        Ok(target) => Some(target),
        Err(ApplyOutcome::SecureField) => {
            structured_log(
                "dictation-start-rejected",
                serde_json::json!({ "code": "secure-field" }),
            );
            show_player_notice(app, "Secure fields don't allow dictation.");
            return;
        }
        Err(_) => None,
    };
    let Some(dictation) = app.try_state::<Dictation>() else {
        structured_log(
            "dictation-start-rejected",
            serde_json::json!({ "code": "state-unavailable" }),
        );
        return;
    };
    {
        let Ok(mut guard) = dictation.0.lock() else {
            structured_log(
                "dictation-start-rejected",
                serde_json::json!({ "code": "state-lock-failed" }),
            );
            return;
        };
        if let Some(active) = guard.as_ref() {
            structured_log(
                "dictation-start-rejected",
                serde_json::json!({
                    "code": "session-active",
                    "active_session": active.id,
                    "active_state": active.status,
                    "stop_requested": active.stop_requested,
                }),
            );
            return; // auto-repeat; already recording
        }
        *guard = Some(DictationSession {
            id: id.clone(),
            status: DictationStatus::Starting,
            started: std::time::Instant::now(),
            child_pid: None,
            target: target.clone(),
            inserted_text: String::new(),
            fallback_reason: target.is_none().then(|| "target-unavailable".into()),
            stop_requested: false,
            cancel_requested: false,
            control: None,
        });
    }
    let Some(paths) = Paths::current() else {
        structured_log(
            "dictation-start-rejected",
            serde_json::json!({ "code": "engine-missing" }),
        );
        if let Ok(mut guard) = dictation.0.lock() {
            *guard = None;
        }
        return;
    };

    // DUCK: pause read-aloud before the microphone opens, or it transcribes our
    // own speech back at us. Only resume what WE paused — the user may have
    // paused deliberately beforehand.
    let ducked = {
        let st = playback_state(app);
        if st == "playing" {
            app.try_state::<playback::PlaybackManager>()
                .is_some_and(|manager| manager.pause())
        } else {
            false
        }
    };

    // Visible feedback. Without it, push-to-talk gives no sign the mic is open,
    // and a failure is indistinguishable from nothing happening.
    set_dictation_status(app, &id, DictationStatus::Starting);
    show_player(app);
    set_player_mode(app, "starting");

    let app2 = app.clone();
    std::thread::spawn(move || {
        // STREAM the client's output rather than collecting it with .output().
        // dictate.py announces TRANSCRIBING the moment recording ends, but a
        // buffered read only delivers that once the process exits — about a
        // second later, after Whisper has finished. The player therefore sat on
        // "Listening…" with the mic already closed, which reads as stuck on and
        // as though it were still recording you.
        // Opening the system microphone must finish before we inspect or
        // quiet other audio processes. A Core Audio tap can contend with an
        // input device while macOS is changing its route.
        let mut media_lease = None;
        let mut recorder = client_command(&paths, "dictate.py");
        recorder
            .arg("--record")
            .args(["--session", &id])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The protocol is stdout-only. Leaving stderr piped without a
            // reader can fill the OS pipe and deadlock a long transcription.
            .stderr(Stdio::null());
        if preferences::Preferences::load(&prefs_file()).live_preview() {
            recorder.arg("--live-preview");
        }
        if let Some(device) = microphone_arg() {
            recorder.args(["--device", &device]);
        }
        let child = recorder.spawn();

        let mut lines_out: Vec<String> = Vec::new();
        let mut startup_timed_out = false;
        let mut released_before_ready = false;
        let mut final_inserted = false;
        let mut final_fallback_notice = false;
        let child_spawn_failed = child.is_err();
        if let Ok(mut child) = child {
            let control = child.stdin.take().map(|stdin| Arc::new(Mutex::new(stdin)));
            let mut pending_command = None;
            if let Some(d) = app2.try_state::<Dictation>() {
                if let Ok(mut guard) = d.0.lock() {
                    if let Some(session) = guard.as_mut().filter(|s| s.id == id) {
                        session.child_pid = Some(child.id());
                        session.control = control.clone();
                        pending_command = if session.cancel_requested {
                            Some("cancel")
                        } else if session.stop_requested {
                            Some("stop")
                        } else {
                            None
                        };
                    }
                }
            }
            if let (Some(control), Some(command)) = (&control, pending_command) {
                let _ = send_dictation_command(control, &id, command);
            }
            if let Some(stdout) = child.stdout.take() {
                use std::io::{BufRead, BufReader};
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                });

                // PortAudio may block forever while opening a denied or broken
                // device. The child must prove that recording started.
                let microphone_opened_at = std::time::Instant::now();
                // Bluetooth microphones may need to switch audio profiles
                // before opening. Eight seconds killed the first attempt just
                // as macOS was making the input available.
                let startup_deadline = microphone_opened_at + std::time::Duration::from_secs(20);
                let first_result = loop {
                    let remaining =
                        startup_deadline.saturating_duration_since(std::time::Instant::now());
                    match rx.recv_timeout(remaining) {
                        Ok(line) if line.starts_with("MIC_PHASE ") => {
                            let phase = line.trim_start_matches("MIC_PHASE ");
                            if matches!(phase, "imports" | "device" | "open" | "start") {
                                structured_log(
                                    "dictation-recorder-phase",
                                    serde_json::json!({
                                        "session": id,
                                        "phase": phase,
                                        "elapsed_ms": microphone_opened_at.elapsed().as_millis(),
                                    }),
                                );
                            }
                        }
                        result => break result,
                    }
                };
                match first_result {
                    Ok(line)
                        if dictation_protocol::parse(&line)
                            == dictation_protocol::Event::Recording =>
                    {
                        structured_log(
                            "dictation-recorder-ready",
                            serde_json::json!({
                                "session": id,
                                "open_ms": microphone_opened_at.elapsed().as_millis(),
                            }),
                        );
                        set_player_mode(&app2, "recording");
                        set_dictation_status(&app2, &id, DictationStatus::Recording);
                        media_lease = acquire_media_focus(&app2);
                        let pending_control = app2.try_state::<Dictation>().and_then(|state| {
                            state.0.lock().ok().and_then(|session| {
                                session.as_ref().filter(|active| active.id == id).and_then(
                                    |active| {
                                        let command = if active.cancel_requested {
                                            "cancel"
                                        } else if active.stop_requested {
                                            "stop"
                                        } else {
                                            return None;
                                        };
                                        active.control.clone().map(|control| (control, command))
                                    },
                                )
                            })
                        });
                        if let Some((control, command)) = pending_control {
                            released_before_ready = command == "stop";
                            let _ = send_dictation_command(&control, &id, command);
                        }
                    }
                    Ok(line) => lines_out.push(line),
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        set_dictation_status(&app2, &id, DictationStatus::TimedOut);
                        startup_timed_out = true;
                    }
                }

                let mut accepting_preview = true;
                let mut live_text = String::new();
                let mut inserted_text = String::new();
                let mut target = target;
                let mut clipboard_fallback = target.is_none();
                while let Ok(line) = rx.recv() {
                    use dictation_protocol::Event;
                    match dictation_protocol::parse(&line) {
                        Event::Transcribing(_) => {
                            // Mic is closed; say so immediately.
                            drop(media_lease.take());
                            accepting_preview = false;
                            show_player(&app2);
                            set_player_mode(&app2, "transcribing");
                            set_dictation_status(&app2, &id, DictationStatus::Transcribing);
                        }
                        Event::RetryingEngine => {
                            structured_log("engine-retry", serde_json::json!({ "session": id }));
                            if let Err(error) = spawn_engine_and_record(&app2) {
                                structured_log(
                                    "engine-restart-failed",
                                    serde_json::json!({ "code": "authenticated-start-failed" }),
                                );
                                eprintln!("{error}");
                            }
                        }
                        Event::InactivityWarning => {
                            show_player_notice(
                                &app2,
                                "Still recording. Release the shortcut or press Escape.",
                            );
                        }
                        Event::Metrics(metrics) => {
                            if let Ok(mut value) =
                                serde_json::from_str::<serde_json::Value>(metrics)
                            {
                                value["schema_version"] = serde_json::json!(1);
                                value["measured_at_ms"] =
                                    serde_json::json!(std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_millis());
                                if let Some(backend) = engine_status().get("stt_backend").cloned() {
                                    value["backend"] = backend;
                                }
                                let _ = write_json_atomic(&performance_profile_file(), &value);
                            }
                        }
                        Event::PreviewFull(text) | Event::PreviewRolling(text)
                            if accepting_preview =>
                        {
                            let desired = match dictation_protocol::parse(&line) {
                                Event::PreviewFull(_) => text.to_string(),
                                Event::PreviewRolling(_) => merge_rolling_text(&live_text, text),
                                _ => unreachable!("matched preview event"),
                            };
                            if desired == live_text {
                                lines_out.push(line);
                                continue;
                            }
                            if !clipboard_fallback {
                                let outcome = PlatformTextBackend::apply_revision(
                                    target.as_mut().expect("checked target"),
                                    &inserted_text,
                                    &desired,
                                );
                                match outcome {
                                    ApplyOutcome::Applied => {
                                        record_verified_insertion(
                                            &mut inserted_text,
                                            &desired,
                                            &ApplyOutcome::Applied,
                                        );
                                        set_dictation_status(
                                            &app2,
                                            &id,
                                            DictationStatus::LiveTyping,
                                        );
                                    }
                                    ApplyOutcome::ClipboardFallback(reason) => {
                                        structured_log(
                                            "dictation-insertion-fallback",
                                            serde_json::json!({
                                                "session": id,
                                                "phase": "preview",
                                                "code": reason,
                                            }),
                                        );
                                        clipboard_fallback = true;
                                        set_dictation_status(
                                            &app2,
                                            &id,
                                            DictationStatus::ClipboardFallback,
                                        );
                                        show_player_notice(
                                            &app2,
                                            if reason == "focus-changed" {
                                                "Focus changed. Final text will be copied."
                                            } else {
                                                "Could not confirm text in this field. Final text will be copied."
                                            },
                                        );
                                        if let Some(d) = app2.try_state::<Dictation>() {
                                            if let Ok(mut guard) = d.0.lock() {
                                                if let Some(session) = guard.as_mut() {
                                                    session.fallback_reason = Some(reason);
                                                }
                                            }
                                        }
                                    }
                                    ApplyOutcome::Unavailable => {
                                        structured_log(
                                            "dictation-insertion-fallback",
                                            serde_json::json!({
                                                "session": id,
                                                "phase": "preview",
                                                "code": "unavailable",
                                            }),
                                        );
                                        clipboard_fallback = true;
                                        set_dictation_status(
                                            &app2,
                                            &id,
                                            DictationStatus::ClipboardFallback,
                                        );
                                        show_player_notice(
                                            &app2,
                                            "Text field unavailable. Final text will be copied.",
                                        );
                                    }
                                    ApplyOutcome::SecureField => {
                                        clipboard_fallback = true;
                                    }
                                }
                            }
                            live_text = desired;
                            if let Some(d) = app2.try_state::<Dictation>() {
                                if let Ok(mut guard) = d.0.lock() {
                                    if let Some(session) = guard.as_mut() {
                                        session.inserted_text = inserted_text.clone();
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    lines_out.push(line);
                }

                // The final full-context pass is authoritative. Correct only
                // the mutable suffix already visible in the target field.
                if let Some(final_text) =
                    lines_out
                        .iter()
                        .find_map(|line| match dictation_protocol::parse(line) {
                            dictation_protocol::Event::FinalText(text) => Some(text),
                            _ => None,
                        })
                {
                    // Always reconcile the authoritative final transcript from
                    // the last text that was actually verified in the field.
                    // A preview fallback must not strand a partial message.
                    hide_player(&app2);
                    let mut outcome = ApplyOutcome::Unavailable;
                    // The last preview was already verified in the original
                    // editor. If the final pass agrees with it, there is no
                    // edit to make and a later caret movement must not turn
                    // that completed insertion into a clipboard fallback.
                    if !clipboard_fallback
                        && !inserted_text.is_empty()
                        && final_text == inserted_text
                    {
                        outcome = ApplyOutcome::Applied;
                    } else if let Some(target) = target.as_mut() {
                        for attempt in 0..2 {
                            outcome = PlatformTextBackend::apply_revision(
                                target,
                                &inserted_text,
                                final_text,
                            );
                            if outcome == ApplyOutcome::Applied {
                                break;
                            }
                            if attempt == 0 {
                                std::thread::sleep(std::time::Duration::from_millis(75));
                            }
                        }
                    }
                    set_clipboard(final_text);
                    if outcome == ApplyOutcome::Applied {
                        final_inserted = true;
                        inserted_text = final_text.to_string();
                        structured_log(
                            "dictation-final-inserted",
                            serde_json::json!({
                                "session": id,
                                "inserted_chars": inserted_text.chars().count(),
                            }),
                        );
                    } else {
                        let code = match &outcome {
                            ApplyOutcome::ClipboardFallback(reason) => reason.as_str(),
                            ApplyOutcome::SecureField => "secure-field",
                            ApplyOutcome::Unavailable => "unavailable",
                            ApplyOutcome::Applied => "applied",
                        };
                        structured_log(
                            "dictation-insertion-fallback",
                            serde_json::json!({
                                "session": id,
                                "phase": "final",
                                "code": code,
                                "inserted_chars": inserted_text.chars().count(),
                                "final_chars": final_text.chars().count(),
                            }),
                        );
                        set_dictation_status(&app2, &id, DictationStatus::ClipboardFallback);
                        let paste_shortcut = if cfg!(target_os = "windows") {
                            "Ctrl+V"
                        } else {
                            "Command+V"
                        };
                        show_player_notice(
                            &app2,
                            &format!("Copied. Press {paste_shortcut} to paste."),
                        );
                        final_fallback_notice = true;
                    }
                }
            }
            let _ = child.wait();
        }

        if !final_fallback_notice {
            hide_player(&app2);
        }
        // Also release on cancellation, startup failure, or protocol/child exit.
        drop(media_lease);
        if ducked {
            if let Some(manager) = app2.try_state::<playback::PlaybackManager>() {
                let _ = manager.resume();
            }
        }
        let mut completed = false;
        for line in &lines_out {
            if let dictation_protocol::Event::FinalText(text) = dictation_protocol::parse(line) {
                let _ = app2.emit("dictated", text);
                completed = true;
            }
        }
        if lines_out
            .iter()
            .any(|line| dictation_protocol::parse(line) == dictation_protocol::Event::Cancelled)
        {
            set_dictation_status(&app2, &id, DictationStatus::CancelledByUser);
        } else if completed {
            set_dictation_status(&app2, &id, successful_dictation_status(final_inserted));
        } else if startup_timed_out {
            // The timeout state was already emitted at the point of failure.
        } else if child_spawn_failed {
            set_dictation_status(&app2, &id, DictationStatus::DeviceUnavailable);
        } else if lines_out.iter().any(|line| {
            matches!(
                dictation_protocol::parse(line),
                dictation_protocol::Event::Error(message) if message.contains("permission")
            )
        }) {
            set_dictation_status(&app2, &id, DictationStatus::PermissionDenied);
        } else if lines_out.iter().any(|line| {
            matches!(
                dictation_protocol::parse(line),
                dictation_protocol::Event::Error(message) if message.starts_with("microphone")
            )
        }) {
            set_dictation_status(&app2, &id, DictationStatus::DeviceUnavailable);
        } else if lines_out.iter().any(|line| {
            matches!(
                dictation_protocol::parse(line),
                dictation_protocol::Event::Error("no audio captured" | "nothing recognized")
            )
        }) {
            set_dictation_status(&app2, &id, DictationStatus::Cancelled);
            show_player_notice(
                &app2,
                if released_before_ready {
                    "The microphone was still opening when you released. Try again."
                } else {
                    "No speech heard. Hold the shortcut while speaking."
                },
            );
            structured_log(
                "dictation-failed",
                serde_json::json!({ "session": id, "code": "no-speech" }),
            );
        } else {
            set_dictation_status(&app2, &id, DictationStatus::Cancelled);
            structured_log(
                "dictation-failed",
                serde_json::json!({ "session": id, "code": "unclassified" }),
            );
        }
        if let Some(d) = app2.try_state::<Dictation>() {
            if let Ok(mut guard) = d.0.lock() {
                if guard.as_ref().is_some_and(|s| s.id == id) {
                    *guard = None;
                }
            }
        }
    });
}

/// idle | starting | playing | paused | stopping from the desktop owner.
fn playback_state(app: &AppHandle) -> String {
    app.try_state::<playback::PlaybackManager>()
        .map(|manager| manager.state().to_string())
        .unwrap_or_else(|| "idle".into())
}

fn stop_managed_playback(app: &AppHandle) {
    if let Some(manager) = app.try_state::<playback::PlaybackManager>() {
        let _ = manager.stop();
    }
    if let Some(focus) = app.try_state::<Arc<media_focus::MediaFocus>>() {
        focus.release_all();
    }
}

fn acquire_media_focus(app: &AppHandle) -> Option<media_focus::Lease> {
    let preferences = preferences::Preferences::load(&prefs_file());
    app.try_state::<Arc<media_focus::MediaFocus>>()?
        .acquire(preferences.media_mode(), preferences.media_duck_level())
}

fn stop_managed_dictation(app: &AppHandle) {
    let active = app.try_state::<Dictation>().and_then(|d| {
        d.0.lock().ok().and_then(|mut session| {
            session.as_mut().map(|current| {
                current.cancel_requested = true;
                current.status = DictationStatus::CancelledByUser;
                (
                    current.id.clone(),
                    current.control.take(),
                    current.child_pid.take(),
                )
            })
        })
    });
    let Some((id, control, child_pid)) = active else {
        return;
    };
    if let Some(control) = control {
        let _ = send_dictation_command(&control, &id, "cancel");
    }
    if let Some(pid) = child_pid {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn dictation_stop(app: &AppHandle) {
    let active = app.try_state::<Dictation>().and_then(|d| {
        d.0.lock().ok().and_then(|mut session| {
            session.as_mut().map(|current| {
                current.stop_requested = true;
                (
                    current.id.clone(),
                    current.status.clone(),
                    current.control.clone(),
                )
            })
        })
    });
    if let Some((id, state, control)) = active {
        structured_log(
            "dictation-trigger",
            serde_json::json!({ "action": "stop", "session": id, "state": state }),
        );
        if let Some(control) = control {
            let _ = send_dictation_command(&control, &id, "stop");
        }
    } else {
        structured_log(
            "dictation-stop-ignored",
            serde_json::json!({ "code": "no-active-session" }),
        );
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn dictation_cancel(app: &AppHandle) {
    let active = app.try_state::<Dictation>().and_then(|d| {
        d.0.lock().ok().and_then(|mut session| {
            session.as_mut().map(|current| {
                current.status = DictationStatus::CancelledByUser;
                current.cancel_requested = true;
                (current.id.clone(), current.control.clone())
            })
        })
    });
    if let Some((id, control)) = active {
        set_dictation_status(app, &id, DictationStatus::CancelledByUser);
        if let Some(control) = control {
            let _ = send_dictation_command(&control, &id, "cancel");
        }
        hide_player(app);
    }
}

// ── hotkeys ──────────────────────────────────────────────────────────────────

// macOS routes every shortcut through Quartz because that is the input boundary
// Deskflow demonstrably reaches. Windows uses registered global accelerators.
#[tauri::command]
fn hotkeys() -> serde_json::Value {
    hotkeys::response(
        &load_prefs(),
        HOTKEYS_REGISTERED.load(Ordering::SeqCst),
        cfg!(target_os = "macos"),
    )
}

/// Emit the same event for both shortcut adapters. The settings UI uses this
/// as the end-to-end proof that a recorded physical shortcut reached HereWord;
/// registration alone is not considered success.
fn hotkey_triggered(app: &AppHandle, slot: hotkeys::Slot) {
    structured_log(
        "hotkey-triggered",
        serde_json::json!({ "slot": slot.name() }),
    );
    let _ = app.emit("hotkey-triggered", slot.name());
}

fn register_hotkeys(app: &AppHandle) -> Result<(), String> {
    HOTKEYS_REGISTERED.store(false, Ordering::SeqCst);
    if !variant::INPUT_CONTROLLER_ENABLED {
        return Err("global input is disabled in the passive Candidate build".into());
    }
    #[cfg(target_os = "windows")]
    windows_chords::suspend(true);
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let config = hotkeys::Config::from_preferences(&load_prefs());

    #[cfg(target_os = "macos")]
    {
        if !objc2_core_graphics::CGPreflightListenEventAccess() {
            return Err(format!(
                "Input Monitoring is required for shortcuts; enable {} in Privacy & Security",
                variant::DISPLAY_NAME
            ));
        }
        chords::configure_shortcuts(&config.read, &config.dictate, &config.snip)?;

        if CHORDS_STARTED
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let read_app = app.clone();
            let read_event_app = app.clone();
            let start_app = app.clone();
            let start_event_app = app.clone();
            let stop_app = app.clone();
            let cancel_app = app.clone();
            let snip_app = app.clone();
            let snip_event_app = app.clone();
            if let Err(e) = chords::watch(
                move || {
                    hotkey_triggered(&read_event_app, hotkeys::Slot::Read);
                    dispatch_read_from_hotkey(read_app.clone());
                },
                move || {
                    hotkey_triggered(&start_event_app, hotkeys::Slot::Dictate);
                    dictation_start(&start_app);
                },
                move || dictation_stop(&stop_app),
                move || dictation_cancel(&cancel_app),
                move || {
                    hotkey_triggered(&snip_event_app, hotkeys::Slot::Snip);
                    snip_and_read(snip_app.clone());
                },
            ) {
                CHORDS_STARTED.store(false, Ordering::SeqCst);
                return Err(e);
            }
        }
        HOTKEYS_REGISTERED.store(true, Ordering::SeqCst);
        structured_log(
            "hotkeys-registered",
            serde_json::json!({
                "profile": "mac-quartz-controller",
                "read": config.read,
                "dictate": config.dictate,
                "snip": config.snip,
                "modifier_prefix_grace_ms": chords::DICTATE_PREFIX_GRACE_MS,
            }),
        );
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        windows_chords::configure(&config)?;
        let gesture_app = app.clone();
        windows_chords::start(move |action| {
            use windows_chords::Action;
            match action {
                Action::Read => {
                    hotkey_triggered(&gesture_app, hotkeys::Slot::Read);
                    read_selection(gesture_app.clone());
                }
                Action::Snip => {
                    hotkey_triggered(&gesture_app, hotkeys::Slot::Snip);
                    snip_and_read(gesture_app.clone());
                }
                Action::DictateStart => {
                    hotkey_triggered(&gesture_app, hotkeys::Slot::Dictate);
                    dictation_start(&gesture_app);
                }
                Action::DictateStop => dictation_stop(&gesture_app),
                Action::DictateCancel => dictation_cancel(&gesture_app),
            }
        })?;

        let parse = |a: &str, what: &str| -> Result<Option<Shortcut>, String> {
            if hotkeys::modifier_only(a) {
                return Ok(None);
            }
            a.parse::<Shortcut>()
                .map(Some)
                .map_err(|_| format!("{what} shortcut is not valid: {a}"))
        };
        let read = parse(&config.read, "read")?;
        let dictate = parse(&config.dictate, "dictate")?;
        let snip = parse(&config.snip, "snip")?;

        let result = gs
            .on_shortcuts(
                [read, dictate, snip].into_iter().flatten(),
                move |app, sc, event| match event.state {
                    ShortcutState::Pressed => {
                        if Some(sc) == read.as_ref() {
                            hotkey_triggered(app, hotkeys::Slot::Read);
                            read_selection(app.clone());
                        } else if Some(sc) == snip.as_ref() {
                            hotkey_triggered(app, hotkeys::Slot::Snip);
                            snip_and_read(app.clone());
                        } else if Some(sc) == dictate.as_ref() {
                            hotkey_triggered(app, hotkeys::Slot::Dictate);
                            dictation_start(app);
                        }
                    }
                    ShortcutState::Released => {
                        if Some(sc) == dictate.as_ref() {
                            dictation_stop(app);
                        }
                    }
                },
            )
            .map_err(|e| format!("could not register hotkeys: {e}"));
        if result.is_ok() {
            windows_chords::suspend(false);
            HOTKEYS_REGISTERED.store(true, Ordering::SeqCst);
            structured_log(
                "hotkeys-registered",
                serde_json::json!({
                    "profile": "windows-global-shortcuts",
                    "read": config.read,
                    "dictate": config.dictate,
                    "snip": config.snip,
                }),
            );
        }
        result
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Err("unsupported platform".into())
}

fn log_runtime_readiness() -> bool {
    #[cfg(target_os = "macos")]
    let (accessibility, input_monitoring, microphone) = (
        macos_accessibility_client::accessibility::application_is_trusted(),
        objc2_core_graphics::CGPreflightListenEventAccess(),
        microphone_permission::status().is_authorized(),
    );
    #[cfg(not(target_os = "macos"))]
    let (accessibility, input_monitoring, microphone) = (true, true, true);
    let hotkeys_registered = HOTKEYS_REGISTERED.load(Ordering::SeqCst);
    let ready = accessibility && input_monitoring && microphone && hotkeys_registered;
    structured_log(
        "runtime-readiness",
        serde_json::json!({
            "ready": ready,
            "accessibility": accessibility,
            "input_monitoring": input_monitoring,
            "microphone": microphone,
            "hotkeys_registered": hotkeys_registered,
        }),
    );
    ready
}

#[cfg(target_os = "macos")]
fn start_permission_readiness_watcher(app: AppHandle) {
    if !variant::INPUT_CONTROLLER_ENABLED || log_runtime_readiness() {
        return;
    }
    std::thread::spawn(move || {
        loop {
            // Privacy approval may include a macOS-required restart or the user
            // may return much later. Keep this low-cost watcher alive instead
            // of silently giving up after three minutes.
            std::thread::sleep(std::time::Duration::from_secs(2));
            if QUITTING.load(Ordering::SeqCst) {
                return;
            }
            let accessibility = macos_accessibility_client::accessibility::application_is_trusted();
            let input_monitoring = objc2_core_graphics::CGPreflightListenEventAccess();
            let microphone = microphone_permission::status().is_authorized();
            if !accessibility || !input_monitoring || !microphone {
                continue;
            }
            if !HOTKEYS_REGISTERED.load(Ordering::SeqCst) {
                let _ = register_hotkeys(&app);
            }
            if log_runtime_readiness() {
                return;
            }
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn start_permission_readiness_watcher(_app: AppHandle) {
    let _ = log_runtime_readiness();
}

/// Save a recorded accelerator and re-register immediately.
#[tauri::command]
fn begin_hotkey_recording(app: AppHandle) -> Result<(), String> {
    HOTKEYS_REGISTERED.store(false, Ordering::SeqCst);
    #[cfg(target_os = "windows")]
    windows_chords::suspend(true);
    app.global_shortcut()
        .unregister_all()
        .map_err(|e| format!("could not pause hotkeys for recording: {e}"))?;
    #[cfg(target_os = "macos")]
    chords::set_recorder_suspended(true);
    structured_log("hotkey-recorder-started", serde_json::json!({}));
    Ok(())
}

#[tauri::command]
fn end_hotkey_recording(app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    chords::set_recorder_suspended(false);
    let result = register_hotkeys(&app);
    structured_log(
        "hotkey-recorder-ended",
        serde_json::json!({ "registered": result.is_ok() }),
    );
    result
}

#[tauri::command]
fn set_hotkey(
    app: AppHandle,
    slot: String,
    accelerator: String,
) -> Result<hotkeys::Capture, String> {
    let slot = hotkeys::Slot::parse(&slot)?;
    let capture = hotkeys::classify_capture(
        slot,
        &accelerator,
        cfg!(any(target_os = "macos", target_os = "windows")),
    )?;
    if capture.kind == hotkeys::CaptureKind::RegisteredShortcut {
        accelerator
            .parse::<Shortcut>()
            .map_err(|_| format!("that combination cannot be used: {accelerator}"))?;
    }

    let mut preferences = preferences::Preferences::load(&prefs_file());
    let key = slot.preference_key();
    let previous = preferences
        .get(key)
        .and_then(|value| value.as_str())
        .map(String::from);
    preferences.set(key, serde_json::json!(accelerator));
    write_json_atomic(&prefs_file(), preferences.as_value())
        .map_err(|error| format!("could not save shortcut: {error}"))?;

    // A successful commit resumes the controller and installs the candidate
    // exactly once. The frontend calls end_hotkey_recording only for cancel,
    // timeout, or validation failure.
    #[cfg(target_os = "macos")]
    chords::set_recorder_suspended(false);

    if let Err(e) = register_hotkeys(&app) {
        let mut preferences = preferences::Preferences::load(&prefs_file());
        match previous {
            Some(previous) => preferences.set(key, serde_json::json!(previous)),
            None => preferences.remove(key),
        }
        let _ = write_json_atomic(&prefs_file(), preferences.as_value());
        let _ = register_hotkeys(&app);
        structured_log(
            "hotkey-save-failed",
            serde_json::json!({ "slot": slot.name(), "code": "registration-failed" }),
        );
        return Err(e);
    }
    structured_log(
        "hotkey-saved",
        serde_json::json!({ "slot": slot.name(), "kind": capture.kind }),
    );
    Ok(capture)
}

// ── signals ──────────────────────────────────────────────────────────────────

#[cfg(unix)]
extern "C" fn handle_signal(_sig: libc::c_int) {
    let fd = SIGNAL_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = [1u8];
        unsafe {
            libc::write(fd, byte.as_ptr().cast(), byte.len());
        }
    }
}

/// Tauri's exit hooks only run when the app quits through its own event loop.
/// A signal — Activity Monitor, `kill`, a logout — bypasses them entirely, and
/// the engine would be left holding the port.
#[cfg(unix)]
fn install_signal_handlers(app: AppHandle) {
    let mut descriptors = [-1; 2];
    if unsafe { libc::pipe(descriptors.as_mut_ptr()) } != 0 {
        structured_log(
            "signal-handler-failed",
            serde_json::json!({ "code": "pipe-unavailable" }),
        );
        return;
    }
    unsafe {
        for descriptor in descriptors {
            libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let flags = libc::fcntl(descriptors[1], libc::F_GETFL);
        libc::fcntl(descriptors[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
        SIGNAL_WRITE_FD.store(descriptors[1], Ordering::Relaxed);
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGHUP,
            handle_signal as *const () as libc::sighandler_t,
        );
    }
    std::thread::spawn(move || {
        let mut byte = 0u8;
        loop {
            let count = unsafe { libc::read(descriptors[0], (&mut byte as *mut u8).cast(), 1) };
            if count > 0 {
                break;
            }
            if count < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            unsafe {
                libc::close(descriptors[0]);
                libc::close(descriptors[1]);
            }
            return;
        }
        SIGNAL_WRITE_FD.store(-1, Ordering::Relaxed);
        unsafe {
            libc::close(descriptors[0]);
            libc::close(descriptors[1]);
        }
        {
            stop_managed_dictation(&app);
            stop_managed_playback(&app);
            stop_engine(&app);
            std::process::exit(0);
        }
    });
}

#[cfg(not(unix))]
fn install_signal_handlers(_app: AppHandle) {}

#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod live_dictation_tests {
    use super::*;

    #[test]
    fn file_hashing_matches_the_known_sha256_without_loading_the_file_whole() {
        let path = std::env::temp_dir().join(format!("kokoro-sha256-test-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_uv_archive_accepts_current_flat_and_legacy_prefixed_layouts() {
        assert_eq!(windows_uv_archive_entry(true, false).unwrap(), "uv.exe");
        assert_eq!(
            windows_uv_archive_entry(false, true).unwrap(),
            "uv-x86_64-pc-windows-msvc/uv.exe"
        );
        assert!(windows_uv_archive_entry(false, false).is_err());
    }

    fn temporary_engine_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "kokoro-source-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn active_source_pointer_accepts_only_an_installed_version_directory() {
        let root = temporary_engine_root("valid");
        let source = root.join("sources/2.1.1-test");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("server.py"), "app = None\n").unwrap();
        std::fs::write(active_source_file(&root), "2.1.1-test\n").unwrap();

        assert_eq!(active_source_root(&root), Some(source));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_source_pointer_rejects_traversal_and_missing_sources() {
        let root = temporary_engine_root("invalid");
        std::fs::create_dir_all(&root).unwrap();

        for value in ["../outside", "sources/version", ".", "missing"] {
            std::fs::write(active_source_file(&root), value).unwrap();
            assert_eq!(active_source_root(&root), None, "accepted {value}");
        }

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn status_window_joins_other_apps_spaces_without_entering_window_cycle() {
        use objc2_app_kit::NSWindowCollectionBehavior as Behavior;

        let behavior = status_window_collection_behavior();
        assert!(behavior.contains(Behavior::CanJoinAllSpaces));
        assert!(behavior.contains(Behavior::CanJoinAllApplications));
        assert!(behavior.contains(Behavior::FullScreenAuxiliary));
        assert!(behavior.contains(Behavior::Stationary));
        assert!(behavior.contains(Behavior::IgnoresCycle));
        assert!(!behavior.contains(Behavior::Transient));
        assert!(!behavior.contains(Behavior::ParticipatesInCycle));
        assert_eq!(
            status_window_level(),
            objc2_app_kit::NSScreenSaverWindowLevel
        );
        let style = status_window_style_mask();
        assert!(style.contains(objc2_app_kit::NSWindowStyleMask::Borderless));
        assert!(style.contains(objc2_app_kit::NSWindowStyleMask::NonactivatingPanel));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn status_window_recreates_once_when_it_leaves_the_active_space() {
        assert_eq!(
            status_window_recovery(true, true, false, true),
            StatusWindowRecovery::Recreate
        );
        assert_eq!(
            status_window_recovery(true, true, false, false),
            StatusWindowRecovery::None
        );
        assert_eq!(
            status_window_recovery(true, false, true, true),
            StatusWindowRecovery::Reassert
        );
        assert_eq!(
            status_window_recovery(true, true, true, true),
            StatusWindowRecovery::None
        );
        assert_eq!(
            status_window_recovery(false, false, false, true),
            StatusWindowRecovery::None
        );
    }

    #[test]
    fn appending_only_inserts_new_suffix() {
        assert_eq!(
            edit_delta("hello world", "hello world again"),
            (0, " again".into())
        );
    }

    #[test]
    fn revision_replaces_the_whole_changed_word() {
        assert_eq!(edit_delta("hello wear", "hello world"), (4, "world".into()));
    }

    #[test]
    fn unicode_deletion_counts_characters_not_bytes() {
        assert_eq!(edit_delta("say cafe", "say café"), (4, "café".into()));
    }

    #[test]
    fn authoritative_final_replaces_a_partial_preview_without_truncation() {
        let preview = "Kokoro validation alpha bravo";
        let final_text =
            "Kokoro validation alpha bravo charlie. This complete message must not be cut off.";
        let (delete, insert) = edit_delta(preview, final_text);
        let keep = preview.chars().count() - delete;
        let mut reconciled: String = preview.chars().take(keep).collect();
        reconciled.push_str(&insert);
        assert_eq!(reconciled, final_text);
    }

    #[test]
    fn rolling_window_extends_at_a_verified_overlap() {
        assert_eq!(
            merge_rolling_text("one two three four five", "three four five six seven"),
            "one two three four five six seven"
        );
    }

    #[test]
    fn rolling_window_without_overlap_cannot_corrupt_existing_text() {
        assert_eq!(
            merge_rolling_text("one two three", "unrelated new phrase"),
            "one two three"
        );
    }

    #[test]
    fn launch_at_login_defaults_on_but_respects_explicit_disable() {
        assert_eq!(
            launch_at_login_preference(&serde_json::json!({})),
            variant::DEFAULT_AUTOSTART
        );
        assert!(!launch_at_login_preference(
            &serde_json::json!({ "launch_at_login": false })
        ));
    }

    #[test]
    fn snip_outcomes_distinguish_cancel_permission_ocr_and_success() {
        assert_eq!(snip_result_code(false, "CANCELLED", ""), Some("cancelled"));
        assert_eq!(
            snip_result_code(false, "CANCELLED", "could not create image"),
            Some("capture-failed")
        );
        assert_eq!(
            snip_result_code(false, "ERROR no text found in that region", ""),
            Some("no-text")
        );
        assert_eq!(
            snip_result_code(false, "", "Vision failed"),
            Some("ocr-failed")
        );
        assert_eq!(
            snip_result_code(true, "recognized words", "OCR timing"),
            None
        );
    }

    #[test]
    fn failed_preview_never_claims_text_was_inserted() {
        let mut inserted = "verified prefix".to_string();
        assert!(!record_verified_insertion(
            &mut inserted,
            "recognized prefix plus words not typed",
            &text_backend::ApplyOutcome::ClipboardFallback("focus-changed".into()),
        ));
        assert_eq!(inserted, "verified prefix");

        assert!(record_verified_insertion(
            &mut inserted,
            "complete final message",
            &text_backend::ApplyOutcome::Applied,
        ));
        assert_eq!(inserted, "complete final message");
    }

    #[test]
    fn final_transcript_without_verified_insertion_stays_clipboard_fallback() {
        assert_eq!(
            successful_dictation_status(true),
            DictationStatus::Completed
        );
        assert_eq!(
            successful_dictation_status(false),
            DictationStatus::ClipboardFallback
        );
    }
}

// ── app ──────────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // A second launch would spawn a second engine and the two would fight
        // over the port. Focus the existing window instead.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            engine_status,
            setup_status,
            setup_engine,
            resume_setup,
            cancel_setup,
            read_selection,
            stop_speaking,
            close_player_popup,
            move_player_by,
            save_player_position,
            snip_and_read,
            speak_text,
            toggle_playback,
            get_prefs,
            set_prefs,
            list_voices,
            set_hotkey,
            begin_hotkey_recording,
            end_hotkey_recording,
            hotkeys,
            microphone_devices,
            set_microphone,
            dictation_status,
            export_diagnostics,
            storage_status,
            remove_local_data,
            permission_status,
            run_capability_test,
            record_capability,
            retry_permission,
            system_check,
            launch_at_login_status,
            set_launch_at_login,
            app_updates::check_for_update,
            app_updates::install_update
        ])
        .setup(|app| {
            use tauri_plugin_autostart::ManagerExt;
            let handle = app.handle().clone();
            app_updates::initialize(&handle)?;
            app.manage(Engine(Mutex::new(None)));
            app.manage(Dictation(Mutex::new(None)));
            app.manage(playback::PlaybackManager::default());
            #[cfg(target_os = "macos")]
            media_controls::install(&handle);
            app.manage(media_focus::MediaFocus::new());
            // HereWord is an accessibility tool whose hotkeys must be available
            // immediately after login. Default autostart on and self-heal a
            // missing LaunchAgent unless the user explicitly disabled it.
            let launch_at_login = launch_at_login_preference(&load_prefs());
            if launch_at_login {
                if let Err(error) = app.autolaunch().enable() {
                    structured_log(
                        "autostart-failed",
                        serde_json::json!({ "code": "registration-failed" }),
                    );
                    eprintln!("could not register launch at login: {error}");
                } else {
                    structured_log("autostart-verified", serde_json::json!({ "enabled": true }));
                }
            }

            if is_installed() {
                if let Err(error) = sync_engine_sources(&handle, &engine_root()) {
                    eprintln!("{error}");
                }
                if let Err(error) = spawn_engine_and_record(&handle) {
                    structured_log(
                        "engine-start-failed",
                        serde_json::json!({ "code": "authenticated-start-failed" }),
                    );
                    eprintln!("{error}");
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            } else if let Some(w) = app.get_webview_window("main") {
                // Nothing to run yet — show the window so the first thing a new
                // user meets is the setup screen, not a silent tray icon.
                let _ = w.show();
                let _ = w.set_focus();
            }
            install_signal_handlers(handle.clone());
            start_watchdog(handle.clone());
            #[cfg(target_os = "macos")]
            start_status_space_watcher(handle.clone());
            if variant::INPUT_CONTROLLER_ENABLED {
                #[cfg(target_os = "macos")]
                {
                    let microphone = microphone_permission::status();
                    if !microphone.is_authorized() {
                        let requested = request_microphone_permission(&handle);
                        structured_log(
                            "microphone-permission-required",
                            serde_json::json!({
                                "permission": microphone.as_code(),
                                "requested": requested,
                            }),
                        );
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                }
                if let Err(error) = register_hotkeys(&handle) {
                    let permission_required = error.contains("Input Monitoring");
                    structured_log(
                        "hotkeys-registration-failed",
                        serde_json::json!({
                            "code": if permission_required {
                                "input-monitoring-required"
                            } else {
                                "registration-failed"
                            }
                        }),
                    );
                    eprintln!("{error}");
                    let notice = if permission_required {
                        "Finish setup in HereWord Settings"
                    } else {
                        "Shortcuts couldn't start. Open Settings."
                    };
                    show_player_notice(&handle, notice);
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                start_permission_readiness_watcher(handle.clone());
            } else {
                structured_log(
                    "input-controller-disabled",
                    serde_json::json!({ "reason": "passive-candidate" }),
                );
            }

            let read = MenuItem::with_id(app, "read", "Read selection", true, None::<&str>)?;
            let snip = MenuItem::with_id(app, "snip", "Snip & read", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?;
            let open = MenuItem::with_id(app, "open", "Settings…", true, None::<&str>)?;
            let quit = MenuItem::with_id(
                app,
                "quit",
                format!("Quit {}", variant::DISPLAY_NAME),
                true,
                None::<&str>,
            )?;
            let menu = Menu::with_items(app, &[&read, &snip, &stop, &open, &quit])?;

            let tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "read" => read_selection(app.clone()),
                    "snip" => snip_and_read(app.clone()),
                    "stop" => stop_speaking(app.clone()),
                    "open" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => {
                        stop_managed_dictation(app);
                        stop_managed_playback(app);
                        stop_engine(app);
                        app.exit(0);
                    }
                    _ => {}
                });

            #[cfg(target_os = "windows")]
            let tray = tray
                .tooltip(format!("{} — Settings", variant::DISPLAY_NAME))
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        if let Some(window) = tray.app_handle().get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                });

            tray.build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .unwrap_or_else(|error| panic!("error while building {}: {error}", variant::DISPLAY_NAME))
        .run(|app, event| {
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                stop_managed_dictation(app);
                stop_managed_playback(app);
                stop_engine(app);
            }
        });
}
