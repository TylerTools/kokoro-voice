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
//! `None` is not an error. It means "ask the clipboard instead", and every
//! caller must keep that fallback: plenty of controls expose no text pattern.

/// The selected text, or `None` when the platform cannot answer.
#[cfg(target_os = "windows")]
pub fn focused_selection() -> Option<String> {
    platform::focused_selection()
}

/// macOS keeps its existing clipboard path; other platforms have none.
#[cfg(not(target_os = "windows"))]
pub fn focused_selection() -> Option<String> {
    None
}

#[cfg(target_os = "windows")]
mod platform {
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId,
    };

    pub fn focused_selection() -> Option<String> {
        unsafe {
            // This runs on whichever thread the hotkey landed on, which may not
            // have an apartment yet. An already-initialized apartment reports an
            // error that is not a failure for us, so the result is discarded.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
            let focused = automation.GetFocusedElement().ok()?;
            // Never read a password out loud, and never route it through the
            // clipboard fallback either — refuse the whole action instead.
            if focused
                .CurrentIsPassword()
                .map(|value| value.as_bool())
                .unwrap_or(false)
            {
                return None;
            }
            let pattern: IUIAutomationTextPattern =
                focused.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
            let ranges = pattern.GetSelection().ok()?;
            // A selection is one range in ordinary controls, but tables and
            // some editors report a disjoint set; concatenating matches what a
            // copy would have produced.
            let mut text = String::new();
            for index in 0..ranges.Length().ok()? {
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
                None
            } else {
                Some(trimmed.to_string())
            }
        }
    }
}
