# Kokoro Voice 2 development line

This is an isolated copy of Kokoro Voice. It exists so the installed
`/Applications/Kokoro Voice.app` remains usable while the replacement is
diagnosed, repaired, and physically verified.

## Isolation contract

| Resource | Kokoro Voice 1 | Kokoro Voice 2 |
| --- | --- | --- |
| Bundle ID | `com.tylertools.kokoro-voice` | `com.tylertools.kokoro-voice-2` |
| Loopback port | `8123` | `8124` |
| Application Support | `Kokoro Voice` | `Kokoro Voice 2` |
| Config and logs | `~/.config/kokoro` | `~/.config/kokoro-voice-2` |
| Temporary controls | `kokoro-*` | `kokoro-voice-2-*` |
| Default complete shortcuts | Existing user settings | `⌃⌥⌘U`, `⌃⌥⌘I`, `⌃⌥⌘O` |

The development copy does not reuse models, preferences, tokens, logs,
playback state, dictation stop files, or autostart state from version 1.

Modifier-only shortcuts are configurable and require at least two distinct
modifiers. V2's defaults include a regular key so they do not collide with a
running V1. Explicitly assigning V1's chord to V2 will make both apps respond
while they are running together.

## Promotion rule

Do not replace version 1. Build version 2 as `Kokoro Voice 2.app`, install it
alongside version 1 only after automated isolation/build checks pass, then grant
permissions to that final binary. Version 1 remains the rollback until the full
application compatibility matrix passes through the installed version 2.
