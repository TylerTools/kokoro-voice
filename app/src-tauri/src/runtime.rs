//! Runtime paths and managed Python-engine lifecycle.
//!
//! This module owns the one supported engine child and every path/environment
//! boundary shared with Python. It does not own setup downloads or user actions.

use crate::{runtime_hygiene, variant};
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Manager};

pub(crate) struct Engine(pub(crate) Mutex<Option<Child>>);

static LOG_LOCK: Mutex<()> = Mutex::new(());
static LOCAL_HTTP_CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
/// Set while intentionally shutting down so the watchdog cannot resurrect the engine.
pub(crate) static QUITTING: AtomicBool = AtomicBool::new(false);

pub(crate) fn local_http_client() -> &'static reqwest::blocking::Client {
    LOCAL_HTTP_CLIENT.get_or_init(reqwest::blocking::Client::new)
}

pub(crate) fn local_json(path: &str, timeout: std::time::Duration) -> Option<serde_json::Value> {
    local_http_client()
        .get(format!("http://127.0.0.1:{}{path}", port()))
        .timeout(timeout)
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .ok()
}

#[cfg(target_os = "macos")]
fn home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

pub(crate) fn engine_root() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(variant::APP_SUPPORT_DIR)
        .join("engine")
}

/// Local name of the Kokoro v1.0 fp16 export from upstream `model-files-v1.1`.
/// Upstream reuses the name `kokoro-v1.0.fp16.onnx` for every re-export, so the
/// local name carries the export. Changing it makes older installs report
/// not-installed, which re-runs setup and rebuilds the locked environment with
/// the model; `tts_engine.py` and `server.py` default to the same name.
pub(crate) const KOKORO_MODEL_FILE: &str = "kokoro-v1.0.fp16-2026-08.onnx";
pub(crate) const KOKORO_VOICES_FILE: &str = "voices-v1.0.bin";

pub(crate) fn stt_cache_home(root: &std::path::Path) -> std::path::PathBuf {
    root.join("models").join("huggingface")
}

#[cfg(target_os = "macos")]
const STT_CACHE_REPOSITORY: &str = "models--mlx-community--whisper-large-v3-turbo";
#[cfg(target_os = "macos")]
const STT_CACHE_REVISION: &str = "a4aaeec0636e6fef84abdcbe3544cb2bf7e9f6fb";

#[cfg(target_os = "macos")]
fn stt_snapshot(cache_home: &std::path::Path) -> std::path::PathBuf {
    cache_home
        .join("hub")
        .join(STT_CACHE_REPOSITORY)
        .join("snapshots")
        .join(STT_CACHE_REVISION)
}

#[cfg(target_os = "macos")]
fn stt_cache_is_complete(cache_home: &std::path::Path) -> bool {
    let snapshot = stt_snapshot(cache_home);
    snapshot.join("config.json").is_file()
        && snapshot
            .join("weights.safetensors")
            .metadata()
            .is_ok_and(|metadata| metadata.len() > 1_000_000_000)
}

