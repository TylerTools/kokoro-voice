//! Kokoro Voice desktop composition root.
//!
//! This module owns first-run setup, engine lifecycle, Tauri commands, action
//! orchestration, tray/UI composition, and shutdown. Domain logic belongs in
//! the sibling modules below; do not add another composition root.
//!
//! Small Python sources ship in the bundle, while the private environment and
//! models live in Application Support so app replacement preserves downloaded
//! data and never writes through the code signature.

#[cfg(target_os = "macos")]
mod chords;
mod dictation_protocol;
mod hotkeys;
mod text_backend;
mod variant;
#[cfg(target_os = "windows")]
mod windows_chords;

use std::process::{Child, Command, Stdio};
#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};
#[cfg(target_os = "windows")]
use tauri_plugin_global_shortcut::ShortcutState;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

struct Engine(Mutex<Option<Child>>);

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
}

struct Dictation(Mutex<Option<DictationSession>>);
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);
static SETUP_CANCELLED: AtomicBool = AtomicBool::new(false);
static HOTKEYS_REGISTERED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static CHORDS_STARTED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static STATUS_PANEL: AtomicPtr<objc2_app_kit::NSPanel> = AtomicPtr::new(std::ptr::null_mut());
#[cfg(target_os = "macos")]
static STATUS_WINDOW_REQUESTED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static STATUS_WINDOW_WIDTH_BITS: AtomicU64 = AtomicU64::new(0);
#[cfg(target_os = "macos")]
static STATUS_SPACE_WATCHER_STARTED: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
static SIGNALLED: AtomicBool = AtomicBool::new(false);
static LOG_LOCK: Mutex<()> = Mutex::new(());
/// Set while we are intentionally shutting down, so the watchdog does not
/// helpfully resurrect the engine we are trying to stop.
static QUITTING: AtomicBool = AtomicBool::new(false);

// ── locations ────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Where the installed engine lives. Deliberately NOT inside the .app bundle:
/// a bundle should be replaceable by dragging a new one over it, and writing
/// inside it breaks the code signature.
fn engine_root() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(variant::APP_SUPPORT_DIR)
        .join("engine")
}

fn config_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    let d = home().join(".config").join(variant::CONFIG_DIR_NAME);
    #[cfg(not(target_os = "macos"))]
    let d = dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(variant::APP_SUPPORT_DIR);
    let _ = std::fs::create_dir_all(&d);
    d
}

fn structured_log(event: &str, fields: serde_json::Value) {
    use std::io::Write;
    let Ok(_guard) = LOG_LOCK.lock() else {
        return;
    };
    let path = config_dir().join("events.jsonl");
    if path.metadata().map(|m| m.len()).unwrap_or(0) > 1_000_000 {
        let _ = std::fs::rename(&path, config_dir().join("events.previous.jsonl"));
    }
    let record = serde_json::json!({
        "timestamp_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis(),
        "event": event,
        "platform": std::env::consts::OS,
        "fields": fields,
    });
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{record}");
    }
}

fn pidfile() -> std::path::PathBuf {
    config_dir().join("engine.pid")
}

fn python_path(root: &std::path::Path) -> std::path::PathBuf {
    if cfg!(windows) {
        root.join(".venv/Scripts/python.exe")
    } else {
        root.join(".venv/bin/python")
    }
}

fn port() -> String {
    std::env::var("KOKORO_PORT").unwrap_or_else(|_| variant::DEFAULT_PORT.into())
}

/// True once the environment and both model files are in place.
fn is_installed() -> bool {
    let root = engine_root();
    python_path(&root).exists()
        && root.join("server.py").exists()
        && root.join("models/kokoro-v1.0.fp16.onnx").exists()
        && root.join("models/voices-v1.0.bin").exists()
        && std::fs::read_to_string(config_dir().join("token"))
            .is_ok_and(|token| !token.trim().is_empty())
}

#[derive(Clone)]
struct Paths {
    python: std::path::PathBuf,
    root: std::path::PathBuf,
}

impl Paths {
    fn current() -> Option<Self> {
        let root = engine_root();
        let python = python_path(&root);
        if python.exists() && root.join("server.py").exists() {
            return Some(Paths { python, root });
        }
        // Development fallback: a repo checkout with its own .venv.
        if let Ok(cwd) = std::env::current_dir() {
            for c in [cwd.clone(), cwd.join(".."), cwd.join("../..")] {
                if let Ok(root) = c.canonicalize() {
                    let python = python_path(&root);
                    if python.exists() && root.join("server.py").exists() {
                        return Some(Paths { python, root });
                    }
                }
            }
        }
        None
    }

    fn client(&self, name: &str) -> std::path::PathBuf {
        self.root.join("client").join(name)
    }
}

