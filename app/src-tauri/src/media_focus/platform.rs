//! Platform adapters for external media only. Commands must match a fresh
//! player/track/state snapshot; an unavailable OS API must leave playback alone.

use super::{MediaPlatform, Snapshot};
pub(super) struct Native;

#[cfg(target_os = "windows")]
#[path = "quiet_windows.rs"]
mod quiet_windows;

#[cfg(target_os = "windows")]
pub(crate) fn run_quiet_worker(root: u32, duck: bool, level: f64) {
    quiet_windows::run_worker(root, duck, level);
}

#[cfg(target_os = "macos")]
mod quiet_macos {
    use super::super::QuietAudio;
    unsafe extern "C" {
        fn hereword_quiet_start(status: *mut i32) -> *mut std::ffi::c_void;
        fn hereword_quiet_refresh(handle: *mut std::ffi::c_void) -> i32;
        fn hereword_quiet_stop(handle: *mut std::ffi::c_void);
    }
    pub(super) struct Quiet(*mut std::ffi::c_void);
    // The native handle is private and only used under MediaFocus's mutex.
    unsafe impl Send for Quiet {}
    impl Quiet {
        pub(super) fn start() -> Option<Self> {
            let mut status = 0;
            let handle = unsafe { hereword_quiet_start(&mut status) };
            if handle.is_null() {
                crate::structured_log(
                    "media-quiet-unavailable",
                    serde_json::json!({"os_status": status}),
                );
                None
            } else {
                Some(Self(handle))
            }
        }
    }
    impl QuietAudio for Quiet {
        fn refresh(&mut self) {
            unsafe {
                hereword_quiet_refresh(self.0);
            }
        }
    }
    impl Drop for Quiet {
        fn drop(&mut self) {
            unsafe {
                hereword_quiet_stop(self.0);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod duck_macos {
    use super::super::QuietAudio;
    use std::io::BufRead;
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    pub(super) struct Duck(Option<Child>);
    impl Duck {
        pub(super) fn start(level: f64) -> Option<Self> {
            let mut child = Command::new("/usr/bin/osascript")
                .args(["-l", "JavaScript", "-e", include_str!("duck_macos.js")])
                .env("HEREWORD_DUCK_LEVEL", format!("{level:.2}"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut line = String::new();
                let _ = std::io::BufReader::new(stdout).read_line(&mut line);
                let _ = tx.send(line);
            });
            let duck = Self(Some(child));
            match rx.recv_timeout(Duration::from_secs(3)) {
                Ok(line) if line.trim().starts_with("READY ") => {
                    let players = line
                        .trim()
                        .strip_prefix("READY ")
                        .and_then(|count| count.parse::<usize>().ok())
                        .unwrap_or(0);
                    crate::structured_log(
                        "media-duck-start",
                        serde_json::json!({"players": players, "level": level}),
                    );
                    if players > 0 { Some(duck) } else { None }
                }
                _ => {
                    crate::structured_log("media-duck-unavailable", serde_json::json!({}));
                    None
                }
            }
        }
    }
    impl QuietAudio for Duck {
        fn refresh(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.try_wait();
            }
        }
    }
    impl Drop for Duck {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                child.stdin.take();
                // Wait for fade-back before another lease can capture a new
                // baseline. Detached restores can overlap the next duck and
                // repeatedly lower music that was already quieted.
                let _ = child.wait();
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn command(action: &str, expected: Option<&Snapshot>) -> Result<serde_json::Value, ()> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", include_str!("macos.js"), action])
        .arg(serde_json::to_string(&expected).map_err(|_| ())?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return Err(()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
        }
    }
    let mut output = String::new();
    child
        .stdout
        .take()
        .ok_or(())?
        .take(8192)
        .read_to_string(&mut output)
        .map_err(|_| ())?;
    serde_json::from_str(&output).map_err(|_| ())
}

#[cfg(target_os = "macos")]
impl MediaPlatform for Native {
    fn quiet(&self) -> Option<Box<dyn super::QuietAudio>> {
        quiet_macos::Quiet::start().map(|quiet| Box::new(quiet) as Box<dyn super::QuietAudio>)
    }
    fn duck(&self, level: f64) -> Option<Box<dyn super::QuietAudio>> {
        duck_macos::Duck::start(level).map(|duck| Box::new(duck) as Box<dyn super::QuietAudio>)
    }
    fn snapshots(&self) -> Result<Vec<Snapshot>, ()> {
        let value = command("get", None)?;
        if value.is_null() {
            return Ok(vec![]);
        }
        Ok(vec![serde_json::from_value(value).map_err(|_| ())?])
    }
    fn set_playing(&self, expected: &Snapshot, playing: bool) -> bool {
        command(if playing { "play" } else { "pause" }, Some(expected))
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
}

#[cfg(target_os = "windows")]
mod windows_adapter {
    use super::*;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    };

    struct Apartment;
    impl Apartment {
        fn new() -> windows::core::Result<Self> {
            unsafe {
                windows::Win32::System::WinRT::RoInitialize(
                    windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
                )?;
            }
            Ok(Self)
        }
    }
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                windows::Win32::System::WinRT::RoUninitialize();
            }
        }
    }
    // Media-control failures must not hang push-to-talk before the mic opens.
    macro_rules! bounded {
        ($operation:expr) => {{
            let operation = $operation?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while operation.Status()?.0 == 0 {
                if std::time::Instant::now() >= deadline {
                    let _ = operation.Cancel();
                    return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                        0x800705B4_u32 as i32,
                    )));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            operation.GetResults()
        }};
    }

