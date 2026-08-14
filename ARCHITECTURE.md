# Kokoro Voice architecture

## System boundary

Kokoro Voice is one local product split into a desktop control plane and a warm
Python model engine. The split keeps model ownership stable while allowing the
desktop host to manage global input, permissions, selection, and insertion.

```text
physical input
    |
    v
Tauri desktop host --------------------------+
  | Quartz input controller (macOS)          |
  | Windows complete-shortcut adapter        |
  | settings + tray + floating status        |
  | target-locked accessibility insertion    |
  +-------------------+----------------------+
                      | loopback HTTP + bearer token
                      v
              FastAPI model engine
                |             |
                v             v
             Kokoro TTS    Whisper STT
```

OCR stays in the client process because Apple Vision and Windows.Media.Ocr are
OS services. Speech models stay in the engine so they load once and remain warm.

## Desktop composition root

`app/src-tauri/src/lib.rs` wires the application together. It owns first-run
setup, engine lifecycle, Tauri commands, action orchestration, the tray, the
floating status window, and structured operational logging.

Domain-heavy behavior belongs in focused modules:

- `hotkeys.rs` owns shortcut meaning and validation;
- `chords.rs` owns macOS modifier gesture timing and Quartz injection;
- `text_backend.rs` owns accessibility target identity and safe revisions.

New behavior should enter through a module and be composed in `lib.rs`; do not
grow a second composition root in the frontend or a client script.

## Shortcut architecture

### Why macOS has one input controller

Deskflow-generated events reach the Quartz event tap, including modifier flags
and complete keydowns, but do not reliably trigger macOS's registered-hotkey
callback. Splitting modifier gestures into Quartz and complete shortcuts into
the Tauri plugin therefore creates a hole between observation and dispatch.
Quartz owns both forms on macOS; Tauri global shortcuts remain Windows-only.

| Controller | Owns | Does not own |
| --- | --- | --- |
| macOS Quartz | Every recorded two-or-more-modifier gesture; every recorded complete accelerator; Escape cancellation; injected text events | Windows shortcuts |
| Windows global shortcuts | Recorded Read, Dictate, and Snip accelerators | macOS and modifier-only gestures |

### Prefix arbitration

Any complete shortcut can begin with the same modifier state as a recorded
modifier-only Dictate binding. Starting Dictate immediately would make those
two shortcuts mutually exclusive.

The Quartz state machine now enters `DictatePending(generation)` and waits 180
milliseconds:

```text
Recorded Dictate modifiers down
    |
    +-- complete-key down before grace ends --> cancel pending Dictate
    |                                           Quartz dispatches saved action
    |
    +-- no complete key, modifiers still held --> DictateStart
                                                    |
                                                    +-- release --> DictateStop
```

Every pending timer carries a generation and recorder epoch. Releases, a
complete key, a newer gesture, or recorder suspension invalidate it. A stale
timer can never start a later Dictate session.

### Recorder transaction

The browser captures the physical event because that is where Deskflow's final
translation is visible. The backend remains authoritative for meaning and OS
registration.

```text
Record click
  -> suspend the Quartz input controller
  -> capture one accelerator
  -> classify in hotkeys.rs
  -> validate with the Tauri parser and Quartz keycode map
  -> atomically persist the modifier gesture or complete shortcut
  -> replace all Quartz bindings atomically and resume once
  -> roll back preference if registration fails
  -> UI asks for a real press
  -> hotkey-triggered proves the press reached an adapter
```

`hotkey-saved` means durable configuration. `hotkeys-registered` means the OS
accepted registration. Only `hotkey-triggered` proves the end-to-end input path.

## Read path

1. A shortcut or tray action calls `read_selection`.
2. The desktop host refuses to compete with active Dictation.
3. `client/speak.py --selection` copies the current accessible selection.
4. The client chunks text, pipelines authenticated `/speak` requests, and plays
   audio while preparing the next chunk.
5. The floating player controls pause/resume/stop without stealing focus.

On macOS the player webview is hosted in a borderless, non-activating native
panel rather than Tauri's ordinary window. Each show operation assigns its
all-Spaces, all-applications, full-screen behavior and screen-saver window level
synchronously before ordering it front. Do not route this through Tauri's
asynchronous `set_always_on_top`: that maps only to the floating-palette level,
can race the order operation, and an ordinary window remains ineligible for
another application's full-screen Space. The hidden Tauri owner retains a
placeholder content view after the player webview is transferred; Tao's window
delegate requires that invariant during resize and shutdown callbacks.

The producer and player share stop state. Stopping only the current audio
process is incorrect because a later synthesized chunk would restart playback.

## Dictation path

1. DictateStart captures the focused accessibility target before opening UI.
2. `client/dictate.py` records 16 kHz mono audio in memory and emits a small
   stdout protocol (`READY`, previews, `TRANSCRIBING`, final text, errors).
3. The engine runs synchronous Whisper inference in a worker thread so `/health`
   remains responsive and the watchdog does not kill a healthy long request.
4. Preview revisions are applied only while `text_backend.rs` proves target,
   process scope, owned text, selection, and caret invariants.
