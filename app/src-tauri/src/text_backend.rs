use serde::Serialize;

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug)]
pub struct TargetSnapshot {
    pub target_id: String,
    pub scope_id: Option<String>,
    pub baseline: String,
    pub start: usize,
    pub selected_len: usize,
}

#[cfg(not(any(target_os = "macos", test)))]
#[derive(Clone, Debug)]
pub struct TargetSnapshot;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplyOutcome {
    Applied,
    ClipboardFallback(String),
    SecureField,
    Unavailable,
}

pub trait TextBackend {
    fn capture_target() -> Result<TargetSnapshot, ApplyOutcome>;
    fn apply_revision(
        target: &mut TargetSnapshot,
        expected: &str,
        replacement: &str,
    ) -> ApplyOutcome;
}

pub struct PlatformTextBackend;

#[cfg(test)]
fn char_slice(text: &str, start: usize, len: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    (start + len <= chars.len()).then(|| chars[start..start + len].iter().collect())
}

#[cfg(any(target_os = "macos", test))]
fn expected_value(target: &TargetSnapshot, inserted: &str) -> String {
    let chars: Vec<char> = target.baseline.chars().collect();
    let mut out: String = chars[..target.start].iter().collect();
    out.push_str(inserted);
    out.extend(chars[target.start + target.selected_len..].iter());
    out
}

#[cfg(any(target_os = "macos", test))]
fn scope_compatible(original: Option<&str>, current: Option<&str>) -> bool {
    original.is_none_or(|expected| current == Some(expected))
}

#[cfg(any(target_os = "macos", test))]
fn can_rebind_target(
    same_process: bool,
    same_scope: bool,
    value_matches: bool,
    selection_matches: bool,
) -> bool {
    same_process && same_scope && value_matches && selection_matches
}

