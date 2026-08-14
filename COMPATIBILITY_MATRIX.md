# Compatibility acceptance matrix

Every public release must record a physical result for each cell. `Fallback`
means the complete transcript was copied without modifying an unverified field.

| Platform | Application/control | Live edit | Safe fallback | Read aloud | OCR | Evidence |
|---|---|---:|---:|---:|---:|---|
| macOS 13+ ARM64 | Apple Notes | Pending | Pending | Pending | N/A | |
| macOS 13+ ARM64 | Chrome textarea/contenteditable | Pending | Pending | Pending | N/A | |
| macOS 13+ ARM64 | VS Code | Pending | Pending | Pending | N/A | |
| macOS 13+ ARM64 | Microsoft Word | Pending | Pending | Pending | N/A | |
| macOS 13+ ARM64 | Microsoft Outlook | Pending | Pending | Pending | N/A | |
| macOS 13+ ARM64 | Slack | Pending | Pending | Pending | N/A | |
| macOS 13+ ARM64 | Deskflow/remote desktop | Pending | Pending | Pending | Pending | |
| macOS 13+ ARM64 | Password/secure field | Block required | N/A | N/A | N/A | |
| Windows x64 CPU | Notepad | Clipboard-only | Pending | Pending | N/A | |
| Windows x64 CPU | Chrome textarea/contenteditable | Clipboard-only | Pending | Pending | N/A | |
| Windows x64 CPU | VS Code | Clipboard-only | Pending | Pending | N/A | |
| Windows x64 CPU | Microsoft Word | Clipboard-only | Pending | Pending | N/A | |
| Windows x64 CPU | Microsoft Outlook | Clipboard-only | Pending | Pending | N/A | |
| Windows x64 NVIDIA | Same editor set | Clipboard-only | Pending | Pending | Pending | |
| Windows x64 | Password/secure field | Block required | N/A | N/A | N/A | |

## Required disruption cases

- Focus changes before a partial, between validation and injection, and before finalization.
- User types, deletes, selects text, or moves the caret during a session.
- Microphone is denied, revoked, disconnected, or replaced.
- Engine exits during preview and final transcription.
- Escape cancellation, rapid release, missed release, inactivity, and 120-second cap.
- Offline relaunch, reboot/autostart, update recovery, and uninstall/data retention.
