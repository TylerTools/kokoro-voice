# Kokoro Voice update path

## Goal

After one intentional signing transition, every later update should download,
verify, replace, relaunch, and retain macOS Accessibility and Input Monitoring
without asking the user to reconnect the app.

## Required identity

Stable must use one persistent `Developer ID Application` identity. macOS
records privacy grants against the app's designated requirement, not its
filename. Ad-hoc signing embeds a changing code hash, so it cannot support
permission-preserving updates.

The current machine has no code-signing identity installed. The one-time setup
is therefore:

1. Create or install a `Developer ID Application` certificate and private key.
2. Build Stable with `KOKORO_CODESIGN_IDENTITY` or `APPLE_SIGNING_IDENTITY`.
3. Promote once with `--allow-signing-transition` and approve macOS permissions.
4. Keep that team and designated requirement for every future release.

The release manager rejects later artifacts whose team or designated
requirement differs, archives the prior app, swaps atomically, verifies the
matching engine and permissions, and automatically restores the prior release
if a gate fails.

## Download-and-install phase

Once the Developer ID identity exists, add Tauri's signed updater using a
stable HTTPS or GitHub Releases endpoint. This requires two independent trust
layers:

- Apple Developer ID signing and notarization identify the app to macOS and
  preserve privacy grants.
- Tauri's updater key signs the downloadable archive so the running app can
  reject a modified update before installation.

The updater private key and Apple credentials belong in Keychain or release
secrets, never in this repository. The public updater key and HTTPS endpoint can
then be embedded in the app. Release acceptance remains Candidate QC first,
followed by a signed Stable artifact and the existing transactional rollback.

The updater is deliberately not enabled with placeholder credentials or an
unowned endpoint: doing so would create an update button that cannot verify or
retrieve a real release. The current checkout has no persistent Apple signing
identity and its Git remote is local-only, so those are the two remaining
release-infrastructure inputs.
