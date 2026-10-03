//! Owns temporary WASAPI session mute or volume reduction, never master volume.
//! Excludes HereWord's process tree and restores only sessions still at our value.
//! Exclusive/driver-bypassing audio may opt out.

use super::super::QuietAudio;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::{
    eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
    MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

struct Apartment;
impl Apartment {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

fn process_tree() -> windows::core::Result<HashMap<u32, u32>> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut result = HashMap::new();
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                result.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
        Ok(result)
    }
}
fn owned(mut pid: u32, parents: &HashMap<u32, u32>, root: u32) -> bool {
    for _ in 0..64 {
        if pid == root || pid == std::process::id() {
            return true;
        }
        if pid == 0 {
            return false;
        }
        let Some(parent) = parents.get(&pid) else {
            return true;
        };
        if *parent == pid {
            return true;
        }
        pid = *parent;
    }
    true
}

fn external_sessions(root: u32) -> windows::core::Result<Vec<(String, ISimpleAudioVolume)>> {
    let parents = process_tree()?;
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let mut result = Vec::new();
        for n in 0..devices.GetCount()? {
            let device = devices.Item(n)?;
            let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
            let sessions = manager.GetSessionEnumerator()?;
            for index in 0..sessions.GetCount()? {
                let session = sessions.GetSession(index)?;
                let control: IAudioSessionControl2 = session.cast()?;
                let pid = control.GetProcessId()?;
                if owned(pid, &parents, root) {
                    continue;
                }
                let identifier = control.GetSessionInstanceIdentifier()?;
                let id = identifier.to_string();
                CoTaskMemFree(Some(identifier.0.cast()));
                if let Ok(id) = id {
                    result.push((id, session.cast()?));
                }
            }
        }
        Ok(result)
    }
}

struct SessionQuiet {
    muted: HashMap<String, ISimpleAudioVolume>,
    observed: HashSet<String>,
    root: u32,
}
// Interfaces are used only inside the coordinator mutex, on initialized MTA
// threads. Audio session interfaces can be accessed across MTA threads.
impl SessionQuiet {
    fn update(&mut self) -> windows::core::Result<()> {
        let sessions = external_sessions(self.root)?;
        self.muted
            .retain(|_, volume| unsafe { volume.GetMute().is_ok_and(|muted| muted.as_bool()) });
        for (id, volume) in sessions {
            // A manual unmute is an override for this entire interruption.
            if !self.observed.insert(id.clone()) {
                continue;
            }
            unsafe {
                if !volume.GetMute()?.as_bool()
                    && volume.SetMute(true, &windows::core::GUID::zeroed()).is_ok()
                {
                    self.muted.insert(id, volume);
                }
            }
        }
        Ok(())
    }
}
impl Drop for SessionQuiet {
    fn drop(&mut self) {
        if let Ok(_apartment) = Apartment::new() {
            for (_, volume) in self.muted.drain() {
                unsafe {
                    if volume.GetMute().is_ok_and(|muted| muted.as_bool()) {
                        let _ = volume.SetMute(false, &windows::core::GUID::zeroed());
                    }
                }
            }
        }
    }
}

pub(super) struct Quiet {
    child: std::process::Child,
}
impl Quiet {
    pub(super) fn start() -> Option<Self> {
        Self::start_worker("--hereword-quiet-worker", 0.80)
    }
    pub(super) fn start_duck(level: f64) -> Option<Self> {
        Self::start_worker("--hereword-duck-worker", level)
    }
    fn start_worker(argument: &str, level: f64) -> Option<Self> {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        let mut child = Command::new(std::env::current_exe().ok()?)
            .args([
                argument,
                &std::process::id().to_string(),
                &level.to_string(),
            ])
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
        let quiet = Self { child };
        match rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(line) if line.trim() == "READY" => Some(quiet),
            _ => None, // Dropping closes stdin; the worker restores before exit.
        }
    }
}
impl QuietAudio for Quiet {
    fn refresh(&mut self) {
        let _ = self.child.try_wait();
    }
}
impl Drop for Quiet {
    fn drop(&mut self) {
        self.child.stdin.take();
        // The worker owns the mutes and sees EOF even if its parent crashes.
        // Never terminate it while it is restoring another app's audio.
    }
}