#[cfg(target_os = "macos")]
fn adopt_stt_cache_from(
    source_home: &std::path::Path,
    destination_home: &std::path::Path,
) -> Result<bool, String> {
    use std::os::unix::fs::symlink;

    if stt_cache_is_complete(destination_home) {
        return Ok(false);
    }
    if !stt_cache_is_complete(source_home) {
        return Err("the pinned offline speech model is not available for adoption".into());
    }
    let source_repository = source_home.join("hub").join(STT_CACHE_REPOSITORY);
    let destination_hub = destination_home.join("hub");
    std::fs::create_dir_all(&destination_hub)
        .map_err(|error| format!("cannot create owned speech-model cache: {error}"))?;
    let destination_repository = destination_hub.join(STT_CACHE_REPOSITORY);
    if destination_repository.exists() {
        std::fs::remove_dir_all(&destination_repository)
            .map_err(|error| format!("cannot replace incomplete speech-model cache: {error}"))?;
    }
    let staging = destination_hub.join(format!(
        ".{STT_CACHE_REPOSITORY}.staging-{}",
        std::process::id()
    ));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|error| format!("cannot clear stale speech-model staging: {error}"))?;
    }
    let result = (|| {
        std::fs::create_dir_all(staging.join("blobs")).map_err(|error| error.to_string())?;
        for entry in std::fs::read_dir(source_repository.join("blobs"))
            .map_err(|error| error.to_string())?
            .flatten()
        {
            let metadata = entry.metadata().map_err(|error| error.to_string())?;
            if metadata.is_file() {
                std::fs::hard_link(entry.path(), staging.join("blobs").join(entry.file_name()))
                    .map_err(|error| {
                        format!("cannot adopt speech-model blob without copying: {error}")
                    })?;
            }
        }
        let source_snapshot = source_repository.join("snapshots").join(STT_CACHE_REVISION);
        let staged_snapshot = staging.join("snapshots").join(STT_CACHE_REVISION);
        std::fs::create_dir_all(&staged_snapshot).map_err(|error| error.to_string())?;
        for entry in std::fs::read_dir(&source_snapshot)
            .map_err(|error| error.to_string())?
            .flatten()
        {
            let metadata =
                std::fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
            let target = staged_snapshot.join(entry.file_name());
            if metadata.file_type().is_symlink() {
                let link = std::fs::read_link(entry.path()).map_err(|error| error.to_string())?;
                let resolved = entry
                    .path()
                    .parent()
                    .unwrap_or(&source_snapshot)
                    .join(&link)
                    .canonicalize()
                    .map_err(|error| error.to_string())?;
                let repository = source_repository
                    .canonicalize()
                    .map_err(|error| error.to_string())?;
                if !resolved.starts_with(repository) {
                    return Err("speech-model cache contains an unsafe link".into());
                }
                symlink(link, target).map_err(|error| error.to_string())?;
            } else if metadata.is_file() {
                std::fs::hard_link(entry.path(), target).map_err(|error| error.to_string())?;
            }
        }
        let source_ref = source_repository.join("refs").join("main");
        if source_ref.is_file() {
            std::fs::create_dir_all(staging.join("refs")).map_err(|error| error.to_string())?;
            std::fs::copy(source_ref, staging.join("refs").join("main"))
                .map_err(|error| error.to_string())?;
        }
        std::fs::rename(&staging, &destination_repository).map_err(|error| error.to_string())?;
        if !stt_cache_is_complete(destination_home) {
            return Err("adopted speech-model cache did not pass validation".into());
        }
        Ok(true)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(target_os = "macos")]
fn prepare_stt_cache(root: &std::path::Path) -> Result<&'static str, String> {
    let destination = stt_cache_home(root);
    if stt_cache_is_complete(&destination) {
        return Ok("owned");
    }
    let sources = [
        home().join(".cache").join("huggingface"),
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("huggingface"),
    ];
    for source in sources {
        if stt_cache_is_complete(&source) {
            let adopted = adopt_stt_cache_from(&source, &destination)?;
            structured_log(
                "stt-cache-adopted",
                serde_json::json!({ "adopted": adopted, "mode": "owned" }),
            );
            return Ok("owned");
        }
    }
    Err("the pinned offline speech model is not available for adoption".into())
}

#[cfg(not(target_os = "macos"))]
fn prepare_stt_cache(_root: &std::path::Path) -> Result<&'static str, String> {
    Ok("platform-default")
}

pub(crate) fn config_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    let directory = home().join(".config").join(variant::CONFIG_DIR_NAME);
    #[cfg(not(target_os = "macos"))]
    let directory = dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(variant::APP_SUPPORT_DIR);
    let _ = std::fs::create_dir_all(&directory);
    directory
}

pub(crate) fn legacy_config_dir() -> Option<std::path::PathBuf> {
    let name = variant::LEGACY_CONFIG_DIR_NAME?;
    #[cfg(target_os = "macos")]
    let directory = home().join(".config").join(name);
    #[cfg(not(target_os = "macos"))]
    let directory = dirs::config_dir()?.join(name);
    Some(directory)
}

