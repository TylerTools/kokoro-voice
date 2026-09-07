# HereWord architecture

## System boundary

HereWord is one local product split into a desktop control plane and a warm
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
3. On macOS the desktop host reads `AXSelectedText` first. Web wrappers that
   expose no focused accessibility element use a user-triggered Copy fallback:
   HereWord snapshots every pasteboard item and type, clears stale content,
   captures the fresh selection, and restores the complete original pasteboard
   before synthesis. A fresh selection is streamed to `client/speak.py` over
   stdin; no selection pauses or resumes active playback.
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
The transport is deliberately plain and compact. Action notices use a wider,
taller two-line surface; they must never inherit the transport's one-line
ellipsis because the recovery action is the reason the notice exists.

## Dictation path

1. DictateStart captures the focused accessibility target before opening UI.
2. `client/dictate.py` records 16 kHz mono audio in memory and emits a small
   stdout protocol (`READY`, previews, `TRANSCRIBING`, final text, errors).
3. The engine delegates synchronous Whisper inference to a lazy child process,
   while the HTTP request itself stays off the ASGI event loop so `/health`
   remains responsive.
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
~/Library/Application Support/Kokoro Voice 2.1/engine/  # retained legacy data path
  .venv/
  models/
  active-source
  sources/
    2.1.1-beta.N-<content hash>/
      server.py
      stt_config.py
      client/
      requirements-macos.lock
