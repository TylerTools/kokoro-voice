//! Shortcut domain model shared by the Tauri commands and platform adapters.
//!
//! # Why this module exists
//!
//! macOS routes modifier-only gestures and complete accelerators through one
//! Quartz controller because Deskflow events do not reliably trigger the
//! registered-hotkey API. Windows uses Tauri's global-shortcut plugin.
//!
//! Those platform controllers are implementation details. This module is the
//! source of truth for slots, defaults, display data, and recorder validation
//! so the UI and runtime cannot quietly invent different shortcut contracts.

use serde::Serialize;

// Deliberately distinct from Kokoro Voice 1 so both event taps can run during
// verification without one physical press dispatching actions in both apps.
#[cfg(not(target_os = "windows"))]
pub const DEFAULT_READ: &str = "Control+Alt+Command+KeyU";
#[cfg(not(target_os = "windows"))]
pub const DEFAULT_DICTATE: &str = "Control+Alt+Command+KeyI";
#[cfg(not(target_os = "windows"))]
pub const DEFAULT_SNIP: &str = "Control+Alt+Command+KeyP";
#[cfg(target_os = "windows")]
pub const DEFAULT_READ: &str = "Control+Alt+Shift+KeyU";
#[cfg(target_os = "windows")]
pub const DEFAULT_DICTATE: &str = "Control+Alt+Shift+KeyI";
#[cfg(target_os = "windows")]
pub const DEFAULT_SNIP: &str = "Control+Alt+Shift+KeyP";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    Read,
    Dictate,
    Snip,
}

impl Slot {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "read" => Ok(Self::Read),
            "dictate" => Ok(Self::Dictate),
            "snip" => Ok(Self::Snip),
            _ => Err(format!("unknown shortcut slot: {value}")),
        }
    }

    pub fn preference_key(self) -> &'static str {
        match self {
            Self::Read => "hk_read",
            Self::Dictate => "hk_dictate",
            Self::Snip => "hk_snip",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Dictate => "dictate",
            Self::Snip => "snip",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub read: String,
    pub dictate: String,
    pub snip: String,
}

