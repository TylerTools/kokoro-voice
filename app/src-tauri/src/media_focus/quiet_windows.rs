//! Owns temporary WASAPI session mutes, never master volume or playback position.
//! Excludes HereWord's process tree, preserves existing mutes, and restores only
//! sessions whose mute remained ours. Exclusive/driver-bypassing audio may opt out.

use super::super::QuietAudio;
use std::collections::{HashMap, HashSet};
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
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        let mut child = Command::new(std::env::current_exe().ok()?)
            .args(["--hereword-quiet-worker", &std::process::id().to_string()])
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

pub(crate) fn run_worker(root: u32) {
    use std::io::{Read, Write};
    let Ok(_apartment) = Apartment::new() else {
        return;
    };
    let mut quiet = SessionQuiet {
        muted: HashMap::new(),
        observed: HashSet::new(),
        root,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut byte = [0];
        let _ = std::io::stdin().read(&mut byte);
        let _ = tx.send(());
    });
    if quiet.update().is_err() {
        return;
    }
    println!("READY");
    let _ = std::io::stdout().flush();
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(150)) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = quiet.update();
            }
        }
    }
    // Drop restores only owned session mutes, including on parent pipe EOF.
}
