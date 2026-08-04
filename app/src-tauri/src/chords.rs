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

pub struct ChordConfig {
    /// Tap-and-release chord, e.g. ["ctrl","cmd"] — fires on release.
    pub tap: Vec<&'static str>,
    /// Push-to-talk chord, e.g. ["shift","cmd"] — press and release callbacks.
    pub hold: Vec<&'static str>,
}

struct State {
    tap_armed: bool,
    tap_at: std::time::Instant,
    /// Set once a real key is pressed while a chord is held: that means the
    /// user was typing an ordinary shortcut (Cmd+Ctrl+Space, say), not making
    /// a gesture. Without this guard every such shortcut fires the hotkey.
    blocked: bool,
    holding: bool,
}

static SUPPRESSED: AtomicBool = AtomicBool::new(false);

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
    cfg: ChordConfig,
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
                    // A real keypress means this is an ordinary shortcut.
                    st.blocked = true;
                    if st.tap_armed {
                        st.tap_armed = false;
                    }
                }
                CGEventType::FlagsChanged => {
                    let mods = active_mods(event);
                    let same = |want: &[&str]| {
                        mods.len() == want.len() && want.iter().all(|w| mods.contains(w))
                    };

                    if mods.is_empty() {
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
                        if same(&cfg.tap) && !st.blocked {
                            st.tap_armed = true;
                            st.tap_at = std::time::Instant::now();
                        }
                        if same(&cfg.hold) {
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