    fn snapshot(session: &Session) -> windows::core::Result<Snapshot> {
        let properties = bounded!(session.TryGetMediaPropertiesAsync())?;
        let timeline = session.GetTimelineProperties()?;
        Ok(Snapshot {
            player: session.SourceAppUserModelId()?.to_string(),
            track: format!(
                "{}\n{}\n{}",
                properties.Title()?,
                properties.Artist()?,
                timeline.EndTime()?.Duration
            ),
            playing: session.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing,
        })
    }
    impl MediaPlatform for Native {
        fn quiet(&self) -> Option<Box<dyn super::super::QuietAudio>> {
            super::quiet_windows::Quiet::start()
                .map(|quiet| Box::new(quiet) as Box<dyn super::super::QuietAudio>)
        }
        fn duck(&self, level: f64) -> Option<Box<dyn super::super::QuietAudio>> {
            super::quiet_windows::Quiet::start_duck(level)
                .map(|quiet| Box::new(quiet) as Box<dyn super::super::QuietAudio>)
        }
        fn snapshots(&self) -> Result<Vec<Snapshot>, ()> {
            let run = || -> windows::core::Result<Vec<Snapshot>> {
                let _apartment = Apartment::new()?;
                let sessions = bounded!(Manager::RequestAsync())?.GetSessions()?;
                let mut result = Vec::new();
                for session in sessions {
                    if let Ok(state) = snapshot(&session) {
                        result.push(state)
                    }
                }
                Ok(result)
            };
            run().map_err(|_| ())
        }
        fn set_playing(&self, expected: &Snapshot, playing: bool) -> bool {
            let run = || -> windows::core::Result<bool> {
                let _apartment = Apartment::new()?;
                let sessions = bounded!(Manager::RequestAsync())?.GetSessions()?;
                // Multiple sessions with the same app/track cannot be identified safely.
                let matches: Vec<_> = sessions
                    .into_iter()
                    .filter(|session| snapshot(session).as_ref().ok() == Some(expected))
                    .collect();
                if matches.len() != 1 {
                    return Ok(false);
                }
                let session = &matches[0];
                if playing {
                    bounded!(session.TryPlayAsync())
                } else {
                    bounded!(session.TryPauseAsync())
                }
            };
            run().unwrap_or(false)
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl MediaPlatform for Native {
    fn snapshots(&self) -> Result<Vec<Snapshot>, ()> {
        Ok(vec![])
    }
    fn set_playing(&self, _: &Snapshot, _: bool) -> bool {
        false
    }
}
