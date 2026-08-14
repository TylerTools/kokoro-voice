//! Target-locked accessibility insertion for live dictation.
//!
//! A preview may revise text already inserted by Kokoro, but it must never edit
//! a different control or overwrite user changes. `TargetSnapshot` records the
//! original process/control, selection, and owned-text projection. Every write
//! revalidates those invariants and permanently falls back to the clipboard
//! when ownership cannot be proven.

use serde::Serialize;

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug)]
pub struct TargetSnapshot {
    pub target_id: String,
    pub scope_id: Option<String>,
    pub baseline: String,
    pub start: usize,
    pub selected_len: usize,
    projection: TextProjection,
}

#[cfg(not(any(target_os = "macos", test)))]
#[derive(Clone, Debug)]
pub struct TargetSnapshot;

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextProjection {
    Stable,
    Chromium,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProjectionMatch {
    owned_start: usize,
    context_reflowed: bool,
}

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
fn canonical_browser_context(text: &[char], trim_leading: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_was_cr = false;
    for &character in text {
        match character {
            '\r' => {
                out.push('\n');
                previous_was_cr = true;
            }
            '\n' if previous_was_cr => previous_was_cr = false,
            '\n' | '\u{2028}' | '\u{2029}' => {
                out.push('\n');
                previous_was_cr = false;
            }
            '\u{00a0}' | '\u{202f}' => {
                out.push(' ');
                previous_was_cr = false;
            }
            other => {
                out.push(other);
                previous_was_cr = false;
            }
        }
    }
    if trim_leading {
        out.strip_prefix('\n').unwrap_or(&out).to_string()
    } else {
        out.strip_suffix('\n').unwrap_or(&out).to_string()
    }
}

/// Chromium content-editables may add or remove a synthetic boundary newline,
/// normalize CR/LF, or expose a DOM space as NBSP after an input event. Match
/// the exact owned text at the caret and permit only those representation
/// changes in the unowned context. Any actual text or caret edit still fails.
#[cfg(any(target_os = "macos", test))]
fn match_projection(
    target: &TargetSnapshot,
    current: &str,
    inserted: &str,
    selection_start: usize,
    selection_len: usize,
) -> Option<ProjectionMatch> {
    if inserted.is_empty() || selection_len != 0 {
        return None;
    }
    let baseline: Vec<char> = target.baseline.chars().collect();
    let current: Vec<char> = current.chars().collect();
    let inserted: Vec<char> = inserted.chars().collect();
    let owned_start = selection_start.checked_sub(inserted.len())?;
    let owned_end = owned_start.checked_add(inserted.len())?;
    if owned_end > current.len()
        || target.start + target.selected_len > baseline.len()
        || current[owned_start..owned_end] != inserted
    {
        return None;
    }

    let expected_prefix = &baseline[..target.start];
    let expected_suffix = &baseline[target.start + target.selected_len..];
    let current_prefix = &current[..owned_start];
    let current_suffix = &current[owned_end..];
    let exact_context = expected_prefix == current_prefix && expected_suffix == current_suffix;
    let browser_context = target.projection == TextProjection::Chromium
        && canonical_browser_context(expected_prefix, true)
            == canonical_browser_context(current_prefix, true)
        && canonical_browser_context(expected_suffix, false)
            == canonical_browser_context(current_suffix, false);
    (exact_context || browser_context).then_some(ProjectionMatch {
        owned_start,
        context_reflowed: !exact_context,
    })
}

#[cfg(any(target_os = "macos", test))]
fn rebase_projection(
    target: &mut TargetSnapshot,
    current: &str,
    inserted: &str,
    observation: ProjectionMatch,
) {
    if !observation.context_reflowed {
        return;
    }
    let baseline: Vec<char> = target.baseline.chars().collect();
    let current: Vec<char> = current.chars().collect();
    let inserted_len = inserted.chars().count();
    let selected: String = baseline[target.start..target.start + target.selected_len]
        .iter()
        .collect();
    let mut rebased: String = current[..observation.owned_start].iter().collect();
    rebased.push_str(&selected);
    rebased.extend(current[observation.owned_start + inserted_len..].iter());
    target.baseline = rebased;
    target.start = observation.owned_start;
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

/// Some WebView content-editables expose an editable AXTextArea and a valid
/// zero-length caret, but return CFNull for AXValue while the editor is empty.
/// Accept only that exact empty-editor shape. Treating any other missing value
/// as empty would discard the baseline that protects user-owned text.
#[cfg(any(target_os = "macos", test))]
fn normalize_missing_editable_value(
    value: Option<String>,
    role: &str,
    value_is_settable: bool,
    selection_start: usize,
    selection_len: usize,
) -> Option<String> {
    value.or_else(|| {
        let editable_text_role = matches!(role, "AXTextArea" | "AXTextField" | "AXComboBox");
        (editable_text_role && value_is_settable && selection_start == 0 && selection_len == 0)
            .then(String::new)
    })
}

#[cfg(any(target_os = "macos", test))]
fn is_chromium_projection(bundle_id: &str) -> bool {
    matches!(
        bundle_id,
        "com.google.Chrome"
            | "org.chromium.Chromium"
            | "com.brave.Browser"
            | "com.microsoft.edgemac"
            | "company.thebrowser.Browser"
            | "com.openai.codex"
    )
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
    use objc2_app_kit::NSRunningApplication;

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

    fn text_projection(pid: i32) -> TextProjection {
        let bundle_id = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .and_then(|application| application.bundleIdentifier())
            .map(|identifier| identifier.to_string());
        if bundle_id.as_deref().is_some_and(is_chromium_projection) {
            TextProjection::Chromium
        } else {
            TextProjection::Stable
        }
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
        let value_is_settable = element.is_settable(&AXAttribute::value()).unwrap_or(false);
        if !value_is_settable {
            return Err(ApplyOutcome::Unavailable);
        }
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
        let selection_start = range.location.max(0) as usize;
        let selection_len = range.length.max(0) as usize;
        let value = normalize_missing_editable_value(
            element
                .value()
                .ok()
                .and_then(|v| v.downcast_into::<CFString>())
                .map(|v| v.to_string()),
            &role,
            value_is_settable,
            selection_start,
            selection_len,
        )
        .ok_or(ApplyOutcome::Unavailable)?;
        let mut pid = 0;
        if unsafe { AXUIElementGetPid(element.as_concrete_TypeRef(), &mut pid) } != 0 {
            return Err(ApplyOutcome::Unavailable);
        }
        let description = element.description().ok().map(|value| value.to_string());
        let (value, selection_start) = normalize_empty_placeholder(
            value,
            description.as_deref(),
            selection_start,
            selection_len,
        );
        Ok((element, pid, value, selection_start, selection_len))
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
                projection: text_projection(pid),
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
            let exact_value_ok = if first_edit {
                current == target.baseline
            } else {
                current == expected_value(target, expected)
            };
            let exact_selection_ok = if first_edit {
                selection_start == target.start && selection_len == target.selected_len
            } else {
                selection_start == target.start + expected.chars().count() && selection_len == 0
            };
            let projection_match = (!first_edit)
                .then(|| {
                    match_projection(target, &current, expected, selection_start, selection_len)
                })
                .flatten();
            let value_ok = exact_value_ok || projection_match.is_some();
            let selection_ok = exact_selection_ok || projection_match.is_some();
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
            if let Some(observation) = projection_match {
                if observation.context_reflowed {
                    crate::structured_log(
                        "dictation-target-projection-reconciled",
                        serde_json::json!({
                            "phase": "precheck",
                            "start_shift": observation.owned_start as isize - target.start as isize,
                        }),
                    );
                    rebase_projection(target, &current, expected, observation);
                }
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
                    let projection_match = match_projection(
                        target,
                        &current,
                        replacement,
                        selection_start,
                        selection_len,
                    );
                    if (current == wanted
                        && selection_start == target.start + replacement.chars().count()
                        && selection_len == 0)
                        || projection_match.is_some()
                    {
                        if let Some(observation) = projection_match {
                            if observation.context_reflowed {
                                crate::structured_log(
                                    "dictation-target-projection-reconciled",
                                    serde_json::json!({
                                        "phase": "postcheck",
                                        "start_shift": observation.owned_start as isize - target.start as isize,
                                    }),
                                );
                                rebase_projection(target, &current, replacement, observation);
                            }
                        }
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
            projection: TextProjection::Stable,
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
            projection: TextProjection::Chromium,
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
    fn missing_value_is_accepted_only_for_a_provably_empty_editable_text_control() {
        assert_eq!(
            normalize_missing_editable_value(None, "AXTextArea", true, 0, 0),
            Some(String::new())
        );
        assert_eq!(
            normalize_missing_editable_value(None, "AXTextField", true, 0, 0),
            Some(String::new())
        );
        assert_eq!(
            normalize_missing_editable_value(Some("draft".into()), "AXTextArea", true, 5, 0),
            Some("draft".into())
        );
        assert_eq!(
            normalize_missing_editable_value(None, "AXTextArea", false, 0, 0),
            None
        );
        assert_eq!(
            normalize_missing_editable_value(None, "AXTextArea", true, 1, 0),
            None
        );
        assert_eq!(
            normalize_missing_editable_value(None, "AXButton", true, 0, 0),
            None
        );
    }

    #[test]
    fn codex_editor_uses_chromium_projection_rules() {
        assert!(is_chromium_projection("com.openai.codex"));
        assert!(!is_chromium_projection("com.apple.TextEdit"));
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

    fn chromium_target(baseline: &str, start: usize) -> TargetSnapshot {
        TargetSnapshot {
            target_id: "chrome".into(),
            scope_id: Some("window".into()),
            baseline: baseline.into(),
            start,
            selected_len: 0,
            projection: TextProjection::Chromium,
        }
    }

    #[test]
    fn chromium_projection_accepts_only_owned_text_with_synthetic_suffix_removed() {
        let mut target = chromium_target("draft\n", 5);
        let observation = match_projection(&target, "drafthello", "hello", 10, 0)
            .expect("synthetic trailing newline should be tolerated");
        assert!(observation.context_reflowed);
        rebase_projection(&mut target, "drafthello", "hello", observation);
        assert_eq!(target.baseline, "draft");
        assert_eq!(target.start, 5);
        assert!(match_projection(&target, "drafthello world", "hello world", 16, 0).is_some());
    }

    #[test]
    fn chromium_projection_tracks_a_synthetic_leading_newline_shift() {
        let mut target = chromium_target("", 0);
        let observation = match_projection(&target, "\nhello", "hello", 6, 0)
            .expect("synthetic leading newline should be tolerated");
        rebase_projection(&mut target, "\nhello", "hello", observation);
        assert_eq!(target.baseline, "\n");
        assert_eq!(target.start, 1);
    }

    #[test]
    fn chromium_projection_normalizes_dom_whitespace_outside_owned_range() {
        let target = chromium_target("hello\u{00a0}world", 6);
        assert!(match_projection(&target, "hello insertedworld", "inserted", 14, 0).is_some());
    }

    #[test]
    fn chromium_projection_rejects_user_text_and_caret_changes() {
        let target = chromium_target("draft\n", 5);
        assert!(match_projection(&target, "editedhello", "hello", 11, 0).is_none());
        assert!(match_projection(&target, "drafthello", "hello", 9, 0).is_none());
        assert!(match_projection(&target, "drafthello", "hello", 10, 1).is_none());
    }

    #[test]
    fn stable_projection_rejects_browser_only_reflow() {
        let mut target = chromium_target("draft\n", 5);
        target.projection = TextProjection::Stable;
        assert!(match_projection(&target, "drafthello", "hello", 10, 0).is_none());
    }
}
