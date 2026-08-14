# Kokoro Voice agent and developer guide

## Kokoro Voice 2.1 isolation boundary

This checkout is the isolated Kokoro Voice 2.1 development line. Never edit,
replace, stop, re-sign, or test through `/Applications/Kokoro Voice 2.app` or the
original `/Users/tylerthompson/tts/kokoro-service` checkout.

Kokoro Voice 2.1 owns only:

- bundle identifier `com.tylertools.kokoro-voice-2-1`;
- loopback port `8125`;
- `~/Library/Application Support/Kokoro Voice 2.1`;
- `~/.config/kokoro-voice-2-1`;
- its own `kokoro-voice-2-1-*` temporary state directory;
- its own launch-at-login registration and complete-key shortcuts.

Do not launch or install a candidate until `tests/test_variant_isolation.py`
passes. Keep the original app running during source work. A candidate may run
beside it only after its bundle, process, port, paths, and shortcuts have been
verified as distinct.

This file is the operating contract for anyone changing this repository. Read
it together with [ARCHITECTURE.md](ARCHITECTURE.md) before editing desktop
shortcuts, process lifecycle, live dictation, setup, or release code.

## Product contract

Kokoro Voice is a local desktop accessibility app with three user actions:

1. **Read** — speak the selected text.
2. **Dictate** — record while held, transcribe locally, and insert safely.
3. **Snip** — OCR a selected screen region and speak it.

The installed desktop app owns the Python engine process and all global input.
The engine binds only to loopback, requires its local bearer token for writes,
and performs no outbound network requests after setup.

## Source map

| Path | Ownership |
| --- | --- |
| `server.py` | FastAPI TTS/STT engine and HTTP contract. |
| `stt_config.py` | Pure cross-platform STT backend selection. |
| `client/speak.py` | Read-aloud selection, chunking, playback, and stop state. |
| `client/dictate.py` | Microphone capture, preview protocol, stop/cancel files, and transcription client. |
| `client/snip.py` | Private temporary capture and OS-native OCR. |
| `app/src/main.ts` | Settings UI, setup controls, recorder capture, and runtime status messages. |
| `app/src-tauri/src/lib.rs` | Desktop composition root: engine lifecycle, commands, tray, action orchestration, and setup. |
| `app/src-tauri/src/dictation_protocol.rs` | Typed parser for the Python dictation child's stdout contract. |
| `app/src-tauri/src/hotkeys.rs` | Shortcut domain model, defaults, display contract, and recorder classification. |
| `app/src-tauri/src/chords.rs` | macOS Quartz adapter for modifier-only Read/Dictate gestures and synthetic text events. |
| `app/src-tauri/src/text_backend.rs` | Target-locked accessibility insertion and browser projection verification. |
| `app/index.html` / `app/player.html` | Settings and floating-status markup. |
| `tests/` | Python service/client contract tests. Rust unit tests live beside their modules. |
| `.github/workflows/ci.yml` | Cross-platform build, test, lockfile, and packaging gates. |
| `requirements-*.lock` | Fully resolved, hashed first-run environment inputs. |

`hosts/macos/` is the legacy Hammerspoon host. Do not copy new behavior into it
unless the task explicitly concerns that legacy integration.

## Authority and legacy boundaries

| Surface | Status | Rule |
| --- | --- | --- |
| `app/` | Authoritative desktop product | Put all current lifecycle, shortcuts, permissions, and UI work here. |
| `server.py`, `stt_config.py`, `client/` | Authoritative engine/client runtime | Keep the HTTP and child-process contracts cross-platform. |
| `install.sh` | Legacy/manual service installer | Never run beside the macOS app; it creates a second engine owner on the same port. |
| `hosts/macos/` | Superseded Hammerspoon host | Reference measured lessons only; do not implement current features here. |
| `PORTING-WINDOWS.md` | Historical design record | Current Windows work belongs in the Tauri host and platform adapters. |
| `smoke_test.py`, `benchmark_stt.py` | Developer measurement tools | Not runtime entrypoints and never bundled. |

Only one process owner may manage the engine. For the supported desktop
product that owner is `/Applications/Kokoro Voice.app`; a launchd service from
`install.sh` or Hammerspoon host running alongside it is a configuration error.

## Failure routing

Start at the first boundary that diverged. Do not change neighboring systems
until evidence points there.

| Symptom | Inspect first | Do not start by changing |
| --- | --- | --- |
| Shortcut does not trigger | `hotkeys.rs`, `chords.rs`, registration/runtime events | STT, TTS, or frontend labels |
| Records but does not insert | macOS Accessibility grant, `text_backend.rs`, dictation events | microphone capture or Whisper |
| Records but produces no transcript | `client/dictate.py`, `/transcribe`, engine logs | Quartz injection |
| Read action triggers but audio fails | `client/speak.py`, `/speak`, playback state | shortcut recorder |
| Snip opens but OCR fails | `client/snip.py`, Screen Recording, native OCR result | TTS model setup |
| App starts but engine is down | `Paths`, engine sync/start/watchdog in `lib.rs`, `/health` | global shortcut bindings |
| Recorder saves but physical key fails | Quartz/Tauri adapter event path and `hotkey-triggered` | preference JSON directly |

The dictation child stdout is a versioned-by-code protocol, not a log stream.
Add or change records in `client/dictate.py`, document them in
`ARCHITECTURE.md`, and update `dictation_protocol.rs` plus its tests in the same
change. Diagnostic details belong on stderr.

