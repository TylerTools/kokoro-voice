//! Authoritative macOS input adapter for Deskflow and local keyboards.
//!
//! Owns one Quartz event tap, modifier-gesture arbitration, complete shortcut
//! dispatch, and synthetic text events. It does not own shortcut defaults or
//! preference decoding (`hotkeys.rs`) or target safety (`text_backend.rs`). A
//! second macOS shortcut adapter would create events that one path sees and the
//! other misses, so all macOS physical input must enter here.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

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
static RECORDER_EPOCH: AtomicU64 = AtomicU64::new(0);
static BINDINGS: OnceLock<RwLock<Bindings>> = OnceLock::new();

const MOD_CONTROL: u8 = 1 << 0;
const MOD_ALT: u8 = 1 << 1;
const MOD_SHIFT: u8 = 1 << 2;
const MOD_COMMAND: u8 = 1 << 3;

/// Grace period used to distinguish the modifier-only Dictate gesture from a
/// complete shortcut with the same prefix, such as Shift+Command+Z for Snip.
/// This is deliberately short enough to keep push-to-talk responsive while
/// giving a normal chorded key press time to arrive and cancel the pending
/// Dictate start.
pub const DICTATE_PREFIX_GRACE_MS: u64 = 180;

/// A complete macOS shortcut as seen by the Quartz event tap. Deskflow events
/// reach this tap even when they do not trigger macOS's registered-hotkey API,
/// so Quartz is the authoritative input controller on macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompleteShortcut {
    modifiers: u8,
    keycode: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Binding {
    Modifiers(u8),
    Complete(CompleteShortcut),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Bindings {
    read: Option<Binding>,
    dictate: Option<Binding>,
    snip: Option<Binding>,
}

/// Replace all macOS bindings as one validated transaction. Modifier-only
/// chords and complete shortcuts share this controller so Deskflow and local
/// keyboard events cannot diverge between two registration paths.
pub fn configure_shortcuts(read: &str, dictate: &str, snip: &str) -> Result<(), String> {
    let bindings = Bindings {
        read: Some(parse_binding(read)?),
        dictate: Some(parse_binding(dictate)?),
        snip: Some(parse_binding(snip)?),
    };
    let shortcuts = [bindings.read, bindings.dictate, bindings.snip];
    for left in 0..shortcuts.len() {
        for right in (left + 1)..shortcuts.len() {
            if shortcuts[left] == shortcuts[right] {
                return Err("Read, Dictate, and Snip must use different shortcuts".into());
            }
        }
    }
    let store = BINDINGS.get_or_init(|| RwLock::new(Bindings::default()));
    *store
        .write()
        .map_err(|_| "shortcut controller state is unavailable".to_string())? = bindings;
    Ok(())
}

fn parse_binding(accelerator: &str) -> Result<Binding, String> {
    let parts: Vec<_> = accelerator.split('+').collect();
    let modifier_only = !parts.is_empty()
        && parts
            .iter()
            .all(|part| matches!(*part, "Control" | "Alt" | "Shift" | "Command"));
    if !modifier_only {
        return parse_complete_shortcut(accelerator).map(Binding::Complete);
    }
    if parts.len() < 2 {
        return Err("modifier-only shortcuts need at least two modifier keys".into());
    }
    let mut modifiers = 0;
    for part in parts {
        let modifier = match part {
            "Control" => MOD_CONTROL,
            "Alt" => MOD_ALT,
            "Shift" => MOD_SHIFT,
            "Command" => MOD_COMMAND,
            _ => unreachable!(),
        };
        if modifiers & modifier != 0 {
            return Err(format!("shortcut repeats a modifier: {accelerator}"));
        }
        modifiers |= modifier;
    }
    Ok(Binding::Modifiers(modifiers))
}

fn parse_complete_shortcut(accelerator: &str) -> Result<CompleteShortcut, String> {
    let mut modifiers = 0;
    let mut keycode = None;
    for part in accelerator.split('+') {
        let modifier = match part {
            "Control" => Some(MOD_CONTROL),
            "Alt" => Some(MOD_ALT),
            "Shift" => Some(MOD_SHIFT),
            "Command" => Some(MOD_COMMAND),
            _ => None,
        };
        if let Some(modifier) = modifier {
            if modifiers & modifier != 0 {
                return Err(format!("shortcut repeats a modifier: {accelerator}"));
            }
            modifiers |= modifier;
        } else if keycode.replace(mac_keycode(part)?).is_some() {
            return Err(format!(
                "shortcut has more than one non-modifier key: {accelerator}"
            ));
        }
    }
    Ok(CompleteShortcut {
        modifiers,
        keycode: keycode.ok_or_else(|| "shortcut needs a non-modifier key".to_string())?,
    })
}

/// Map Web KeyboardEvent.code values to macOS virtual keycodes. The recorder
/// stores physical codes, which is intentional: Deskflow can rewrite the key
/// label while preserving the hardware position.
fn mac_keycode(code: &str) -> Result<i64, String> {
    // Older preferences and Tauri defaults use `R`/`W`, while the web recorder
    // stores `KeyR`/`KeyW`. Normalize both contracts before matching.
    let normalized = if code.len() == 1 && code.as_bytes()[0].is_ascii_alphabetic() {
        Some(format!("Key{}", code.to_ascii_uppercase()))
    } else if code.len() == 1 && code.as_bytes()[0].is_ascii_digit() {
        Some(format!("Digit{code}"))
    } else {
        None
    };
    let code = normalized.as_deref().unwrap_or(code);
    let keycode = match code {
        "KeyA" => 0,
        "KeyS" => 1,
        "KeyD" => 2,
        "KeyF" => 3,
        "KeyH" => 4,
        "KeyG" => 5,
        "KeyZ" => 6,
        "KeyX" => 7,
        "KeyC" => 8,
        "KeyV" => 9,
        "IntlBackslash" => 10,
        "KeyB" => 11,
        "KeyQ" => 12,
        "KeyW" => 13,
        "KeyE" => 14,
        "KeyR" => 15,
        "KeyY" => 16,
        "KeyT" => 17,
        "Digit1" => 18,
        "Digit2" => 19,
        "Digit3" => 20,
        "Digit4" => 21,
        "Digit6" => 22,
        "Digit5" => 23,
        "Equal" => 24,
        "Digit9" => 25,
        "Digit7" => 26,
        "Minus" => 27,
        "Digit8" => 28,
        "Digit0" => 29,
        "BracketRight" => 30,
        "KeyO" => 31,
        "KeyU" => 32,
        "BracketLeft" => 33,
        "KeyI" => 34,
        "KeyP" => 35,
        "Enter" => 36,
        "KeyL" => 37,
        "KeyJ" => 38,
        "Quote" => 39,
        "KeyK" => 40,
        "Semicolon" => 41,
        "Backslash" => 42,
        "Comma" => 43,
        "Slash" => 44,
        "KeyN" => 45,
        "KeyM" => 46,
        "Period" => 47,
        "Tab" => 48,
        "Space" => 49,
        "Backquote" => 50,
        "Backspace" => 51,
        "Escape" => 53,
        "NumpadDecimal" => 65,
        "NumpadMultiply" => 67,
        "NumpadAdd" => 69,
        "NumLock" => 71,
        "NumpadDivide" => 75,
        "NumpadEnter" => 76,
        "NumpadSubtract" => 78,
        "NumpadEqual" => 81,
        "Numpad0" => 82,
        "Numpad1" => 83,
        "Numpad2" => 84,
        "Numpad3" => 85,
        "Numpad4" => 86,
        "Numpad5" => 87,
        "Numpad6" => 88,
        "Numpad7" => 89,
        "Numpad8" => 91,
        "Numpad9" => 92,
        "F5" => 96,
        "F6" => 97,
        "F7" => 98,
        "F3" => 99,
        "F8" => 100,
        "F9" => 101,
        "F11" => 103,
        "F13" => 105,
        "F16" => 106,
        "F14" => 107,
        "F10" => 109,
        "F12" => 111,
        "F15" => 113,
        "Insert" | "Help" => 114,
        "Home" => 115,
        "PageUp" => 116,
        "Delete" => 117,
        "F4" => 118,
        "End" => 119,
        "F2" => 120,
        "PageDown" => 121,
        "F1" => 122,
        "ArrowLeft" => 123,
        "ArrowRight" => 124,
        "ArrowDown" => 125,
        "ArrowUp" => 126,
        _ => return Err(format!("shortcut key is not supported on macOS: {code}")),
    };
    Ok(keycode)
}

/// The settings window temporarily pauses modifier gestures while it records a
/// replacement shortcut. Without this, pressing Shift+Command in the recorder
/// starts a real dictation session behind the settings window.
pub fn set_recorder_suspended(suspended: bool) {
    SUSPENDED_FOR_RECORDER.store(suspended, Ordering::SeqCst);
    RECORDER_EPOCH.fetch_add(1, Ordering::SeqCst);
    log_event(if suspended {
        "recorder=suspended"
    } else {
        "recorder=resumed"
    });
}

#[derive(Debug, Default)]
struct GestureState {
    tap_armed: Option<Action>,
    tap_modifiers: u8,
    dictate_pending: Option<u64>,
    generation: u64,
    holding: bool,
    blocked: bool,
    modifiers_down: bool,
    dictate_chord_down: bool,
    active_complete: Option<(CompleteShortcut, bool)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Read,
    ArmDictate(u64),
    DictateStart,
    DictateStop,
    Snip,
    Cancel,
}

impl GestureState {
    /// Reset one physical gesture without reusing a generation number. A stale
    /// grace-period timer must never be able to start a later gesture.
    fn reset_transient(&mut self) {
        let generation = self.generation;
        *self = Self::default();
        self.generation = generation;
    }

    fn flags(&mut self, mods: &[&str], bindings: Bindings) -> Vec<Action> {
        let mut actions = Vec::new();
        let current_modifier_mask = modifier_mask_from_names(mods);
        if let Some((shortcut, is_dictate)) = self.active_complete {
            if current_modifier_mask != shortcut.modifiers {
                self.active_complete = None;
                if is_dictate {
                    actions.push(Action::DictateStop);
                }
            }
        }
        self.modifiers_down = !mods.is_empty();
        self.dictate_chord_down =
            bindings.dictate == Some(Binding::Modifiers(current_modifier_mask));

        if mods.is_empty() {
            if self.holding {
                actions.push(Action::DictateStop);
            }
            self.dictate_pending = None;
            if let Some(action) = self.tap_armed {
                if !self.blocked {
                    actions.push(action);
                }
            }
            self.reset_transient();
            return actions;
        }

        if self.tap_armed.is_some() && current_modifier_mask & !self.tap_modifiers != 0 {
            self.tap_armed = None;
            self.blocked = true;
        }
        if self.tap_armed.is_none() && !self.blocked {
            let tap_action = if bindings.read == Some(Binding::Modifiers(current_modifier_mask)) {
                Some(Action::Read)
            } else if bindings.snip == Some(Binding::Modifiers(current_modifier_mask)) {
                Some(Action::Snip)
            } else {
                None
            };
            if let Some(action) = tap_action {
                self.tap_armed = Some(action);
                self.tap_modifiers = current_modifier_mask;
            }
        }
        if self.dictate_chord_down
            && !self.blocked
            && !self.holding
            && self.dictate_pending.is_none()
        {
            self.generation = self.generation.wrapping_add(1);
            self.dictate_pending = Some(self.generation);
            actions.push(Action::ArmDictate(self.generation));
        } else if !self.dictate_chord_down {
            self.dictate_pending = None;
            if self.holding {
                self.holding = false;
                actions.push(Action::DictateStop);
            }
        }
        actions
    }

    #[cfg(test)]
    fn fixed_flags(&mut self, mods: &[&str]) -> Vec<Action> {
        self.flags(
            mods,
            Bindings {
                read: Some(Binding::Modifiers(MOD_CONTROL | MOD_COMMAND)),
                dictate: Some(Binding::Modifiers(MOD_SHIFT | MOD_COMMAND)),
                snip: None,
            },
        )
    }

    /// Commit a pending modifier-only Dictate gesture after the prefix grace
    /// period. A full shortcut keydown, modifier release, recorder transition,
    /// or newer gesture invalidates the generation and returns false.
    fn commit_dictation(&mut self, generation: u64) -> bool {
        if self.dictate_pending == Some(generation)
            && self.dictate_chord_down
            && !self.blocked
            && !self.holding
        {
            self.dictate_pending = None;
            self.holding = true;
            true
        } else {
            false
        }
    }

    fn key_down(&mut self, keycode: i64) -> Vec<Action> {
        let mut actions = Vec::new();
        if keycode == 53 {
            self.reset_transient();
            actions.push(Action::Cancel);
            return actions;
        }
        // KVMs may emit modifier KeyDown events in addition to FlagsChanged.
        if self.modifiers_down && !matches!(keycode, 54..=63) {
            self.blocked = true;
            self.tap_armed = None;
            self.dictate_pending = None;
            if self.holding {
                self.holding = false;
                actions.push(Action::DictateStop);
            }
        }
        actions
    }

    fn complete_key_down(
        &mut self,
        bindings: Bindings,
        modifiers: u8,
        keycode: i64,
    ) -> Vec<Action> {
        let pressed = CompleteShortcut { modifiers, keycode };
        if self.active_complete.is_some() {
            Vec::new()
        } else if bindings.read == Some(Binding::Complete(pressed)) {
            self.active_complete = Some((pressed, false));
            vec![Action::Read]
        } else if bindings.snip == Some(Binding::Complete(pressed)) {
            self.active_complete = Some((pressed, false));
            vec![Action::Snip]
        } else if bindings.dictate == Some(Binding::Complete(pressed)) {
            self.active_complete = Some((pressed, true));
            vec![Action::DictateStart]
        } else {
            Vec::new()
        }
    }

    fn complete_key_up(&mut self, keycode: i64) -> Vec<Action> {
        match self.active_complete {
            Some((shortcut, is_dictate)) if shortcut.keycode == keycode => {
                self.active_complete = None;
                if is_dictate {
                    vec![Action::DictateStop]
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
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

fn active_modifier_mask(event: &CGEvent) -> u8 {
    modifier_mask_from_names(&active_mods(event))
}

fn modifier_mask_from_names(modifiers: &[&str]) -> u8 {
    modifiers.iter().fold(0, |mask, modifier| {
        mask | match *modifier {
            "ctrl" => MOD_CONTROL,
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "cmd" => MOD_COMMAND,
            _ => 0,
        }
    })
}

fn log_event(line: &str) {
    let path = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".config")
        .join(crate::variant::CONFIG_DIR_NAME)
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

pub fn watch<F1, F2, F3, F4, F5>(
    read: F1,
    start: F2,
    stop: F3,
    cancel: F4,
    snip: F5,
) -> Result<(), String>
where
    F1: Fn() + Send + 'static,
    F2: Fn() + Send + 'static,
    F3: Fn() + Send + 'static,
    F4: Fn() + Send + 'static,
    F5: Fn() + Send + 'static,
{
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let state = Arc::new(Mutex::new(GestureState::default()));
        let start = Arc::new(Mutex::new(start));
        // The grace timer runs on a worker thread while release/cancel events
        // arrive on the Quartz run-loop thread. Serialize their callbacks so a
        // boundary release cannot execute Stop before Start has established the
        // dictation session.
        let dictation_gate = Arc::new(Mutex::new(()));
        let event_state = Arc::clone(&state);
        let event_start = Arc::clone(&start);
        let event_dictation_gate = Arc::clone(&dictation_gate);
        let handler = move |_proxy, kind, event: &CGEvent| -> CallbackResult {
            if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
                == INJECTED_EVENT_MARKER
            {
                return CallbackResult::Keep;
            }
            let Ok(mut state) = event_state.lock() else {
                return CallbackResult::Keep;
            };
            if SUSPENDED_FOR_RECORDER.load(Ordering::SeqCst) {
                *state = GestureState::default();
                return CallbackResult::Keep;
            }
            let bindings = BINDINGS
                .get()
                .and_then(|store| store.read().ok().map(|bindings| *bindings))
                .unwrap_or_default();
            let actions = match kind {
                CGEventType::FlagsChanged => {
                    let mods = active_mods(event);
                    log_event(&format!(
                        "flags={} blocked={}",
                        mods.join("+"),
                        state.blocked
                    ));
                    state.flags(&mods, bindings)
                }
                CGEventType::KeyDown => {
                    let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                    let was_blocked = state.blocked;
                    let mut actions = state.key_down(code);
                    if !was_blocked && state.blocked {
                        log_event(&format!("rejected keydown={code}"));
                    }
                    actions.extend(state.complete_key_down(
                        bindings,
                        active_modifier_mask(event),
                        code,
                    ));
                    actions
                }
                CGEventType::KeyUp => {
                    let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                    state.complete_key_up(code)
                }
                _ => Vec::new(),
            };
            drop(state);
            for action in actions {
                log_event(&format!("action={action:?}"));
                match action {
                    Action::Read => read(),
                    Action::ArmDictate(generation) => {
                        let timer_state = Arc::clone(&event_state);
                        let timer_start = Arc::clone(&event_start);
                        let timer_dictation_gate = Arc::clone(&event_dictation_gate);
                        let recorder_epoch = RECORDER_EPOCH.load(Ordering::SeqCst);
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(
                                DICTATE_PREFIX_GRACE_MS,
                            ));
                            if SUSPENDED_FOR_RECORDER.load(Ordering::SeqCst)
                                || RECORDER_EPOCH.load(Ordering::SeqCst) != recorder_epoch
                            {
                                return;
                            }
                            let Ok(_dispatch_guard) = timer_dictation_gate.lock() else {
                                return;
                            };
                            let should_start = timer_state
                                .lock()
                                .map(|mut state| state.commit_dictation(generation))
                                .unwrap_or(false);
                            if should_start {
                                log_event("action=DictateStart");
                                if let Ok(start) = timer_start.lock() {
                                    start();
                                }
                            }
                        });
                    }
                    Action::DictateStart => {
                        if let Ok(_dispatch_guard) = event_dictation_gate.lock() {
                            if let Ok(start) = event_start.lock() {
                                start();
                            }
                        }
                    }
                    Action::DictateStop => {
                        if let Ok(_dispatch_guard) = event_dictation_gate.lock() {
                            stop();
                        }
                    }
                    Action::Cancel => {
                        if let Ok(_dispatch_guard) = event_dictation_gate.lock() {
                            cancel();
                        }
                    }
                    Action::Snip => snip(),
                }
            }
            CallbackResult::Keep
        };

        let tap = CGEventTap::new(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            vec![
                CGEventType::FlagsChanged,
                CGEventType::KeyDown,
                CGEventType::KeyUp,
            ],
            handler,
        );
        let Ok(tap) = tap else {
            let _ = ready_tx.send(Err(
                "shortcut watcher unavailable; grant Input Monitoring".into()
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
        assert!(s.fixed_flags(&["ctrl", "cmd"]).is_empty());
        assert_eq!(s.fixed_flags(&[]), vec![Action::Read]);
    }

    #[test]
    fn ordinary_shortcut_blocks_read() {
        let mut s = GestureState::default();
        s.fixed_flags(&["ctrl", "cmd"]);
        s.key_down(8);
        assert!(s.fixed_flags(&[]).is_empty());
    }

    #[test]
    fn modifier_keydown_does_not_block() {
        let mut s = GestureState::default();
        s.fixed_flags(&["ctrl", "cmd"]);
        s.key_down(55);
        assert_eq!(s.fixed_flags(&[]), vec![Action::Read]);
    }

    #[test]
    fn dictation_has_paired_start_and_stop() {
        let mut s = GestureState::default();
        assert_eq!(
            s.fixed_flags(&["shift", "cmd"]),
            vec![Action::ArmDictate(1)]
        );
        assert!(s.commit_dictation(1));
        assert_eq!(s.fixed_flags(&[]), vec![Action::DictateStop]);
    }

    #[test]
    fn full_shortcut_keydown_cancels_pending_dictation_prefix() {
        let mut s = GestureState::default();
        assert_eq!(
            s.fixed_flags(&["shift", "cmd"]),
            vec![Action::ArmDictate(1)]
        );
        assert!(s.key_down(6).is_empty()); // macOS virtual keycode 6 is Z.
        assert!(!s.commit_dictation(1));
        assert!(s.fixed_flags(&[]).is_empty());
    }

    #[test]
    fn deskflow_snip_is_dispatched_by_the_same_tap_that_sees_its_keydown() {
        let mut state = GestureState::default();
        let bindings = Bindings {
            dictate: Some(Binding::Modifiers(MOD_SHIFT | MOD_COMMAND)),
            snip: Some(Binding::Complete(
                parse_complete_shortcut("Shift+Command+KeyZ").unwrap(),
            )),
            ..Bindings::default()
        };
        assert_eq!(
            state.flags(&["shift", "cmd"], bindings),
            vec![Action::ArmDictate(1)]
        );
        assert!(state.key_down(6).is_empty());
        assert_eq!(
            state.complete_key_down(bindings, MOD_SHIFT | MOD_COMMAND, 6),
            vec![Action::Snip]
        );
        assert!(!state.commit_dictation(1));
    }

    #[test]
    fn complete_dictation_starts_once_and_stops_on_key_release() {
        let mut state = GestureState::default();
        let bindings = Bindings {
            dictate: Some(Binding::Complete(
                parse_complete_shortcut("Control+Alt+KeyW").unwrap(),
            )),
            ..Bindings::default()
        };
        assert_eq!(
            state.complete_key_down(bindings, MOD_CONTROL | MOD_ALT, 13),
            vec![Action::DictateStart]
        );
        assert!(state
            .complete_key_down(bindings, MOD_CONTROL | MOD_ALT, 13)
            .is_empty());
        assert_eq!(state.complete_key_up(13), vec![Action::DictateStop]);
        assert!(state.complete_key_up(13).is_empty());
    }

    #[test]
    fn releasing_a_complete_dictation_modifier_also_stops_it() {
        let mut state = GestureState::default();
        let bindings = Bindings {
            dictate: Some(Binding::Complete(
                parse_complete_shortcut("Control+Alt+KeyW").unwrap(),
            )),
            ..Bindings::default()
        };
        assert_eq!(
            state.complete_key_down(bindings, MOD_CONTROL | MOD_ALT, 13),
            vec![Action::DictateStart]
        );
        assert_eq!(state.flags(&["ctrl"], bindings), vec![Action::DictateStop]);
        assert!(state.complete_key_up(13).is_empty());
    }

    #[test]
    fn mac_shortcut_parser_maps_recorded_physical_codes() {
        assert_eq!(
            parse_complete_shortcut("Shift+Command+KeyZ").unwrap(),
            CompleteShortcut {
                modifiers: MOD_SHIFT | MOD_COMMAND,
                keycode: 6,
            }
        );
        assert_eq!(
            parse_complete_shortcut("Control+Alt+KeyD").unwrap(),
            CompleteShortcut {
                modifiers: MOD_CONTROL | MOD_ALT,
                keycode: 2,
            }
        );
        assert_eq!(
            parse_complete_shortcut("Control+Alt+R").unwrap(),
            parse_complete_shortcut("Control+Alt+KeyR").unwrap()
        );
        assert_eq!(
            parse_complete_shortcut("Command+1").unwrap(),
            parse_complete_shortcut("Command+Digit1").unwrap()
        );
        assert!(parse_complete_shortcut("Shift+Command").is_err());
        assert!(parse_complete_shortcut("Command+UnknownKey").is_err());
    }

    #[test]
    fn configurable_modifier_bindings_dispatch_each_action() {
        let bindings = Bindings {
            read: Some(parse_binding("Control+Shift").unwrap()),
            dictate: Some(parse_binding("Control+Command").unwrap()),
            snip: Some(parse_binding("Alt+Shift").unwrap()),
        };

        let mut read = GestureState::default();
        assert!(read.flags(&["ctrl", "shift"], bindings).is_empty());
        assert_eq!(read.flags(&[], bindings), vec![Action::Read]);

        let mut dictate = GestureState::default();
        assert_eq!(
            dictate.flags(&["ctrl", "cmd"], bindings),
            vec![Action::ArmDictate(1)]
        );
        assert!(dictate.commit_dictation(1));
        assert_eq!(dictate.flags(&[], bindings), vec![Action::DictateStop]);

        let mut snip = GestureState::default();
        assert!(snip.flags(&["alt", "shift"], bindings).is_empty());
        assert_eq!(snip.flags(&[], bindings), vec![Action::Snip]);
    }

    #[test]
    fn modifier_bindings_reject_unsafe_or_duplicate_chords() {
        assert!(parse_binding("Control").is_err());
        assert!(parse_binding("Control+Control").is_err());
        assert!(configure_shortcuts("Control+Shift", "Shift+Control", "Alt+Command").is_err());
    }

    #[test]
    fn extra_modifier_cancels_a_tap_gesture() {
        let bindings = Bindings {
            read: Some(parse_binding("Control+Shift").unwrap()),
            ..Bindings::default()
        };
        let mut state = GestureState::default();
        assert!(state.flags(&["ctrl", "shift"], bindings).is_empty());
        assert!(state.flags(&["ctrl", "shift", "alt"], bindings).is_empty());
        assert!(state.flags(&[], bindings).is_empty());
    }

    #[test]
    fn non_modifier_after_dictation_started_forces_stop() {
        let mut s = GestureState::default();
        s.fixed_flags(&["shift", "cmd"]);
        assert!(s.commit_dictation(1));
        assert_eq!(s.key_down(8), vec![Action::DictateStop]);
        assert!(s.fixed_flags(&[]).is_empty());
    }

    #[test]
    fn stale_prefix_timer_cannot_start_a_later_gesture() {
        let mut s = GestureState::default();
        assert_eq!(
            s.fixed_flags(&["shift", "cmd"]),
            vec![Action::ArmDictate(1)]
        );
        assert!(s.fixed_flags(&[]).is_empty());
        assert_eq!(
            s.fixed_flags(&["shift", "cmd"]),
            vec![Action::ArmDictate(2)]
        );
        assert!(!s.commit_dictation(1));
        assert!(s.commit_dictation(2));
    }

    #[test]
    fn escape_is_a_cancel_action() {
        let mut state = GestureState::default();
        assert_eq!(state.key_down(53), vec![Action::Cancel]);
    }
}