fn cleanup_runtime_state() {
    let current = runtime_hygiene::cleanup_managed_runtime(
        &config_dir().join("runtime"),
        std::time::Duration::from_secs(5 * 60),
        runtime_hygiene::speaker_is_live,
    );
    let legacy = legacy_config_dir()
        .map(|directory| {
            runtime_hygiene::cleanup_managed_runtime(
                &directory.join("runtime"),
                std::time::Duration::from_secs(60 * 60),
                runtime_hygiene::speaker_is_live,
            )
        })
        .unwrap_or_default();
    if current.files_removed + legacy.files_removed > 0 {
        structured_log(
            "runtime-hygiene",
            serde_json::json!({
                "current_files_removed": current.files_removed,
                "current_bytes_removed": current.bytes_removed,
                "legacy_files_removed": legacy.files_removed,
                "legacy_bytes_removed": legacy.bytes_removed,
            }),
        );
    }
}

fn ensure_auth_token_with<F>(
    directory: &std::path::Path,
    generate: F,
) -> Result<std::path::PathBuf, String>
where
    F: FnOnce() -> Result<String, String>,
{
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("cannot create private configuration directory: {error}"))?;
    let directory_metadata = std::fs::symlink_metadata(directory)
        .map_err(|error| format!("cannot inspect private configuration directory: {error}"))?;
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        return Err("private configuration path is not an owned directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure private configuration directory: {error}"))?;
    }

    let token_file = directory.join("token");
    match std::fs::symlink_metadata(&token_file) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err("token path is not a regular file".into());
        }
        Ok(_) => match std::fs::read_to_string(&token_file) {
            Ok(token) if !token.trim().is_empty() => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600))
                        .map_err(|error| format!("cannot secure token file: {error}"))?;
                }
                return Ok(token_file);
            }
            Ok(_) => {}
            Err(error) => return Err(format!("cannot read token file: {error}")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect token file: {error}")),
    }

    let token = generate()?;
    if token.trim().len() < 32 {
        return Err("generated token did not meet the minimum length".into());
    }
    let temporary = directory.join(format!(".token.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("cannot create private token: {error}"))?;
    if let Err(error) = writeln!(file, "{}", token.trim()).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("cannot write private token: {error}"));
    }
    drop(file);
    if let Err(error) = replace_file(&temporary, &token_file) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("cannot activate private token: {error}"));
    }
    Ok(token_file)
}

pub(crate) fn ensure_auth_token(python: &std::path::Path) -> Result<std::path::PathBuf, String> {
    ensure_auth_token_with(&config_dir(), || {
        let output = Command::new(python)
            .args(["-c", "import secrets;print(secrets.token_urlsafe(32))"])
            .output()
            .map_err(|error| format!("cannot generate token: {error}"))?;
        if !output.status.success() {
            return Err("private Python runtime could not generate a token".into());
        }
        String::from_utf8(output.stdout).map_err(|_| "generated token was not UTF-8".into())
    })
}

