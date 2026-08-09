# Release gate

A draft release must not be published until every item is recorded as passed.

- [ ] CI passes on macOS ARM64 and Windows x64.
- [ ] macOS signature and notarization verified with `codesign` and `spctl`.
- [ ] Windows Authenticode signature verified on the installer and installed executable.
- [ ] Installer SHA-256 hashes recorded in the release.
- [ ] Fresh-machine first-run setup succeeds with no Python, uv, models, config, or prior permissions.
- [ ] macOS physical tests pass locally and through Deskflow: read, hold/release dictation, snip, stop, pause/resume.
- [ ] Windows physical tests pass on CPU-only and NVIDIA machines.
- [ ] Microphone denied, granted, revoked, missing, changed, and stalled-open cases report the correct state.
- [ ] Rapid release, missed release, 120-second watchdog, repeated dictation, and playback duck/resume pass.
- [ ] Offline relaunch works after setup; reboot, single-instance behavior, update, and uninstall pass.
- [ ] Time-to-first-audio, transcription latency, and inter-chunk gap results are attached.
- [ ] Diagnostic export contains no token, transcript, clipboard content, or full environment dump.
- [ ] Compatibility matrix is complete; every unsupported editor proves clipboard fallback without text deletion.
- [ ] Focus change, caret movement, and manual edits never modify text outside Kokoro's owned range.
- [ ] Secure/password fields reject dictation before microphone capture.
- [ ] Pinned runtime and model downloads pass hash, interruption, resume, and atomic-install tests.
- [ ] Prompted updater metadata and artifacts verify with the release updater key; previous signed installer recovery passes.
- [ ] Local structured logs rotate and the seeded-secret privacy scan passes.
