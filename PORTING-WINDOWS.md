# Windows release status and acceptance contract

Kokoro Voice is one Tauri desktop application with one local authenticated
engine contract on macOS and Windows. Windows is a platform adapter, not a
separate Python service, PowerShell daemon, or AutoHotkey installation.

## Supported target

- Windows 11 x64.
- CPU-only systems and supported NVIDIA CUDA systems.
- Per-user installation with no system Python requirement.
- Local processing only after first-run runtime and model downloads complete.

The Windows installer and the desktop app own the engine lifecycle. The app
must prevent duplicate app and engine instances, register launch-at-login with
the platform-native mechanism, and keep its runtime, models, preferences,
tokens, and logs in the user's application-data directory.

## Shared contracts

The following remain identical across platforms:

- `/health`, `/voices`, `/speak`, and `/transcribe` service contracts.
- Authenticated loopback-only service binding.
- Session-specific recording stop and cancellation signals.
- 120-second recording watchdog and bounded microphone-open deadline.
- Complete transcript on the clipboard for every successful session.
- No audio, transcript, selected text, clipboard content, or token in logs.
- Pinned, hash-verified setup assets and resumable model downloads.

## Windows adapters

### Hotkeys

- `Ctrl+Alt+R`: read selected text.
- Hold `Ctrl+Alt+W`: dictate; release stops the same recording session.
- `Ctrl+Alt+D`: snip and read.
- Failed registration must leave the last working binding active.
- The UI, tray, preferences, and registered accelerator must report the same
  authoritative binding.

### Dictation and text ownership

Password, secure, read-only, and non-editable controls are rejected before the
microphone opens. UI Automation must capture a stable process, window, control,
selection, and editable range before recording.

Every partial and final revision must revalidate the original control, the
owned range, its expected text, and the caret before editing. Focus movement,
manual edits, caret movement, unsupported controls, or failed post-validation
permanently switch that session to clipboard fallback. No recovery path may
undo user edits or modify text outside Kokoro's verified range.

Until the UI Automation owned-range implementation passes physical testing,
Windows dictation remains deliberately clipboard-only. It must not be described
or released as live-edit parity before that gate passes.

### Speech recognition

Windows uses `faster-whisper` at a pinned model revision:

- Probe NVIDIA/CUDA and attempt `float16` first.
- If CUDA discovery, library loading, warmup, or execution fails, retry on CPU.
- Benchmark supported CPU compute types during setup and persist the fastest
  valid result with its validation time.
- Warm the selected backend before reporting `stt_ready`.

### OCR and playback

- OCR uses `Windows.Media.Ocr` and never uploads the captured image.
- Playback stays in one long-running process using `sounddevice`; no
  PowerShell `SoundPlayer` process is spawned per speech chunk.
- Pause, resume, stop, simultaneous dictation, and chunk-gap behavior require
  physical acceptance evidence.

## Installer and first run

The signed installer is downloaded from GitHub Releases. First run downloads a
pinned private Python manager, installs the hash-locked Windows dependencies,
downloads pinned models, verifies SHA-256 and expected sizes, and atomically
promotes completed `.part` files. Cancellation and interruption must resume
without deleting valid progress.

Uninstall removes the app and autostart registration and offers an explicit
retain-or-remove choice for models, preferences, and diagnostics.

## Release gates

CI compilation is necessary but insufficient. A Windows release remains a
draft until all of the following are recorded:

- Rust, frontend, Python, service, packaging, signature, install, launch, and
  uninstall checks pass on a Windows x64 runner.
- Fresh-machine setup succeeds without Python, uv, models, preferences, or
  prior permissions, followed by an offline relaunch.
- Physical CPU-only and supported NVIDIA machines pass read-aloud, push-to-talk
  dictation, final reconciliation or safe fallback, OCR, audio controls,
  microphone selection, reboot/autostart, update recovery, and uninstall.
- Permission denial/revocation, disconnected or changed microphone, rapid
  release, missed release, long hold, focus change, caret movement, manual
  edits, engine crash, and secure fields all produce the specified safe state.
- Installer Authenticode verification and published SHA-256 match.
- The compatibility matrix and measured latency results are attached to the
  release checklist.

Synthetic keystrokes, process existence, configuration inspection, or a
successful cross-compile do not count as physical acceptance.
