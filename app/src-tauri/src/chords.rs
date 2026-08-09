//! Passive macOS modifier gestures used when Deskflow is in the input path.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

/// Marks Kokoro's synthetic text-edit events so the modifier gesture tap does
/// not mistake live transcript insertion for a contaminated physical chord.
const INJECTED_EVENT_MARKER: i64 = 0x4b4f_4b4f_524f;
static SUSPENDED_FOR_RECORDER: AtomicBool = AtomicBool::new(false);

/// The settings window temporarily pauses modifier gestures while it records a
/// replacement shortcut. Without this, pressing Shift+Command in the recorder
/// starts a real dictation session behind the settings window.
pub fn set_recorder_suspended(suspended: bool) {
    SUSPENDED_FOR_RECORDER.store(suspended, Ordering::SeqCst);
    log_event(if suspended {
        "recorder=suspended"
    } else {
        "recorder=resumed"
    });
}

#[derive(Debug, Default)]
struct GestureState {
    tap_armed: bool,
    holding: bool,
    blocked: bool,
    modifiers_down: bool,
}

#[derive(Debug, PartialEq)]
enum Action {
    Read,
    DictateStart,
    DictateStop,
    Cancel,
}

impl GestureState {
    fn flags(&mut self, mods: &[&str]) -> Vec<Action> {
        let mut actions = Vec::new();
        let is =
            |wanted: &[&str]| mods.len() == wanted.len() && wanted.iter().all(|m| mods.contains(m));
        self.modifiers_down = !mods.is_empty();

        if mods.is_empty() {
            if self.holding {
                actions.push(Action::DictateStop);
            }
            if self.tap_armed && !self.blocked {
                actions.push(Action::Read);
            }
            *self = Self::default();
            return actions;
        }

        if is(&["ctrl", "cmd"]) && !self.blocked {
            self.tap_armed = true;
        }
        if is(&["shift", "cmd"]) && !self.blocked && !self.holding {
            self.holding = true;
            actions.push(Action::DictateStart);
        } else if self.holding && !is(&["shift", "cmd"]) {
            self.holding = false;
            actions.push(Action::DictateStop);
        }
        actions
    }

    fn key_down(&mut self, keycode: i64) -> Vec<Action> {
        let mut actions = Vec::new();
        if keycode == 53 {
            *self = Self::default();
            actions.push(Action::Cancel);
            return actions;
        }
        // KVMs may emit modifier KeyDown events in addition to FlagsChanged.
        if self.modifiers_down && !matches!(keycode, 54..=63) {
            self.blocked = true;
            self.tap_armed = false;
            if self.holding {
                self.holding = false;
                actions.push(Action::DictateStop);
            }
        }
        actions
    }
}

fn active_mods(event: &CGEvent) -> Vec<&'static str> {
    let bits = event.get_flags().bits();
    let mut out = Vec::new();
    if bits & 0x0004_0000 != 0 {
        out.push("ctrl");
    }
    if bits & 0x0002_0000 != 0 {
        out.push("shift");
    }
    if bits & 0x0008_0000 != 0 {
        out.push("alt");
    }
    if bits & 0x0010_0000 != 0 {
        out.push("cmd");
    }
    out
}

