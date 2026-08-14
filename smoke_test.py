"""Developer-only smoke test for local Kokoro synthesis.

This is not a runtime or installation entrypoint. It reads the same default
model filenames as the service and writes one explicitly named WAV artifact.
"""

import argparse
import os
import time

import soundfile as sf
from kokoro_onnx import Kokoro


MODEL = os.path.join("models", os.environ.get("KOKORO_MODEL", "kokoro-v1.0.fp16.onnx"))
VOICES = os.path.join("models", "voices-v1.0.bin")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", default="smoke_test.wav")
    args = parser.parse_args()

    started = time.time()
    kokoro = Kokoro(MODEL, VOICES)
    load_seconds = time.time() - started
    print(f"model load: {load_seconds:.2f}s")

    voices = sorted(kokoro.get_voices())
    print(f"voices available: {len(voices)}")
    print("sample:", ", ".join(voices[:12]))

    text = (
        "Kokoro is running locally. "
        "This audio was generated with no network calls and no API keys."
    )
    started = time.time()
    samples, sample_rate = kokoro.create(
        text, voice="af_heart", speed=1.0, lang="en-us"
    )
    synthesis_seconds = time.time() - started
    duration = len(samples) / sample_rate
    sf.write(args.output, samples, sample_rate)

    print(f"output      : {args.output}")
    print(f"sample_rate : {sample_rate}")
    print(f"audio length: {duration:.2f}s")
    print(f"synth time  : {synthesis_seconds:.2f}s")
    print(f"realtime factor: {duration / synthesis_seconds:.1f}x faster than realtime")


if __name__ == "__main__":
    main()
