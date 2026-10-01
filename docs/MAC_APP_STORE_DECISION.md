# HereWord Mac App Store decision

Status: feasibility assessment, 2026-09-29. Tyler selected a $49 Mac App
Store launch target. No App Store build has been created or submitted.

## Goal

Sell HereWord to Mac users and test Facebook/Instagram ads. Preserve the current
Read, Dictate, and Snip behavior, including cross-app shortcuts and text access.

## What the current build does

- `app/src-tauri/src/chords.rs` uses a Quartz event tap for global gestures.
- `app/src-tauri/src/text_backend.rs` reads and edits the focused control in
  other apps through macOS Accessibility APIs.
- `app/src-tauri/src/lib.rs` downloads `uv`, installs a Python environment and
  packages, then downloads model data during setup.
- `app/src-tauri/tauri.conf.json` enables `macOSPrivateApi` for the transparent
  floating player and includes the direct-distribution updater configuration.
- `app/src-tauri/Entitlements.plist` contains microphone access for the
  Developer ID build, but does not enable App Sandbox.

## Mac App Store feasibility gate

Apple requires App Sandbox for Mac App Store distribution. Its sandbox guidance
lists use of Accessibility APIs in assistive apps among activities incompatible
with App Sandbox. That collides with the current product's core cross-app Read
and Dictate path. Apple also requires App Store apps to be self-contained,
prohibits downloading/installing executable code that changes functionality,
requires App Store updates, and prohibits a launch license key. Tauri documents
that `macOSPrivateApi` prevents App Store acceptance.

The current HereWord build therefore must not be represented as ready for Mac
App Store submission. A store edition would need a product redesign and a
sandboxed proof of each retained cross-app behavior before preparing a listing.
Removing only the updater or changing the signing certificate is insufficient.

| Current user action | Store-edition status | First proof needed |
| --- | --- | --- |
| Read selected text in another app | Current Accessibility path blocked; Services may replace it | Prototype a HereWord Read service that receives the selected text, and test a Services keyboard shortcut across target apps. |
| Dictate into another app | Current target-locked live insertion blocked; Services may return final text | Prototype a service that returns one final transcript to the requesting editable app. Test recording duration and host compatibility; live partial insertion is not established. |
| Snip and read from the screen | Unverified in sandbox | Replace or validate the capture path using supported screen capture APIs and permission prompts. |
| Read or dictate inside HereWord | Plausible | Bundle runtime dependencies and demonstrate local TTS/STT in a sandboxed app. |
| AirPods playback controls | Unverified in sandbox | Exercise actual hardware controls in the signed Store candidate. |

Sources:

- Apple App Sandbox: https://developer.apple.com/documentation/security/protecting-user-data-with-app-sandbox
- Apple Developer Technical Support confirms sandboxed apps cannot use
  Accessibility APIs: https://developer.apple.com/forums/thread/789663
- Apple Developer Technical Support says a listen-only Quartz event tap can run
  in a sandbox with Input Monitoring: https://developer.apple.com/forums/thread/811443
- Apple Services can receive selected text and return replacement text through
  a service-specific pasteboard: https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/SysServices/Articles/overview.html
- Apple App Review Guidelines, especially 2.4.5 and 2.5.2:
  https://developer.apple.com/app-store/review/guidelines/
- Tauri private API configuration:
  https://v2.tauri.app/reference/config/#macosprivateapi
- Tauri App Store packaging: https://v2.tauri.app/distribute/app-store/

## Route A: full HereWord, direct Mac distribution

1. Finish signed/notarized Mac release and the in-app update feed.
2. Build a public product page with a short demonstration of Read, Dictate,
   Snip, AirPods controls, privacy, and Mac compatibility.
3. Use a hosted checkout and customer delivery for the notarized Mac installer.
4. Run a small Meta Ads test to the product page. Measure page visits, checkout
   starts, purchases, installation success, and support requests before
   increasing spend.
5. Keep speech and transcription local; do not add an ad-tracking SDK to the
   desktop app just to test ads.

This route preserves the current product. It requires the seller to manage
payment, taxes, delivery, support, and updates.

## Route B: separate Mac App Store edition

1. First produce a sandboxed prototype that proves useful Read, Dictate, and
   Snip behavior on a clean Mac. Stop if cross-app access cannot be retained.
2. Package all executable runtime components inside the submitted app. Move
   mutable data into the app container and use only allowed downloads.
3. Replace the private transparent window implementation and remove the
   independent updater from this edition.
4. Prepare Mac App Store signing, provisioning, a self-contained package,
   privacy disclosures, screenshots, and review notes. Use Apple's paid-app
   pricing, which requires the Paid Apps Agreement.
5. Test the App Store purchase/install/update path before linking ads to the
   listing. App Review decides final acceptance.

The key gate is step 1. Do not spend on a store listing or ads until it passes.

## Current decision gate

The Mac App Store and $49 are selected. A sandboxed Services prototype is the
next useful gate: it might retain user-invoked cross-app Read and final dictation
insertion, though it cannot simply reuse the current Accessibility workflow.
The existing modifier-only hotkeys may still detect keys through Input
Monitoring, but a hotkey alone cannot retrieve another app's selected text or
take ownership of its edit target. Do not spend on ads or create a paid Store
listing until the supported handoff has been demonstrated across representative
apps and the user accepts the changed interaction.
