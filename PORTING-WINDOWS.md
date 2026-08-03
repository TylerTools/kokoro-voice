# Porting to Windows — standalone install

Spec for whoever builds the Windows side. The macOS version is **working and
measured**; this is a port, not a rewrite. Read `README.md` first — it carries
the numbers and the traps.

**Target:** the Windows PC runs its **own** service and its **own** models,
fully independent of the Mac Mini. Nothing crosses the network.

---

## 1. Read this before writing code

Everything below was **measured on the Mac**, not guessed. Some of it transfers
and some explicitly does not. Re-deriving it costs hours — it already did.

| Finding | Transfers to Windows? |
|---|---|
| Kokoro **fp16** beats fp32 (0.50s vs 0.59s) and int8 is much slower (1.27s) | **Re-measure.** int8 was slow because ARM lacks good int8 kernels; x86 has AVX-512/VNNI and may invert this. Benchmark all three. |
| CoreML made Kokoro *slower* (graph fragmented into 109 partitions) | N/A — but the *lesson* transfers: check whether an accelerator actually helps before adopting it. DirectML/CUDA EP for onnxruntime deserve the same scepticism and the same benchmark. |
| **mlx-whisper** for STT, 0.86s for 6.85s audio | **Does not transfer.** MLX is Apple-only. See §3. |
| Chunk-size ramp 60→200→400 chars | **Recompute** from the machine's own realtime factor. The rule: a chunk's audio must last longer than the next chunk's synthesis. |
| `afplay` cost ~0.93s per invocation → play in-process | **Transfers as a principle.** Do NOT spawn a player per chunk on Windows either (see §4). |
| Deskflow mangles Shift+letter hotkeys | **Does not apply.** The keyboard is physically on the PC, so keys arrive untranslated. All the recorder/KVM complexity is Mac-only — Windows can bind normal combos directly. |

---

## 2. Reuse verbatim

- **`server.py`** — FastAPI service. Portable except the STT engine (§3). Keep
  the auth, the body caps, and the single-lock serialization.
- **`client/speak.py`** — **stdlib-only and already cross-platform.** It has
  `IS_WIN` branches for clipboard (`Get-Clipboard`) and playback. The chunking
  and pipelining logic is the valuable part; do not reimplement it.
- **`client/dictate.py`** — `sounddevice`/`soundfile` are cross-platform; the
  record loop and stop-file protocol port as-is.
- **The HTTP contract** — `/health`, `/voices`, `POST /speak`, `POST /transcribe`.
  Keep it identical so both machines stay debuggable the same way.

Python via **uv** (works on Windows). Do not install a system-wide Python.

---

## 3. What must be rebuilt: the STT engine

`mlx-whisper` is Apple-silicon only. Use **faster-whisper** (CTranslate2).

**Check the GPU first — it decides everything:**

```powershell
nvidia-smi          # if this works, you have CUDA
```

- **NVIDIA present** → `device="cuda"`, `compute_type="float16"`. This should
  comfortably beat the Mac's 0.86s. Needs the CUDA runtime + cuDNN.
- **No NVIDIA** → `device="cpu"`, and **benchmark `int8` vs `float32`**. On the
  Mac, float32 won; on x86 with VNNI, int8 may genuinely win. Measure, don't
  assume — that mistake has been made twice on this project.

Whichever you pick, keep the same interface: `POST /transcribe` takes a raw WAV
body and returns `{"text", "audio_seconds", "transcribe_seconds"}`.

**Warm the model in a background thread at startup.** The first cold request on
the Mac took 7.2s vs 0.86s warm. `/health` must expose `stt_ready`.

Also verify **Kokoro** itself on Windows: `kokoro-onnx` needs espeak-ng, and on
macOS the `espeakng-loader` wheel supplied the library so no system install was
needed. Confirm the equivalent DLL ships on Windows; if not, espeak-ng must be
installed separately.

---

## 4. What must be built new: the host

Hammerspoon is macOS-only. The host owns hotkeys, audio playback, and the UI.

