#!/usr/bin/env python3
"""Measure a passive HereWord Candidate without exposing speech or credentials.

Owns repeatable local runtime acceptance for memory, TTS/STT latency, worker
retirement, and transcript equivalence. It does not launch, install, stop, or
re-sign an application; the caller owns the Candidate process lifecycle.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import statistics
import subprocess
import time
import urllib.parse
import urllib.request


SCHEMA_VERSION = 2
CANDIDATE_PORT = 8126
QC_TEXT = (
    "The local speech system is running a private quality check for clear and "
    "reliable dictation."
)
TTS_STRESS_SENTENCE = (
    "A dependable local voice should read longer documents clearly while "
    "returning memory to the operating system when the work is complete. "
)


def normalized_text(value: str) -> str:
    return " ".join("".join(char.lower() if char.isalnum() else " " for char in value).split())


def tts_stress_text(target_chars: int) -> str:
    if target_chars < 1 or target_chars > 20_000:
        raise ValueError("TTS stress text must contain 1 to 20000 characters")
    repetitions = (target_chars // len(TTS_STRESS_SENTENCE)) + 1
    return (TTS_STRESS_SENTENCE * repetitions)[:target_chars]


def parse_footprint(output: str) -> float:
    match = re.search(r"\bFootprint:\s*([0-9.]+)\s*([KMGT]B)\b", output)
    if not match:
        raise ValueError("footprint output did not contain a physical footprint")
    value = float(match.group(1))
    unit = match.group(2)
    multiplier = {"KB": 1 / 1024, "MB": 1, "GB": 1024, "TB": 1024 * 1024}[unit]
    return round(value * multiplier, 3)


def idle_wait_timeout(health: dict, override: float | None) -> float:
    if override is not None:
        return override
    current_lease = float(health.get("stt_status", {}).get("idle_seconds") or 0)
    return current_lease + 30


def process_footprint_mb(pid: int | None) -> float:
    if not pid:
        return 0.0
    result = subprocess.run(
        ["/usr/bin/footprint", str(pid)],
        capture_output=True,
        text=True,
        check=False,
        timeout=15,
    )
    if result.returncode != 0:
        raise RuntimeError(f"footprint failed for managed PID {pid}")
    return parse_footprint(result.stdout)


def read_pid(path: Path) -> int:
    try:
        pid = int(path.read_text(encoding="utf-8").strip())
    except (OSError, ValueError) as error:
        raise RuntimeError(f"cannot read Candidate engine PID from {path}") from error
    try:
        os.kill(pid, 0)
    except OSError as error:
        raise RuntimeError(f"Candidate engine PID {pid} is not running") from error
    return pid


def candidate_app_pid() -> int | None:
    result = subprocess.run(
        [
            "/usr/bin/pgrep",
            "-f",
            "/HereWord Candidate.app/Contents/MacOS/kokoro-voice-2-1$",
        ],
        capture_output=True,
        text=True,
        check=False,
        timeout=5,
    )
    for line in result.stdout.splitlines():
        try:
            return int(line)
        except ValueError:
            continue
    return None


class CandidateClient:
    def __init__(self, base_url: str, token: str) -> None:
        parsed = urllib.parse.urlparse(base_url)
        if parsed.hostname not in {"127.0.0.1", "localhost"}:
            raise ValueError("runtime QC requires a loopback Candidate URL")
        if parsed.port != CANDIDATE_PORT:
            raise ValueError(f"runtime QC refuses non-Candidate port {parsed.port}")
        self.base_url = base_url.rstrip("/")
        self.token = token

    def request(
        self,
        path: str,
        *,
        body: bytes | None = None,
        content_type: str | None = None,
        timeout: float = 180,
    ) -> tuple[bytes, dict[str, str]]:
        headers = {"Authorization": f"Bearer {self.token}"}
        if content_type:
            headers["Content-Type"] = content_type
        request = urllib.request.Request(
            f"{self.base_url}{path}",
            data=body,
            headers=headers,
            method="POST" if body is not None else "GET",
        )
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.read(), dict(response.headers.items())

    def health(self) -> dict:
        raw, _ = self.request("/health", timeout=10)
        return json.loads(raw)

    def speak_text(self, text: str, *, session_end: bool = True) -> tuple[bytes, dict]:
        payload = json.dumps(
            {
                "text": text,
                "voice": "af_heart",
                "speed": 1.0,
                "session_end": session_end,
            }
        ).encode()
        started = time.perf_counter()
        audio, headers = self.request(
            "/speak", body=payload, content_type="application/json", timeout=300
        )
        headers = {name.lower(): value for name, value in headers.items()}
        return audio, {
            "request_seconds": round(time.perf_counter() - started, 4),
            "audio_sha256": hashlib.sha256(audio).hexdigest(),
            "audio_bytes": len(audio),
            "synth_seconds": float(headers.get("x-synth-seconds", 0)),
            "audio_seconds": float(headers.get("x-audio-duration", 0)),
            "input_characters": len(text),
        }

    def speak_fixture(self) -> tuple[bytes, dict]:
        return self.speak_text(QC_TEXT)

    def speak_stress(self, text: str, chunk_chars: int) -> dict:
        digest = hashlib.sha256()
        totals = {
            "request_seconds": 0.0,
            "synth_seconds": 0.0,
            "audio_seconds": 0.0,
            "audio_bytes": 0,
        }
        chunks = 0
        for offset in range(0, len(text), chunk_chars):
            end = min(offset + chunk_chars, len(text))
            audio, timing = self.speak_text(
                text[offset:end], session_end=end == len(text)
            )
            digest.update(audio)
            chunks += 1
            for name in totals:
                totals[name] += timing[name]
        return {
            **{name: round(value, 4) for name, value in totals.items()},
            "audio_sha256": digest.hexdigest(),
            "input_characters": len(text),
            "request_chunks": chunks,
        }

    def transcribe_fixture(self, audio: bytes) -> dict:
        started = time.perf_counter()
        raw, _ = self.request(
            "/transcribe", body=audio, content_type="audio/wav", timeout=180
        )
        result = json.loads(raw)
        transcript = str(result.pop("text", ""))
        result.update(
            {
                "request_seconds": round(time.perf_counter() - started, 4),
                "transcript_matches_fixture": normalized_text(transcript)
                == normalized_text(QC_TEXT),
                "transcript_sha256": hashlib.sha256(
                    normalized_text(transcript).encode()
                ).hexdigest(),
                "transcript_characters": len(transcript),
            }
        )
        return result


def combined_footprint(
    engine_pid: int,
    stt_worker_pid: int | None,
    tts_worker_pid: int | None,
) -> dict:
    app_pid = candidate_app_pid()
    parts = {
        "app_pid": app_pid,
        "app_mb": process_footprint_mb(app_pid),
        "engine_pid": engine_pid,
        "engine_mb": process_footprint_mb(engine_pid),
        "stt_worker_pid": stt_worker_pid,
        "stt_worker_mb": process_footprint_mb(stt_worker_pid),
        "tts_worker_pid": tts_worker_pid,
        "tts_worker_mb": process_footprint_mb(tts_worker_pid),
    }
    parts["combined_mb"] = round(
        parts["app_mb"]
        + parts["engine_mb"]
        + parts["stt_worker_mb"]
        + parts["tts_worker_mb"],
        3,
    )
    return parts


def wait_for_cold(client: CandidateClient, timeout: float) -> dict:
    deadline = time.monotonic() + timeout
    latest = client.health()
    while time.monotonic() < deadline:
        if latest.get("stt_status", {}).get("state") == "cold":
            return latest
        time.sleep(0.25)
        latest = client.health()
    raise TimeoutError(
        f"STT worker did not retire within {timeout:.1f}s; "
        f"last state was {latest.get('stt_status', {}).get('state')}"
    )


def baseline_checks(
    report: dict,
    baseline: dict | None,
    max_idle_mb: float,
    expected_cpu_arena: bool | None = None,
) -> list[dict]:
    checks = [
        {
            "name": "health",
            "passed": report["health_initial"].get("status") == "ok",
        },
        {
            "name": "authentication-required",
            "passed": report["health_initial"].get("auth_required") is True,
        },
        {
            "name": "owned-stt-cache",
            "passed": report["health_initial"].get("stt_cache_mode") == "owned",
            "observed": report["health_initial"].get("stt_cache_mode"),
        },
        {
            "name": "transcript-equivalence",
            "passed": all(run["transcript_matches_fixture"] for run in report["stt_runs"]),
        },
    ]
    if expected_cpu_arena is not None:
        checks.append(
            {
                "name": "tts-cpu-arena-configuration",
                "passed": report["health_initial"].get("tts_cpu_mem_arena")
                is expected_cpu_arena,
                "observed": report["health_initial"].get("tts_cpu_mem_arena"),
                "expected": expected_cpu_arena,
            }
        )
    idle = report.get("memory_idle")
    if idle:
        checks.append(
            {
                "name": "idle-memory",
                "passed": idle["combined_mb"] <= max_idle_mb,
                "observed_mb": idle["combined_mb"],
                "limit_mb": max_idle_mb,
            }
        )
    if baseline:
        current_warm = statistics.median(
            run["request_seconds"] for run in report["stt_runs"] if not run["stt_cold_start"]
        )
        baseline_warm = statistics.median(
            run["request_seconds"] for run in baseline["stt_runs"] if not run["stt_cold_start"]
        )
        checks.append(
            {
                "name": "warm-stt-regression",
                "passed": current_warm <= baseline_warm * 1.10,
                "baseline_seconds": round(baseline_warm, 4),
                "observed_seconds": round(current_warm, 4),
                "limit_seconds": round(baseline_warm * 1.10, 4),
            }
        )
        checks.append(
            {
                "name": "tts-regression",
                "passed": report["tts"]["request_seconds"]
                <= baseline["tts"]["request_seconds"] * 1.10,
                "baseline_seconds": baseline["tts"]["request_seconds"],
                "observed_seconds": report["tts"]["request_seconds"],
                "limit_seconds": round(baseline["tts"]["request_seconds"] * 1.10, 4),
            }
        )
        if report.get("tts_stress_runs") and baseline.get("tts_stress_runs"):
            current_stress = statistics.median(
                run["request_seconds"] for run in report["tts_stress_runs"]
            )
            baseline_stress = statistics.median(
                run["request_seconds"] for run in baseline["tts_stress_runs"]
            )
            checks.append(
                {
                    "name": "tts-stress-regression",
                    "passed": current_stress <= baseline_stress * 1.10,
                    "baseline_seconds": round(baseline_stress, 4),
                    "observed_seconds": round(current_stress, 4),
                    "limit_seconds": round(baseline_stress * 1.10, 4),
                }
            )
    return checks


def main() -> int:
    parser = argparse.ArgumentParser(description="Measure a running passive Candidate")
    parser.add_argument("--base-url", default="http://127.0.0.1:8126")
    parser.add_argument(
        "--token-file",
        type=Path,
        default=Path.home() / ".config/kokoro-voice-candidate/token",
    )
    parser.add_argument(
        "--engine-pid-file",
        type=Path,
        default=Path.home() / ".config/kokoro-voice-candidate/engine.pid",
    )
    parser.add_argument("--warm-runs", type=int, default=5)
    parser.add_argument("--tts-stress-chars", type=int, default=4000)
    parser.add_argument("--tts-stress-runs", type=int, default=3)
    parser.add_argument("--tts-stress-chunk-chars", type=int, default=400)
    parser.add_argument("--expect-cpu-arena", choices=("on", "off"))
    parser.add_argument("--wait-for-idle", action="store_true")
    parser.add_argument("--idle-timeout", type=float)
    # A normal warm TTS worker should fit under this combined cap. After a long
    # read, recycling should place the Candidate comfortably below it.
    parser.add_argument("--max-idle-mb", type=float, default=600)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    if args.warm_runs < 1:
        parser.error("--warm-runs must be at least 1")
    if args.tts_stress_runs < 1:
        parser.error("--tts-stress-runs must be at least 1")
    if args.tts_stress_chunk_chars < 1:
        parser.error("--tts-stress-chunk-chars must be at least 1")
    try:
        stress_text = tts_stress_text(args.tts_stress_chars)
    except ValueError as error:
        parser.error(str(error))
    token = args.token_file.read_text(encoding="utf-8").strip()
    if not token:
        raise RuntimeError("Candidate token file is empty")
    client = CandidateClient(args.base_url, token)
    health_initial = client.health()
    if health_initial.get("stt_status", {}).get("state") != "cold":
        raise RuntimeError("start runtime QC with Candidate STT in the cold state")
    engine_pid = read_pid(args.engine_pid_file)

    report = {
        "schema_version": SCHEMA_VERSION,
        "measured_at_unix": int(time.time()),
        "service_version": health_initial.get("service_version"),
        "health_initial": health_initial,
        "memory_cold": combined_footprint(
            engine_pid,
            None,
            health_initial.get("tts_status", {}).get("worker_pid"),
        ),
    }
    audio, report["tts"] = client.speak_fixture()
    report["stt_runs"] = [client.transcribe_fixture(audio)]
    warm_health = client.health()
    report["memory_warm"] = combined_footprint(
        engine_pid,
        warm_health.get("stt_status", {}).get("worker_pid"),
        warm_health.get("tts_status", {}).get("worker_pid"),
    )
    for _ in range(args.warm_runs):
        report["stt_runs"].append(client.transcribe_fixture(audio))

    report["tts_stress_runs"] = []
    for _ in range(args.tts_stress_runs):
        report["tts_stress_runs"].append(
            client.speak_stress(stress_text, args.tts_stress_chunk_chars)
        )
    stress_health = client.health()
    report["memory_tts_stress"] = combined_footprint(
        engine_pid,
        stress_health.get("stt_status", {}).get("worker_pid"),
        stress_health.get("tts_status", {}).get("worker_pid"),
    )

    if args.wait_for_idle:
        lease_health = client.health()
        configured_idle = float(
            lease_health.get("stt_status", {}).get("idle_seconds") or 0
        )
        report["health_before_idle_wait"] = lease_health
        timeout = idle_wait_timeout(lease_health, args.idle_timeout)
        idle_health = wait_for_cold(client, timeout)
        report["health_idle"] = idle_health
        report["memory_idle"] = combined_footprint(
            engine_pid,
            None,
            idle_health.get("tts_status", {}).get("worker_pid"),
        )

    baseline = None
    if args.baseline:
        baseline = json.loads(args.baseline.read_text(encoding="utf-8"))
    expected_cpu_arena = None
    if args.expect_cpu_arena:
        expected_cpu_arena = args.expect_cpu_arena == "on"
    report["checks"] = baseline_checks(
        report,
        baseline,
        args.max_idle_mb,
        expected_cpu_arena,
    )
    report["passed"] = all(check["passed"] for check in report["checks"])

    args.output.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.output.with_suffix(args.output.suffix + ".tmp")
    temporary.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, args.output)
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
