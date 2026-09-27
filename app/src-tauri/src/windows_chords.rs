//! Windows modifier-only gesture adapter; Tauri still owns complete shortcuts.
//!
//! The hook never consumes input or logs keys. It only queues action transitions.
//! A longer shortcut must cancel a modifier gesture, and suspended/stale state
//! must never start dictation. No microphone or engine work runs in the hook.

use crate::hotkeys::{self, Config, Slot};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const DICTATE_GRACE_MS: u64 = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Read,
    Snip,
    DictateStart,
    DictateStop,
    DictateCancel,
}

#[derive(Default)]
struct Gesture {
    bindings: Vec<(Slot, u8)>,
    armed: Option<(Slot, u8, u64)>,
    modifiers: u8,
    recording: bool,
    blocked: bool,
    suspended: bool,
}

impl Gesture {
    fn reset(&mut self, suspended: bool) -> Vec<Action> {
        let actions = if self.recording {
            vec![Action::DictateCancel]
        } else {
            vec![]
        };
        self.recording = false;
        self.armed = None;
        self.blocked = self.modifiers != 0;
        self.suspended = suspended;
        actions
    }

    fn input(&mut self, modifiers: u8, other_key: bool, now: u64) -> Vec<Action> {
        self.modifiers = modifiers;
        if self.suspended || other_key {
            let actions = self.reset(self.suspended);
            self.blocked = modifiers != 0;
            return actions;
        }
        if self.blocked {
            if modifiers == 0 {
                self.blocked = false;
            }
            return vec![];
        }
        if let Some((slot, mask, _)) = self.armed {
            if modifiers & !mask != 0 {
                let actions = self.reset(false);
                self.blocked = true;
                return actions;
            }
            if slot == Slot::Dictate && modifiers != mask {
                let actions = if self.recording {
                    vec![Action::DictateStop]
                } else {
                    vec![]
                };
                self.recording = false;
                self.armed = None;
                self.blocked = modifiers != 0;
                return actions;
            }
            if modifiers == 0 {
                self.armed = None;
                return match slot {
                    Slot::Read => vec![Action::Read],
                    Slot::Snip => vec![Action::Snip],
                    Slot::Dictate => vec![],
                };
            }
        } else if let Some(&(slot, mask)) =
            self.bindings.iter().find(|(_, mask)| *mask == modifiers)
        {
            self.armed = Some((slot, mask, now));
        }
        self.tick(now)
    }

    fn tick(&mut self, now: u64) -> Vec<Action> {
        if self.suspended || self.blocked || self.recording {
            return vec![];
        }
        if let Some((Slot::Dictate, mask, started)) = self.armed {
            if self.modifiers == mask && now.saturating_sub(started) >= DICTATE_GRACE_MS {
                self.recording = true;
                return vec![Action::DictateStart];
            }
        }
        vec![]
    }
}

struct Controller {
    gesture: Gesture,
    // Only held-state bits are retained; no input history or typed text.
    keys: [bool; 256],
    events: Option<mpsc::Sender<Action>>,
    clock: Instant,
}

static CONTROLLER: OnceLock<Mutex<Controller>> = OnceLock::new();
static STARTED: OnceLock<Result<(), String>> = OnceLock::new();

fn controller() -> &'static Mutex<Controller> {
    CONTROLLER.get_or_init(|| {
        Mutex::new(Controller {
            gesture: Gesture::default(),
            keys: [false; 256],
            events: None,
            clock: Instant::now(),
        })
    })
}

fn modifier(vk: usize) -> u8 {
    match vk {
        0x11 | 0xA2 | 0xA3 => 1, // Control
        0x12 | 0xA4 | 0xA5 => 2, // Alt
        0x10 | 0xA0 | 0xA1 => 4, // Shift
        0x5B | 0x5C => 8,        // Windows
        _ => 0,
    }
}

fn emit(state: &Controller, actions: Vec<Action>) {
    if let Some(sender) = &state.events {
        for action in actions {
            let _ = sender.send(action);
        }
    }
}

pub fn suspend(value: bool) {
    if let Ok(mut state) = controller().lock() {
        let actions = state.gesture.reset(value);
        emit(&state, actions);
    }
}

pub fn configure(config: &Config) -> Result<(), String> {
    let mut bindings = Vec::new();
    for (slot, accelerator) in [
        (Slot::Read, &config.read),
        (Slot::Dictate, &config.dictate),
        (Slot::Snip, &config.snip),
    ] {
        if !hotkeys::modifier_only(accelerator) {
            continue;
        }
        hotkeys::classify_capture(slot, accelerator, true)?;
        let mask = accelerator.split('+').fold(0, |mask, part| {
            mask | match part {
                "Control" => 1,
                "Alt" => 2,
                "Shift" => 4,
                "Command" => 8,
                _ => 0,
            }
        });
        if bindings.iter().any(|&(_, existing)| existing == mask) {
            return Err("Read, Dictate, and Snip must use different shortcuts".into());
        }
        bindings.push((slot, mask));
    }
    let mut state = controller()
        .lock()
        .map_err(|_| "Windows gesture controller is unavailable")?;
    let actions = state.gesture.reset(true);
    emit(&state, actions);
    state.gesture.bindings = bindings;
    Ok(())
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let message = wparam.0 as u32;
        if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP) {
            let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let vk = event.vkCode as usize;
            if vk < 256 {
                if let Ok(mut state) = controller().lock() {
                    let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
                    state.keys[vk] = down;
                    let modifiers = state
                        .keys
                        .iter()
                        .enumerate()
                        .filter(|(_, held)| **held)
                        .fold(0, |mask, (key, _)| mask | modifier(key));
                    let other_held = state
                        .keys
                        .iter()
                        .enumerate()
                        .any(|(key, held)| *held && modifier(key) == 0);
                    let now = state.clock.elapsed().as_millis() as u64;
                    let actions = state.gesture.input(modifiers, other_held, now);
                    emit(&state, actions);
                }
            }
        }
    }
    // Every original key event continues to Windows and other applications.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