pub(crate) fn structured_log(event: &str, fields: serde_json::Value) {
    use std::io::Write;
    let Ok(_guard) = LOG_LOCK.lock() else {
        return;
    };
    let path = config_dir().join("events.jsonl");
    if path.metadata().map(|metadata| metadata.len()).unwrap_or(0) > 1_000_000 {
        let _ = std::fs::rename(&path, config_dir().join("events.previous.jsonl"));
    }
    let record = serde_json::json!({
        "timestamp_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis(),
        "event": event,
        "platform": std::env::consts::OS,
        "app_version": env!("CARGO_PKG_VERSION"),
        "process_id": std::process::id(),
        "product": variant::DISPLAY_NAME,
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

pub(crate) fn python_path(root: &std::path::Path) -> std::path::PathBuf {
    if cfg!(windows) {
        root.join(".venv/Scripts/python.exe")
    } else {
        root.join(".venv/bin/python")
    }
}

pub(crate) fn port() -> String {
    std::env::var("KOKORO_PORT").unwrap_or_else(|_| variant::DEFAULT_PORT.into())
}

pub(crate) fn is_installed() -> bool {
    let root = engine_root();
    let models = root.join("models");
    python_path(&root).exists()
        && models.join(KOKORO_MODEL_FILE).exists()
        && models.join(KOKORO_VOICES_FILE).exists()
}

pub(crate) fn active_source_file(root: &std::path::Path) -> std::path::PathBuf {
    root.join("active-source")
}

pub(crate) fn active_source_root(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let name = std::fs::read_to_string(active_source_file(root)).ok()?;
    let path = std::path::Path::new(name.trim());
    if path.components().count() != 1
        || !matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return None;
    }
    let source = root.join("sources").join(path);
    source.join("server.py").exists().then_some(source)
}

pub(crate) fn replace_file(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    #[cfg(windows)]
    if destination.exists() {
        std::fs::remove_file(destination)?;
    }
    std::fs::rename(source, destination)
}

#[derive(Clone)]
pub(crate) struct Paths {
    pub(crate) python: std::path::PathBuf,
    pub(crate) root: std::path::PathBuf,
    pub(crate) runtime_root: std::path::PathBuf,
}

impl Paths {
    pub(crate) fn current() -> Option<Self> {
        let root = engine_root();
        let python = python_path(&root);
        if python.exists() {
            if let Some(source) = active_source_root(&root) {
                return Some(Self {
                    python,
                    root: source,
                    runtime_root: root,
                });
            }
            if root.join("server.py").exists() {
                return Some(Self {
                    python,
                    root: root.clone(),
                    runtime_root: root,
                });
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            for candidate in [cwd.clone(), cwd.join(".."), cwd.join("../..")] {
                if let Ok(root) = candidate.canonicalize() {
                    let python = python_path(&root);
                    if python.exists() && root.join("server.py").exists() {
                        return Some(Self {
                            python,
                            runtime_root: root.clone(),
                            root,
                        });
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

pub(crate) fn client_command(paths: &Paths, script: &str) -> Command {
    let mut command = Command::new(&paths.python);
    command
        .arg(paths.client(script))
        .current_dir(&paths.root)
        .env("KOKORO_HOST", variant::CLIENT_HOST)
        .env("KOKORO_TOKEN_FILE", config_dir().join("token"))
        .env("KOKORO_STATE_DIR", config_dir().join("runtime"))
        .env("KOKORO_MODEL_DIR", paths.runtime_root.join("models"));
    command
}

fn start_engine(paths: &Paths) -> Result<Child, String> {
    let token_file = ensure_auth_token(&paths.python)?;
    let stt_cache_mode = prepare_stt_cache(&paths.runtime_root)?;
    let mut command = Command::new(&paths.python);
    command
        .args([
            "-m",
            "uvicorn",
            "server:app",
            "--host",
            "127.0.0.1",
            "--port",
            &port(),
        ])
        .current_dir(&paths.root)
        .env("HF_HUB_OFFLINE", "1")
        .env("HF_HOME", stt_cache_home(&paths.runtime_root))
        .env("KOKORO_TOKEN_FILE", token_file)
        .env("KOKORO_MODEL_DIR", paths.runtime_root.join("models"))
        .env("KOKORO_SERVICE_VERSION", env!("CARGO_PKG_VERSION"))
        .env("KOKORO_STT_CACHE_MODE", stt_cache_mode)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if std::env::var_os("KOKORO_ONNX_CPU_MEM_ARENA").is_none() {
        command.env("KOKORO_ONNX_CPU_MEM_ARENA", variant::TTS_CPU_MEM_ARENA);
    }
    command
        .spawn()
        .map_err(|error| format!("cannot start authenticated engine: {error}"))
}

pub(crate) fn start_watchdog(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(20));
        let mut failures = 0u32;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            if QUITTING.load(Ordering::SeqCst) || !is_installed() {
                continue;
            }
            let ok = local_json("/health", std::time::Duration::from_secs(4)).is_some();
            if ok {
                failures = 0;
                continue;
            }
            failures += 1;
            if failures < 2 {
                continue;
            }
            failures = 0;
            let _ = app.emit("engine-restarting", ());
            if let Err(error) = spawn_engine_and_record(&app) {
                structured_log(
                    "engine-restart-failed",
                    serde_json::json!({ "code": "authenticated-start-failed" }),
                );
                eprintln!("{error}");
            }
        }
    });
}

fn stop_managed_child(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    #[cfg(windows)]
    let _ = child.kill();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn stop_engine(app: &AppHandle) {
    QUITTING.store(true, Ordering::SeqCst);
    if let Some(engine) = app.try_state::<Engine>() {
        if let Ok(mut guard) = engine.0.lock() {
            if let Some(mut child) = guard.take() {
                stop_managed_child(&mut child);
            }
        }
    }
    let _ = std::fs::remove_file(pidfile());
}

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
            .map(|output| {
                let command = String::from_utf8_lossy(&output.stdout);
                command.contains("uvicorn") && command.contains("server:app")
            })
            .unwrap_or(false);
        #[cfg(target_os = "windows")]
        let ours = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine"),
            ])
            .output()
            .ok()
            .is_some_and(|output| {
                let command = String::from_utf8_lossy(&output.stdout);
                command.contains("uvicorn") && command.contains("server:app")
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

pub(crate) fn spawn_engine_and_record(app: &AppHandle) -> Result<(), String> {
    let Some(paths) = Paths::current() else {
        return Err("private engine runtime is unavailable".into());
    };
    cleanup_runtime_state();
    reap_orphan();
    let mut child = start_engine(&paths)?;
    if let Err(error) = std::fs::write(pidfile(), child.id().to_string()) {
        stop_managed_child(&mut child);
        return Err(format!("cannot record engine process: {error}"));
    }
    let Some(engine) = app.try_state::<Engine>() else {
        stop_managed_child(&mut child);
        let _ = std::fs::remove_file(pidfile());
        return Err("engine process state is unavailable".into());
    };
    let Ok(mut guard) = engine.0.lock() else {
        stop_managed_child(&mut child);
        let _ = std::fs::remove_file(pidfile());
        return Err("engine process state is unavailable".into());
    };
    *guard = Some(child);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMPORARY_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn temporary_directory(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "kokoro-runtime-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn missing_token_is_created_and_existing_token_is_preserved() {
        let directory = temporary_directory("token");
        let _ = std::fs::remove_dir_all(&directory);
        let token_path = ensure_auth_token_with(&directory, || Ok("a".repeat(43))).unwrap();
        assert_eq!(
            std::fs::read_to_string(&token_path).unwrap().trim(),
            "a".repeat(43)
        );
        ensure_auth_token_with(&directory, || panic!("existing token must be preserved")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&token_path).unwrap().trim(),
            "a".repeat(43)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn token_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let directory = temporary_directory("symlink");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        symlink("elsewhere", directory.join("token")).unwrap();
        let error = ensure_auth_token_with(&directory, || Ok("b".repeat(43))).unwrap_err();
        assert!(error.contains("regular file"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pinned_stt_cache_is_adopted_with_hard_links() {
        use std::os::unix::fs::{symlink, MetadataExt};
        let root = temporary_directory("stt-cache");
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        let destination = root.join("destination");
        let repository = source.join("hub").join(STT_CACHE_REPOSITORY);
        let blobs = repository.join("blobs");
        let snapshot = repository.join("snapshots").join(STT_CACHE_REVISION);
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::create_dir_all(&snapshot).unwrap();
        let weights = blobs.join("weights");
        let weights_file = std::fs::File::create(&weights).unwrap();
        weights_file.set_len(1_000_000_001).unwrap();
        std::fs::write(blobs.join("config"), b"{}").unwrap();
        symlink("../../blobs/weights", snapshot.join("weights.safetensors")).unwrap();
        symlink("../../blobs/config", snapshot.join("config.json")).unwrap();

        assert!(adopt_stt_cache_from(&source, &destination).unwrap());
        assert!(stt_cache_is_complete(&destination));
        let adopted_weights = destination
            .join("hub")
            .join(STT_CACHE_REPOSITORY)
            .join("blobs")
            .join("weights");
        assert_eq!(
            std::fs::metadata(weights).unwrap().ino(),
            std::fs::metadata(adopted_weights).unwrap().ino()
        );
        assert!(!adopt_stt_cache_from(&source, &destination).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
}
