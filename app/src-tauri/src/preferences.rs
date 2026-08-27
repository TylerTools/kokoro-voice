//! Typed access to the persisted user-preference document.
//!
//! This module owns defaults and value normalization, but deliberately preserves
//! unknown JSON fields so an older or newer build cannot erase another build's
//! settings. Shortcut meaning remains owned by `hotkeys.rs`.

use serde_json::{Map, Value};
use std::path::Path;

pub const DEFAULT_VOICE: &str = "af_heart";
pub const DEFAULT_SPEED: f64 = 1.0;
pub const MIN_SPEED: f64 = 0.5;
pub const MAX_SPEED: f64 = 2.0;
pub const DEFAULT_CUE_VOLUME: f64 = 0.22;

#[derive(Clone, Debug, PartialEq)]
pub struct Preferences {
    document: Value,
}

impl Default for Preferences {
    fn default() -> Self {
        Self::from_value(Value::Object(Map::new()))
    }
}

impl Preferences {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .map(Self::from_value)
            .unwrap_or_default()
    }

    pub fn from_value(value: Value) -> Self {
        let mut object = value.as_object().cloned().unwrap_or_default();
        let voice = object
            .get("voice")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(DEFAULT_VOICE)
            .to_string();
        object.insert("voice".into(), Value::String(voice));

        let speed = object
            .get("speed")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .unwrap_or(DEFAULT_SPEED)
            .clamp(MIN_SPEED, MAX_SPEED);
        object.insert("speed".into(), serde_json::json!(speed));

        let cue_enabled = object
            .get("cue_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        object.insert("cue_enabled".into(), Value::Bool(cue_enabled));

        let cue_volume = object
            .get("cue_volume")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .unwrap_or(DEFAULT_CUE_VOLUME)
            .clamp(0.0, 1.0);
        object.insert("cue_volume".into(), serde_json::json!(cue_volume));

        let live_preview = object
            .get("live_preview")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        object.insert("live_preview".into(), Value::Bool(live_preview));

        if object
            .get("microphone_device")
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            object.insert("microphone_device".into(), Value::Null);
        }
        if object
            .get("launch_at_login")
            .is_some_and(|value| !value.is_boolean())
        {
            object.remove("launch_at_login");
        }
        Self {
            document: Value::Object(object),
        }
    }

    pub fn as_value(&self) -> &Value {
        &self.document
    }

    pub fn into_value(self) -> Value {
        self.document
    }

    pub fn voice(&self) -> &str {
        self.document["voice"].as_str().unwrap_or(DEFAULT_VOICE)
    }

    pub fn speed(&self) -> f64 {
        self.document["speed"].as_f64().unwrap_or(DEFAULT_SPEED)
    }

    pub fn live_preview(&self) -> bool {
        self.document["live_preview"].as_bool().unwrap_or(false)
    }

    pub fn microphone_device(&self) -> Option<&str> {
        self.document
            .get("microphone_device")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    }

    pub fn launch_at_login(&self, default: bool) -> bool {
        self.document
            .get("launch_at_login")
            .and_then(Value::as_bool)
            .unwrap_or(default)
    }

    pub fn set_general(
        &mut self,
        voice: Option<String>,
        speed: Option<f64>,
        cue_enabled: Option<bool>,
        cue_volume: Option<f64>,
        live_preview: Option<bool>,
    ) {
        if let Some(value) = voice.filter(|value| !value.trim().is_empty()) {
            self.set("voice", Value::String(value));
        }
        if let Some(value) = speed.filter(|value| value.is_finite()) {
            self.set(
                "speed",
                serde_json::json!(value.clamp(MIN_SPEED, MAX_SPEED)),
            );
        }
        if let Some(value) = cue_enabled {
            self.set("cue_enabled", Value::Bool(value));
        }
        if let Some(value) = cue_volume.filter(|value| value.is_finite()) {
            self.set("cue_volume", serde_json::json!(value.clamp(0.0, 1.0)));
        }
        if let Some(value) = live_preview {
            self.set("live_preview", Value::Bool(value));
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.document.get(key)
    }

    pub fn set(&mut self, key: &str, value: Value) {
        if let Some(object) = self.document.as_object_mut() {
            object.insert(key.to_string(), value);
        }
    }

    pub fn remove(&mut self, key: &str) {
        if let Some(object) = self.document.as_object_mut() {
            object.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_engine_speed_contract_are_applied() {
        let defaults = Preferences::default();
        assert_eq!(defaults.voice(), DEFAULT_VOICE);
        assert_eq!(defaults.speed(), DEFAULT_SPEED);
        assert!(!defaults.live_preview());

        let low = Preferences::from_value(serde_json::json!({ "speed": 0.25 }));
        let high = Preferences::from_value(serde_json::json!({ "speed": 3.0 }));
        assert_eq!(low.speed(), MIN_SPEED);
        assert_eq!(high.speed(), MAX_SPEED);
    }

    #[test]
    fn unknown_and_shortcut_fields_survive_normalization() {
        let preferences = Preferences::from_value(serde_json::json!({
            "hk_read": "Control+Alt+KeyU",
            "future_setting": { "enabled": true }
        }));
        assert_eq!(preferences.as_value()["hk_read"], "Control+Alt+KeyU");
        assert_eq!(preferences.as_value()["future_setting"]["enabled"], true);
    }

    #[test]
    fn mutations_are_bounded_and_launch_default_is_channel_specific() {
        let mut preferences = Preferences::default();
        preferences.set_general(None, Some(9.0), None, Some(-1.0), Some(true));
        assert_eq!(preferences.speed(), MAX_SPEED);
        assert_eq!(preferences.as_value()["cue_volume"], 0.0);
        assert!(preferences.live_preview());
        assert!(preferences.launch_at_login(true));
        assert!(!preferences.launch_at_login(false));
    }
}
