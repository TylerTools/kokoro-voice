# HereWord release process

This workflow keeps the installed Stable app running throughout development.
Installing, promoting, or rolling back remains a deliberate operator action.

## 1. Build and test Candidate

```sh
scripts/release/build-candidate.sh
scripts/release/verify-macos-bundle.sh \
  "app/src-tauri/target/release/bundle/macos/HereWord Candidate.app" \
  --channel candidate
```

Candidate is a reusable passive app with its own bundle ID, port, engine/config
directories, and launch-at-login state. It does not register global hotkeys or
start the macOS input controller, so it cannot compete with Stable. Use it for
the UI, engine, settings, relaunch, resource lifecycle, and forced engine-restart
checks. Accessibility, Input Monitoring, and physical shortcuts are deliberately
reserved for the transactional Stable cutover.

Candidate acceptance also requires a resource-lifecycle check on Apple Silicon:

1. Confirm `/health` reports STT `cold` and record the TTS-only footprint before
   the first dictation.
2. Dictate once and record cold-start latency plus the main and worker process
   footprints. Dictate again inside the reported base lease and verify it stays
   warm; `/health` should then report the longer repeat lease.
3. Wait for the currently reported idle expiry and confirm the worker exits, `/health`
   returns STT to `cold`, and memory returns close to the TTS-only baseline.
4. Repeat at least twice. No request may be interrupted and the post-idle
   baseline must not grow across cycles.

For a faster acceptance test only, start Candidate with a shorter legacy
`KOKORO_STT_IDLE_SECONDS`, which overrides both adaptive leases. Production
defaults are 90 seconds after a cold request and 180 seconds after a warm repeat.
A value of 0 intentionally keeps the worker warm indefinitely.

Candidate health is accepted only when bearer authentication is required, the
service version matches the bundle, and `stt_cache_mode` is `owned`. This keeps
promotion from accepting a process that silently fell back to a global model
cache or started without write authentication.

## 2. Build the accepted code as Stable

Build Stable from the exact commit tested as Candidate:

```sh
KOKORO_CODESIGN_IDENTITY="Developer ID Application: Example (TEAMID)" \
  scripts/release/build-stable.sh
scripts/release/verify-macos-bundle.sh \
  "app/src-tauri/target/release/bundle/macos/HereWord.app" \
  --channel stable
```

Stable builds fail closed when neither `KOKORO_CODESIGN_IDENTITY` nor
`APPLE_SIGNING_IDENTITY` is present. `KOKORO_ALLOW_AD_HOC_STABLE=1` is an
explicit development escape hatch and must be expected to require macOS
permission repair.

Do not rename Candidate into Stable. Its identity and runtime namespace are
intentionally incompatible with promotion.

## 3. Promote

```sh
scripts/release/promote-macos.sh \
  --allow-signing-transition \
  "app/src-tauri/target/release/bundle/macos/HereWord.app"
```

Promotion verifies the bundle, archives current Stable and selected settings,
stages the update beside the installed app while Stable remains available, and
then quits, atomically swaps, and relaunches. It waits up to 90 seconds for the
matching-version engine health endpoint. It also requires a fresh readiness
event from that exact Stable process proving Accessibility, Input Monitoring,
and hotkey registration. If any gate fails, it automatically restores the prior
app.

The default policy rejects ad-hoc-signed promotion because a changing signing
identity can invalidate macOS privacy permissions. `--allow-ad-hoc` exists only
for an intentional local test; it should not be the normal release path.
Without `KOKORO_CODESIGN_IDENTITY`, Stable builds stop before compiling. Setting
`KOKORO_ALLOW_AD_HOC_STABLE=1` produces an ad-hoc local artifact that can be
tested but is not eligible for normal promotion.
The one-time transition from the currently ad-hoc-signed Stable app to a
Developer ID build can reset macOS privacy permissions, so promotion requires
the explicit `--allow-signing-transition` acknowledgement. Later releases must
match that Developer ID team and the installed app's designated requirement;
they do not use the transition flag.
During that one-time transition the updater waits up to three minutes for the
privacy permissions to be granted; Stable rechecks them without creating a
second input owner. Normal signed updates use a 30-second readiness window.

For an intentional local ad-hoc update whose new code requirement must be
re-added in macOS Privacy & Security, `--defer-accessibility-check` commits only
after matching engine health. Repair Accessibility and Input Monitoring
immediately, validate a fresh `runtime-readiness` event, and run rollback if
either grant cannot be restored.

## 4. Roll back

```sh
scripts/release/rollback-macos.sh
```

Rollback verifies the archived app, snapshots the release being replaced,
atomically swaps back, restores compatibility-sensitive settings, and checks
health. It preserves the replaced release as the next rollback target.

Release archives and state live in:

```text
~/Library/Application Support/Kokoro Voice Release Manager/
```

That legacy internal directory name is intentionally retained so existing
rollback state survives the public HereWord rename.

The archive excludes the bearer token, logs, generated audio, models, and the
Python environment. Engine sources are independently versioned beneath Stable's
Application Support directory so a partial source copy cannot mix releases.
