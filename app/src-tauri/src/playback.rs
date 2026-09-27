//! Desktop-owned playback session state and process control.
//!
//! The Tauri host is the source of truth for the supported app path. Python
//! still synthesizes and plays audio, but it no longer needs a second Python
//! process merely to discover, pause, resume, or stop the active app session.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Starting,
    Playing,
    Paused,
    Stopping,
}

impl State {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Playing => "playing",
            Self::Paused => "paused",
            Self::Stopping => "stopping",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Session {
    generation: u64,
    pid: u32,
    state: State,
}

pub(crate) struct PlaybackManager {
    next_generation: AtomicU64,
    session: Mutex<Option<Session>>,
}

impl Default for PlaybackManager {
    fn default() -> Self {
        Self {
            next_generation: AtomicU64::new(1),
            session: Mutex::new(None),
        }
    }
}

impl PlaybackManager {
    pub(crate) fn begin(&self, pid: u32) -> u64 {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut session) = self.session.lock() {
            *session = Some(Session {
                generation,
                pid,
                state: State::Starting,
            });
        }
        generation
    }

    pub(crate) fn mark_playing(&self, generation: u64) {
        if let Ok(mut session) = self.session.lock() {
            if let Some(active) = session
                .as_mut()
                .filter(|active| active.generation == generation)
            {
                active.state = State::Playing;
            }
        }
    }

    /// Clear only the session that actually exited. A replaced process must not
    /// hide or reset the newer playback session when its waiter finishes later.
    pub(crate) fn finish(&self, generation: u64) -> bool {
        let Ok(mut session) = self.session.lock() else {
            return false;
        };
        if session
            .as_ref()
            .is_some_and(|active| active.generation == generation)
        {
            *session = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn is_current(&self, generation: u64) -> bool {
        self.session
            .lock()
            .ok()
            .and_then(|session| session.map(|active| active.generation == generation))
            .unwrap_or(false)
    }

    pub(crate) fn state(&self) -> &'static str {
        self.session
            .lock()
            .ok()
            .and_then(|session| session.map(|active| active.state.as_str()))
            .unwrap_or("idle")
    }

    pub(crate) fn pause(&self) -> bool {
        self.signal_transition(State::Playing, State::Paused, Signal::Pause)
    }

    pub(crate) fn resume(&self) -> bool {
        self.signal_transition(State::Paused, State::Playing, Signal::Resume)
    }

    pub(crate) fn toggle(&self) -> &'static str {
        match self.state() {
            "playing" if self.pause() => "paused",
            "paused" if self.resume() => "playing",
            state => state,
        }
    }

    pub(crate) fn stop(&self) -> bool {
        let Ok(mut session) = self.session.lock() else {
            return false;
        };
        let Some(active) = session.as_mut() else {
            return false;
        };
        if active.state == State::Paused {
            let _ = send_signal(active.pid, Signal::Resume);
        }
        active.state = State::Stopping;
        send_signal(active.pid, Signal::Stop)
    }

    fn signal_transition(&self, from: State, to: State, signal: Signal) -> bool {
        let Ok(mut session) = self.session.lock() else {
            return false;
        };
        let Some(active) = session.as_mut().filter(|active| active.state == from) else {
            return false;
        };
        if send_signal(active.pid, signal) {
            active.state = to;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy)]
enum Signal {
    Pause,
    Resume,
    Stop,
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: Signal) -> bool {
    let raw = match signal {
        Signal::Pause => libc::SIGSTOP,
        Signal::Resume => libc::SIGCONT,
        Signal::Stop => libc::SIGTERM,
    };
    unsafe { libc::kill(pid as libc::pid_t, raw) == 0 }
}

#[cfg(windows)]
fn send_signal(pid: u32, signal: Signal) -> bool {
    if !matches!(signal, Signal::Stop) {
        return false;
    }
    std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(any(unix, windows)))]
fn send_signal(_pid: u32, _signal: Signal) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaced_session_cannot_clear_new_owner() {
        let manager = PlaybackManager::default();
        let old = manager.begin(101);
        let current = manager.begin(202);
        assert!(!manager.finish(old));
        assert!(manager.is_current(current));
        assert!(manager.finish(current));
        assert_eq!(manager.state(), "idle");
    }

    #[test]
    fn session_state_is_explicit() {
        let manager = PlaybackManager::default();
        let generation = manager.begin(101);
        assert_eq!(manager.state(), "starting");
        manager.mark_playing(generation);
        assert_eq!(manager.state(), "playing");
    }
}
