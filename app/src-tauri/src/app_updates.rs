//! Owns verified app updates, never speech data or permission grants.
//! macOS installation delegates to the existing transactional release manager;
//! Candidate and builds without a pinned public key cannot install updates.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

const DEFAULT_ENDPOINT: &str =
    "https://github.com/TylerTools/kokoro-voice/releases/latest/download/latest.json";

#[derive(Default)]
pub(crate) struct PendingUpdate {
    update: Mutex<Option<Update>>,
    installing: AtomicBool,
}

#[derive(serde::Serialize)]
pub(crate) struct UpdateStatus {
    configured: bool,
    version: Option<String>,
}

fn configured() -> bool {
    crate::variant::INPUT_CONTROLLER_ENABLED
        && option_env!("HEREWORD_UPDATE_PUBLIC_KEY").is_some_and(|key| !key.trim().is_empty())
}

pub(crate) fn initialize(app: &AppHandle) -> tauri::Result<()> {
    app.manage(PendingUpdate::default());
    if configured() {
        app.plugin(
            tauri_plugin_updater::Builder::new()
                .pubkey(option_env!("HEREWORD_UPDATE_PUBLIC_KEY").unwrap_or_default())
                .build(),
        )?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn check_for_update(app: AppHandle) -> Result<UpdateStatus, String> {
    if !configured() {
        return Ok(UpdateStatus {
            configured: false,
            version: None,
        });
    }
    let endpoint = option_env!("HEREWORD_UPDATE_ENDPOINT").unwrap_or(DEFAULT_ENDPOINT);
    let endpoint = reqwest::Url::parse(endpoint).map_err(|_| "The update address is invalid.")?;
    if endpoint.scheme() != "https" {
        return Err("Updates require a secure connection.".into());
    }
    let builder = app
        .updater_builder()
        .pubkey(option_env!("HEREWORD_UPDATE_PUBLIC_KEY").unwrap_or_default())
        .endpoints(vec![endpoint])
        .map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    let builder = builder.on_before_exit({
        let app = app.clone();
        move || {
            crate::stop_managed_playback(&app);
            crate::stop_managed_dictation(&app);
            crate::runtime::stop_engine(&app);
        }
    });
    let update = builder
        .build()
        .map_err(|error| error.to_string())?
        .check()
        .await
        .map_err(|_| "Couldn't check for updates. Try again later.")?;
    let version = update.as_ref().map(|update| update.version.clone());
    let state = app.state::<PendingUpdate>();
    *state
        .update
        .lock()
        .map_err(|_| "Update state is unavailable.")? = update;
    Ok(UpdateStatus {
        configured: true,
        version,
    })
}

fn speech_busy(app: &AppHandle) -> bool {
    crate::is_dictating(app)
        || app
            .try_state::<crate::playback::PlaybackManager>()
            .is_some_and(|manager| manager.state() != "idle")
}

#[tauri::command]
pub(crate) async fn install_update(app: AppHandle) -> Result<(), String> {
    if !configured() {
        return Err("Updates are unavailable in this build.".into());
    }
    if speech_busy(&app) {
        return Err("Finish reading or dictation before updating.".into());
    }
    let state = app.state::<PendingUpdate>();
    if state.installing.swap(true, Ordering::SeqCst) {
        return Err("An update is already in progress.".into());
    }
    let result = install_pending(&app).await;
    #[cfg(target_os = "macos")]
    if result.is_err() {
        state.installing.store(false, Ordering::SeqCst);
    }
    #[cfg(not(target_os = "macos"))]
    state.installing.store(false, Ordering::SeqCst);
    result
}

async fn install_pending(app: &AppHandle) -> Result<(), String> {
    let update = app
        .state::<PendingUpdate>()
        .update
        .lock()
        .map_err(|_| "Update state is unavailable.")?
        .clone()
        .ok_or("Check for updates first.")?;
    // download() verifies the mandatory updater signature before returning bytes.
    let mut received = 0u64;
    let bytes = update
        .download(
            |chunk, total| {
                received += chunk as u64;
                let _ = app.emit(
                    "update-progress",
                    serde_json::json!({ "received": received, "total": total }),
                );
            },
            || {},
        )
        .await
        .map_err(|_| "Couldn't download and verify the update. Your app has not changed.")?;
    if speech_busy(app) {
        return Err("Finish reading or dictation before updating.".into());
    }
    #[cfg(target_os = "macos")]
    {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            stage_macos_update(&app, &bytes, &update.version)
        })
        .await
        .map_err(|error| error.to_string())??;
    }
    #[cfg(not(target_os = "macos"))]
    {
        update.install(bytes).map_err(|error| error.to_string())?;
        crate::stop_managed_playback(app);
        crate::stop_managed_dictation(app);
        crate::runtime::stop_engine(app);
        app.restart();
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn stage_macos_update(app: &AppHandle, bytes: &[u8], expected_version: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let root = crate::runtime::engine_root();
    let staging = root.join("updates").join(format!(
        "download-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    let result = (|| {
        let gzip = flate2::read::GzDecoder::new(bytes);
        let mut archive = tar::Archive::new(gzip);
        archive
            .unpack(&staging)
            .map_err(|_| "Couldn't unpack the verified update.")?;
        let bundle = staging.join("HereWord.app");
        if !bundle.join("Contents/Info.plist").is_file() {
            return Err("The update does not contain HereWord.".into());
        }
        // Run the bundled helper before the app exits; its interpreter lives
        // outside the replaced bundle and owns readiness checks plus rollback.
        let helper = app
            .path()
            .resource_dir()
            .map_err(|error| error.to_string())?
            .join("release-manager.py");
        let python = crate::runtime::python_path(&root);
        let verified = Command::new(&python)
            .arg(&helper)
            .arg("preflight")
            .arg(&bundle)
            .output()
            .map_err(|error| error.to_string())?;
        if !verified.status.success() {
            return Err("This update is incompatible with the installed app's signing identity. Your app has not changed.".into());
        }
        let metadata: serde_json::Value = serde_json::from_slice(&verified.stdout)
            .map_err(|_| "The update's metadata could not be verified.")?;
        if metadata["artifact"]["version"].as_str() != Some(expected_version) {
            return Err("The downloaded app does not match the offered update version.".into());
        }
        let log_path = crate::runtime::config_dir().join("update-install.log");
        let mut log = std::fs::File::create(log_path).map_err(|error| error.to_string())?;
        writeln!(
            log,
            "Installing verified HereWord update; previous app retained for rollback."
        )
        .map_err(|error| error.to_string())?;
        let mut child = Command::new(python)
            .arg(helper)
            .arg("promote")
            .arg(bundle)
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().map_err(|error| error.to_string())?,
            ))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|error| error.to_string())?;
        let app = app.clone();
        let cleanup = staging.clone();
        std::thread::spawn(move || {
            let failed = child.wait().map_or(true, |status| !status.success());
            let _ = std::fs::remove_dir_all(cleanup);
            app.state::<PendingUpdate>()
                .installing
                .store(false, Ordering::SeqCst);
            if failed {
                let _ = app.emit("update-failed", "The update could not be installed. Your previous app is retained; see update-install.log for details.");
            }
        });
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(staging);
    }
    result
}
