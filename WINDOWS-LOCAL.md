# Local Windows build

Open **Kokoro Voice 2.1** from the Start menu or its desktop shortcut.
The desktop app owns the local speech engine on `127.0.0.1:8125`.

The Windows tray update in source adds the tooltip **Kokoro Voice 2.1 — Settings**.
Left-click the Kokoro icon beside the clock to open or restore Settings;
right-click for Settings, Read selection, Snip & read, Stop, and Quit.
Windows may place the icon under the **^** hidden-icons arrow; drag it beside the
clock if desired. This source update requires a new build/install and is not in
the previously published beta installer.

| Action | Default shortcut |
| --- | --- |
| Read selected text | Ctrl + Alt + Shift + U |
| Dictate | Hold Ctrl + Alt + Shift + I, speak, then release |
| Snip and read | Ctrl + Alt + Shift + P |

Windows dictation copies the final transcript to the clipboard. Press **Ctrl+V**
in the destination app. Automatic live insertion is not available in this build.
Use **Preview** in Voice & speed to hear the selected voice, and **Stop** to stop
reading. Windows pause/resume and screen OCR still need separate physical checks.

Modifier-only combinations such as **Ctrl+Alt** and **Ctrl+Shift** can also be
saved with **Record…**. For Read, press and release the combination. For Dictate,
hold it briefly to start recording, speak, and release. Adding another key cancels
the gesture so a longer keyboard shortcut can continue to its destination.
The local installation has Read set to Ctrl+Alt and Dictate set to Ctrl+Shift;
these are user preferences, not changes to the defaults above.

The runtime and TTS models are under `%LOCALAPPDATA%\Kokoro Voice 2.1\engine`.
Preferences are under `%APPDATA%\Kokoro Voice 2.1`; do not share its private token.
Whisper uses the pinned Hugging Face cache in `%USERPROFILE%\.cache\huggingface`.
The models remain local after setup.

The locally built installer is
`app/src-tauri/target/release/bundle/nsis/Kokoro Voice 2.1_2.1.0-beta.1_x64-setup.exe`.
It is a local development build, not a signed public Windows release.

Local changes fix the Windows uv archive path, reuse an existing Python
environment during setup, keep desktop Python helpers hidden, display Windows
shortcut names and clipboard instructions, and require a private token before
considering setup complete.

`tools/download_windows_whisper.ps1` provides a checksum-verified download of
the same pinned Whisper model if the normal downloader stalls.
`tools/verify_local_windows.py` checks the running app's TTS/STT round trip using
synthetic audio held in memory. It does not start another engine or record a user.

Verified locally on September 7, 2026: setup completed; 54 voices and
`faster-whisper-cpu-int8` reported ready; microphone probe passed; synthetic
TTS → STT round trip passed; the installed playback client completed successfully.
The synthetic sample took 0.91 seconds to synthesize 3.2 seconds of speech and
3.90 seconds to transcribe. Physical user shortcut presses and OCR remain unverified.
The rebuilt installer passed 37 Rust tests, 21 Python tests, release Clippy,
TypeScript/Vite build, and NSIS packaging. Both modifier-only shortcuts were saved
and registered in the installed app; registration alone does not verify a physical
hold-to-dictate session.
