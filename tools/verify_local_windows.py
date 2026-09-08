"""Verify the installed loopback engine using synthetic speech, never user audio.

This helper does not start an engine or change desktop lifecycle. Authentication
stays private; output contains readiness, timing, and round-trip results only.
"""

import io
import json
import os
from pathlib import Path
import time
import urllib.request

import sounddevice as sd
import soundfile as sf


def main():
    config = Path(os.environ["APPDATA"]) / "Kokoro Voice 2.1"
    token = (config / "token").read_text().strip()
    base = "http://127.0.0.1:8125"

    with urllib.request.urlopen(base + "/health", timeout=10) as response:
        health = json.load(response)
    assert health.get("stt_ready"), health

    sentence = "The local voice system is working on this Windows computer."
    started = time.monotonic()
    request = urllib.request.Request(
        base + "/speak",
        data=json.dumps({"text": sentence, "voice": "af_heart", "speed": 1.0}).encode(),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {token}"},
    )
    with urllib.request.urlopen(request, timeout=120) as response:
        audio = response.read()
    samples, rate = sf.read(io.BytesIO(audio), dtype="float32")
    synthesis_seconds = time.monotonic() - started
    assert len(samples) > rate, "Expected at least one second of synthesized speech"

    request = urllib.request.Request(
        base + "/transcribe", data=audio,
        headers={"Content-Type": "audio/wav", "Authorization": f"Bearer {token}"},
    )
    with urllib.request.urlopen(request, timeout=180) as response:
        result = json.load(response)
    transcript = result.get("text", "").lower()
    assert "voice system" in transcript and "windows computer" in transcript, "Synthetic speech round-trip mismatch"

    print(json.dumps({
        "health": health,
        "tts_audio_seconds": round(len(samples) / rate, 2),
        "tts_synthesis_seconds": round(synthesis_seconds, 2),
        "stt_round_trip_passed": True,
        "stt_seconds": result.get("transcribe_seconds"),
        "default_input": sd.query_devices(kind="input")["name"],
        "default_output": sd.query_devices(kind="output")["name"],
    }, indent=2))


if __name__ == "__main__":
    main()
