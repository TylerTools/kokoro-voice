# Windows development installer

[Download the exact locally verified Windows x64 installer](https://github.com/TylerTools/kokoro-voice/raw/refs/heads/main/releases/windows/Kokoro-Voice-2.1_2.1.0-beta.1_x64-setup.exe).

This is the **unsigned, locally built Kokoro Voice 2.1 beta development build**
verified on September 7, 2026, not a signed production release or a fresh rebuild.
The installer is 3,309,759 bytes; verify it against [SHA256SUMS](SHA256SUMS).

Source: [f2518c57d2b4a86b39382278b029f06c3440a07f](https://github.com/TylerTools/kokoro-voice/commit/f2518c57d2b4a86b39382278b029f06c3440a07f),
whose tree is identical to local build commit `0b60ff4ccc34050b9282f482bb38f149b85dce81`.
The audited installed EXE SHA-256 is
`7f8e5f73ee674739e5a662aa8693fa4cb0af7599a744e70efcb582058b7cc826`;
it matches the local build EXE except for Tauri's three-byte NSIS bundle marker.

Local checks passed: 37 Rust tests, 21 Python tests, release Clippy, frontend
build, NSIS packaging, and a synthetic TTS/STT round trip. Physical shortcut and
OCR verification remain separate. Speech models download during first-run setup;
models, credentials, runtime state, and caches are not included here.

Run the installer from Windows File Explorer, outside an MSIX-packaged agent's
process tree, to avoid AppData virtualization. See [Windows usage notes](../../WINDOWS-LOCAL.md).
