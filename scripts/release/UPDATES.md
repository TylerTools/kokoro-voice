# HereWord updates

Settings includes **Check for updates** and **Update and restart**. Signed
release builds pin the updater public key. Candidate and ordinary local builds
cannot install updates. Downloads require HTTPS and a valid Tauri updater
signature. Updating waits until reading and dictation are idle.

## One-time signing setup

Apple Developer membership must have an exported **Developer ID Application**
certificate and its private key. The existing release workflow expects these
GitHub Actions secrets:

- `APPLE_CERTIFICATE`: base64 encoded exported `.p12` certificate/private key.
- `APPLE_CERTIFICATE_PASSWORD`: password protecting the export.
- `APPLE_SIGNING_IDENTITY`: full Developer ID Application identity.
- `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`: notarization credentials;
  `APPLE_PASSWORD` is an app-specific password.

Generate a separate updater key using `npm run tauri -- signer generate` from
`app/`. Keep its private key in a password manager and the GitHub secret
`TAURI_SIGNING_PRIVATE_KEY`; store its password in
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Put the generated public key in the
repository Actions variable `HEREWORD_UPDATE_PUBLIC_KEY`. Never commit private
keys or credentials. Keep the same updater key and Apple signing team for future
versions; changing either needs a deliberate migration.
The public key is also pinned in `app/src-tauri/tauri.conf.json` for Tauri's
release bundler. Keep that public value and the Actions variable synchronized.

The first Developer ID release needs one transactional installation using
`promote-macos.sh --allow-signing-transition`. macOS may require privacy grants
again during this initial migration from an ad-hoc app. Future signed updates
keep the same app identity. Membership alone does not install a signing identity
on this Mac.

## Release and install

The paired release workflow generates detached signatures and `latest.json`
for Apple Silicon macOS and Windows x64. It leaves everything in a draft for
physical acceptance. Signing, notarizing, uploading, and publishing require
separate release authorization. GitHub's `/releases/latest/download/latest.json`
endpoint serves a published, non-prerelease release; a beta draft cannot make
that endpoint work. A deliberately chosen beta endpoint can be compiled with
`HEREWORD_UPDATE_ENDPOINT` when beta distribution is needed.

On macOS, the app verifies the download, checks the installed signing identity,
then runs the bundled transactional release manager using the existing external
Python environment. That helper retains the previous bundle and checks the new
version's engine and privacy readiness, rolling back on failure. Preferences,
models, and the Python environment stay in their existing directories. Logs are
in `~/.config/kokoro-voice-2-1/update-install.log` and contain no speech text.
The app only requests release metadata when the user clicks Check for updates.

Windows uses the updater's signed NSIS installer. Windows installation still
needs validation on Windows before publication.
