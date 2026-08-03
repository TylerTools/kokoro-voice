# macOS host

Global hotkeys, the floating mini player, and dictation-into-the-focused-app.
Built on [Hammerspoon](https://www.hammerspoon.org/), which gives us a scripting
layer over the macOS accessibility APIs without writing a menu-bar app.

## Setup

1. Install Hammerspoon:

   ```sh
   brew install --cask hammerspoon
   ```

2. Point it at this config:

   ```sh
   ln -sf "$(pwd)/init.lua" ~/.hammerspoon/init.lua
   ```

   A symlink means `git pull` updates your config. Copy the file instead if you
   want to diverge from upstream.

3. Launch Hammerspoon and grant **Accessibility** when prompted
   (System Settings → Privacy & Security → Accessibility). This is required to
   read the selected text and to type transcripts into other apps.

4. Grant **Microphone** access on the first dictation, and **Screen Recording**
   on the first snip (System Settings → Privacy & Security → Screen Recording).
   Without the latter, screen capture fails with "could not create image from
   display" — an unhelpful message for a permissions problem.

5. Reload the config from the Hammerspoon menu (🔨 → Reload Config). You should
   see a "ready" alert and a 🔈 menu-bar icon.

## Default keys

All three are **recorded, not typed into a config file** — press 🔈 → *Record
hotkey…* and press the combination you want. Recording captures whatever
actually arrives at the OS, which is the only reliable approach if a software
KVM sits between your keyboard and this machine (some mangle modifiers in
transit).

| | Default | Behaviour |
|---|---|---|
| Read aloud | `⌃⌘` (tap both, release) | New selection → read it. Nothing new → pause/resume. |
| Dictate | `⇧⌘` (hold) | Push-to-talk. Release to transcribe. |
| Snip & read | `F7` | Drag a box; the text in it is OCR'd and read aloud. |

## The mini player

Appears on play, in the upper right, and is draggable — its position persists.

| Glyph | Meaning |
|---|---|
| ◜◝◞◟ | working — synthesizing, or waiting on the next chunk |
| ❙❙ | playing (click to pause) |
| ▶ | paused (click to resume) |
| ⟳ | finished (click to replay) |
| ■ | stop and dismiss |

The ■ button is immediate. The hotkey's pause costs a ~250ms clipboard
round-trip, because it has to look at your selection to tell "pause this" apart
from "read this instead".

## Notes on the implementation

Worth knowing before changing anything:

- **AppleScript support is explicitly disabled** (`hs.allowAppleScript(false)`).
  This is a *persisted preference*, so removing the call is not the same as
  turning it off — it has to be asserted. With it enabled, any process able to
  send an Apple Event can execute arbitrary Lua here, inheriting Hammerspoon's
  Accessibility grant.
- **Playback runs in-process** via `hs.sound`, not by shelling out to a player.
  Measured ~0.93s of dead air per spawned player versus ~0.21s in-process.
- **The audio queue and the synthesis producer are one system.** Anything that
  changes what is playing must also reach the producer, or the next synthesized
  chunk restarts playback you just stopped. Reads carry a generation number so
  a superseded producer's callbacks cannot corrupt the read that replaced it.
- **Dictation ducks playback** rather than letting the microphone hear it.

## Troubleshooting

Everything logs here:

```sh
tail -f ~/.hammerspoon/hotkey-debug.log
```

The log deliberately records character counts, never the text itself, so
dictated content never lands on disk.

- **Hotkey does nothing** — check Accessibility is granted; the log prints
  `accessibility=true` at load.
- **"No text selected"** — the app must support ⌘C. Native Cocoa apps are
  reliable; some Electron apps are inconsistent.
- **Nothing plays** — check the service is up: `curl localhost:8123/health`.