pub fn start(callback: impl Fn(Action) + Send + 'static) -> Result<(), String> {
    STARTED
        .get_or_init(|| {
            let (events, receiver) = mpsc::channel();
            controller()
                .lock()
                .map_err(|_| "Windows gesture controller is unavailable")?
                .events = Some(events);
            let (ready, result) = mpsc::sync_channel(1);
            std::thread::spawn(move || unsafe {
                let module = match GetModuleHandleW(None) {
                    Ok(module) => module,
                    Err(error) => {
                        let _ = ready.send(Err(error.to_string()));
                        return;
                    }
                };
                let handle = match SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(hook),
                    Some(HINSTANCE(module.0)),
                    0,
                ) {
                    Ok(handle) => handle,
                    Err(error) => {
                        let _ = ready.send(Err(error.to_string()));
                        return;
                    }
                };
                let timer = SetTimer(None, 0, 15, None);
                if timer == 0 {
                    let _ = UnhookWindowsHookEx(handle);
                    let _ = ready.send(Err("Could not start the Windows gesture timer".into()));
                    return;
                }
                let _ = ready.send(Ok(()));
                let mut message = MSG::default();
                while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                    if message.message == WM_TIMER {
                        if let Ok(mut state) = controller().lock() {
                            let now = state.clock.elapsed().as_millis() as u64;
                            let actions = state.gesture.tick(now);
                            emit(&state, actions);
                        }
                    }
                }
                let _ = KillTimer(None, timer);
                let _ = UnhookWindowsHookEx(handle);
            });
            result
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "Windows gesture hook did not start".to_string())??;
            std::thread::spawn(move || {
                for action in receiver {
                    callback(action);
                }
            });
            Ok(())
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn gestures() -> Gesture {
        Gesture {
            bindings: vec![(Slot::Read, 3), (Slot::Dictate, 5), (Slot::Snip, 6)],
            ..Default::default()
        }
    }
    #[test]
    fn read_waits_for_all_modifiers_to_be_released() {
        let mut g = gestures();
        assert!(g.input(3, false, 0).is_empty());
        assert!(g.input(1, false, 20).is_empty());
        assert_eq!(g.input(0, false, 30), vec![Action::Read]);
        assert!(g.input(0, false, 40).is_empty());
    }
    #[test]
    fn a_longer_shortcut_never_reads_or_starts_dictation() {
        for mask in [3, 5] {
            let mut g = gestures();
            g.input(mask, false, 0);
            assert!(g.input(mask, true, 100).is_empty());
            assert!(g.tick(300).is_empty());
            assert!(g.input(0, false, 350).is_empty());
        }
    }
    #[test]
    fn hold_starts_once_and_first_release_stops_once() {
        let mut g = gestures();
        g.input(5, false, 0);
        assert!(g.tick(179).is_empty());
        assert_eq!(g.tick(180), vec![Action::DictateStart]);
        assert!(g.tick(250).is_empty());
        assert_eq!(g.input(1, false, 300), vec![Action::DictateStop]);
        assert!(g.input(0, false, 310).is_empty());
    }
    #[test]
    fn quick_release_cannot_leave_a_stale_start() {
        let mut g = gestures();
        g.input(5, false, 0);
        g.input(0, false, 30);
        assert!(g.tick(500).is_empty());
    }
    #[test]
    fn recording_is_cancelled_when_a_letter_is_pressed() {
        let mut g = gestures();
        g.input(5, false, 0);
        g.tick(200);
        assert_eq!(g.input(5, true, 300), vec![Action::DictateCancel]);
        assert!(g.input(0, false, 400).is_empty());
    }
    #[test]
    fn extra_modifier_cannot_dispatch_a_subset_on_release() {
        let mut g = gestures();
        g.input(3, false, 0);
        g.input(7, false, 20);
        g.input(3, false, 40);
        assert!(g.input(0, false, 60).is_empty());
    }
    #[test]
    fn recorder_suspension_cancels_and_requires_release() {
        let mut g = gestures();
        g.input(5, false, 0);
        g.tick(200);
        assert_eq!(g.reset(true), vec![Action::DictateCancel]);
        assert!(g.tick(400).is_empty());
        g.reset(false);
        assert!(g.input(5, false, 500).is_empty());
        assert!(g.tick(800).is_empty());
        g.input(0, false, 900);
        g.input(5, false, 1000);
        assert_eq!(g.tick(1200), vec![Action::DictateStart]);
    }
}
