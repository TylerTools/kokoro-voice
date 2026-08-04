// Kokoro Voice — desktop app.
//
// This is the shell: tray icon, settings UI, and — importantly — it OWNS THE
// ENGINE PROCESS. Starting the app starts the local service; quitting stops it.
// That is what makes this one piece of software rather than a service you
// install separately plus a config file you symlink into someone else's app.
//
// The actual work is still done by the Python clients (speak/dictate/snip),
// which are measured, debugged, and shared with the Windows build. The shell
// invokes them rather than reimplementing their logic in Rust.

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager, State,
};

/// Handle on the engine process so we can stop what we started.
struct Engine(Mutex<Option<Child>>);

/// Resolved once at startup: where the Python runtime and the service live.
#[derive(Clone)]
struct Paths {
    python: std::path::PathBuf,
    root: std::path::PathBuf,
}

impl Paths {
    /// In a bundled .app the engine ships inside Resources. In development it
    /// sits in the repo above us. Try the bundle first so a shipped build never
    /// accidentally picks up a developer checkout.
    fn resolve(app: &tauri::AppHandle) -> Option<Self> {
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(res) = app.path().resource_dir() {
            candidates.push(res.join("engine"));
        }
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd.clone());
            if let Ok(up) = cwd.join("..").canonicalize() {
                candidates.push(up);
            }
            if let Ok(up2) = cwd.join("../..").canonicalize() {
                candidates.push(up2);
            }
        }
        for root in candidates {
            let python = if cfg!(windows) {
                root.join(".venv/Scripts/python.exe")
            } else {
                root.join(".venv/bin/python")
            };
            if python.exists() && root.join("server.py").exists() {
                return Some(Paths { python, root });
            }
        }
        None
    }

    fn client(&self, name: &str) -> std::path::PathBuf {
        self.root.join("client").join(name)
    }
}

fn port() -> String {
    std::env::var("KOKORO_PORT").unwrap_or_else(|_| "8123".into())
}

/// Start the engine as a child process.
///
/// HF_HUB_OFFLINE is set here rather than left to chance: without it the model
/// hub is contacted on every load to resolve "latest", which breaks the offline
/// guarantee and lets an upstream change swap the weights silently.
fn start_engine(paths: &Paths) -> Result<Child, String> {
    Command::new(&paths.python)
        .args([
            "-m",
            "uvicorn",
            "server:app",
            // Loopback only. Never 0.0.0.0 — that would publish the engine to
            // every network the machine joins.
            "--host",
            "127.0.0.1",
            "--port",
            &port(),
        ])
        .current_dir(&paths.root)
        .env("HF_HUB_OFFLINE", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start the engine: {e}"))
}

fn stop_engine(app: &tauri::AppHandle) {
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

/// Where we record the engine PID we spawned.
fn pidfile() -> std::path::PathBuf {
    let base = dirs_config().join("kokoro");
    let _ = std::fs::create_dir_all(&base);
    base.join("engine.pid")
}

fn dirs_config() -> std::path::PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".config")
    } else {
        std::env::temp_dir()
    }
}

/// Kill an engine left behind by a previous run.
///
/// Necessary because a SIGKILL/SIGTERM to the app, or a crash, never runs our
/// cleanup — verified: killing the app left the engine alive and still holding
/// the port, which would make the next launch fail to bind. A recorded PID is
/// the only way to tell OUR orphan apart from something else on that port.
fn reap_orphan() {
    let path = pidfile();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(pid) = text.trim().parse::<i32>() else {
        let _ = std::fs::remove_file(&path);
        return;
    };
    // Confirm it is actually our engine before signalling anything: PIDs are
    // recycled, and killing a stranger's process would be far worse than
    // leaving a stale file behind.
    let looks_like_ours = Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|o| {
            let c = String::from_utf8_lossy(&o.stdout);
            c.contains("uvicorn") && c.contains("server:app")
        })
        .unwrap_or(false);
    if looks_like_ours {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    let _ = std::fs::remove_file(&path);
}

