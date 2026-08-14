# Windows implementation and release notes

## Status and authority

Windows uses the same Tauri desktop host, local FastAPI engine, and Python
clients as macOS. This is not a proposal for a separate Python, AutoHotkey, or
tray host. Current code and tests are authoritative; this document records only
the platform boundary and the physical evidence still required for release.

Current implementation status:

- Tauri owns setup, engine lifecycle, registered shortcuts, tray, and UI.
- `stt_config.py` selects CUDA float16 first, then the persisted CPU benchmark.
- `client/speak.py` and `client/dictate.py` are cross-platform.
- `client/snip.py` uses Windows.Media.Ocr through PowerShell.
- Windows dictation is clipboard-only until a target-owned UI Automation range
  adapter passes the same safety tests as macOS.
- CI compiles and tests Windows, but CI is not physical hotkey, audio, OCR, or
  installer evidence. Do not describe Windows as released from CI alone.

## Shared topology

```text
physical shortcut
    -> Tauri Windows shortcut adapter
    -> Read / Dictate / Snip orchestration
    -> local authenticated engine at 127.0.0.1:8123
```

The desktop app is the only engine owner. Do not add Task Scheduler, a Windows
service, AutoHotkey, or another tray process beside it.

## Platform boundaries

| Concern | Shared implementation | Windows-specific implementation |
| --- | --- | --- |
| Setup and engine ownership | `app/src-tauri/src/lib.rs` | uv/Python executable paths and process flags |
| Shortcut meaning/defaults | `hotkeys.rs` | Tauri global-shortcut registration |
| Read-aloud client | `client/speak.py` | clipboard access and playback branch |
| Dictation capture/protocol | `client/dictate.py`, `dictation_protocol.rs` | sounddevice selection and clipboard fallback |
| STT selection | `stt_config.py`, `server.py` | CUDA/CPU candidates and CPU compute benchmark |
| OCR flow | `client/snip.py` | Windows.Media.Ocr and screen-clip capture |
| Safe text insertion | `text_backend.rs` contract | UI Automation adapter remains intentionally unavailable |

Keep OS-specific code behind these adapters. Do not add `platform.system()` or
`cfg!(windows)` branches to the settings UI to compensate for a backend gap.

## Measured constraints to preserve

- MLX is Apple-only. Windows uses faster-whisper with the pinned repository and
  revision in `stt_config.py`.
- CUDA uses float16. CPU setup benchmarks int8 versus float32 and persists the
  faster result; do not assume one compute type wins on every x86 machine.
- The Whisper model must warm in a background thread so `/health` stays useful
  and first dictation does not absorb cold-start latency.
- `/speak` and `/transcribe` remain loopback-only, bearer-authenticated, and
  request-capped.
- Dictation audio remains in memory. No temporary WAV may be introduced.
- Playback and its synthesis producer are one lifecycle. Stop/pause must reach
  both or a later chunk can restart audio.
- Synthetic shortcut tests do not prove physical key handling.

## Text insertion safety gate

Clipboard-only is the intentional Windows behavior today. Direct insertion may
ship only after a UI Automation TextPattern/TextRange adapter can prove:

1. the same process and control still own focus;
2. the original value and selection still match;
3. only Kokoro-owned text is revised;
4. caret movement, manual edits, focus changes, and secure fields fail closed;
5. post-insertion value and caret are verified;
6. every mismatch copies the final transcript without deleting field text.

Do not replace this with blind SendKeys or paste. Immediate insertion is less
important than never editing the wrong control.

## Physical release gate

- [ ] CPU-only and NVIDIA setup complete from a clean Windows x64 machine.
- [ ] CPU benchmark result and selected compute type are recorded.
- [ ] Offline relaunch works after setup and reboot/autostart.
- [ ] Physical Read, Dictate press/release, Snip, pause/resume, and stop pass.
- [ ] Notepad, Chrome contenteditable, VS Code, Word, Outlook, and a secure field
      match `COMPATIBILITY_MATRIX.md`.
- [ ] Microphone denied, revoked, missing, changed, and stalled-open cases show
      the correct state.
- [ ] Windows.Media.Ocr cancellation, permission, no-text, and success paths pass.
- [ ] Installer signature, hash, upgrade, uninstall, and retained-data behavior
      pass `RELEASE_CHECKLIST.md`.
- [ ] Diagnostic export contains no transcript, clipboard content, audio, token,
      environment dump, or full process command line.

Record the physical results in `COMPATIBILITY_MATRIX.md`; do not convert pending
cells to passed without machine, application/control, and evidence details.