/// Chromium/WebKit content-editables sometimes expose an empty editor through
/// Accessibility as `"\n<description>"` with the caret parked after the
/// synthetic newline.  The description is placeholder text, not user text.
/// Treating it as the baseline makes the first injected preview look like an
/// unowned edit as soon as the placeholder disappears.
#[cfg(any(target_os = "macos", test))]
fn normalize_empty_placeholder(
    value: String,
    description: Option<&str>,
    selection_start: usize,
    selection_len: usize,
) -> (String, usize) {
    let Some(description) = description.filter(|value| !value.is_empty()) else {
        return (value, selection_start);
    };
    let value_chars: Vec<char> = value.chars().collect();
    let description_chars: Vec<char> = description.chars().collect();
    if selection_len != 0 || value_chars.len() < description_chars.len() {
        return (value, selection_start);
    }
    let prefix_len = value_chars.len() - description_chars.len();
    let placeholder_matches = value_chars[prefix_len..] == description_chars
        && value_chars[..prefix_len].iter().all(|c| c.is_whitespace())
        && selection_start <= prefix_len;
    if placeholder_matches {
        (String::new(), 0)
    } else {
        (value, selection_start)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use accessibility::{AXAttribute, AXUIElement, AXUIElementAttributes};
    use accessibility_sys::{
        kAXFocusedUIElementAttribute, kAXSelectedTextRangeAttribute, kAXValueTypeCFRange,
        AXUIElementGetPid, AXValueGetType, AXValueGetValue, AXValueRef,
    };
    use core_foundation::{
        base::{CFRange, CFType, TCFType},
        string::CFString,
    };

    fn custom(name: &str) -> AXAttribute<CFType> {
        AXAttribute::<CFType>::new(&CFString::new(name))
    }

    fn identities(element: &AXUIElement, pid: i32) -> (String, Option<String>) {
        let element_hash = unsafe { core_foundation::base::CFHash(element.as_CFTypeRef()) };
        let scope = element.window().ok().map(|window| {
            let hash = unsafe { core_foundation::base::CFHash(window.as_CFTypeRef()) };
            format!("{pid}:{hash}")
        });
        (format!("{pid}:{element_hash}"), scope)
    }

    fn focused() -> Result<(AXUIElement, i32, String, usize, usize), ApplyOutcome> {
        if !macos_accessibility_client::accessibility::application_is_trusted() {
            return Err(ApplyOutcome::Unavailable);
        }
        let any = AXUIElement::system_wide()
            .attribute(&custom(kAXFocusedUIElementAttribute))
            .map_err(|_| ApplyOutcome::Unavailable)?;
        let element = any
            .downcast_into::<AXUIElement>()
            .ok_or(ApplyOutcome::Unavailable)?;
        let role = element.role().map(|v| v.to_string()).unwrap_or_default();
        let subrole = element.subrole().map(|v| v.to_string()).unwrap_or_default();
        if role.contains("Secure") || subrole.contains("Secure") {
            return Err(ApplyOutcome::SecureField);
        }
        if !element.is_settable(&AXAttribute::value()).unwrap_or(false) {
            return Err(ApplyOutcome::Unavailable);
        }
        let value = element
            .value()
            .ok()
            .and_then(|v| v.downcast_into::<CFString>())
            .map(|v| v.to_string())
            .ok_or(ApplyOutcome::Unavailable)?;
        let range_any = element
            .attribute(&custom(kAXSelectedTextRangeAttribute))
            .map_err(|_| ApplyOutcome::Unavailable)?;
        let value_ref = range_any.as_CFTypeRef() as AXValueRef;
        if unsafe { AXValueGetType(value_ref) } != kAXValueTypeCFRange {
            return Err(ApplyOutcome::Unavailable);
        }
        let mut range = CFRange::init(0, 0);
        if !unsafe {
            AXValueGetValue(
                value_ref,
                kAXValueTypeCFRange,
                (&mut range as *mut CFRange).cast(),
            )
        } {
            return Err(ApplyOutcome::Unavailable);
        }
        let mut pid = 0;
        if unsafe { AXUIElementGetPid(element.as_concrete_TypeRef(), &mut pid) } != 0 {
            return Err(ApplyOutcome::Unavailable);
        }
        let description = element.description().ok().map(|value| value.to_string());
        let (value, selection_start) = normalize_empty_placeholder(
            value,
            description.as_deref(),
            range.location.max(0) as usize,
            range.length.max(0) as usize,
        );
        Ok((
            element,
            pid,
            value,
            selection_start,
            range.length.max(0) as usize,
        ))
    }

    impl TextBackend for PlatformTextBackend {
        fn capture_target() -> Result<TargetSnapshot, ApplyOutcome> {
            let (element, pid, baseline, start, selected_len) = focused()?;
            let (target_id, scope_id) = identities(&element, pid);
            Ok(TargetSnapshot {
                target_id,
                scope_id,
                baseline,
                start,
                selected_len,
            })
        }

        fn apply_revision(
            target: &mut TargetSnapshot,
            expected: &str,
            replacement: &str,
        ) -> ApplyOutcome {
            let Ok((element, pid, current, selection_start, selection_len)) = focused() else {
                return ApplyOutcome::ClipboardFallback("target-unavailable".into());
            };
            let (current_target_id, current_scope_id) = identities(&element, pid);
            let target_pid = target
                .target_id
                .split_once(':')
                .and_then(|(pid, _)| pid.parse::<i32>().ok());
            let first_edit = expected.is_empty();
            let value_ok = if first_edit {
                current == target.baseline
            } else {
                current == expected_value(target, expected)
            };
            let selection_ok = if first_edit {
                selection_start == target.start && selection_len == target.selected_len
            } else {
                selection_start == target.start + expected.chars().count() && selection_len == 0
            };
            if current_target_id != target.target_id {
                let same_process = Some(pid) == target_pid;
                let same_scope =
                    scope_compatible(target.scope_id.as_deref(), current_scope_id.as_deref());
                if can_rebind_target(same_process, same_scope, value_ok, selection_ok) {
                    crate::structured_log(
                        "dictation-target-rebound",
                        serde_json::json!({
                            "phase": "precheck",
                            "scope_verified": target.scope_id.is_some(),
                        }),
                    );
                    target.target_id = current_target_id;
                    target.scope_id = current_scope_id;
                } else {
                    crate::structured_log(
                        "dictation-target-identity-rejected",
                        serde_json::json!({
                            "same_process": same_process,
                            "same_scope": same_scope,
                            "scope_verified": target.scope_id.is_some(),
                            "value_matches": value_ok,
                            "selection_matches": selection_ok,
                            "current_chars": current.chars().count(),
                            "expected_chars": expected_value(target, expected).chars().count(),
                        }),
                    );
                    return ApplyOutcome::ClipboardFallback("focus-changed".into());
                }
            }
            if !value_ok || !selection_ok {
                crate::structured_log(
                    "dictation-target-precheck-failed",
                    serde_json::json!({
                        "current_chars": current.chars().count(),
                        "expected_chars": expected_value(target, expected).chars().count(),
                        "selection_start": selection_start,
                        "selection_len": selection_len,
                        "expected_selection_start": if first_edit {
                            target.start
                        } else {
                            target.start + expected.chars().count()
                        },
                        "value_matches": value_ok,
                        "selection_matches": selection_ok,
                    }),
                );
                return ApplyOutcome::ClipboardFallback("text-or-caret-changed".into());
            }
            let (delete, insert) = crate::edit_delta(expected, replacement);
            if crate::chords::replace_focused_text(delete, &insert).is_err() {
                return ApplyOutcome::ClipboardFallback("injection-failed".into());
            }
            // WebKit/React editors may replace their AX node as soon as its
            // value changes. The target identity was verified immediately
            // before injection; afterward, verify the owned value and caret in
            // the same process rather than requiring the obsolete node hash.
            let wanted = expected_value(target, replacement);
            let mut last_observation = serde_json::json!({
                "readable": false,
                "wanted_chars": wanted.chars().count(),
                "expected_selection_start": target.start + replacement.chars().count(),
            });
            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                if let Ok((element, pid, current, selection_start, selection_len)) = focused() {
                    let (current_target_id, current_scope_id) = identities(&element, pid);
                    let same_scope =
                        scope_compatible(target.scope_id.as_deref(), current_scope_id.as_deref());
                    last_observation = serde_json::json!({
                        "readable": true,
                        "same_process": Some(pid) == target_pid,
                        "same_scope": same_scope,
                        "current_chars": current.chars().count(),
                        "wanted_chars": wanted.chars().count(),
                        "value_matches": current == wanted,
                        "selection_start": selection_start,
                        "selection_len": selection_len,
                        "expected_selection_start": target.start + replacement.chars().count(),
                    });
                    if Some(pid) != target_pid || !same_scope {
                        return ApplyOutcome::ClipboardFallback("focus-changed".into());
                    }
                    if current == wanted
                        && selection_start == target.start + replacement.chars().count()
                        && selection_len == 0
                    {
                        if current_target_id != target.target_id {
                            crate::structured_log(
                                "dictation-target-rebound",
                                serde_json::json!({
                                    "phase": "postcheck",
                                    "scope_verified": target.scope_id.is_some(),
                                }),
                            );
                            target.target_id = current_target_id;
                            target.scope_id = current_scope_id;
                        }
                        return ApplyOutcome::Applied;
                    }
                }
            }
            crate::structured_log("dictation-target-postcheck-failed", last_observation);
            ApplyOutcome::ClipboardFallback("verification-failed".into())
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use windows::Win32::{
        System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
        },
        UI::Accessibility::{CUIAutomation, IUIAutomation},
    };

    impl TextBackend for PlatformTextBackend {
        fn capture_target() -> Result<TargetSnapshot, ApplyOutcome> {
            // Block password controls with native UI Automation. Other Windows
            // controls remain clipboard-only until TextPattern range ownership
            // is physically certified.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let automation: IUIAutomation =
                    CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                        .map_err(|_| ApplyOutcome::Unavailable)?;
                let focused = automation
                    .GetFocusedElement()
                    .map_err(|_| ApplyOutcome::Unavailable)?;
                if focused
                    .CurrentIsPassword()
                    .map(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    return Err(ApplyOutcome::SecureField);
                }
            }
            Err(ApplyOutcome::ClipboardFallback("clipboard-only".into()))
        }

        fn apply_revision(
            _target: &mut TargetSnapshot,
            _expected: &str,
            _replacement: &str,
        ) -> ApplyOutcome {
            ApplyOutcome::Unavailable
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::*;
    impl TextBackend for PlatformTextBackend {
        fn capture_target() -> Result<TargetSnapshot, ApplyOutcome> {
            Err(ApplyOutcome::Unavailable)
        }
        fn apply_revision(_: &mut TargetSnapshot, _: &str, _: &str) -> ApplyOutcome {
            ApplyOutcome::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_value_replaces_only_initial_selection() {
        let target = TargetSnapshot {
            target_id: "x".into(),
            scope_id: None,
            baseline: "hello old world".into(),
            start: 6,
            selected_len: 3,
        };
        assert_eq!(expected_value(&target, "new"), "hello new world");
        assert_eq!(char_slice("aé🙂", 1, 2).as_deref(), Some("é🙂"));
    }

    #[test]
    fn chromium_empty_editor_placeholder_is_not_owned_text() {
        let (value, start) =
            normalize_empty_placeholder("\nDo anything".into(), Some("Do anything"), 1, 0);
        assert_eq!(value, "");
        assert_eq!(start, 0);

        let target = TargetSnapshot {
            target_id: "x".into(),
            scope_id: None,
            baseline: value,
            start,
            selected_len: 0,
        };
        assert_eq!(
            expected_value(&target, "complete message"),
            "complete message"
        );
    }

    #[test]
    fn real_text_equal_to_description_is_not_mistaken_for_placeholder() {
        let (value, start) =
            normalize_empty_placeholder("Do anything".into(), Some("Do anything"), 11, 0);
        assert_eq!(value, "Do anything");
        assert_eq!(start, 11);
    }

    #[test]
    fn selected_or_nonmatching_values_are_not_normalized() {
        assert_eq!(
            normalize_empty_placeholder("\nDo anything".into(), Some("Do anything"), 1, 2),
            ("\nDo anything".into(), 1)
        );
        assert_eq!(
            normalize_empty_placeholder("draft".into(), Some("Do anything"), 5, 0),
            ("draft".into(), 5)
        );
    }

    #[test]
    fn target_rebind_requires_process_scope_value_and_caret_proof() {
        assert!(can_rebind_target(true, true, true, true));
        assert!(!can_rebind_target(false, true, true, true));
        assert!(!can_rebind_target(true, false, true, true));
        assert!(!can_rebind_target(true, true, false, true));
        assert!(!can_rebind_target(true, true, true, false));
        assert!(scope_compatible(None, None));
        assert!(scope_compatible(None, Some("new")));
        assert!(scope_compatible(Some("window"), Some("window")));
        assert!(!scope_compatible(Some("window"), Some("other")));
        assert!(!scope_compatible(Some("window"), None));
    }
}
