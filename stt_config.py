"""Pure STT backend selection shared by setup, runtime, and tests."""

import json


FASTER_WHISPER_REPO = "dropbox-dash/faster-whisper-large-v3-turbo"
FASTER_WHISPER_REVISION = "0a363e9161cbc7ed1431c9597a8ceaf0c4f78fcf"


def candidates(system: str, requested: str = "auto", cpu_compute: str = "int8"):
    if system == "Darwin":
        return [("mlx", "gpu", "float16")]
    if requested == "cpu":
        return [("faster-whisper", "cpu", cpu_compute)]
    if requested not in ("auto", "cuda"):
        raise ValueError(f"unsupported STT device: {requested}")
    return [
        ("faster-whisper", "cuda", "float16"),
        ("faster-whisper", "cpu", cpu_compute),
    ]


def load_cpu_compute(path: str, default: str = "int8") -> str:
    try:
        with open(path, encoding="utf-8") as handle:
            value = json.load(handle).get("cpu_compute")
        return value if value in ("int8", "float32") else default
    except (OSError, ValueError, TypeError):
        return default