**Build ONE long-running Python process, not a per-keypress script.** Reason:
audio playback must be in-process. `speak.py`'s `IS_WIN` playback path shells
out to PowerShell `SoundPlayer` per chunk — that is the exact mistake `afplay`
made on the Mac, costing ~0.93s of dead air per chunk. Use **`sounddevice`**
(already a dependency) to play in-process, preloading the next chunk while the
current one plays.

Suggested stack:

| Need | Library |
|---|---|
| Global hotkeys, incl. **push-to-talk** press/release | `keyboard`, or AutoHotkey v2 signalling the daemon |
| Gapless playback | `sounddevice` |
| Tray icon + menu | `pystray` + `Pillow` |
| Mini player | small always-on-top `tkinter` window |

If AutoHotkey is already in use on the machine, an AHK script
that signals a Python daemon is a legitimate alternative to `keyboard` — but
**the daemon must still own playback.**

### Behaviour to match (all of it exists on the Mac)

- **Read-aloud:** hotkey → copy selection → chunked, pipelined synthesis →
  gapless playback. Restore the user's previous clipboard afterwards.
- **Dictation:** **push-to-talk** — hold to record, release to transcribe.
  Type the result into the focused window **and put it on the clipboard**
  (safety net if focus wasn't a text field). Enforce a **minimum hold** (~0.45s)
  and say "hold the key while you speak" rather than "no audio captured".
  Add a **watchdog** (120s) so a missed key-release cannot leave the recorder
  running — that happened, and ran for 3 minutes.
- **Mini player:** draggable, position persisted, with states
  spinner (working, incl. buffer underrun) / pause / resume / replay / stop.
  Return to the spinner on underrun so it never looks stalled.
- **Settings:** hotkey rebinding and playback speed (0.75×–2×). Speed is applied
  by Kokoro at synthesis, not as a playback-rate hack, so pitch is unaffected.

---

## 5. Traps that will bite the same way

1. **Blocking stdin.** `speak.py --stdin` reads until EOF. If the host writes
   to the child's stdin without closing it, the process hangs **forever** and
   looks like slow synthesis. On macOS this was `hs.task:closeInput()`. In
   Python: `proc.stdin.close()`.
2. **Never consume a queue index when no clip is ready.** Doing so skips a
   late-arriving chunk and audio stops after the first sentence.
3. **Synthetic keystrokes prove nothing.** They bypass the real input path.
   Only a physical keypress validates a hotkey.
4. **Don't let dictation hijack a system shortcut.** A stray recording once
   bound ⌘C. Validate the captured combo before saving.
5. **Body size limits.** `/transcribe` needs its own cap (8MB); the text cap
   (256KB) rejects even a few seconds of speech.

---

## 6. Definition of done

- [ ] `nvidia-smi` checked; STT device chosen with a **recorded benchmark**
- [ ] Kokoro benchmarked on this hardware: fp16 vs fp32 vs int8, result recorded
- [ ] Chunk ramp recomputed from the measured realtime factor
- [ ] Service autostarts (Task Scheduler or a service wrapper) and survives reboot
- [ ] `/health` reports `stt_ready`; model warmed in the background
- [ ] Read-aloud: select → hotkey → speech, **no gaps between chunks**
- [ ] Dictation: push-to-talk types into a native app **and** sets the clipboard
- [ ] Minimum-hold message and the 120s watchdog both present
- [ ] Time-to-first-audio measured and recorded (Mac reference: **0.54s**)
- [ ] Transcribe latency measured and recorded (Mac reference: **0.86s / 6.85s audio**)
- [ ] Token auth on, service **not** bound to 0.0.0.0

---

## 7. Open question, not yet decided

**In a shared-peripheral setup the microphone lives on one machine only.** If the keyboard is on the
PC. Windows dictation needs its own audio input — a second mic, a USB switch,
or a decision to route audio across the link (explicitly rejected so far on
latency grounds). **Resolve this before building the dictation half**; the
read-aloud half has no such dependency and can proceed immediately.
