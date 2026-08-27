//! Product identity and runtime namespace for stable and candidate builds.
//!
//! Stable defaults preserve the installed 2.1 namespace. Candidate builds set
//! compile-time environment values from `scripts/release/build-candidate.sh`;
//! they must never share mutable state, ports, hotkeys, or autostart with Stable.

pub const DISPLAY_NAME: &str = match option_env!("KOKORO_DISPLAY_NAME") {
    Some(value) => value,
    None => "Kokoro Voice 2.1",
};
pub const APP_SUPPORT_DIR: &str = match option_env!("KOKORO_APP_SUPPORT_DIR") {
    Some(value) => value,
    None => "Kokoro Voice 2.1",
};
#[cfg(target_os = "macos")]
pub const CONFIG_DIR_NAME: &str = match option_env!("KOKORO_CONFIG_DIR_NAME") {
    Some(value) => value,
    None => "kokoro-voice-2-1",
};
pub const DEFAULT_PORT: &str = match option_env!("KOKORO_DEFAULT_PORT") {
    Some(value) => value,
    None => "8125",
};
pub const CLIENT_HOST: &str = match option_env!("KOKORO_CLIENT_HOST") {
    Some(value) => value,
    None => "127.0.0.1:8125",
};
pub const DIAGNOSTICS_FILE: &str = match option_env!("KOKORO_DIAGNOSTICS_FILE") {
    Some(value) => value,
    None => "kokoro-voice-2-1-diagnostics.json",
};
pub const TTS_CPU_MEM_ARENA: &str = match option_env!("KOKORO_TTS_CPU_MEM_ARENA") {
    Some(value) => value,
    None => "1",
};
pub const DEFAULT_AUTOSTART: bool = option_env!("KOKORO_BUILD_CHANNEL").is_none();
pub const INPUT_CONTROLLER_ENABLED: bool = option_env!("KOKORO_BUILD_CHANNEL").is_none();
/// Only Stable may inspect the superseded Stable runtime namespace. Candidate
/// must never read or mutate state belonging to another release channel.
pub const LEGACY_CONFIG_DIR_NAME: Option<&str> = if option_env!("KOKORO_BUILD_CHANNEL").is_none() {
    Some("kokoro-voice-2")
} else {
    None
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_is_passive_and_stable_owns_input() {
        let candidate = option_env!("KOKORO_BUILD_CHANNEL").is_some();
        assert_eq!(INPUT_CONTROLLER_ENABLED, !candidate);
        assert_eq!(DEFAULT_AUTOSTART, !candidate);
    }
}