## Naming and labeling standard

Use these terms consistently:

| Term | Meaning |
| --- | --- |
| action | One product operation: Read, Dictate, or Snip. |
| slot | The saved configuration position for an action. |
| gesture | A modifier-only macOS interaction with press/release semantics. |
| accelerator | A complete modifier-plus-key string such as `Shift+Command+KeyZ`. |
| binding | The runtime association between an accelerator and an action. |
| recorder | The settings transaction that captures and saves a binding; not microphone recording. |
| session | One dictation lifecycle with a unique stop/cancel namespace. |
| target | The accessibility control and owned text range captured before dictation. |
| preview | Advisory partial STT output that may be revised. |
| final transcript | The single authoritative `TEXT` protocol record. |
| engine | The warm FastAPI process that owns TTS/STT models. |
| client | A short-lived Python process that captures input or consumes engine output. |

Every new module must begin with a short ownership statement: what it owns,
what it does not own, and the invariant that would make a wrong edit dangerous.
Comments should explain boundary decisions and failure behavior, not narrate
syntax. Centralize event names, protocol prefixes, defaults, and status enums;
do not repeat string contracts across call sites.

## Shortcut invariants

macOS uses one Quartz input controller:

- Quartz owns every configurable modifier-only gesture and complete
  accelerator. Modifier-only gestures require at least two distinct modifiers;
  a Windows/Super key arriving through Deskflow is stored as Command.
- Tauri global shortcuts are Windows-only. Deskflow-generated key events reach
  Quartz but do not reliably trigger macOS's registered-hotkey callback; never
  route macOS actions back through that split path.

Any modifier-only Dictate chord can be a prefix of a complete shortcut using
the same modifiers. The Quartz controller must leave a short grace period
before starting Dictate. A
non-modifier keydown during that period cancels Dictate and dispatches the
complete shortcut in the same event tap. Never remove this arbitration without
replacing its tests and proving the physical-key path.

The settings recorder follows one transaction:

1. Suspend the input controller.
2. Capture one canonical accelerator.
3. Validate against the action model and platform parser.
4. Persist only complete accelerators.
5. Replace all complete bindings atomically and resume the controller once.
6. Roll back preferences if registration fails.
7. Ask the user to press the shortcut and wait for `hotkey-triggered` before
   claiming end-to-end success.

Do not equate `hotkey-saved` or `hotkeys-registered` with a working physical
shortcut. The proof event is `hotkey-triggered` followed by the action event.

## Runtime copies and state

There are three different artifacts; always identify which one was verified:

1. Repository source: this checkout.
2. Installed bundle: `/Applications/Kokoro Voice 2.1.app`.
3. Synced engine source: `~/Library/Application Support/Kokoro Voice 2.1/engine`.

User state is under `~/.config/kokoro-voice-2-1/`:

- `prefs.json` — voice, speed, microphone, and complete shortcut fallbacks.
- `token` — private loopback bearer token; never print it.
- `events.jsonl` — structured lifecycle/action events without user text.
- `hotkey.log` — raw modifier state transitions and arbitration diagnostics.
- `engine.pid` — current managed engine PID.

The private environment and models remain in Application Support across app
updates. Startup synchronizes bundled Python sources before launching the
engine. Never write into the installed `.app` at runtime; it breaks signing.

Local ad-hoc signatures use the binary CDHash as their designated requirement.
Every rebuilt binary therefore appears to macOS privacy controls as a different
app and can lose Accessibility/Input Monitoring approval. Never claim a local
reinstall works until the final installed binary passes System Check; do not
rebuild again after Tyler grants the final binary unless another grant is
expected. A stable signing identity removes this development-only churn.

## Required change discipline

- Preserve unrelated dirty work. This repository may contain active local
  changes that are not yours.
- Use `apply_patch` for source and documentation edits.
- Keep OS-specific behavior behind explicit adapters; do not spread `cfg!`
  branches through the UI.
- Keep preference decoding in `hotkeys.rs`. Do not introduce another shortcut
  default or display formatter elsewhere.
- Never log selected text, dictated text, audio, tokens, or OCR results.
- Do not weaken loopback binding, bearer authentication, request limits,
  target-lock checks, secure-field rejection, or offline model pins.
- Do not mark a shortcut working from unit tests alone. Verify the installed
  bundle and runtime events.
- Do not run `install.sh` on a machine using the desktop app. Two engine owners
  will contend for port 8123 and produce misleading health/lifecycle failures.
- Local bundle replacement is recoverable. Public signing, notarization,
  publishing, or release upload requires separate authority.

## Verification commands

Run from the repository root unless noted:

```sh
uv run --python .venv/bin/python python -m unittest discover -s tests -v
cd app && npm run build
cargo fmt --check --manifest-path app/src-tauri/Cargo.toml
cargo test --manifest-path app/src-tauri/Cargo.toml
cargo clippy --manifest-path app/src-tauri/Cargo.toml --all-targets -- -D warnings
git diff --check
```

For a local macOS bundle:

```sh
cd app
PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$PATH" npm run tauri -- build --bundles app
codesign --verify --deep --strict "src-tauri/target/release/bundle/macos/Kokoro Voice 2.1.app"
```

After installing, verify `/health`, Accessibility and Input Monitoring for the
final binary, the `mac-quartz-controller` registration event, all three visible
recorder buttons, a real `hotkey-triggered` event, and the matching action event.
Keep a recoverable copy of the previous installed bundle until that evidence
exists.
