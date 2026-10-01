//! Owns accessory playback commands and Now Playing state for HereWord speech.
//! Never captures hardware keys or controls other players; idle speech releases
//! the system media slot, and Candidate must remain passive beside Stable.

use std::sync::OnceLock;
use tauri::{AppHandle, Manager};

use crate::{playback::PlaybackManager, runtime::structured_log};

static APP: OnceLock<AppHandle> = OnceLock::new();

unsafe extern "C" {
    fn hereword_media_controls_init(handler: extern "C" fn(i32) -> bool);
    fn hereword_media_controls_update(state: i32);
}

fn handle_command(manager: &PlaybackManager, command: i32) -> bool {
    match command {
        0 => manager.state() == "playing" || manager.resume(),
        1 => manager.state() == "paused" || manager.pause(),
        2 => matches!(manager.toggle(), "playing" | "paused"),
        3 => manager.stop(),
        _ => false,
    }
}

extern "C" fn remote_command(command: i32) -> bool {
    let Some(app) = APP.get() else { return false };
    let Some(manager) = app.try_state::<PlaybackManager>() else {
        return false;
    };
    let handled = handle_command(&manager, command);
    structured_log(
        "media-control-command",
        serde_json::json!({
            "command": command, "handled": handled, "state": manager.state()
        }),
    );
    handled
}

fn system_state(state: &str) -> i32 {
    match state {
        "playing" => 1,
        "paused" => 2,
        _ => 0,
    }
}

pub(crate) fn install(app: &AppHandle) {
    if !crate::variant::INPUT_CONTROLLER_ENABLED || APP.set(app.clone()).is_err() {
        return;
    }
    // Setup runs on AppKit's main thread. Keep native command targets alive for
    // the app lifetime, while publishing metadata only during owned speech.
    unsafe { hereword_media_controls_init(remote_command) };
    let app = app.clone();
    std::thread::spawn(move || {
        let mut previous = -1;
        while !crate::runtime::QUITTING.load(std::sync::atomic::Ordering::SeqCst) {
            let state = app
                .try_state::<PlaybackManager>()
                .map(|manager| system_state(manager.state()))
                .unwrap_or(0);
            if state != previous {
                let _ = app.run_on_main_thread(move || unsafe {
                    hereword_media_controls_update(state);
                });
                previous = state;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_commands_never_start_speech() {
        let manager = PlaybackManager::default();
        for command in 0..5 {
            assert!(!handle_command(&manager, command));
        }
        assert_eq!(manager.state(), "idle");
        assert_eq!(system_state("starting"), 0);
        assert_eq!(system_state("stopping"), 0);
    }

    #[test]
    fn accessory_pause_and_play_are_idempotent_and_toggle_resumes() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let manager = PlaybackManager::default();
        let generation = manager.begin(child.id());
        manager.mark_playing(generation);
        assert!(handle_command(&manager, 1));
        assert!(handle_command(&manager, 1));
        assert_eq!(manager.state(), "paused");
        assert_eq!(system_state(manager.state()), 2);
        assert!(handle_command(&manager, 2));
        assert!(handle_command(&manager, 0));
        assert_eq!(manager.state(), "playing");
        assert!(handle_command(&manager, 1));
        assert!(handle_command(&manager, 0));
        assert!(handle_command(&manager, 3));
        let _ = child.kill();
        let _ = child.wait();
        manager.finish(generation);
        assert_eq!(system_state(manager.state()), 0);
    }
}
