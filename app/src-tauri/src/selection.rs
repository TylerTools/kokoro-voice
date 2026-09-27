//! Reads what the user has selected, straight from the platform's
//! accessibility layer.
//!
//! Owns: answering "what text is selected right now" for the Read action.
//! Does not own: the clipboard, playback, or the dictation target lock.
//!
//! The clipboard route this backs up copies by synthesizing Ctrl+C. That is
//! destructive — it overwrites whatever the user had copied — and it is
//! unreliable, because a modifier-only shortcut is still physically held when
//! Read fires, so the keystroke arrives carrying extra modifiers and copies
//! nothing. macOS has never had that problem for dictation because it reads
//! `kAXSelectedTextRange` directly; this is the same idea on Windows.
//!
//! `Unavailable` means "ask the clipboard instead": plenty of controls expose
//! no text pattern. `Secure` is deliberately distinct so password fields can
//! never fall through to the clipboard fallback.

pub enum Selection {
    Selected(String),
    Secure,
    Unavailable,
}

/// The selected text or the reason direct capture cannot be used.
pub fn focused_selection() -> Selection {
    platform::focused_selection()
}

#[cfg(target_os = "windows")]
mod platform {
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId,
    };

    use super::Selection;

    pub fn focused_selection() -> Selection {
        unsafe {
            // This runs on whichever thread the hotkey landed on, which may not
            // have an apartment yet. An already-initialized apartment reports an
            // error that is not a failure for us, so the result is discarded.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let Ok(automation): Result<IUIAutomation, _> =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
            else {
                return Selection::Unavailable;
            };
            let Ok(focused) = automation.GetFocusedElement() else {
                return Selection::Unavailable;
            };
            // Never read a password out loud, and never route it through the
            // clipboard fallback either — refuse the whole action instead.
            if focused
                .CurrentIsPassword()
                .map(|value| value.as_bool())
                .unwrap_or(false)
            {
                return Selection::Secure;
            }
            let Ok(pattern): Result<IUIAutomationTextPattern, _> =
                focused.GetCurrentPatternAs(UIA_TextPatternId)
            else {
                return Selection::Unavailable;
            };
            let Ok(ranges) = pattern.GetSelection() else {
                return Selection::Unavailable;
            };
            // A selection is one range in ordinary controls, but tables and
            // some editors report a disjoint set; concatenating matches what a
            // copy would have produced.
            let mut text = String::new();
            let Ok(length) = ranges.Length() else {
                return Selection::Unavailable;
            };
            for index in 0..length {
                if let Ok(range) = ranges.GetElement(index) {
                    if let Ok(part) = range.GetText(-1) {
                        text.push_str(&part.to_string());
                    }
                }
            }
            let trimmed = text.trim();
            if trimmed.is_empty() {
                // An empty selection is indistinguishable from "this control
                // cannot tell us", and the clipboard may still hold something
                // worth reading, so defer rather than declaring nothing.
                Selection::Unavailable
            } else {
                Selection::Selected(trimmed.to_string())
            }
        }
    }
}
