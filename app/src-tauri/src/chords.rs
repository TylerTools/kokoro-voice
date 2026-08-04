// Modifier-only chord hotkeys.
//
// Tauri's global-shortcut plugin binds ACCELERATORS — a key plus modifiers.
// It cannot express "tap Control and Command together, then release", which is
// what the macOS host used and what the muscle memory here is built on. So we
// watch the modifier flags directly with a passive CGEventTap, the same
// mechanism the Hammerspoon config used.
//
// This matters more than convenience on this setup: a software KVM sits between
// the keyboard and this machine and CONSUMES Shift on letter keys (measured:
// Shift+Z arrives as plain `z`). Modifier-only chords and function keys are the
// combinations that survive the trip intact.
//
// The tap is LISTEN-ONLY. It observes; it never swallows events, so no other
// application's shortcuts are affected.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult,
};

/// Which modifiers are currently held, as a sorted set of short names.
fn active_mods(event: &CGEvent) -> Vec<&'static str> {
    let f = event.get_flags();
    let mut out = Vec::new();
    // Bit masks from CGEventFlags. Checked explicitly rather than via bitflags
    // helpers so the intent stays readable.
    if f.bits() & 0x0004_0000 != 0 {
        out.push("ctrl");
    }
    if f.bits() & 0x0002_0000 != 0 {
        out.push("shift");
    }
    if f.bits() & 0x0008_0000 != 0 {
        out.push("alt");
    }
    if f.bits() & 0x0010_0000 != 0 {
        out.push("cmd");
    }
    out
}

#[derive(Clone)]
pub struct ChordConfig {
    /// Tap-and-release chord, e.g. ["ctrl","cmd"] — fires on release.
    pub tap: Vec<String>,
    /// Push-to-talk chord, e.g. ["shift","cmd"] — press and release callbacks.
    pub hold: Vec<String>,
}

struct State {
    tap_armed: bool,
    tap_at: std::time::Instant,
    /// Whether any modifier is currently held. Without this, `blocked` below
    /// was set by ORDINARY TYPING and never cleared until a modifier happened
    /// to be pressed and released — so the first chord after typing anything
    /// silently did nothing.
    mods_down: bool,
    /// Highest modifier count seen during the current hold, so a chord can be
    /// captured on release even though the flags arrive incrementally.
    peak: Vec<String>,
    /// Set once a real key is pressed while a chord is held: that means the
    /// user was typing an ordinary shortcut (Cmd+Ctrl+Space, say), not making
    /// a gesture. Without this guard every such shortcut fires the hotkey.
    blocked: bool,
    holding: bool,
}

static SUPPRESSED: AtomicBool = AtomicBool::new(false);

/// Live config, so a re-bind from the settings panel takes effect immediately
/// rather than at the next launch.
pub static CONFIG: Mutex<Option<ChordConfig>> = Mutex::new(None);

/// When set, the next chord is CAPTURED instead of acted on. This is how the
/// hotkey gets bound: record whatever actually ARRIVES, after the KVM has
/// translated it, rather than trusting a combination typed into a box.
pub static RECORDING: Mutex<Option<String>> = Mutex::new(None);
/// Result of the last capture: (slot, mods).
pub static RECORDED: Mutex<Option<(String, Vec<String>)>> = Mutex::new(None);

pub fn set_config(cfg: ChordConfig) {
    if let Ok(mut c) = CONFIG.lock() {
        *c = Some(cfg);
    }
}

pub fn start_recording(slot: &str) {
    if let Ok(mut r) = RECORDING.lock() {
        *r = Some(slot.to_string());
    }
}

pub fn take_recorded() -> Option<(String, Vec<String>)> {
    RECORDED.lock().ok().and_then(|mut r| r.take())
}

/// Temporarily ignore chords — used while we synthesize keystrokes ourselves,
/// so typing a transcript cannot retrigger the thing that produced it.
pub fn suppress(on: bool) {
    SUPPRESSED.store(on, Ordering::SeqCst);
}

