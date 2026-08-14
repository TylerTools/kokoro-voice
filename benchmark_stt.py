"""Developer/setup benchmark for selecting the Windows CPU STT compute type.

This is a measurement tool, not a runtime entrypoint. The desktop setup invokes
it explicitly and consumes only the JSON file named by ``--output``.
"""

import argparse
import json
import time

import numpy as np
from faster_whisper import WhisperModel
from stt_config import FASTER_WHISPER_REPO, FASTER_WHISPER_REVISION


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    audio = np.zeros(16000, dtype="float32")
    results = {}
    for compute in ("int8", "float32"):
        started = time.perf_counter()
        model = WhisperModel(
            FASTER_WHISPER_REPO,
            revision=FASTER_WHISPER_REVISION,
            device="cpu",
            compute_type=compute,
        )
        list(model.transcribe(audio, language="en")[0])
        results[compute] = time.perf_counter() - started
        del model
    selected = min(results, key=results.get)
    with open(args.output, "w", encoding="utf-8") as handle:
        json.dump({"cpu_compute": selected, "benchmarks_seconds": results}, handle, indent=2)


if __name__ == "__main__":
    main()
