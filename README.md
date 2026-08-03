# kokoro-voice

Local read-aloud, dictation, and screen-snip OCR for your desktop. Select text
anywhere and hear it; hold a key and speak to type; drag a box around anything
on screen and have it read to you. All of it runs on your own machine.

**No cloud, no API keys, no account.** Once the models are downloaded the
service makes no outbound network calls at all — enforced, not just intended.

| | Engine | Speed on an M4 Mac mini |
|---|---|---|
| Text → speech | [Kokoro](https://github.com/thewh1teagle/kokoro-onnx) (ONNX, 54 voices) | ~6x realtime |
| Speech → text | [Whisper large-v3-turbo](https://github.com/ml-explore/mlx-examples) (MLX) | ~8x realtime |
| Screen → text | Apple Vision OCR (ships with macOS) | 0.17–0.34s |

---

## How it's put together

One local HTTP service owns the models. Thin per-platform clients talk to it.

```
┌──────────────┐   POST /speak        ┌─────────────────┐
│ hotkey /     │ ───────────────────► │  service        │
│ desktop host │ ◄─────────────────── │  (FastAPI)      │
│              │   audio/wav          │                 │
│              │   POST /transcribe   │  Kokoro   TTS   │
│              │ ───────────────────► │  Whisper  STT   │
└──────────────┘ ◄─────────────────── └─────────────────┘
                    {"text": ...}
```

Splitting it this way means the models load once and stay warm, the clients stay
tiny, and both platforms share one HTTP contract.

- **`server.py`** — the service. Owns both models.
- **`client/speak.py`** — read-aloud client. **Standard library only**, so it
  runs on a stock Python with nothing installed.
- **`client/dictate.py`** — dictation client. Needs `sounddevice`/`soundfile`.
- **`client/snip.py`** — screen-snip OCR. Uses the OS OCR engine, not a model.
- **`hosts/<platform>/`** — desktop integration: hotkeys, mini player, tray.

## Requirements

- Python 3.12+ (the installer fetches it via [uv](https://docs.astral.sh/uv/) if missing)
- macOS on Apple silicon, or Windows — see [Platform support](#platform-support)
- ~1.3 GB disk: ~500 MB models, ~750 MB environment

## Install

```sh
git clone https://github.com/TylerTools/kokoro-voice
cd kokoro-voice
./install.sh          # macOS / Linux
```

```powershell
.\install.ps1         # Windows
```

The installer creates the environment, downloads the models, generates an auth
token, installs the service to start at login, and verifies it end to end. It is
idempotent — safe to re-run.

Then check it:

```sh
curl localhost:8123/health
# {"status":"ok","voices":54,"auth_required":true,"stt_ready":true}
```

## Usage

**Read aloud** — select text in any app and press the hotkey. Press again with a
*new* selection to switch to it; press with nothing newly selected to pause.

**Dictate** — hold the dictation key, speak, release. The text is typed into
whatever has focus, and also placed on the clipboard as a fallback (restored
after 45 seconds so transcripts don't linger).

**Snip and read** — press the snip key, drag a box around anything on screen,
and it is OCR'd and read aloud. This is for text you *cannot* select: images,
PDFs in a viewer, video frames, remote desktops, screenshots someone sent you.
OCR uses Apple's Vision framework, which ships with the OS — nothing is
downloaded and nothing leaves the machine. The capture is deleted as soon as the
text is extracted.

Requires the **Screen Recording** permission for whichever app triggers it.
Without it `screencapture` fails with "could not create image from display".

From the command line:

```sh
python3 client/speak.py --text "hello"    # speak a string
python3 client/speak.py --clipboard       # speak the clipboard
python3 client/speak.py --stop            # stop playback
python3 client/speak.py --voices          # list all 54 voices

python3 client/dictate.py --record        # record until --stop, print transcript
python3 client/dictate.py --devices       # list input devices

python3 client/snip.py                    # select a region, print the text
python3 client/snip.py --speak            # select a region, read it aloud
python3 client/snip.py --file shot.png    # OCR an existing image
```

## Configuration

Environment variables, all optional:

| Variable | Default | Meaning |
|---|---|---|
| `KOKORO_HOST` | `127.0.0.1:8123` | Where the client looks for the service |
| `KOKORO_VOICE` | `af_heart` | Default voice |
| `KOKORO_LANG` | `en-us` | Default language |
| `KOKORO_MAX_CHARS` | `20000` | Reject longer text |
| `KOKORO_TOKEN` | — | Auth token; normally read from the token file instead |
| `WHISPER_REPO` | `mlx-community/whisper-large-v3-turbo` | STT model |
| `WHISPER_LANG` | `en` | Transcription language |

## HTTP API

| Method | Path | Body | Returns |
|---|---|---|---|
| `GET` | `/health` | — | status, voice count, `stt_ready` |
| `GET` | `/voices` | — | all voice names |
| `POST` | `/speak` | `{"text", "voice", "speed", "lang"}` | `audio/wav` |
| `POST` | `/transcribe` | raw WAV bytes | `{"text", "audio_seconds", ...}` |

Both `POST` routes require `Authorization: Bearer <token>` when a token is
configured. `/speak` also returns `X-Audio-Duration`, `X-Synth-Seconds` and
`X-Voice` headers.

## Security

The defaults are deliberately closed:

- **Binds to `127.0.0.1`**, not `0.0.0.0`. Serving other machines is opt-in —
  bind one specific interface, never all of them.
- **Token auth** on both `POST` routes, compared with `hmac.compare_digest` so it
  can't be timed. The token lives in a `0600` file that the service and clients
  both read, keeping it out of `ps` output, shell history and environment dumps.
- **Request bodies are capped before being buffered**, not after parsing.
- **No outbound network calls at runtime.** The Whisper revision is pinned and
  the service runs with `HF_HUB_OFFLINE=1`. Otherwise the model hub is contacted
  on every load to resolve "latest", which breaks the offline guarantee and lets
  an upstream change swap your weights silently.
- **Nothing sensitive is logged.** Transcripts and spoken text never reach the
  logs — only character counts, durations, and a chars-per-second rate used to
  flag anomalies.
- Dictation audio is held in memory and posted to the service. It is never
  written to disk.

## Measured performance

On an M4 Mac mini (16 GB). Numbers, not vibes — re-measure on your own hardware.

- Kokoro model load: 0.3–0.5s · 54 voices · 24 kHz mono
- Synthesis: **~6x realtime**
- Transcription: 7.5s of audio in **0.95s (~8x realtime)**
- Time to first audio on a 386-char passage: **~0.7–1.2s** pipelined, versus
  4.81s if you synthesize the whole passage before playing any of it

## Findings worth keeping

Each of these was measured, and each cost real time to learn.

**fp16 beats fp32, and int8 is much slower.** 0.50s vs 0.59s vs 1.27s for first
chunk on ARM — Apple silicon lacks good int8 kernels for this graph. On x86 with
VNNI the ranking may invert. Benchmark before assuming.

**Pipeline the synthesis; ramp the chunk sizes.** Split on sentence boundaries
and synthesize chunk N+1 while chunk N plays. Sizes ramp 60 → 200 → 400 chars: a
small first chunk gets sound out fast, growing chunks keep the producer ahead of
playback. A uniformly small size starts fast then stutters. The constraint is
that a chunk's audio must last longer than the next chunk's synthesis.

**Don't spawn a player process per chunk.** Measured ~0.93s of dead air per
invocation versus ~0.21s playing in-process — and the latter disappears entirely
if you preload the next clip while the current one plays.

**Trim silence before Whisper sees it.** The single biggest quality fix for
dictation. Push-to-talk always captures dead air, and silence is exactly what
makes Whisper emit training-set filler. Measured: 10s of speech followed by 75s
of silence decoded to the correct 106 characters *plus the word "sent" repeated
about 180 times*. Trimming removes the trigger and is faster besides.

**Set `condition_on_previous_text=False` for dictation.** Whisper decodes in 30s
windows and by default primes each window with the previous window's output, so
one bad window snowballs. On the repro above that was 1232 characters of runaway
versus 128 — and turning it off was 4.6x faster.

**`hallucination_silence_threshold` is a trap.** It is only consulted inside the
`if word_timestamps:` branch, and setting it alone raises no error — it silently
does nothing. Enabling both doubled latency (1.1s → 2.0s) and did not suppress
the artifact at all. Trimming silence is what works.

**Playback and synthesis are one system, not two.** Stopping audio without
terminating the producer means the next synthesized chunk restarts the playback
you just stopped, and the stop button appears dead. Anything that changes what is
playing must reach the producer too.

## Platform support

| | Status |
|---|---|
| **macOS** (Apple silicon) | Complete — service, hotkeys, mini player, dictation, snip OCR |
| **Windows** | Service and clients port directly; host integration in progress. OCR maps to the built-in `Windows.Media.Ocr`, also on-device. See [PORTING-WINDOWS.md](PORTING-WINDOWS.md) |
| **Linux** | Clients work; no host integration |

The service and both clients are portable. What is platform-specific is the
desktop integration — global hotkeys, the floating player, and typing text into
the focused application.

## License

MIT — see [LICENSE](LICENSE).

Kokoro and Whisper carry their own licenses. Model weights are downloaded from
their upstream sources at install time and are not redistributed here.
