mod chords;
// Kokoro Voice — desktop app.
//
// The shell: first-run setup, tray, global hotkeys, settings UI, and ownership
// of the engine process. Launching starts the engine; quitting stops it.
//
// The engine SOURCE ships inside the app bundle (a few small Python files), but
// the heavy parts — the Python environment and ~500MB of model weights — are
// built into the user's Application Support directory on first run. That keeps
// the download at ~10MB instead of ~1.3GB, and lets the app be updated without
// re-shipping the models.
//
// The actual work is still done by the Python clients (speak/dictate/snip),
// which are measured, debugged, and shared with the Windows build.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

struct Engine(Mutex<Option<Child>>);

/// Guards against key auto-repeat re-entering the recorder while it is already
/// running: holding a push-to-talk key fires Pressed repeatedly.
static DICTATING: AtomicBool = AtomicBool::new(false);
static SIGNALLED: AtomicBool = AtomicBool::new(false);

// ── locations ────────────────────────────────────────────────────────────────

fn home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Where the installed engine lives. Deliberately NOT inside the .app bundle:
/// a bundle should be replaceable by dragging a new one over it, and writing
/// inside it breaks the code signature.
fn engine_root() -> std::path::PathBuf {
    home().join("Library/Application Support/Kokoro Voice/engine")
}

fn config_dir() -> std::path::PathBuf {
    let d = home().join(".config/kokoro");
    let _ = std::fs::create_dir_all(&d);
    d
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
    std::env::var("KOKORO_PORT").unwrap_or_else(|_| "8123".into())
}

