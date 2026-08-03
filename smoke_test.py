"""Phase 1 gate: prove Kokoro synthesizes audio locally on the Mini."""
import time
import soundfile as sf
from kokoro_onnx import Kokoro

MODEL = "models/kokoro-v1.0.onnx"
VOICES = "models/voices-v1.0.bin"

t0 = time.time()
kokoro = Kokoro(MODEL, VOICES)
load_s = time.time() - t0
print(f"model load: {load_s:.2f}s")

voices = sorted(kokoro.get_voices())
print(f"voices available: {len(voices)}")
print("sample:", ", ".join(voices[:12]))

text = (
    "Kokoro is running locally on the Mac Mini. "
    "This audio was generated with no network calls and no API keys."
)

t0 = time.time()
samples, sample_rate = kokoro.create(text, voice="af_heart", speed=1.0, lang="en-us")
synth_s = time.time() - t0

duration = len(samples) / sample_rate
sf.write("smoke_test.wav", samples, sample_rate)

print(f"sample_rate : {sample_rate}")
print(f"audio length: {duration:.2f}s")
print(f"synth time  : {synth_s:.2f}s")
print(f"realtime factor: {duration / synth_s:.1f}x faster than realtime")
