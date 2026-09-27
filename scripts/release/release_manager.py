#!/usr/bin/env python3
"""Transactional macOS promotion and rollback for HereWord.

The reusable Candidate app is deliberately never promoted: its bundle identity
and runtime namespace are different from Stable. After Candidate acceptance,
build the same commit with build-stable.sh and promote that artifact here.
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import plistlib
import shutil
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from contextlib import contextmanager
from pathlib import Path


STABLE_IDENTIFIER = "com.tylertools.kokoro-voice-2-1"
CANDIDATE_IDENTIFIER = "com.tylertools.kokoro-voice-candidate"
CONFIG_FILES = (
    "prefs.json",
    "setup-state.json",
    "performance-profile.json",
    "stt-backend.json",
)


class ReleaseError(RuntimeError):
    pass


def settings() -> dict[str, Path | str | bool]:
    home = Path.home()
    return {
        "stable_app": Path(
            os.environ.get("KOKORO_STABLE_APP", "/Applications/HereWord.app")
        ),
        "legacy_stable_app": Path(
            os.environ.get(
                "KOKORO_LEGACY_STABLE_APP", "/Applications/Kokoro Voice 2.1.app"
            )
        ),
        "vault": Path(
            os.environ.get(
                "KOKORO_RELEASE_VAULT",
                home / "Library/Application Support/Kokoro Voice Release Manager",
            )
        ),
        "config": Path(
            os.environ.get(
                "KOKORO_CONFIG_DIR", home / ".config/kokoro-voice-2-1"
            )
        ),
        "codesign": os.environ.get("KOKORO_CODESIGN_BIN", "/usr/bin/codesign"),
        "ditto": os.environ.get("KOKORO_DITTO_BIN", "/usr/bin/ditto"),
        "open": os.environ.get("KOKORO_OPEN_BIN", "/usr/bin/open"),
        "osascript": os.environ.get("KOKORO_OSASCRIPT_BIN", "/usr/bin/osascript"),
        "copy_mode": os.environ.get("KOKORO_COPY_MODE", "ditto"),
        "skip_launch": os.environ.get("KOKORO_SKIP_LAUNCH") == "1",
        "skip_health": os.environ.get("KOKORO_SKIP_HEALTH") == "1",
        "skip_accessibility": False,
        "accessibility_timeout": float(
            os.environ.get("KOKORO_ACCESSIBILITY_TIMEOUT", "30")
        ),
        "health_url": os.environ.get("KOKORO_HEALTH_URL", "http://127.0.0.1:8125/health"),
    }


def atomic_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    os.replace(temporary, path)


@contextmanager
def release_lock(vault: Path):
    vault.mkdir(parents=True, exist_ok=True)
    with (vault / "release.lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ReleaseError("another release operation is already running") from error
        yield


def bundle_info(app: Path) -> dict:
    plist = app / "Contents/Info.plist"
    if not plist.is_file():
        raise ReleaseError(f"not a macOS app bundle: {app}")
    with plist.open("rb") as handle:
        return plistlib.load(handle)


def run(command: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, text=True, capture_output=True)
    if check and result.returncode:
        detail = (result.stderr or result.stdout).strip()
        raise ReleaseError(f"command failed ({' '.join(command)}): {detail}")
    return result


def signing_team(app: Path, config: dict) -> str | None:
    result = run([str(config["codesign"]), "-dv", "--verbose=4", str(app)])
    detail = f"{result.stdout}\n{result.stderr}"
    for line in detail.splitlines():
        if line.startswith("TeamIdentifier="):
            value = line.partition("=")[2].strip()
            return None if value in {"", "not set"} else value
    return None


def designated_requirement(app: Path, config: dict) -> str | None:
    result = run(
        [str(config["codesign"]), "-d", "-r-", str(app)],
        check=False,
    )
    detail = f"{result.stdout}\n{result.stderr}"
    for line in detail.splitlines():
        value = line.strip()
        if value.startswith("designated =>"):
            return value.partition("=>")[2].strip()
    return None


def signed_entitlements(app: Path, config: dict) -> dict:
    result = run(
        [str(config["codesign"]), "-d", "--entitlements", ":-", str(app)],
        check=False,
    )
    detail = f"{result.stdout}\n{result.stderr}"
    start = detail.find("<?xml")
    end = detail.rfind("</plist>")
    if start < 0 or end < start:
        return {}
    try:
        return plistlib.loads(detail[start : end + len("</plist>")].encode())
    except (ValueError, plistlib.InvalidFileException):
        return {}


def require_audio_input_entitlement(app: Path, config: dict) -> None:
    entitlements = signed_entitlements(app, config)
    if entitlements.get("com.apple.security.device.audio-input") is not True:
        raise ReleaseError(
            "signed app is missing com.apple.security.device.audio-input; "
            "Hardened Runtime would prevent microphone access"
        )


def verify_bundle(app: Path, expected_identifier: str, config: dict) -> dict:
    info = bundle_info(app)
    actual = info.get("CFBundleIdentifier")
    if actual != expected_identifier:
        raise ReleaseError(
            f"wrong bundle identity: expected {expected_identifier}, found {actual}"
        )
    executable = info.get("CFBundleExecutable")
    if not executable or not (app / "Contents/MacOS" / executable).is_file():
        raise ReleaseError("bundle executable is missing")
    run([str(config["codesign"]), "--verify", "--deep", "--strict", str(app)])
    return {
        "path": str(app.resolve()),
        "identifier": actual,
        "version": str(info.get("CFBundleShortVersionString", "unknown")),
        "team_id": signing_team(app, config),
        "designated_requirement": designated_requirement(app, config),
    }


def require_compatible_update_identity(stable: dict, artifact: dict) -> None:
    """Require the exact long-lived identity macOS uses for privacy grants."""
    if not stable.get("team_id") or not artifact.get("team_id"):
        return
    stable_requirement = stable.get("designated_requirement")
    artifact_requirement = artifact.get("designated_requirement")
    if not stable_requirement or not artifact_requirement:
        raise ReleaseError("could not read the signed app designated requirement")
    if stable_requirement != artifact_requirement:
        raise ReleaseError(
            "the update designated requirement does not match installed Stable; "
            "macOS may treat it as a different app and reset privacy permissions"
        )


def copy_bundle(source: Path, destination: Path, config: dict) -> None:
    if destination.exists():
        raise ReleaseError(f"refusing to overwrite existing path: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    if config["copy_mode"] == "python":
        shutil.copytree(source, destination, symlinks=True, copy_function=shutil.copy2)
    else:
        run([str(config["ditto"]), str(source), str(destination)])


def snapshot_config(source: Path, destination: Path) -> list[str]:
    copied: list[str] = []
    destination.mkdir(parents=True, exist_ok=True)
    for name in CONFIG_FILES:
        item = source / name
        if item.is_file():
            shutil.copy2(item, destination / name)
            copied.append(name)
    return copied


def restore_config(snapshot: Path, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    for name in CONFIG_FILES:
        archived = snapshot / name
        target = destination / name
        if archived.is_file():
            temporary = target.with_name(f".{target.name}.{os.getpid()}.tmp")
            shutil.copy2(archived, temporary)
            os.replace(temporary, target)
        elif target.exists():
            target.unlink()


def app_pids(app: Path) -> list[int]:
    executable_dir = str(app.resolve() / "Contents/MacOS") + "/"
    result = run(["/bin/ps", "-axo", "pid=,command="])
    pids: list[int] = []
    for line in result.stdout.splitlines():
        pieces = line.strip().split(maxsplit=1)
        if len(pieces) == 2 and pieces[1].startswith(executable_dir):
            pids.append(int(pieces[0]))
    return pids


def stop_stable(app: Path, config: dict, timeout: float = 15.0) -> None:
    pids = app_pids(app)
    if not pids:
        return
    run(
        [
            str(config["osascript"]),
            "-e",
            f'tell application id "{STABLE_IDENTIFIER}" to quit',
        ],
        check=False,
    )
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not app_pids(app):
            return
        time.sleep(0.2)
    for pid in app_pids(app):
        os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if not app_pids(app):
            return
        time.sleep(0.2)
    raise ReleaseError("Stable did not quit cleanly; no files were replaced")


def latest_runtime_readiness(
    event_file: Path,
    *,
    expected_version: str,
    process_ids: set[int],
    not_before_ms: int,
) -> dict | None:
    try:
        lines = event_file.read_text(errors="replace").splitlines()
    except OSError:
        return None
    for line in reversed(lines):
        try:
            record = json.loads(line)
        except (TypeError, json.JSONDecodeError):
            continue
        try:
            timestamp_ms = int(record.get("timestamp_ms", 0))
        except (TypeError, ValueError):
            continue
        if (
            record.get("event") != "runtime-readiness"
            or record.get("app_version") != expected_version
            or record.get("process_id") not in process_ids
            or timestamp_ms < not_before_ms
        ):
            continue
        return record.get("fields") if isinstance(record.get("fields"), dict) else None
    return None


def wait_for_runtime_readiness(
    config: dict,
    *,
    app: Path,
    expected_version: str,
    process_ids: set[int],
    not_before_ms: int,
) -> None:
    deadline = time.monotonic() + float(config["accessibility_timeout"])
    latest = None
    observed_process_ids = set(process_ids)
    event_file = Path(config["config"]) / "events.jsonl"
    while time.monotonic() < deadline:
        # macOS can require Quit & Reopen after a privacy change. Follow only
        # processes launched from the newly installed bundle so that restart is
        # part of the same verified transaction without accepting stale logs.
        observed_process_ids.update(app_pids(app))
        latest = latest_runtime_readiness(
            event_file,
            expected_version=expected_version,
            process_ids=observed_process_ids,
            not_before_ms=not_before_ms,
        )
        if latest and all(
            latest.get(name) is True
            for name in (
                "ready",
                "accessibility",
                "input_monitoring",
                "microphone",
                "hotkeys_registered",
            )
        ):
            return
        time.sleep(0.25)
    detail = json.dumps(latest, sort_keys=True) if latest else "no fresh readiness event"
    raise ReleaseError(f"permission readiness failed: {detail}")


def acceptable_health(health: dict, expected_version: str | None) -> bool:
    """A promoted engine is acceptable only when writes are authenticated."""
    return (
        health.get("status") == "ok"
        and health.get("auth_required") is True
        and health.get("stt_cache_mode") == "owned"
        and (
            expected_version is None
            or health.get("service_version") == expected_version
        )
    )


def launch_and_check(app: Path, config: dict, expected_version: str | None = None) -> None:
    if config["skip_launch"]:
        return
    launched_after_ms = int(time.time() * 1000)
    run([str(config["open"]), "-a", str(app)])
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if app_pids(app):
            break
        time.sleep(0.25)
    else:
        raise ReleaseError("the promoted app did not start")
    process_ids = set(app_pids(app))

    if not config["skip_health"]:
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(str(config["health_url"]), timeout=3) as response:
                    if response.status == 200:
                        health = json.load(response)
                        if acceptable_health(health, expected_version):
                            break
            except (OSError, ValueError, urllib.error.URLError):
                pass
            time.sleep(1)
        else:
            raise ReleaseError(
                "the promoted app started, but its matching authenticated engine did not become healthy"
            )

    if not config["skip_accessibility"] and expected_version is not None:
        wait_for_runtime_readiness(
            config,
            app=app,
            expected_version=expected_version,
            process_ids=process_ids,
            not_before_ms=launched_after_ms,
        )


def release_directory(vault: Path, label: str, version: str) -> Path:
    stamp = time.strftime("%Y%m%d-%H%M%S")
    safe_version = "".join(c if c.isalnum() or c in ".-_" else "_" for c in version)
    return vault / "releases" / f"{stamp}-{label}-{safe_version}-{uuid.uuid4().hex[:8]}"


def swap_apps(stable: Path, staged: Path) -> Path:
    previous = stable.parent / f".{stable.name}.previous-{uuid.uuid4().hex}.app"
    os.replace(stable, previous)
    try:
        os.replace(staged, stable)
    except BaseException:
        os.replace(previous, stable)
        raise
    return previous


def installed_stable_path(config: dict) -> Path:
    """Resolve the current app once, including the one-time public-name migration."""
    stable = Path(config["stable_app"])
    if stable.exists():
        return stable
    legacy = config.get("legacy_stable_app")
    if legacy is not None and Path(legacy).exists():
        return Path(legacy)
    return stable


def replace_installed_app(installed: Path, destination: Path, staged: Path) -> Path:
    """Atomically replace Stable, allowing its Finder-visible name to change once."""
    if installed == destination:
        return swap_apps(installed, staged)
    if destination.exists():
        raise ReleaseError(f"refusing to overwrite existing path: {destination}")
    previous = installed.parent / f".{installed.name}.previous-{uuid.uuid4().hex}.app"
    os.replace(installed, previous)
    try:
        os.replace(staged, destination)
    except BaseException:
        os.replace(previous, installed)
        raise
    return previous


def promote(
    artifact: Path,
    *,
    allow_ad_hoc: bool,
    allow_signing_transition: bool = False,
    config: dict,
) -> dict:
    destination = Path(config["stable_app"])
    stable = installed_stable_path(config)
    vault = Path(config["vault"])
    stable_meta = verify_bundle(stable, STABLE_IDENTIFIER, config)
    artifact_meta = verify_bundle(artifact, STABLE_IDENTIFIER, config)
    require_audio_input_entitlement(artifact, config)
    if artifact.resolve() == stable.resolve():
        raise ReleaseError("the installed Stable app cannot be its own update artifact")
    if stable_meta["version"] == artifact_meta["version"]:
        raise ReleaseError("the update version must differ from installed Stable")
    if not artifact_meta["team_id"] and not allow_ad_hoc:
        raise ReleaseError(
            "the update is ad-hoc signed; use a durable Developer ID signature, "
            "or pass --allow-ad-hoc only for an intentional local test"
        )
    if (
        not stable_meta["team_id"]
        and artifact_meta["team_id"]
        and not allow_signing_transition
    ):
        raise ReleaseError(
            "installed Stable is ad-hoc signed; changing it to a Developer ID "
            "can reset macOS permissions, so repeat with --allow-signing-transition"
        )
    if stable_meta["team_id"] and artifact_meta["team_id"] != stable_meta["team_id"]:
        raise ReleaseError("the update signing team does not match installed Stable")
    require_compatible_update_identity(stable_meta, artifact_meta)

    archive = release_directory(vault, "previous", stable_meta["version"])
    archived_app = archive / stable.name
    archived_config = archive / "config"
    copy_bundle(stable, archived_app, config)
    snapshot_config(Path(config["config"]), archived_config)
    verify_bundle(archived_app, STABLE_IDENTIFIER, config)

    staged = destination.parent / f".{destination.name}.staged-{uuid.uuid4().hex}.app"
    copy_bundle(artifact, staged, config)
    verify_bundle(staged, STABLE_IDENTIFIER, config)

    journal = vault / "pending.json"
    atomic_json(
        journal,
        {
            "operation": "promote",
            "stable": str(stable),
            "destination": str(destination),
            "staged": str(staged),
            "archive": str(archive),
        },
    )
    previous_slot: Path | None = None
    active = stable
    try:
        stop_stable(stable, config)
        previous_slot = replace_installed_app(stable, destination, staged)
        active = destination
        launch_config = {
            **config,
            "accessibility_timeout": 600
            if allow_signing_transition
            else config["accessibility_timeout"],
        }
        launch_and_check(active, launch_config, artifact_meta["version"])
    except BaseException as error:
        if previous_slot and previous_slot.exists():
            try:
                stop_stable(active, config)
                failed_slot = active.parent / f".{active.name}.failed-{uuid.uuid4().hex}.app"
                if active.exists():
                    os.replace(active, failed_slot)
                os.replace(previous_slot, stable)
                restore_config(archived_config, Path(config["config"]))
                launch_and_check(
                    stable,
                    {**config, "skip_health": True, "skip_accessibility": True},
                    stable_meta["version"],
                )
                if failed_slot.exists():
                    shutil.rmtree(failed_slot)
            except BaseException as recovery_error:
                raise ReleaseError(
                    f"promotion failed ({error}); automatic recovery also failed ({recovery_error})"
                ) from recovery_error
        if staged.exists():
            shutil.rmtree(staged)
        journal.unlink(missing_ok=True)
        raise ReleaseError(f"promotion failed and Stable was restored: {error}") from error

    if previous_slot and previous_slot.exists():
        shutil.rmtree(previous_slot)
    state = {
        "schema_version": 1,
        "installed": artifact_meta,
        "rollback": {
            "app": str(archived_app),
            "config": str(archived_config),
            "metadata": stable_meta,
        },
        "updated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    }
    atomic_json(vault / "state.json", state)
    journal.unlink(missing_ok=True)
    return state


def rollback(config: dict) -> dict:
    stable = Path(config["stable_app"])
    vault = Path(config["vault"])
    state_file = vault / "state.json"
    if not state_file.is_file():
        raise ReleaseError("no verified rollback release is recorded")
    state = json.loads(state_file.read_text())
    rollback_record = state.get("rollback") or {}
    archived_app = Path(rollback_record.get("app", ""))
    archived_config = Path(rollback_record.get("config", ""))
    target_meta = verify_bundle(archived_app, STABLE_IDENTIFIER, config)
    current_meta = verify_bundle(stable, STABLE_IDENTIFIER, config)

    failed_archive = release_directory(vault, "replaced", current_meta["version"])
    failed_app = failed_archive / stable.name
    failed_config = failed_archive / "config"
    copy_bundle(stable, failed_app, config)
    snapshot_config(Path(config["config"]), failed_config)
    verify_bundle(failed_app, STABLE_IDENTIFIER, config)

    staged = stable.parent / f".{stable.name}.rollback-{uuid.uuid4().hex}.app"
    copy_bundle(archived_app, staged, config)
    verify_bundle(staged, STABLE_IDENTIFIER, config)
    previous_slot: Path | None = None
    try:
        stop_stable(stable, config)
        previous_slot = swap_apps(stable, staged)
        restore_config(archived_config, Path(config["config"]))
        launch_and_check(stable, config, target_meta["version"])
    except BaseException as error:
        if previous_slot and previous_slot.exists():
            stop_stable(stable, config)
            if stable.exists():
                shutil.rmtree(stable)
            os.replace(previous_slot, stable)
            restore_config(failed_config, Path(config["config"]))
            launch_and_check(
                stable,
                {**config, "skip_health": True, "skip_accessibility": True},
                current_meta["version"],
            )
        if staged.exists():
            shutil.rmtree(staged)
        raise ReleaseError(f"rollback failed and the current release was restored: {error}") from error

    if previous_slot and previous_slot.exists():
        shutil.rmtree(previous_slot)
    new_state = {
        "schema_version": 1,
        "installed": target_meta,
        "rollback": {
            "app": str(failed_app),
            "config": str(failed_config),
            "metadata": current_meta,
        },
        "updated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    }
    atomic_json(state_file, new_state)
    return new_state


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    verify_parser = subcommands.add_parser("verify", help="verify an app artifact")
    verify_parser.add_argument("app", type=Path)
    verify_parser.add_argument(
        "--channel", choices=("stable", "candidate"), required=True
    )
    promote_parser = subcommands.add_parser("promote", help="install a Stable artifact")
    promote_parser.add_argument("app", type=Path)
    promote_parser.add_argument("--allow-ad-hoc", action="store_true")
    promote_parser.add_argument("--allow-signing-transition", action="store_true")
    promote_parser.add_argument(
        "--defer-accessibility-check",
        action="store_true",
        help=(
            "commit after matching engine health, then repair and validate "
            "macOS privacy grants separately"
        ),
    )
    subcommands.add_parser("rollback", help="swap to the recorded prior release")
    subcommands.add_parser("status", help="show the recorded release state")
    args = parser.parse_args(argv)
    config = settings()
    try:
        with release_lock(Path(config["vault"])):
            if args.command == "verify":
                expected = (
                    STABLE_IDENTIFIER
                    if args.channel == "stable"
                    else CANDIDATE_IDENTIFIER
                )
                result = verify_bundle(args.app, expected, config)
                require_audio_input_entitlement(args.app, config)
            elif args.command == "promote":
                if args.defer_accessibility_check:
                    config["skip_accessibility"] = True
                result = promote(
                    args.app,
                    allow_ad_hoc=args.allow_ad_hoc,
                    allow_signing_transition=args.allow_signing_transition,
                    config=config,
                )
            elif args.command == "rollback":
                result = rollback(config)
            else:
                state_file = Path(config["vault"]) / "state.json"
                result = (
                    json.loads(state_file.read_text())
                    if state_file.is_file()
                    else {"status": "no releases recorded"}
                )
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except (OSError, ReleaseError, ValueError, json.JSONDecodeError) as error:
        print(f"release error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