/// Start watching for chords. Returns immediately; the tap runs on its own
/// thread with its own run loop.
///
/// Requires the Accessibility permission. If it is not granted the tap simply
/// fails to create, which is reported rather than failing silently — a dead
/// hotkey with no explanation is the worst outcome here.
pub fn watch<F1, F2, F3>(
    on_tap: F1,
    on_hold_start: F2,
    on_hold_end: F3,
) -> Result<(), String>
where
    F1: Fn() + Send + 'static,
    F2: Fn() + Send + 'static,
    F3: Fn() + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();

    std::thread::spawn(move || {
        let state = Mutex::new(State {
            tap_armed: false,
            tap_at: std::time::Instant::now(),
            mods_down: false,
            peak: Vec::new(),
            blocked: false,
            holding: false,
        });

        let handler = move |_proxy: CGEventTapProxyShim,
                            etype: CGEventType,
                            event: &CGEvent|
              -> CallbackResult {
            if SUPPRESSED.load(Ordering::SeqCst) {
                return CallbackResult::Keep;
            }
            let mut st = match state.lock() {
                Ok(s) => s,
                Err(_) => return CallbackResult::Keep,
            };

            match etype {
                CGEventType::KeyDown => {
                    // A real keypress only means "ordinary shortcut" if
                    // modifiers are actually down. Blocking on every keystroke
                    // left the flag stuck true through normal typing.
                    if st.mods_down {
                        st.blocked = true;
                        st.tap_armed = false;
                    }
                }
                CGEventType::FlagsChanged => {
                    let mods = active_mods(event);
                    st.mods_down = !mods.is_empty();
                    if mods.len() > st.peak.len() {
                        st.peak = mods.iter().map(|m| m.to_string()).collect();
                    }

                    // Recording swallows the gesture rather than acting on it.
                    if mods.is_empty() {
                        let slot = RECORDING.lock().ok().and_then(|mut r| r.take());
                        if let Some(slot) = slot {
                            if !st.peak.is_empty() {
                                if let Ok(mut rec) = RECORDED.lock() {
                                    *rec = Some((slot, st.peak.clone()));
                                }
                            }
                            st.peak.clear();
                            st.tap_armed = false;
                            st.blocked = false;
                            st.holding = false;
                            return CallbackResult::Keep;
                        }
                    }

                    let cfg_guard = CONFIG.lock().ok();
                    let cfg_ref = cfg_guard.as_ref().and_then(|c| c.as_ref());
                    let (want_tap, want_hold): (Vec<String>, Vec<String>) = match cfg_ref {
                        Some(c) => (
                            c.tap.iter().map(|s| s.to_string()).collect(),
                            c.hold.iter().map(|s| s.to_string()).collect(),
                        ),
                        None => (Vec::new(), Vec::new()),
                    };
                    let same = |want: &[String]| {
                        !want.is_empty()
                            && mods.len() == want.len()
                            && want.iter().all(|w| mods.contains(&w.as_str()))
                    };

                    if mods.is_empty() {
                        st.peak.clear();
                        // Everything released.
                        if st.holding {
                            st.holding = false;
                            on_hold_end();
                        }
                        if st.tap_armed
                            && !st.blocked
                            && st.tap_at.elapsed() < std::time::Duration::from_millis(1200)
                        {
                            st.tap_armed = false;
                            on_tap();
                        }
                        st.tap_armed = false;
                        st.blocked = false;
                    } else {
                        if same(&want_tap) && !st.blocked {
                            st.tap_armed = true;
                            st.tap_at = std::time::Instant::now();
                        }
                        if same(&want_hold) {
                            if !st.holding && !st.blocked {
                                st.holding = true;
                                on_hold_start();
                            }
                        } else if st.holding {
                            // Moved off the hold chord without fully releasing:
                            // end the recording rather than leaving the
                            // microphone open with no matching stop.
                            st.holding = false;
                            on_hold_end();
                        }
                    }
                }
                _ => {}
            }
            // Always Keep: this tap observes, it never consumes, so no other
            // application's shortcuts are affected.
            CallbackResult::Keep
        };

        let tap = CGEventTap::new(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            // Listen-only: observe without consuming, so nothing else breaks.
            CGEventTapOptions::ListenOnly,
            vec![CGEventType::FlagsChanged, CGEventType::KeyDown],
            handler,
        );

        let tap = match tap {
            Ok(t) => t,
            Err(_) => {
                let _ = tx.send(Err(
                    "could not watch modifier keys — grant Accessibility to Kokoro Voice".into(),
                ));
                return;
            }
        };

        let loop_source = match tap.mach_port().create_runloop_source(0) {
            Ok(s) => s,
            Err(_) => {
                let _ = tx.send(Err("could not attach the key watcher".into()));
                return;
            }
        };
        let current = CFRunLoop::get_current();
        unsafe { current.add_source(&loop_source, kCFRunLoopCommonModes) };
        tap.enable();
        let _ = tx.send(Ok(()));
        CFRunLoop::run_current();
    });

    rx.recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "key watcher did not start".to_string())?
}

// The proxy type the callback receives; we never use it, but the closure
// signature must match what core-graphics expects.
type CGEventTapProxyShim = core_graphics::event::CGEventTapProxy;