/// Construct a Python client command with every mutable/runtime boundary pinned
/// to Kokoro Voice 2.1. Keeping this in one place prevents a new call site from
/// silently talking to version 1 on port 8123 or sharing its control files.
fn client_command(paths: &Paths, script: &str) -> Command {
    let mut command = background_command(&paths.python);
    command
        .arg(paths.client(script))
        .current_dir(&paths.root)
        .env("KOKORO_HOST", variant::CLIENT_HOST)
        .env("KOKORO_TOKEN_FILE", config_dir().join("token"))
        .env("KOKORO_STATE_DIR", config_dir().join("runtime"));
    command
}

// ── engine lifecycle ─────────────────────────────────────────────────────────

/// Helpers must not create a console and steal focus from the dictation target.
fn background_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let command = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let mut command = command;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        command
    }
    #[cfg(not(target_os = "windows"))]
    command
}

fn start_engine(paths: &Paths) -> Option<Child> {
    background_command(&paths.python)
        .args([
            "-m",
            "uvicorn",
            "server:app",
            // Loopback only. Never 0.0.0.0.
            "--host",
            "127.0.0.1",
            "--port",
            &port(),
        ])
        .current_dir(&paths.root)
        // Without this the model hub is contacted on every load to resolve
        // "latest", which breaks the offline guarantee and lets an upstream
        // change swap the weights silently.
        .env("HF_HUB_OFFLINE", "1")
        .env("KOKORO_TOKEN_FILE", config_dir().join("token"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

/// Keep the engine alive.
///
/// launchd used to do this with KeepAlive and it was lost in the move to the
/// app. Without it a crashed engine stays dead until the app is quit and
/// reopened, and the failure is INVISIBLE — hotkeys simply stop doing
/// anything, which reads as "the app is broken".
fn start_watchdog(app: AppHandle) {
    std::thread::spawn(move || {
        // Give the first start time to bind before we begin judging it.
        std::thread::sleep(std::time::Duration::from_secs(20));
        let mut failures = 0u32;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            if QUITTING.load(Ordering::SeqCst) || !is_installed() {
                continue;
            }
            let url = format!("http://127.0.0.1:{}/health", port());
            let ok = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(4))
                .build()
                .ok()
                .and_then(|c| c.get(&url).send().ok())
                .is_some_and(|r| r.status().is_success());
            if ok {
                failures = 0;
                continue;
            }
            // Two consecutive misses, not one: the engine is unresponsive for
            // several seconds while a long synthesis holds the GIL, and
            // restarting mid-sentence would be worse than waiting.
            failures += 1;
            if failures < 2 {
                continue;
            }
            failures = 0;
            let _ = app.emit("engine-restarting", ());
            spawn_engine_and_record(&app);
        }
    });
}

fn stop_engine(app: &AppHandle) {
    QUITTING.store(true, Ordering::SeqCst);
    if let Some(engine) = app.try_state::<Engine>() {
        if let Ok(mut guard) = engine.0.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let _ = std::fs::remove_file(pidfile());
}

/// Kill an engine left behind by a previous run.
///
/// A SIGKILL or a crash never runs our cleanup — verified — and the orphan then
/// holds the port so the next launch cannot bind. The recorded PID is checked
/// against the live command line first: PIDs get recycled, and killing a
/// stranger's process would be far worse than leaving a stale file behind.
fn reap_orphan() {
    let path = pidfile();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    if let Ok(pid) = text.trim().parse::<i32>() {
        #[cfg(not(target_os = "windows"))]
        let ours = Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .ok()
            .map(|o| {
                let c = String::from_utf8_lossy(&o.stdout);
                c.contains("uvicorn") && c.contains("server:app")
            })
            .unwrap_or(false);
        #[cfg(target_os = "windows")]
        let ours = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine"),
            ])
            .output()
            .ok()
            .is_some_and(|o| {
                let c = String::from_utf8_lossy(&o.stdout);
                c.contains("uvicorn") && c.contains("server:app")
            });
        if ours {
            #[cfg(not(target_os = "windows"))]
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            #[cfg(target_os = "windows")]
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .status();
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
    }
    let _ = std::fs::remove_file(&path);
}

fn spawn_engine_and_record(app: &AppHandle) {
    let Some(paths) = Paths::current() else {
        return;
    };
    reap_orphan();
    let child = start_engine(&paths);
    if let Some(c) = child.as_ref() {
        let _ = std::fs::write(pidfile(), c.id().to_string());
    }
    if let Some(engine) = app.try_state::<Engine>() {
        if let Ok(mut g) = engine.0.lock() {
            *g = child;
        }
    }
}

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

fn download_verified(
    url: &str,
    destination: &std::path::Path,
    expected_size: Option<u64>,
    expected_sha256: &str,
) -> Result<(), String> {
    use reqwest::header::RANGE;
    use sha2::Digest;
    use std::io::{Read, Write};
    let partial = destination.with_extension("part");
    let verify = |path: &std::path::Path| -> bool {
        if expected_size.is_some_and(|size| path.metadata().map(|m| m.len()).ok() != Some(size)) {
            return false;
        }
        std::fs::read(path)
            .ok()
            .map(|bytes| format!("{:x}", sha2::Sha256::digest(bytes)) == expected_sha256)
            .unwrap_or(false)
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
    let bytes = std::fs::read(&partial).map_err(|e| e.to_string())?;
    let actual = format!("{:x}", sha2::Sha256::digest(bytes));
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

fn sync_engine_sources(app: &AppHandle, root: &std::path::Path) -> Result<(), String> {
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
    std::fs::create_dir_all(root).map_err(|e| format!("cannot create {root:?}: {e}"))?;
    for name in [
        "server.py",
        "stt_config.py",
        "benchmark_stt.py",
        "requirements.txt",
        platform_requirements(),
        "requirements-macos.lock",
        "requirements-windows.lock",
    ] {
        let from = src.join(name);
        if from.exists() {
            std::fs::copy(&from, root.join(name)).map_err(|e| format!("copy {name}: {e}"))?;
        }
    }
    std::fs::create_dir_all(root.join("client")).map_err(|e| format!("create client dir: {e}"))?;
    for name in ["speak.py", "dictate.py", "snip.py"] {
        std::fs::copy(
            src.join("client").join(name),
            root.join("client").join(name),
        )
        .map_err(|e| format!("copy {name}: {e}"))?;
    }
    Ok(())
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
        let mut source = zip.by_name("uv.exe").map_err(|e| e.to_string())?;
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
    sync_engine_sources(app, &root)?;

    // 2. uv — manages Python without touching the system install.
    emit_step(app, 15, "Setting up Python…");
    let uv = find_or_install_uv(app)?;

    // 3. Environment.
    if !python_path(&root).exists() {
        let venv_ok = background_command(&uv)
            .args(["venv", "--python", "3.12"])
            .arg(root.join(".venv"))
            .status()
            .map_err(|e| format!("uv venv: {e}"))?;
        if !venv_ok.success() {
            return Err("could not create the Python environment".into());
        }
    }

    emit_step(app, 30, "Installing components… (a minute or two)");
    let lockfile = platform_lockfile();
    let lock = root.join(lockfile);
    if !lock.exists() {
        return Err(format!("locked dependency set is missing: {lockfile}"));
    }
    let st = background_command(&uv)
        .args(["pip", "install", "--require-hashes", "-r"])
        .arg(&lock)
        .env("VIRTUAL_ENV", root.join(".venv"))
        .status()
        .map_err(|e| format!("uv pip install: {e}"))?;
    if !st.success() {
        return Err(format!(
            "could not install verified dependencies from {lockfile}"
        ));
    }
    // mlx-whisper declares torch but only imports it in the weight-CONVERSION
    // path, which we never take. Verified that torch never enters sys.modules
    // during import or a real transcribe(). Dropping it saves ~480MB.
    if cfg!(target_os = "macos") {
        let _ = Command::new(&uv)
            .args(["pip", "uninstall", "torch"])
            .env("VIRTUAL_ENV", root.join(".venv"))
            .status();
    }

    // 4. Models — not redistributed, fetched from upstream. Sizes are verified
    //    because a truncated download fails much later and far less obviously.
    std::fs::create_dir_all(root.join("models")).ok();
    let base = "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0";
    for (name, expected, sha256, pct) in [
        (
            "kokoro-v1.0.fp16.onnx",
            177_464_787u64,
            "c1610a859f3bdea01107e73e50100685af38fff88f5cd8e5c56df109ec880204",
            55u32,
        ),
        (
            "voices-v1.0.bin",
            28_214_398u64,
            "bca610b8308e8d99f32e6fe4197e7ec01679264efed0cac9140fe9c29f1fbf7d",
            80u32,
        ),
    ] {
        let dest = root.join("models").join(name);
        if dest.metadata().map(|m| m.len()).ok() == Some(expected) {
            use sha2::Digest;
            if let Ok(bytes) = std::fs::read(&dest) {
                if format!("{:x}", sha2::Sha256::digest(bytes)) == sha256 {
                    continue;
                }
            }
            let _ = std::fs::remove_file(&dest);
        }
        emit_step(app, pct, &format!("Downloading voices ({name})…"));
        download_verified(&format!("{base}/{name}"), &dest, Some(expected), sha256)?;
    }

    // Prime the platform STT model while networking is deliberately available.
    // The engine itself starts with HF_HUB_OFFLINE=1, so a clean install must
    // populate the cache here rather than failing on its first dictation.
    emit_step(app, 88, "Downloading speech recognition…");
    #[cfg(target_os = "macos")]
    let stt_prime = background_command(python_path(&root)).env_remove("HF_HUB_OFFLINE").args(["-c",
        "import numpy as np, mlx_whisper; from huggingface_hub import snapshot_download; p=snapshot_download('mlx-community/whisper-large-v3-turbo',revision='a4aaeec0636e6fef84abdcbe3544cb2bf7e9f6fb'); mlx_whisper.transcribe(np.zeros(16000,dtype='float32'), path_or_hf_repo=p, language='en')"
    ]).status();
    #[cfg(target_os = "windows")]
    let stt_prime = background_command(python_path(&root)).env_remove("HF_HUB_OFFLINE").env("HF_HUB_DISABLE_XET", "1").args(["-c",
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
        let benchmark = background_command(python_path(&root))
            .arg(root.join("benchmark_stt.py"))
            .arg("--output")
            .arg(config_dir().join("stt-backend.json"))
            .env_remove("HF_HUB_OFFLINE")
            .status();
        if !benchmark.map(|s| s.success()).unwrap_or(false) {
            return Err("could not benchmark the Windows speech-recognition backend".into());
        }
    }

    // 5. Auth token.
    emit_step(app, 92, "Finishing…");
    let token_file = config_dir().join("token");
    if std::fs::read_to_string(&token_file)
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
    {
        let out = background_command(python_path(&root))
            .args(["-c", "import secrets;print(secrets.token_urlsafe(32))"])
            .output()
            .map_err(|e| format!("token: {e}"))?;
        std::fs::write(&token_file, out.stdout).map_err(|e| format!("token: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600));
        }
    }

    emit_step(app, 100, "Ready");
    spawn_engine_and_record(app);
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
    std::fs::read_to_string(prefs_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| {
            serde_json::json!({
                "voice": "af_heart", "speed": 1.0, "live_preview": false
            })
        })
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
) -> serde_json::Value {
    let mut p = load_prefs();
    if let Some(v) = voice {
        p["voice"] = serde_json::Value::String(v);
    }
    if let Some(sp) = speed {
        // Clamp to what the engine accepts (gt 0.1, le 3.0) so a bad value
        // fails here rather than as an opaque 422 mid-read.
        p["speed"] = serde_json::json!(sp.clamp(0.25, 3.0));
    }
    if let Some(enabled) = cue_enabled {
        p["cue_enabled"] = serde_json::Value::Bool(enabled);
    }
    if let Some(volume) = cue_volume {
        p["cue_volume"] = serde_json::json!(volume.clamp(0.0, 1.0));
    }
    if let Some(enabled) = live_preview {
        p["live_preview"] = serde_json::Value::Bool(enabled);
    }
    let _ = write_json_atomic(&prefs_file(), &p);
    p
}

#[tauri::command]
fn list_voices() -> Vec<String> {
    let url = format!("http://127.0.0.1:{}/voices", port());
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()
        .and_then(|c| c.get(url).send().ok())
        .and_then(|r| r.json::<serde_json::Value>().ok())
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
    let p = load_prefs();
    let mut out = Vec::new();
    if let Some(v) = p.get("voice").and_then(|v| v.as_str()) {
        out.push("--voice".into());
        out.push(v.to_string());
    }
    if let Some(sp) = p.get("speed").and_then(|v| v.as_f64()) {
        out.push("--speed".into());
        out.push(format!("{sp}"));
    }
    out
}

fn microphone_arg() -> Option<String> {
    load_prefs()
        .get("microphone_device")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
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
    let mut prefs = load_prefs();
    prefs["microphone_device"] = device
        .map(serde_json::Value::String)
        .unwrap_or(serde_json::Value::Null);
    let _ = write_json_atomic(&prefs_file(), &prefs);
    prefs
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
        "app_version": env!("CARGO_PKG_VERSION"),
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
            entry
                .metadata()
                .ok()
                .map(|m| {
                    if m.is_dir() {
                        directory_size(&entry.path())
                    } else {
                        m.len()
                    }
                })
                .unwrap_or(0)
        })
        .sum()
}

#[tauri::command]
fn storage_status() -> serde_json::Value {
    serde_json::json!({
        "engine_bytes": directory_size(&engine_root()),
        "preferences_present": prefs_file().exists(),
    })
}

#[tauri::command]
fn permission_status() -> serde_json::Value {
    #[cfg(target_os = "macos")]
    {
        let accessibility = macos_accessibility_client::accessibility::application_is_trusted();
        let input_monitoring = objc2_core_graphics::CGPreflightListenEventAccess();
        serde_json::json!({
            "accessibility": if accessibility { "available" } else { "required" },
            "input_monitoring": if input_monitoring { "available" } else { "required" },
            "microphone": "checked-on-use",
            "screen_capture": "checked-on-use",
            "direct_insertion": true,
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        serde_json::json!({
            "accessibility": "not-required",
            "input_monitoring": "not-required",
            "microphone": "checked-on-use",
            "screen_capture": "available",
            "direct_insertion": false,
        })
    }
}

#[tauri::command]
fn run_capability_test(capability: String) -> serde_json::Value {
    match capability.as_str() {
        "engine" => engine_status(),
        "permissions" => permission_status(),
        "microphone" => {
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

#[tauri::command]
fn retry_permission(capability: String) -> serde_json::Value {
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

    #[cfg(not(target_os = "macos"))]
    let _ = capability;

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
    prefs
        .get("launch_at_login")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
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
    let mut prefs = load_prefs();
    prefs["launch_at_login"] = serde_json::Value::Bool(active);
    write_json_atomic(&prefs_file(), &prefs)?;
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
    let url = format!("http://127.0.0.1:{}/health", port());
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()
        .and_then(|c| c.get(url).send().ok())
        .and_then(|r| r.json().ok())
        .unwrap_or_else(|| serde_json::json!({ "status": "down" }))
}

fn run_client(app: &AppHandle, script: &str, args: &[&str]) {
    let Some(paths) = Paths::current() else {
        let _ = app.emit("engine-missing", ());
        return;
    };
    let _ = client_command(&paths, script)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
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

    // This is a transport overlay, not one of Kokoro's normal application
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
    // content. The transport is visible only while Kokoro is actively playing,
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
fn status_window_needs_reassertion(requested: bool, visible: bool, on_active_space: bool) -> bool {
    requested && (!visible || !on_active_space)
}

#[cfg(target_os = "macos")]
fn place_status_panel(panel: &objc2_app_kit::NSPanel, width: f64) {
    let Some(main_thread) = objc2::MainThreadMarker::new() else {
        return;
    };
    let mut frame = objc2_app_kit::NSScreen::mainScreen(main_thread)
        .map(|screen| screen.visibleFrame())
        .unwrap_or_else(|| panel.frame());
    frame.origin.x += frame.size.width - width - 18.0;
    frame.origin.y += frame.size.height - 50.0 - 14.0;
    frame.size.width = width;
    frame.size.height = 50.0;
    panel.setFrame_display(frame, true);
}

/// macOS can leave an already-visible all-Spaces panel assigned to the Space
/// it was first ordered on when the user moves into another app's full-screen
/// Space. The audio client remains alive, so hiding the only controls is an
/// invalid state. Reassert the existing panel only after AppKit reports that
/// it has fallen off the active Space; normal app switches do no extra work.
#[cfg(target_os = "macos")]
fn start_status_space_watcher(app: AppHandle) {
    if STATUS_SPACE_WATCHER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(150));
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
            if !status_window_needs_reassertion(true, panel.isVisible(), panel.isOnActiveSpace()) {
                return;
            }
            let width = f64::from_bits(STATUS_WINDOW_WIDTH_BITS.load(Ordering::SeqCst));
            panel.setCollectionBehavior(status_window_collection_behavior());
            panel.setHidesOnDeactivate(false);
            panel.setCanHide(false);
            panel.setLevel(status_window_level());
            place_status_panel(panel, width);
            panel.orderFrontRegardless();
            structured_log(
                "status-window-reasserted",
                serde_json::json!({
                    "visible": panel.isVisible(),
                    "active_space": panel.isOnActiveSpace(),
                    "level": panel.level(),
                }),
            );
        });
    });
}

#[cfg(target_os = "macos")]
fn show_status_window_without_activation(window: &tauri::WebviewWindow, width: f64) {
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
            frame.origin.y += frame.size.height - 50.0 - 14.0;
            frame.size.width = width;
            frame.size.height = 50.0;
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
            panel.setBackgroundColor(Some(&native.backgroundColor()));
            panel.setHasShadow(false);
            unsafe { panel.setReleasedWhenClosed(false) };
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
        STATUS_WINDOW_REQUESTED.store(true, Ordering::SeqCst);
        place_status_panel(panel, width);
        panel.setCollectionBehavior(status_window_collection_behavior());
        panel.setHidesOnDeactivate(false);
        panel.setCanHide(false);
        // Do this synchronously in the same main-thread turn as ordering the
        // window. Tauri's set_always_on_top queues an asynchronous floating-
        // level update that can race this order operation and leave the player
        // under a full-screen app or on another Space.
        panel.setLevel(status_window_level());
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
fn show_status_window_without_activation(window: &tauri::WebviewWindow, _width: f64) {
    let _ = window.show();
}

#[cfg(target_os = "macos")]
fn hide_status_window(window: &tauri::WebviewWindow) {
    STATUS_WINDOW_REQUESTED.store(false, Ordering::SeqCst);
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
        let _ = w.set_size(tauri::LogicalSize::new(152.0, 50.0));
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
    show_status_window_without_activation(&w, 152.0);
}

fn show_player_notice(app: &AppHandle, message: &str) {
    let Some(w) = app.get_webview_window("player") else {
        return;
    };
    let _ = w.set_focusable(false);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = w.set_size(tauri::LogicalSize::new(250.0, 50.0));
        if let Ok(Some(mon)) = w.primary_monitor() {
            let scale = mon.scale_factor();
            let work_area = mon.work_area();
            let size = work_area.size.to_logical::<f64>(scale);
            let pos = work_area.position.to_logical::<f64>(scale);
            let _ = w.set_position(tauri::LogicalPosition::new(
                pos.x + size.width - 268.0,
                pos.y + 14.0,
            ));
        }
    }
    let encoded = serde_json::to_string(message).unwrap_or_else(|_| "\"Kokoro error\"".into());
    let _ = w.eval(format!(
        "window.__kokoroShowNotice && window.__kokoroShowNotice({encoded})"
    ));
    show_status_window_without_activation(&w, 250.0);
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

/// Spawn a client and keep the transport visible for exactly as long as it
/// runs. Fire-and-forget left the user with no way to stop audio: the mini
/// player was a host feature that did not survive the move off Hammerspoon.
fn run_client_monitored(app: &AppHandle, script: &str, args: Vec<String>) {
    let Some(paths) = Paths::current() else {
        let _ = app.emit("engine-missing", ());
        return;
    };
    show_player(app);
    set_player_mode(app, "playing");
    // Own the script name because the monitoring thread outlives this call.
    let script = script.to_string();
    let app2 = app.clone();
    std::thread::spawn(move || {
        let output = client_command(&paths, &script).args(&args).output();
        let notice = match output {
            Ok(result) => String::from_utf8_lossy(&result.stdout)
                .lines()
                .find_map(|line| line.strip_prefix("NOTICE ").map(str::to_owned)),
            Err(error) => Some(format!("Could not start Kokoro: {error}")),
        };
        if let Some(message) = notice {
            show_player_notice(&app2, &message);
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        hide_player(&app2);
    });
}

#[tauri::command]
fn read_selection(app: AppHandle) {
    // A synthetic Cmd+C mid-recording lands in whatever app has focus, and both
    // paths contend for the pasteboard. The previous host refused for the same
    // reason.
    if is_dictating(&app) {
        return;
    }
    let mut args = vec!["--selection".to_string()];
    args.extend(voice_args());
    run_client_monitored(&app, "speak.py", args);
}

/// Pause or resume, returning the engine's own view of the state.
#[tauri::command]
fn toggle_playback() -> String {
    let Some(paths) = Paths::current() else {
        return "idle".into();
    };
    client_command(&paths, "speak.py")
        .arg("--toggle")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "idle".into())
}

#[tauri::command]
fn stop_speaking(app: AppHandle) {
    if is_dictating(&app) {
        dictation_stop(&app);
    }
    run_client(&app, "speak.py", &["--stop"]);
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
        "capture-failed" => Some("Screen capture failed — check Screen Recording permission"),
        "no-text" => Some("No readable text found in that area"),
        "ocr-failed" => Some("Text recognition failed — try the snip again"),
        _ => Some("Snip could not start — run System Check"),
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
        let Ok(out) = out else {
            structured_log("snip-failed", serde_json::json!({ "code": "spawn-failed" }));
            show_player_notice(&app2, "Snip could not start — run System Check");
            std::thread::sleep(std::time::Duration::from_secs(3));
            hide_player(&app2);
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
                hide_player(&app2);
            }
            return;
        }
        structured_log(
            "snip-ocr-completed",
            serde_json::json!({ "characters": text.chars().count() }),
        );
        let mut args = vec!["--text".to_string(), text];
        args.extend(voice_args());
        run_client_monitored(&app2, "speak.py", args);
    });
}

#[tauri::command]
fn speak_text(app: AppHandle, text: String) {
    let mut args = vec!["--text".to_string(), text];
    args.extend(voice_args());
    run_client_monitored(&app, "speak.py", args);
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
    let mut copy = background_command("powershell")
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
    let target = match PlatformTextBackend::capture_target() {
        Ok(target) => Some(target),
        Err(ApplyOutcome::SecureField) => {
            structured_log(
                "dictation-start-rejected",
                serde_json::json!({ "code": "secure-field" }),
            );
            show_player_notice(app, "Dictation unavailable in secure fields");
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
        let st = playback_state(&paths);
        if st == "playing" {
            let _ = client_command(&paths, "speak.py").arg("--pause").status();
            true
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
        let mut recorder = client_command(&paths, "dictate.py");
        recorder
            .arg("--record")
            .args(["--session", &id])
            .stdout(Stdio::piped())
            // The protocol is stdout-only. Leaving stderr piped without a
            // reader can fill the OS pipe and deadlock a long transcription.
            .stderr(Stdio::null());
        if load_prefs()
            .get("live_preview")
            .and_then(|value| value.as_bool())
            .unwrap_or(true)
        {
            recorder.arg("--live-preview");
        }
        if let Some(device) = microphone_arg() {
            recorder.args(["--device", &device]);
        }
        let child = recorder.spawn();

        let mut lines_out: Vec<String> = Vec::new();
        let mut startup_timed_out = false;
        let mut final_inserted = false;
        let child_spawn_failed = child.is_err();
        if let Ok(mut child) = child {
            if let Some(d) = app2.try_state::<Dictation>() {
                if let Ok(mut guard) = d.0.lock() {
                    if let Some(session) = guard.as_mut().filter(|s| s.id == id) {
                        session.child_pid = Some(child.id());
                        if session.stop_requested {
                            run_client(&app2, "dictate.py", &["--stop", "--session", &id]);
                        }
                    }
                }
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
                match rx.recv_timeout(std::time::Duration::from_secs(8)) {
                    Ok(line)
                        if dictation_protocol::parse(&line)
                            == dictation_protocol::Event::Recording =>
                    {
                        structured_log(
                            "dictation-recorder-ready",
                            serde_json::json!({ "session": id }),
                        );
                        set_player_mode(&app2, "recording");
                        set_dictation_status(&app2, &id, DictationStatus::Recording);
                        let stopped_early = app2
                            .try_state::<Dictation>()
                            .and_then(|state| {
                                state.0.lock().ok().and_then(|session| {
                                    session
                                        .as_ref()
                                        .filter(|active| active.id == id)
                                        .map(|active| active.stop_requested)
                                })
                            })
                            .unwrap_or(false);
                        if stopped_early {
                            run_client(&app2, "dictate.py", &["--stop", "--session", &id]);
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
                            accepting_preview = false;
                            show_player(&app2);
                            set_player_mode(&app2, "transcribing");
                            set_dictation_status(&app2, &id, DictationStatus::Transcribing);
                        }
                        Event::RetryingEngine => {
                            structured_log("engine-retry", serde_json::json!({ "session": id }));
                            spawn_engine_and_record(&app2);
                        }
                        Event::InactivityWarning => {
                            show_player_notice(&app2, "Still recording — release or press Escape");
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
                                            "Focus changed — final will be copied",
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
                                            "Target unavailable — final will be copied",
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
                    if let Some(target) = target.as_mut() {
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
                        show_player_notice(
                            &app2,
                            if cfg!(target_os = "windows") {
                                "Full message copied — press Ctrl+V to paste it"
                            } else {
                                "Full message copied — press Command+V to paste it"
                            },
                        );
                    }
                }
            }
            let _ = child.wait();
        }

        hide_player(&app2);
        if ducked {
            let _ = client_command(&paths, "speak.py").arg("--resume").status();
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
            dictation_protocol::parse(line) == dictation_protocol::Event::Error("no audio captured")
        }) {
            set_dictation_status(&app2, &id, DictationStatus::Cancelled);
            show_player_notice(&app2, "No speech captured — hold the keys while speaking");
        } else {
            set_dictation_status(&app2, &id, DictationStatus::Cancelled);
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

/// idle | playing | paused, straight from the speak client.
fn playback_state(paths: &Paths) -> String {
    client_command(paths, "speak.py")
        .arg("--status")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn dictation_stop(app: &AppHandle) {
    let active = app.try_state::<Dictation>().and_then(|d| {
        d.0.lock().ok().and_then(|mut session| {
            session.as_mut().map(|current| {
                current.stop_requested = true;
                (current.id.clone(), current.status.clone())
            })
        })
    });
    if let Some((id, state)) = active {
        structured_log(
            "dictation-trigger",
            serde_json::json!({ "action": "stop", "session": id, "state": state }),
        );
        run_client(app, "dictate.py", &["--stop", "--session", &id]);
    } else {
        structured_log(
            "dictation-stop-ignored",
            serde_json::json!({ "code": "no-active-session" }),
        );
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn dictation_cancel(app: &AppHandle) {
    let id = app.try_state::<Dictation>().and_then(|d| {
        d.0.lock().ok().and_then(|mut session| {
            session.as_mut().map(|current| {
                current.status = DictationStatus::CancelledByUser;
                current.id.clone()
            })
        })
    });
    if let Some(id) = id {
        set_dictation_status(app, &id, DictationStatus::CancelledByUser);
        run_client(app, "dictate.py", &["--cancel", "--session", &id]);
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
/// as the end-to-end proof that a recorded physical shortcut reached Kokoro;
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
    #[cfg(target_os = "windows")]
    windows_chords::suspend(true);
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let config = hotkeys::Config::from_preferences(&load_prefs());

    #[cfg(target_os = "macos")]
    {
        if !objc2_core_graphics::CGPreflightListenEventAccess() {
            return Err(
                "Input Monitoring is required for shortcuts; enable Kokoro Voice 2.1 in Privacy & Security"
                    .into(),
            );
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
                    read_selection(read_app.clone());
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
                move |app, sc, event| {
                    match event.state {
                        ShortcutState::Pressed => {
                            if Some(sc) == read.as_ref() {
                                hotkey_triggered(app, hotkeys::Slot::Read);
                                read_selection(app.clone());
                            } else if Some(sc) == snip.as_ref() {
                                hotkey_triggered(app, hotkeys::Slot::Snip);
                                snip_and_read(app.clone());
                            } else if Some(sc) == dictate.as_ref() {
                                hotkey_triggered(app, hotkeys::Slot::Dictate);
                                dictation_start(app); // push to talk
                            }
                        }
                        ShortcutState::Released => {
                            if Some(sc) == dictate.as_ref() {
                                dictation_stop(app);
                            }
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
                    "read": config.read, "dictate": config.dictate, "snip": config.snip,
                }),
            );
        }
        result
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Err("unsupported platform".into())
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
    let capture = hotkeys::classify_capture(slot, &accelerator, cfg!(target_os = "macos"))?;
    if capture.kind == hotkeys::CaptureKind::RegisteredShortcut {
        accelerator
            .parse::<Shortcut>()
            .map_err(|_| format!("that combination cannot be used: {accelerator}"))?;
    }

    let mut p = load_prefs();
    let key = slot.preference_key();
    let previous = p.get(key).and_then(|v| v.as_str()).map(String::from);
    p[key] = serde_json::json!(accelerator);
    write_json_atomic(&prefs_file(), &p)
        .map_err(|error| format!("could not save shortcut: {error}"))?;

    // A successful commit resumes the controller and installs the candidate
    // exactly once. The frontend calls end_hotkey_recording only for cancel,
    // timeout, or validation failure.
    #[cfg(target_os = "macos")]
    chords::set_recorder_suspended(false);

    if let Err(e) = register_hotkeys(&app) {
        let mut p = load_prefs();
        match previous {
            Some(prev) => p[key] = serde_json::json!(prev),
            None => {
                p.as_object_mut().map(|object| object.remove(key));
            }
        }
        let _ = write_json_atomic(&prefs_file(), &p);
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
    SIGNALLED.store(true, Ordering::Relaxed);
}

/// Tauri's exit hooks only run when the app quits through its own event loop.
/// A signal — Activity Monitor, `kill`, a logout — bypasses them entirely, and
/// the engine would be left holding the port.
#[cfg(unix)]
fn install_signal_handlers(app: AppHandle) {
    unsafe {
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
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if SIGNALLED.load(Ordering::Relaxed) {
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
    fn status_window_is_reasserted_only_when_requested_and_off_space() {
        assert!(status_window_needs_reassertion(true, true, false));
        assert!(status_window_needs_reassertion(true, false, true));
        assert!(!status_window_needs_reassertion(true, true, true));
        assert!(!status_window_needs_reassertion(false, false, false));
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
        assert!(launch_at_login_preference(&serde_json::json!({})));
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

fn show_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // A tray click must also restore Settings after it was minimized.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // A second launch would spawn a second engine and the two would fight
        // over the port. Focus the existing window instead.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_settings(app);
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
            set_launch_at_login
        ])
        .setup(|app| {
            use tauri_plugin_autostart::ManagerExt;
            let handle = app.handle().clone();
            app.manage(Engine(Mutex::new(None)));
            app.manage(Dictation(Mutex::new(None)));
            // Kokoro is an accessibility tool whose hotkeys must be available
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
                spawn_engine_and_record(&handle);
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
                show_player_notice(
                    &handle,
                    if permission_required {
                        "Enable Kokoro Voice 2.1 in Input Monitoring"
                    } else {
                        "Kokoro shortcuts could not start"
                    },
                );
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
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
                    "open" => show_settings(app),
                    "quit" => {
                        stop_engine(app);
                        app.exit(0);
                    }
                    _ => {}
                });

            // Windows uses left-click for Settings; right-click keeps the menu.
            // Leave macOS menu-bar interaction unchanged and create only one icon.
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
                        show_settings(tray.app_handle());
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
        .expect("error while building Kokoro Voice 2.1")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                stop_engine(app);
            }
        });
}