/// Stop the engine on SIGTERM/SIGINT.
///
/// Tauri's exit hooks only run when the app quits through its own event loop.
/// A signal — Activity Monitor, `kill`, a logout — bypasses them entirely.
fn install_signal_handlers(app: tauri::AppHandle) {
    unsafe {
        libc::signal(libc::SIGTERM, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, handle_signal as *const () as libc::sighandler_t);
    }
    // The handler itself must stay async-signal-safe, so it only flips a flag;
    // this thread does the actual teardown.
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if SIGNALLED.load(std::sync::atomic::Ordering::Relaxed) {
            stop_engine(&app);
            std::process::exit(0);
        }
    });
}

static SIGNALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn handle_signal(_sig: libc::c_int) {
    SIGNALLED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Ask the engine how it is doing. Returns the raw /health payload, or a
/// synthetic status the UI can render while things are still coming up.
#[tauri::command]
fn engine_status() -> serde_json::Value {
    let url = format!("http://127.0.0.1:{}/health", port());
    let out = Command::new("curl")
        .args(["-fsS", "-m", "3", &url])
        .output();
    match out {
        Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|_| serde_json::json!({ "status": "starting" })),
        _ => serde_json::json!({ "status": "down" }),
    }
}

/// Run one of the Python clients. Fire-and-forget: they own their own UX
/// (crosshair, notifications, audio) and the shell must not block on them.
fn run_client(paths: &Paths, script: &str, args: &[&str]) {
    let _ = Command::new(&paths.python)
        .arg(paths.client(script))
        .args(args)
        .current_dir(&paths.root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[tauri::command]
fn read_selection(paths: State<'_, Paths>) {
    run_client(&paths, "speak.py", &["--selection"]);
}

#[tauri::command]
fn stop_speaking(paths: State<'_, Paths>) {
    run_client(&paths, "speak.py", &["--stop"]);
}

#[tauri::command]
fn snip_and_read(paths: State<'_, Paths>) {
    run_client(&paths, "snip.py", &["--speak"]);
}

/// Used by the settings window to preview a voice.
#[tauri::command]
fn speak_text(paths: State<'_, Paths>, text: String) {
    run_client(&paths, "speak.py", &["--text", &text]);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            engine_status,
            read_selection,
            stop_speaking,
            snip_and_read,
            speak_text
        ])
        .setup(|app| {
            let paths = Paths::resolve(&app.handle())
                .ok_or("could not locate the engine (.venv + server.py)")?;
            app.manage(paths.clone());

            // Clear any engine left behind by a previous run BEFORE spawning,
            // or the new one cannot bind the port.
            reap_orphan();

            let child = start_engine(&paths).ok();
            if let Some(c) = child.as_ref() {
                let _ = std::fs::write(pidfile(), c.id().to_string());
            }
            app.manage(Engine(Mutex::new(child)));
            install_signal_handlers(app.handle().clone());

            // The window is a settings panel, not the app itself — this lives
            // in the menu bar the way a utility should.
            let read = MenuItem::with_id(app, "read", "Read selection", true, None::<&str>)?;
            let snip = MenuItem::with_id(app, "snip", "Snip & read", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?;
            let open = MenuItem::with_id(app, "open", "Settings…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Kokoro Voice", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&read, &snip, &stop, &open, &quit])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| {
                    let paths = app.state::<Paths>();
                    match event.id().as_ref() {
                        "read" => run_client(&paths, "speak.py", &["--selection"]),
                        "snip" => run_client(&paths, "snip.py", &["--speak"]),
                        "stop" => run_client(&paths, "speak.py", &["--stop"]),
                        "open" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "quit" => {
                            // Stop what we started. A leaked engine still
                            // holding the port is exactly the failure that
                            // makes people think the app "didn't really quit".
                            stop_engine(app);
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the settings window hides it rather than quitting: the
            // app's real home is the tray.
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
