//! Product identity and runtime namespace for the isolated Kokoro Voice 2.1 build.
//!
//! These values must never overlap Kokoro Voice 1. The two apps are expected to
//! run on the same Mac during development without sharing processes, mutable
//! data, logs, control files, or loopback ports.

pub const DISPLAY_NAME: &str = "Kokoro Voice 2.1";
pub const APP_SUPPORT_DIR: &str = "Kokoro Voice 2.1";
pub const CONFIG_DIR_NAME: &str = "kokoro-voice-2-1";
pub const DEFAULT_PORT: &str = "8125";
pub const CLIENT_HOST: &str = "127.0.0.1:8125";
pub const DIAGNOSTICS_FILE: &str = "kokoro-voice-2-1-diagnostics.json";
