# Kokoro Voice desktop host

This directory contains the Tauri desktop control plane. It is not a stock
Tauri template.

## Responsibilities

- Own the engine process, setup, watchdog, tray, and floating status window.
- Capture global shortcuts and route Read, Dictate, and Snip actions.
- Capture the focused accessibility target and apply verified dictation edits.
- Present settings, permissions, storage, diagnostics, and shortcut recording.

Read the repository [agent guide](../AGENTS.md) and
[architecture](../ARCHITECTURE.md) before changing lifecycle or shortcut code.

## Layout

- `src/main.ts` — settings UI and recorder capture.
- `index.html` / `src/styles.css` — settings structure and styling.
- `player.html` — floating playback/dictation status surface.
- `src-tauri/src/lib.rs` — desktop composition root.
- `src-tauri/src/dictation_protocol.rs` — typed Python-child stdout parser.
- `src-tauri/src/hotkeys.rs` — shortcut model and recorder classification.
- `src-tauri/src/chords.rs` — authoritative macOS Quartz shortcut controller.
- `src-tauri/src/text_backend.rs` — safe accessibility insertion.
- `src-tauri/tauri.conf.json` — bundle metadata and engine resources.

## Local checks

```sh
npm run build
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

Build a macOS app bundle with:

```sh
PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH" npm run tauri -- build --bundles app
```

Do not call a shortcut fixed until the installed bundle logs
`hotkey-triggered` from a physical press and then logs the requested action.

Do not run the legacy repository `install.sh` beside this host. The desktop app
is the sole engine owner and synchronizes its own bundled Python sources.