/// True once the environment and both model files are in place.
fn is_installed() -> bool {
    let root = engine_root();
    python_path(&root).exists()
        && root.join("server.py").exists()
        && root.join("models/kokoro-v1.0.fp16.onnx").exists()
        && root.join("models/voices-v1.0.bin").exists()
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

// ── engine lifecycle ─────────────────────────────────────────────────────────

fn start_engine(paths: &Paths) -> Option<Child> {
    Command::new(&paths.python)
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
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn stop_engine(app: &AppHandle) {
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
        let ours = Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .ok()
            .map(|o| {
                let c = String::from_utf8_lossy(&o.stdout);
                c.contains("uvicorn") && c.contains("server:app")
            })
            .unwrap_or(false);
        if ours {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
    }
    let _ = std::fs::remove_file(&path);
}

fn spawn_engine_and_record(app: &AppHandle) {
    let Some(paths) = Paths::current() else { return };
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
    let _ = app.emit(
        "setup-progress",
        serde_json::json!({ "pct": pct, "message": message }),
    );
}

fn find_or_install_uv(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    for c in [
        home().join(".local/bin/uv"),
        std::path::PathBuf::from("/opt/homebrew/bin/uv"),
        std::path::PathBuf::from("/usr/local/bin/uv"),
    ] {
        if c.exists() {
            return Ok(c);
        }
    }
    emit_step(app, 18, "Downloading the Python manager…");
    let st = Command::new("sh")
        .arg("-c")
        .arg("curl -LsSf https://astral.sh/uv/install.sh | sh")
        .status()
        .map_err(|e| format!("uv install: {e}"))?;
    let path = home().join(".local/bin/uv");
    if st.success() && path.exists() {
        Ok(path)
    } else {
        Err("could not install uv (the Python manager)".into())
    }
}

/// Build the engine into Application Support.
///
/// Every step is idempotent so an interrupted run can simply be retried — a
/// half-built environment is the most likely failure and the least forgivable
/// one to strand someone in.
#[tauri::command]
async fn setup_engine(app: AppHandle) -> Result<String, String> {
    let root = engine_root();
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {root:?}: {e}"))?;

    // 1. Copy the engine source out of the app bundle.
    emit_step(&app, 5, "Unpacking…");
    let src = app
        .path()
        .resource_dir()
        .map_err(|e| format!("no resource dir: {e}"))?
        .join("engine-src");
    if !src.exists() {
        return Err(format!("engine source missing from the app bundle ({src:?})"));
    }
    for name in ["server.py", "requirements.txt", "requirements-macos.txt"] {
        let from = src.join(name);
        if from.exists() {
            std::fs::copy(&from, root.join(name)).map_err(|e| format!("copy {name}: {e}"))?;
        }
    }
    std::fs::create_dir_all(root.join("client")).ok();
    for name in ["speak.py", "dictate.py", "snip.py"] {
        let from = src.join("client").join(name);
        if from.exists() {
            std::fs::copy(&from, root.join("client").join(name))
                .map_err(|e| format!("copy {name}: {e}"))?;
        }
    }

    // 2. uv — manages Python without touching the system install.
    emit_step(&app, 15, "Setting up Python…");
    let uv = find_or_install_uv(&app)?;

    // 3. Environment.
    let venv_ok = Command::new(&uv)
        .args(["venv", "--python", "3.12"])
        .arg(root.join(".venv"))
        .status()
        .map_err(|e| format!("uv venv: {e}"))?;
    if !venv_ok.success() {
        return Err("could not create the Python environment".into());
    }

    emit_step(&app, 30, "Installing components… (a minute or two)");
    for req in ["requirements.txt", "requirements-macos.txt"] {
        let f = root.join(req);
        if !f.exists() {
            continue;
        }
        let st = Command::new(&uv)
            .args(["pip", "install", "-r"])
            .arg(&f)
            .env("VIRTUAL_ENV", root.join(".venv"))
            .status()
            .map_err(|e| format!("uv pip install: {e}"))?;
        if !st.success() {
            return Err(format!("could not install {req}"));
        }
    }
    // mlx-whisper declares torch but only imports it in the weight-CONVERSION
    // path, which we never take. Verified that torch never enters sys.modules
    // during import or a real transcribe(). Dropping it saves ~480MB.
    let _ = Command::new(&uv)
        .args(["pip", "uninstall", "torch"])
        .env("VIRTUAL_ENV", root.join(".venv"))
        .status();

    // 4. Models — not redistributed, fetched from upstream. Sizes are verified
    //    because a truncated download fails much later and far less obviously.
    std::fs::create_dir_all(root.join("models")).ok();
    let base = "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0";
    for (name, expected, pct) in [
        ("kokoro-v1.0.fp16.onnx", 177_464_787u64, 55u32),
        ("voices-v1.0.bin", 28_214_398u64, 80u32),
    ] {
        let dest = root.join("models").join(name);
        if dest.metadata().map(|m| m.len()).ok() == Some(expected) {
            continue;
        }
        emit_step(&app, pct, &format!("Downloading voices ({name})…"));
        let st = Command::new("curl")
            .args(["-fL", "--retry", "3", "-o"])
            .arg(&dest)
            .arg(format!("{base}/{name}"))
            .status()
            .map_err(|e| format!("download {name}: {e}"))?;
        if !st.success() {
            return Err(format!("could not download {name}"));
        }
        let got = dest.metadata().map(|m| m.len()).unwrap_or(0);
        if got != expected {
            let _ = std::fs::remove_file(&dest);
            return Err(format!("{name} downloaded {got} bytes, expected {expected}"));
        }
    }

    // 5. Auth token.
    emit_step(&app, 92, "Finishing…");
    let token_file = config_dir().join("token");
    if std::fs::read_to_string(&token_file)
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
    {
        let out = Command::new(python_path(&root))
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

    emit_step(&app, 100, "Ready");
    spawn_engine_and_record(&app);
    Ok("installed".into())
}

// ── preferences ──────────────────────────────────────────────────────────────

fn prefs_file() -> std::path::PathBuf {
    config_dir().join("prefs.json")
}

fn load_prefs() -> serde_json::Value {
    std::fs::read_to_string(prefs_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({ "voice": "af_heart", "speed": 1.0 }))
}

#[tauri::command]
fn get_prefs() -> serde_json::Value {
    load_prefs()
}

#[tauri::command]
fn set_prefs(voice: Option<String>, speed: Option<f64>) -> serde_json::Value {
    let mut p = load_prefs();
    if let Some(v) = voice {
        p["voice"] = serde_json::Value::String(v);
    }
    if let Some(sp) = speed {
        // Clamp to what the engine accepts (gt 0.1, le 3.0) so a bad value
        // fails here rather than as an opaque 422 mid-read.
        p["speed"] = serde_json::json!(sp.clamp(0.25, 3.0));
    }
    let _ = std::fs::write(prefs_file(), serde_json::to_string_pretty(&p).unwrap_or_default());
    p
}

#[tauri::command]
fn list_voices() -> Vec<String> {
    let url = format!("http://127.0.0.1:{}/voices", port());
    Command::new("curl")
        .args(["-fsS", "-m", "5", &url])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .and_then(|v| {
            v.get("voices")?
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
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

// ── commands ─────────────────────────────────────────────────────────────────

#[tauri::command]
fn engine_status() -> serde_json::Value {
    if !is_installed() {
        return serde_json::json!({ "status": "not-installed" });
    }
    let url = format!("http://127.0.0.1:{}/health", port());
    match Command::new("curl").args(["-fsS", "-m", "3", &url]).output() {
        Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|_| serde_json::json!({ "status": "starting" })),
        _ => serde_json::json!({ "status": "down" }),
    }
}

fn run_client(app: &AppHandle, script: &str, args: &[&str]) {
    let Some(paths) = Paths::current() else {
        let _ = app.emit("engine-missing", ());
        return;
    };
    let _ = Command::new(&paths.python)
        .arg(paths.client(script))
        .args(args)
        .current_dir(&paths.root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Park the transport in the upper-right of the work area and show it.
fn show_player(app: &AppHandle) {
    let Some(w) = app.get_webview_window("player") else { return };
    if let Ok(Some(mon)) = w.primary_monitor() {
        let scale = mon.scale_factor();
        let size = mon.size().to_logical::<f64>(scale);
        let pos = mon.position().to_logical::<f64>(scale);
        let _ = w.set_position(tauri::LogicalPosition::new(
            pos.x + size.width - 158.0,
            pos.y + 14.0,
        ));
    }
    let _ = w.show();
    // Never steal focus: this appears mid-read, and taking focus would yank the
    // caret out of whatever the user is actually working in.
    let _ = w.set_always_on_top(true);
}

fn hide_player(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("player") {
        let _ = w.hide();
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
    // Resolve the script path up front: the &str cannot outlive this call, and
    // the monitoring thread does.
    let client = paths.client(script);
    let app2 = app.clone();
    std::thread::spawn(move || {
        let status = Command::new(&paths.python)
            .arg(client)
            .args(&args)
            .current_dir(&paths.root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = status;
        hide_player(&app2);
    });
}

#[tauri::command]
fn read_selection(app: AppHandle) {
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
    Command::new(&paths.python)
        .arg(paths.client("speak.py"))
        .arg("--toggle")
        .current_dir(&paths.root)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "idle".into())
}

#[tauri::command]
fn stop_speaking(app: AppHandle) {
    run_client(&app, "speak.py", &["--stop"]);
    hide_player(&app);
}

#[tauri::command]
fn snip_and_read(app: AppHandle) {
    // The crosshair comes first, so the transport only appears once snip.py
    // hands off to playback — showing it during selection would cover the very
    // thing being selected.
    run_client_monitored(&app, "snip.py", vec!["--speak".into()]);
}

#[tauri::command]
fn speak_text(app: AppHandle, text: String) {
    let mut args = vec!["--text".to_string(), text];
    args.extend(voice_args());
    run_client_monitored(&app, "speak.py", args);
}

/// Type the transcript into whatever has focus.
///
/// Goes through System Events, so the app needs the Accessibility permission —
/// the same grant that lets it read your selection. The text becomes an
/// AppleScript string literal, so quotes and backslashes MUST be escaped: an
/// unescaped quote would not merely break the script, it would change it.
fn type_text(text: &str) {
    // Our own synthesized keystrokes go through the same event tap, so deafen
    // it while typing or a transcript could retrigger the gesture that made it.
    chords::suppress(true);
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(r#"tell application "System Events" to keystroke "{escaped}""#);
    let _ = Command::new("osascript")
        .args(["-e", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    chords::suppress(false);
}

/// Start recording. The transcript is typed when the recorder exits.
fn dictation_start(app: &AppHandle) {
    if DICTATING.swap(true, Ordering::SeqCst) {
        return; // auto-repeat; already recording
    }
    let Some(paths) = Paths::current() else {
        DICTATING.store(false, Ordering::SeqCst);
        return;
    };
    let app2 = app.clone();
    std::thread::spawn(move || {
        let out = Command::new(&paths.python)
            .arg(paths.client("dictate.py"))
            .arg("--record")
            .current_dir(&paths.root)
            .output();
        DICTATING.store(false, Ordering::SeqCst);
        let Ok(out) = out else { return };
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(text) = line.strip_prefix("TEXT ") {
                type_text(text);
                let _ = app2.emit("dictated", text);
            }
        }
    });
}

fn dictation_stop(app: &AppHandle) {
    run_client(app, "dictate.py", &["--stop"]);
}

// ── hotkeys ──────────────────────────────────────────────────────────────────

// Snip is a plain accelerator — a real key survives the KVM fine.
const HK_SNIP: &str = "Control+Alt+D";
// Read and dictate are MODIFIER-ONLY CHORDS, which the global-shortcut plugin
// cannot express. See chords.rs: they are watched with a passive event tap,
// matching the bindings this setup already had muscle memory for.
const CHORD_READ: [&str; 2] = ["ctrl", "cmd"];
const CHORD_DICTATE: [&str; 2] = ["shift", "cmd"];

/// Chord bindings, from prefs, defaulting to the bindings the previous host used.
fn chord_config_from_prefs() -> chords::ChordConfig {
    let p = load_prefs();
    let get = |k: &str, fallback: &[&str]| -> Vec<String> {
        p.get(k)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>())
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| fallback.iter().map(|s| s.to_string()).collect())
    };
    chords::ChordConfig {
        tap: get("chord_read", &CHORD_READ),
        hold: get("chord_dictate", &CHORD_DICTATE),
    }
}

fn pretty_chord(mods: &[String]) -> String {
    let glyph = |m: &str| match m {
        "ctrl" => "⌃",
        "alt" => "⌥",
        "shift" => "⇧",
        "cmd" => "⌘",
        _ => "?",
    };
    // Fixed order so the same chord always renders identically.
    ["ctrl", "alt", "shift", "cmd"]
        .iter()
        .filter(|o| mods.iter().any(|m| m == *o))
        .map(|o| glyph(o))
        .collect()
}

/// Begin capturing. The next chord pressed and released is recorded rather
/// than acted on — the only reliable way to bind a key when a KVM may be
/// rewriting modifiers in transit.
#[tauri::command]
fn record_chord(slot: String) {
    chords::start_recording(&slot);
}

/// Poll for a completed capture; saves it and re-applies the binding live.
#[tauri::command]
fn poll_recorded() -> Option<serde_json::Value> {
    let (slot, mods) = chords::take_recorded()?;
    let key = match slot.as_str() {
        "read" => "chord_read",
        "dictate" => "chord_dictate",
        _ => return None,
    };
    let mut p = load_prefs();
    p[key] = serde_json::json!(mods);
    let _ = std::fs::write(prefs_file(), serde_json::to_string_pretty(&p).unwrap_or_default());
    chords::set_config(chord_config_from_prefs());
    Some(serde_json::json!({ "slot": slot, "label": pretty_chord(&mods) }))
}

#[tauri::command]
fn hotkeys() -> serde_json::Value {
    let c = chord_config_from_prefs();
    serde_json::json!({
        "read": format!("{} (tap)", pretty_chord(&c.tap)),
        "snip": "⌃⌥D",
        "dictate": format!("{} (hold)", pretty_chord(&c.hold))
    })
}

fn register_hotkeys(app: &AppHandle) -> Result<(), String> {
    // Modifier chords first — these are the ones with muscle memory behind them.
    chords::set_config(chord_config_from_prefs());
    let a1 = app.clone();
    let a2 = app.clone();
    let a3 = app.clone();
    chords::watch(
        move || read_selection(a1.clone()),
        move || dictation_start(&a2),
        move || dictation_stop(&a3),
    )?;

    let snip: Shortcut = HK_SNIP.parse().map_err(|_| "bad snip shortcut")?;
    app.global_shortcut()
        .on_shortcuts([snip], move |app, sc, event| {
            // Read and snip fire once on press. Dictation is PUSH TO TALK:
            // record while held, transcribe on release. Toggle semantics were
            // rejected because a start with no matching stop leaves the
            // microphone recording invisibly.
            if event.state == ShortcutState::Pressed && sc == &snip {
                snip_and_read(app.clone());
            }
        })
        .map_err(|e| format!("could not register hotkeys: {e}"))
}

// ── signals ──────────────────────────────────────────────────────────────────

extern "C" fn handle_signal(_sig: libc::c_int) {
    SIGNALLED.store(true, Ordering::Relaxed);
}

/// Tauri's exit hooks only run when the app quits through its own event loop.
/// A signal — Activity Monitor, `kill`, a logout — bypasses them entirely, and
/// the engine would be left holding the port.
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

// ── app ──────────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            engine_status,
            setup_engine,
            read_selection,
            stop_speaking,
            snip_and_read,
            speak_text,
            toggle_playback,
            get_prefs,
            set_prefs,
            list_voices,
            record_chord,
            poll_recorded,
            hotkeys
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(Engine(Mutex::new(None)));

            if is_installed() {
                spawn_engine_and_record(&handle);
            } else if let Some(w) = app.get_webview_window("main") {
                // Nothing to run yet — show the window so the first thing a new
                // user meets is the setup screen, not a silent tray icon.
                let _ = w.show();
                let _ = w.set_focus();
            }
            install_signal_handlers(handle.clone());
            if let Err(e) = register_hotkeys(&handle) {
                eprintln!("{e}");
            }

            let read = MenuItem::with_id(app, "read", "Read selection  (⌃⌘ tap)", true, None::<&str>)?;
            let snip = MenuItem::with_id(app, "snip", "Snip & read", true, Some(HK_SNIP))?;
            let dict = MenuItem::with_id(app, "dict", "Dictate  (⇧⌘ hold)", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?;
            let open = MenuItem::with_id(app, "open", "Settings…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Kokoro Voice", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&read, &dict, &snip, &stop, &open, &quit])?;

            TrayIconBuilder::new()
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
                        stop_engine(app);
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Kokoro Voice")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                stop_engine(app);
            }
        });
}
