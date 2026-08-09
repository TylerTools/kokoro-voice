# Privacy

Kokoro Voice performs speech synthesis, speech recognition, and OCR locally.
It has no account system, cloud transcription, telemetry, advertising, or
automatic diagnostic upload.

## Network access

Network access is used during first-run setup to download the pinned private
Python manager, dependencies, and model snapshots. It is also used to check for
approved application updates. After successful setup, speech, dictation, and
OCR operate without sending their content to a remote service.

## Local data

The application stores its private runtime, downloaded models, preferences,
hotkey configuration, authentication token, setup state, hardware profile, and
bounded diagnostic logs in the current user's application-data directories.
Recorded audio remains in memory for the active dictation session and is not
written to disk by the application.

The completed dictation transcript is placed on the clipboard as a recovery
copy. Kokoro Voice does not maintain transcript history. Other applications and
the operating system may independently retain clipboard or accessibility data.

## Diagnostics

Logs contain state transitions, timing, platform/backend information, redacted
device identifiers, outcome codes, and recovery actions. They must not contain
audio, transcripts, selected text, clipboard contents, authentication tokens,
environment dumps, or full command lines.

Diagnostics remain local unless the user explicitly exports the redacted file
and chooses where to send it.

## Removal

Settings provide controls to remove downloaded runtime/models and optionally
preferences and diagnostics. Uninstall removes application binaries and
autostart registration and must offer a clear retain-or-remove choice for local
models and preferences.