impl Config {
    /// Decode persisted fallbacks without mutating the preferences document.
    /// Missing or empty values use conservative complete-key defaults.
    pub fn from_preferences(preferences: &serde_json::Value) -> Self {
        let get = |key: &str, default: &str| {
            preferences
                .get(key)
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .unwrap_or(default)
                .to_string()
        };
        Self {
            read: get("hk_read", DEFAULT_READ),
            dictate: get("hk_dictate", DEFAULT_DICTATE),
            snip: get("hk_snip", DEFAULT_SNIP),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureKind {
    ModifierGesture,
    RegisteredShortcut,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Capture {
    pub slot: Slot,
    pub accelerator: String,
    pub kind: CaptureKind,
}

/// Classify a recorder result before the runtime touches preferences.
///
/// Complete accelerators are validated by the platform shortcut parser in
/// `lib.rs`. Modifier gestures use the platform input adapter and require at least
/// two distinct modifiers so an ordinary Control, Shift, Alt, or Command press
/// can never trigger an action by itself.
pub fn classify_capture(slot: Slot, accelerator: &str, _macos: bool) -> Result<Capture, String> {
    if modifier_only(accelerator) {
        let modifiers: Vec<_> = accelerator.split('+').collect();
        if modifiers.len() < 2 {
            return Err("Use at least two modifier keys for a modifier-only shortcut.".into());
        }
        let mut unique = modifiers.clone();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() != modifiers.len() {
            return Err("A shortcut cannot repeat the same modifier key.".into());
        }
        return Ok(Capture {
            slot,
            accelerator: accelerator.to_string(),
            kind: CaptureKind::ModifierGesture,
        });
    }
    Ok(Capture {
        slot,
        accelerator: accelerator.to_string(),
        kind: CaptureKind::RegisteredShortcut,
    })
}

pub fn response(
    preferences: &serde_json::Value,
    registered: bool,
    macos: bool,
) -> serde_json::Value {
    let config = Config::from_preferences(preferences);
    let read = config.read.clone();
    let dictate = config.dictate.clone();
    let snip = config.snip.clone();
    serde_json::json!({
        "modifier_labels": if macos {
            serde_json::json!({"Control":"⌃", "Alt":"⌥", "Shift":"⇧", "Command":"⌘"})
        } else {
            serde_json::json!({"Control":"Ctrl", "Alt":"Alt", "Shift":"Shift", "Command":"Win"})
        },
        "separator": if macos { "" } else { "+" },
        "read": read,
        "dictate": dictate,
        "snip": snip,
        "bindings": {
            "read": { "label": read, "registered": registered, "configurable": true },
            "dictate": { "label": dictate, "registered": registered, "configurable": true },
            "snip": { "label": snip, "registered": registered, "configurable": true }
        }
    })
}

pub(crate) fn modifier_only(accelerator: &str) -> bool {
    !accelerator.is_empty()
        && accelerator
            .split('+')
            .all(|part| matches!(part, "Control" | "Alt" | "Shift" | "Command"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_shortcuts_display_named_keys() {
        let value = response(&serde_json::json!({}), true, false);
        assert_eq!(value["modifier_labels"]["Control"], "Ctrl");
        assert_eq!(value["modifier_labels"]["Command"], "Win");
        assert_eq!(value["separator"], "+");
    }

    #[test]
    fn defaults_and_saved_values_have_one_source_of_truth() {
        let defaults = Config::from_preferences(&serde_json::json!({}));
        assert_eq!(defaults.read, DEFAULT_READ);
        assert_eq!(defaults.dictate, DEFAULT_DICTATE);
        assert_eq!(defaults.snip, DEFAULT_SNIP);

        let saved = Config::from_preferences(&serde_json::json!({
            "hk_snip": "Shift+Command+KeyZ"
        }));
        assert_eq!(saved.snip, "Shift+Command+KeyZ");
    }

    #[test]
    fn response_keeps_every_recorder_visible() {
        let response = response(&serde_json::json!({}), true, true);
        for slot in ["read", "dictate", "snip"] {
            assert_eq!(response["bindings"][slot]["configurable"], true);
        }
    }

    #[test]
    fn mac_modifier_gestures_accept_any_safe_multi_modifier_chord() {
        for (slot, accelerator) in [
            (Slot::Read, "Control+Shift"),
            (Slot::Dictate, "Control+Command"),
            (Slot::Snip, "Alt+Shift"),
        ] {
            assert_eq!(
                classify_capture(slot, accelerator, true).unwrap().kind,
                CaptureKind::ModifierGesture
            );
        }
        assert!(classify_capture(Slot::Read, "Control", true).is_err());
        assert!(classify_capture(Slot::Read, "Control+Control", true).is_err());
    }

    #[test]
    fn complete_shortcuts_are_sent_to_the_platform_parser() {
        let capture = classify_capture(Slot::Snip, "Shift+Command+KeyZ", true).unwrap();
        assert_eq!(capture.kind, CaptureKind::RegisteredShortcut);
        assert_eq!(capture.accelerator, "Shift+Command+KeyZ");
    }

    #[test]
    fn windows_accepts_modifier_chords_but_rejects_single_or_duplicate_modifiers() {
        assert_eq!(
            classify_capture(Slot::Read, "Control+Alt", false)
                .unwrap()
                .kind,
            CaptureKind::ModifierGesture
        );
        assert_eq!(
            classify_capture(Slot::Dictate, "Control+Shift", false)
                .unwrap()
                .kind,
            CaptureKind::ModifierGesture
        );
        assert!(classify_capture(Slot::Read, "Control", false).is_err());
        assert!(classify_capture(Slot::Read, "Control+Control", false).is_err());
    }
}
