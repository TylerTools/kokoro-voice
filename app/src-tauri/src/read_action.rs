//! Read-action decision policy.
//!
//! This module decides whether a Read trigger should speak a fresh selection,
//! toggle existing playback, or show a bounded notice. It does not acquire
//! accessibility text or own playback processes; keeping that split prevents
//! platform failures from silently becoming stale-clipboard reads.

use crate::text_backend::ApplyOutcome;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Speak(String),
    Toggle,
    Notice {
        code: &'static str,
        message: &'static str,
    },
}

pub(crate) fn decide(
    selection: Result<Option<String>, ApplyOutcome>,
    playback_state: &str,
) -> Decision {
    match selection {
        Ok(Some(text)) if !text.trim().is_empty() => Decision::Speak(text),
        Ok(Some(_)) | Ok(None) if matches!(playback_state, "playing" | "paused") => {
            Decision::Toggle
        }
        Ok(Some(_)) | Ok(None) => Decision::Notice {
            code: "no-selection",
            message: "Select some text, then press Read",
        },
        Err(ApplyOutcome::SecureField) => Decision::Notice {
            code: "secure-field",
            message: "Kokoro will not read from a secure field",
        },
        Err(_) => Decision::Notice {
            code: "selection-unavailable",
            message: "Can't access this selection. Open Settings.",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_selection_always_replaces_existing_playback() {
        assert_eq!(
            decide(Ok(Some("new selection".into())), "playing"),
            Decision::Speak("new selection".into())
        );
    }

    #[test]
    fn empty_selection_toggles_only_active_playback() {
        assert_eq!(decide(Ok(None), "playing"), Decision::Toggle);
        assert_eq!(decide(Ok(None), "paused"), Decision::Toggle);
        assert!(matches!(
            decide(Ok(None), "idle"),
            Decision::Notice {
                code: "no-selection",
                ..
            }
        ));
    }

    #[test]
    fn secure_and_unavailable_targets_never_fall_back_to_clipboard_text() {
        assert!(matches!(
            decide(Err(ApplyOutcome::SecureField), "idle"),
            Decision::Notice {
                code: "secure-field",
                ..
            }
        ));
        assert!(matches!(
            decide(Err(ApplyOutcome::Unavailable), "idle"),
            Decision::Notice {
                code: "selection-unavailable",
                ..
            }
        ));
    }
}
