# HereWord update path

## Goal

After one intentional signing transition, every later update should download,
verify, replace, relaunch, and retain macOS Accessibility and Input Monitoring
without asking the user to reconnect the app.

## Required identity

Stable must use one persistent `Developer ID Application` identity. macOS
records privacy grants against the app's designated requirement, not its
filename. Ad-hoc signing embeds a changing code hash, so it cannot support
permission-preserving updates.

The current Mac has the persistent `Developer ID Application: Tyler Thompson
(WLA7TM6BDW)` identity installed. Future releases must keep that team, bundle
identifier, and designated requirement. A signed local installation and a
published update are separate milestones; the running app needs a reachable
release manifest before **Check for updates** can deliver a new version.

The release manager rejects later artifacts whose team or designated
requirement differs, archives the prior app, swaps atomically, verifies the
matching engine and permissions, and automatically restores the prior release
if a gate fails.

The HereWord rename does not change that identity. On the first branded update,
the release manager accepts the existing `/Applications/Kokoro Voice 2.1.app`,
installs the verified build as `/Applications/HereWord.app`, and restores the
legacy app automatically if readiness fails.

## Download-and-install phase

The signed updater is implemented for release builds. Delivery needs an
approved public binary-only GitHub Releases endpoint with a published manifest;
draft releases are for acceptance and do not appear at the default
`/releases/latest/download/latest.json` URL. The source repository may remain
private; the desktop app must never embed or store a source-repository token.
The two trust layers are:

- Apple Developer ID signing and notarization identify the app to macOS and
  preserve privacy grants.
- Tauri's updater key signs the downloadable archive so the running app can
  reject a modified update before installation.

The updater private key and Apple credentials belong in Keychain or release
secrets, never in this repository. The public updater key and HTTPS endpoint can
then be embedded in the app. Release acceptance remains Candidate QC first,
followed by a signed Stable artifact and the existing transactional rollback.

Candidate and ordinary local builds cannot install updates. A Stable release
build enables the updater only with its pinned public key; it still needs a
published manifest at the configured HTTPS endpoint. See
[release setup](scripts/release/UPDATES.md) for the build variables and
[the release gate](RELEASE_CHECKLIST.md) before publication. Ordinary
same-identity macOS updates should retain privacy grants, subject to the
post-install readiness check and rollback.