struct DuckSession {
    volume: ISimpleAudioVolume,
    original: f32,
    applied: f32,
    started: Instant,
}

struct SessionDuck {
    sessions: HashMap<String, DuckSession>,
    observed: HashSet<String>,
    root: u32,
    level: f32,
}

impl SessionDuck {
    fn update(&mut self) -> windows::core::Result<()> {
        let sessions = external_sessions(self.root)?;
        let now = Instant::now();
        for (id, volume) in sessions {
            if self.sessions.contains_key(&id) {
                unsafe {
                    let actual = volume.GetMasterVolume()?;
                    let applied = self.sessions.get(&id).expect("checked session").applied;
                    if (actual - applied).abs() > 0.03 {
                        // A user or player changed the level; don't restore it later.
                        self.sessions.remove(&id);
                        continue;
                    }
                    let session = self.sessions.get_mut(&id).expect("checked session");
                    let progress = (now - session.started).as_secs_f32() / 0.32;
                    let t = progress.clamp(0.0, 1.0);
                    let smooth = t * t * (3.0 - 2.0 * t);
                    let next = session.original * (1.0 - (1.0 - self.level) * smooth);
                    if (next - session.applied).abs() >= 0.005 {
                        volume.SetMasterVolume(next, &windows::core::GUID::zeroed())?;
                        session.applied = next;
                    }
                }
                continue;
            }
            if !self.observed.insert(id.clone()) {
                continue;
            }
            unsafe {
                if volume.GetMute()?.as_bool() {
                    continue;
                }
                let original = volume.GetMasterVolume()?;
                if original > 0.01 {
                    self.sessions.insert(
                        id,
                        DuckSession {
                            volume,
                            original,
                            applied: original,
                            started: now,
                        },
                    );
                }
            }
        }
        Ok(())
    }

    fn restore(&mut self) {
        let starts: HashMap<_, _> = self
            .sessions
            .iter()
            .map(|(id, session)| (id.clone(), session.applied))
            .collect();
        for step in 1..=8 {
            let t = step as f32 / 8.0;
            let smooth = t * t * (3.0 - 2.0 * t);
            self.sessions.retain(|id, session| unsafe {
                let Ok(actual) = session.volume.GetMasterVolume() else {
                    return false;
                };
                if (actual - session.applied).abs() > 0.03 {
                    return false;
                }
                let start = starts.get(id).copied().unwrap_or(session.applied);
                let next = start + (session.original - start) * smooth;
                if session
                    .volume
                    .SetMasterVolume(next, &windows::core::GUID::zeroed())
                    .is_err()
                {
                    return false;
                }
                session.applied = next;
                true
            });
            std::thread::sleep(Duration::from_millis(40));
        }
    }
}

pub(crate) fn run_worker(root: u32, duck: bool, level: f64) {
    use std::io::{Read, Write};
    let Ok(_apartment) = Apartment::new() else {
        return;
    };
    let mut quiet = if duck {
        None
    } else {
        Some(SessionQuiet {
            muted: HashMap::new(),
            observed: HashSet::new(),
            root,
        })
    };
    let mut ducked = if duck {
        Some(SessionDuck {
            sessions: HashMap::new(),
            observed: HashSet::new(),
            root,
            level: level.clamp(0.40, 0.95) as f32,
        })
    } else {
        None
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut byte = [0];
        let _ = std::io::stdin().read(&mut byte);
        let _ = tx.send(());
    });
    if quiet
        .as_mut()
        .map_or_else(|| ducked.as_mut().unwrap().update(), SessionQuiet::update)
        .is_err()
    {
        return;
    }
    println!("READY");
    let _ = std::io::stdout().flush();
    loop {
        match rx.recv_timeout(Duration::from_millis(if duck { 40 } else { 150 })) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(quiet) = quiet.as_mut() {
                    let _ = quiet.update();
                }
                if let Some(ducked) = ducked.as_mut() {
                    let _ = ducked.update();
                }
            }
        }
    }
    if let Some(ducked) = ducked.as_mut() {
        ducked.restore();
    }
    // Drop restores only owned session mutes, including on parent pipe EOF.
}