fn log_event(line: &str) {
    let path = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".config")
        .join("kokoro")
        .join("hotkey.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

pub fn watch<F1, F2, F3, F4>(read: F1, start: F2, stop: F3, cancel: F4) -> Result<(), String>
where
    F1: Fn() + Send + 'static,
    F2: Fn() + Send + 'static,
    F3: Fn() + Send + 'static,
    F4: Fn() + Send + 'static,
{
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let state = Mutex::new(GestureState::default());
        let handler = move |_proxy, kind, event: &CGEvent| -> CallbackResult {
            if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
                == INJECTED_EVENT_MARKER
            {
                return CallbackResult::Keep;
            }
            let Ok(mut state) = state.lock() else {
                return CallbackResult::Keep;
            };
            if SUSPENDED_FOR_RECORDER.load(Ordering::SeqCst) {
                *state = GestureState::default();
                return CallbackResult::Keep;
            }
            let actions = match kind {
                CGEventType::FlagsChanged => {
                    let mods = active_mods(event);
                    log_event(&format!(
                        "flags={} blocked={}",
                        mods.join("+"),
                        state.blocked
                    ));
                    state.flags(&mods)
                }
                CGEventType::KeyDown => {
                    let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                    let was_blocked = state.blocked;
                    let actions = state.key_down(code);
                    if !was_blocked && state.blocked {
                        log_event(&format!("rejected keydown={code}"));
                    }
                    actions
                }
                _ => Vec::new(),
            };
            drop(state);
            for action in actions {
                log_event(&format!("action={action:?}"));
                match action {
                    Action::Read => read(),
                    Action::DictateStart => start(),
                    Action::DictateStop => stop(),
                    Action::Cancel => cancel(),
                }
            }
            CallbackResult::Keep
        };

        let tap = CGEventTap::new(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            vec![CGEventType::FlagsChanged, CGEventType::KeyDown],
            handler,
        );
        let Ok(tap) = tap else {
            let _ = ready_tx.send(Err(
                "modifier watcher unavailable; grant Accessibility".into()
            ));
            return;
        };
        let Ok(source) = tap.mach_port().create_runloop_source(0) else {
            let _ = ready_tx.send(Err("modifier watcher could not attach to run loop".into()));
            return;
        };
        let run_loop = CFRunLoop::get_current();
        unsafe { run_loop.add_source(&source, kCFRunLoopCommonModes) };
        tap.enable();
        let _ = ready_tx.send(Ok(()));
        CFRunLoop::run_current();
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "modifier watcher startup timed out".to_string())?
}

fn post_key(
    source: CGEventSource,
    keycode: u16,
    down: bool,
    text: Option<&str>,
) -> Result<(), String> {
    let event = CGEvent::new_keyboard_event(source, keycode, down)
        .map_err(|_| "could not create keyboard event".to_string())?;
    event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, INJECTED_EVENT_MARKER);
    event.set_flags(CGEventFlags::empty());
    if let Some(text) = text {
        event.set_string(text);
    }
    event.post(CGEventTapLocation::HID);
    Ok(())
}

/// Replace the mutable suffix of text in the currently focused control.
pub fn replace_focused_text(delete_chars: usize, insert: &str) -> Result<(), String> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "could not create keyboard event source".to_string())?;
    for _ in 0..delete_chars {
        post_key(source.clone(), 51, true, None)?;
        post_key(source.clone(), 51, false, None)?;
    }
    // Quartz keyboard events have a small practical Unicode payload. Chunk on
    // character boundaries so emoji and non-ASCII dictation stay intact.
    for chunk in insert.chars().collect::<Vec<_>>().chunks(16) {
        let text: String = chunk.iter().collect();
        post_key(source.clone(), 0, true, Some(&text))?;
        post_key(source.clone(), 0, false, None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_fires_only_on_clean_release() {
        let mut s = GestureState::default();
        assert!(s.flags(&["ctrl", "cmd"]).is_empty());
        assert_eq!(s.flags(&[]), vec![Action::Read]);
    }

    #[test]
    fn ordinary_shortcut_blocks_read() {
        let mut s = GestureState::default();
        s.flags(&["ctrl", "cmd"]);
        s.key_down(8);
        assert!(s.flags(&[]).is_empty());
    }

    #[test]
    fn modifier_keydown_does_not_block() {
        let mut s = GestureState::default();
        s.flags(&["ctrl", "cmd"]);
        s.key_down(55);
        assert_eq!(s.flags(&[]), vec![Action::Read]);
    }

    #[test]
    fn dictation_has_paired_start_and_stop() {
        let mut s = GestureState::default();
        assert_eq!(s.flags(&["shift", "cmd"]), vec![Action::DictateStart]);
        assert_eq!(s.flags(&[]), vec![Action::DictateStop]);
    }

    #[test]
    fn non_modifier_during_dictation_forces_stop() {
        let mut s = GestureState::default();
        s.flags(&["shift", "cmd"]);
        assert_eq!(s.key_down(8), vec![Action::DictateStop]);
        assert!(s.flags(&[]).is_empty());
    }

    #[test]
    fn escape_is_a_cancel_action() {
        let mut state = GestureState::default();
        assert_eq!(state.key_down(53), vec![Action::Cancel]);
    }
}