```

Setup downloads pinned model artifacts, verifies their hashes, and installs the
hashed platform lockfile without dependency resolution. Large model hashes are
streamed instead of loading entire artifacts into desktop memory, and the macOS
lock intentionally excludes Torch because MLX Whisper does not use it. Startup
copies bundled sources into an immutable,
content-addressed version directory and atomically changes `active-source` only
after the copy is complete. The environment and models remain shared, but the
running server and clients always come from one complete source version. The
host then starts Uvicorn, writes its PID, and begins a watchdog. Graceful app
exit sends the managed child SIGTERM and allows its Uvicorn shutdown hook to
retire the STT worker before a bounded SIGKILL fallback. Both Tauri exit events
are covered; signal handlers cover termination paths that skip those hooks.

The host creates or repairs a private bearer token before every engine launch;
startup fails closed if the token cannot be stored as a regular `0600` file.
On macOS, Whisper runs from the app-owned Hugging Face cache below `models/`.
An existing pinned global cache is adopted with validated hard links so the app
owns its namespace without duplicating the multi-gigabyte model. The installed
bundle is replaceable and read-only. Models and the private environment survive
application updates.

Whisper is not loaded at general application startup. The main engine warms a
recyclable `tts_worker.py` child and launches `stt_worker.py` only for voiced
Dictation audio. A serialized parent/worker exchange accounts for each active request; the
same lock protects the idle timer, so expiry cannot terminate a transcription.
The first cold transcription gets a 90-second idle lease. A warm repeat extends
the current burst to 180 seconds, balancing rapid follow-up dictation against
the roughly 2.36 GB warm STT footprint. `KOKORO_STT_BASE_IDLE_SECONDS` and
`KOKORO_STT_REPEAT_IDLE_SECONDS` tune those leases; the legacy
`KOKORO_STT_IDLE_SECONDS` overrides both, and `0` retains the worker forever.
At expiry, the parent asks the worker to exit cleanly and uses bounded
terminate/kill fallbacks only if it does not respond. Process exit releases
mlx-whisper's model singleton and every associated MLX/Metal allocation.
`/health` remains healthy while reporting STT as `cold`, `loading`, `busy`,
`warm`, or `error`;
`stt_ready` describes operational readiness and `stt_warm` describes residency.
The desktop therefore does not confuse intentional STT retirement with an
incomplete startup or engine crash.

ONNX Runtime's CPU memory arena remains enabled: disabling it increased retained
memory in Candidate QC. Kokoro instead lives in a recyclable child process. The
normal short-read path stays warm; cumulative long-read text retires the worker
between requests, and the client's final-chunk marker guarantees retirement at
the end of a long session. Process exit returns ONNX and allocator memory to macOS.
Cancelled playback explicitly retires a partially filled worker as well.

Dictation audio stays in memory and is not a recording archive. Read-aloud WAV
chunks are temporary working state; completed and cancelled playback removes
them immediately. Startup
also scavenges only exact app-managed WAV/control filenames, preserves live
speaker files, refuses symlinks and unknown files, and applies an age gate.
Stable alone may clean its legacy runtime namespace; Candidate cannot touch it.
Operational logs are bounded to one current and one previous hotkey log.

### Guided first-run and permission setup

The settings window presents setup as one resumable transaction rather than
separate download, permission, and verification chores. A single **Finish
setup** action installs the local models, requests Microphone access, requests
Accessibility, opens the exact macOS pane only when a native request remains
unresolved, detects each grant, advances to Input Monitoring, and registers
shortcuts when all grants are available. The in-progress marker lives in the
settings webview's local storage so a macOS-required **Quit & Reopen** resumes
the same transaction.
Screen Recording remains a just-in-time approval. Microphone authorization is
an explicit setup and release gate because macOS can return silent audio before
the user has answered its permission prompt.

The desktop host keeps a low-frequency permission watcher alive until startup
readiness succeeds; it does not abandon setup after an arbitrary timeout. The
release manager likewise admits fresh readiness evidence from a replacement
process launched from the newly installed bundle, while retaining version,
timestamp, and bundle-path checks so a restart cannot accidentally accept an
old release's log entry.

After setup, the settings window is an operating surface rather than product
documentation: shortcut bindings and voice/dictation controls remain visible,
while privacy explanation, storage, diagnostics, and local-data removal stay
under Advanced. Routine health is expressed once in the header; background
polling must not overwrite action feedback with repeated readiness prose.

## Release channels and rollback

Stable and Candidate are separate installed products. Stable retains bundle ID
`com.tylertools.kokoro-voice-2-1`, port 8125, its existing permissions, and its
runtime directories. Candidate uses bundle ID
`com.tylertools.kokoro-voice-candidate`, port 8126, separate config and engine
directories, and launch-at-login off by default. Candidate is passive: it does
not register global hotkeys or start the Quartz input controller, so it cannot
compete with the running Stable accessibility owner.

Candidate is never renamed or promoted into Stable. Once Candidate is accepted,
the same source is built again with Stable identity. The release manager
verifies the bundle identity and signature, copies both the incoming artifact
and the prior Stable release before stopping Stable, then performs an atomic
same-directory swap. It starts the new release and waits for `/health`; failure
restores and relaunches the previous app automatically. Promotion also requires
a fresh `runtime-readiness` event from the new app's exact version and process,
proving Microphone, Accessibility, Input Monitoring, and hotkey registration.
Old log records cannot satisfy this gate. The explicit rollback command swaps
to the archived bundle and restores the small set of compatibility-sensitive settings.
Models, tokens, audio, logs, and the Python environment are not duplicated into
release archives.

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
text. Microphone access is required before dictation may start. The app checks
all three and must not report runtime readiness while any one is unavailable.

Developer builds are ad-hoc signed, so their designated requirement is the
binary CDHash. Replacing the bundle changes that identity and can invalidate
both privacy approvals. The final installed binary must receive approval after
the last rebuild. A production or local stable code-signing identity avoids
this repeated approval cycle.

Stable builds fail closed when no persistent signing identity is configured.
After the one-time transition to Developer ID, the release manager requires the
incoming and installed bundles to have the same designated requirement before
cutover. This is the identity macOS uses to carry privacy grants across
versions; matching only the bundle identifier is not sufficient.

## Verification layers

1. Pure tests: shortcut model, prefix state machine, text projections, STT
   selection, request validation, and client behavior.
2. Build gates: TypeScript/Vite, Rust formatting/tests/Clippy, Python tests,
   hashed lockfile drift, and diff whitespace.
3. Bundle gates: bundled source equality and strict local code-signature check.
4. Installed runtime: process identity, `/health`, Microphone, Accessibility,
   and Input Monitoring for the final signed binary, synchronized source,
   `hotkeys-registered`, recorder visibility, real `hotkey-triggered`, and the
   matching action event.

No lower layer substitutes for the layer above it. In particular, unit tests
cannot prove Deskflow's physical event translation.