5. Any focus/manual-edit/unsupported-control mismatch permanently falls back to
   clipboard for that session.
6. Final text is always copied as recovery; secure fields are rejected.

The child protocol is stdout-only. Its stderr must not be an unread pipe because
a full pipe can deadlock a long recording.

### Dictation child protocol

`client/dictate.py` is a child process of the desktop host. Each stdout line is
one machine-readable record parsed only by `dictation_protocol.rs`; stderr is
human diagnostic output and must never be parsed as state.

| Record | Meaning |
| --- | --- |
| `RECORDING` | Microphone stream opened; push-to-talk is active. |
| `PREVIEW_FULL <text>` | Advisory transcript of the complete bounded preview. |
| `PREVIEW_ROLLING <text>` | Advisory transcript of the newest rolling window. |
| `INACTIVITY_WARNING` | Recorder is open but no recent speech was detected. |
| `MAXLEN` | The hard recording duration cap ended capture. |
| `TRANSCRIBING <seconds>` | Microphone closed; authoritative inference started. |
| `RETRYING engine` | The child retained audio while the host restarts the engine. |
| `METRICS <json>` | Non-sensitive timing data for the performance profile. |
| `TEXT <text>` | One authoritative, sanitized final transcript. |
| `CANCELLED` | The session-specific cancel control won. |
| `ERROR <message>` | Terminal failure; no final transcript follows. |

Text payloads are single-line and contain no control characters. Adding a
record requires coordinated writer, parser, parser-test, and table updates;
unknown records are retained for diagnostics but do not drive UI state.

## Snip path

1. The complete shortcut adapter or tray starts `snip_and_read`.
2. `client/snip.py` captures a user-selected region into a private temporary
   file.
3. OS-native OCR returns text locally.
4. The capture is deleted immediately.
5. Recognized text enters the normal Speak path; cancellation and permission
   failures receive distinct user-visible notices.

The floating player is not shown over the crosshair because it can obstruct or
appear inside the capture.

## Engine and installation lifecycle

The `.app` bundles small Python sources and lockfiles, not the models or private
environment. First run creates:

```text
~/Library/Application Support/Kokoro Voice/engine/
  .venv/
  models/
  server.py
  stt_config.py
  client/
  requirements-macos.lock
```

Setup downloads pinned model artifacts, verifies their hashes, and installs the
hashed platform lockfile. Startup synchronizes bundled sources into the engine
directory, starts Uvicorn, writes its PID, and begins a watchdog. Graceful app
exit stops the managed child; signal handlers cover termination paths that skip
Tauri exit hooks.

The installed bundle is replaceable and read-only. Models and the private
environment survive application updates.

The supported desktop topology has exactly one engine owner: the Tauri app.
`install.sh` and `hosts/macos/` predate that ownership model and are retained
only for manual service use and implementation history. Running their launchd
or Hammerspoon paths beside the app creates competing engine/input owners and
is unsupported.

## HTTP contract and security

- The engine binds to `127.0.0.1` by default.
- `/speak` and `/transcribe` require the private bearer token.
- Request bodies and text lengths are bounded before expensive work.
- Long TTS input is segmented below Kokoro's context limit and concatenated.
- Whisper runs offline from pinned local model revisions.
- Logs contain state, timing, counts, and error categories—not text, audio,
  screenshots, selections, transcripts, or tokens.

## Operational evidence

| Evidence | Meaning |
| --- | --- |
| `/health` | Engine process, models, and backend readiness. |
| `events.jsonl` | Structured desktop lifecycle, registration, triggers, and action states. |
| `hotkey.log` | Raw Quartz flags, rejected keydowns, prefix arbitration, and dispatched actions. |
| `kokoro.err.log` | Engine stderr and model failures. |
| Settings system check | User-readable permissions and capability status. |

Historical log corruption can remain from older builds. New records are guarded
by a process-local write lock; validate only records appended by the candidate
build when testing a repair.

## macOS privacy and local signing

The Quartz controller requires Input Monitoring to receive key events.
Accessibility is separately required to read selections and insert dictated
text. The app checks both and must not report shortcuts as registered while
Input Monitoring is unavailable.

Developer builds are ad-hoc signed, so their designated requirement is the
binary CDHash. Replacing the bundle changes that identity and can invalidate
both privacy approvals. The final installed binary must receive approval after
the last rebuild. A production or local stable code-signing identity avoids
this repeated approval cycle.

## Verification layers

1. Pure tests: shortcut model, prefix state machine, text projections, STT
   selection, request validation, and client behavior.
2. Build gates: TypeScript/Vite, Rust formatting/tests/Clippy, Python tests,
   hashed lockfile drift, and diff whitespace.
3. Bundle gates: bundled source equality and strict local code-signature check.
4. Installed runtime: process identity, `/health`, Accessibility and Input
   Monitoring for the final signed binary, synchronized source,
   `hotkeys-registered`, recorder visibility, real `hotkey-triggered`, and the
   matching action event.

No lower layer substitutes for the layer above it. In particular, unit tests
cannot prove Deskflow's physical event translation.
