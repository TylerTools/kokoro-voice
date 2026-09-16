#!/usr/bin/env python3
"""Verify every release-facing version resolves to one immutable value.

This script reads repository metadata only. It never creates tags, releases,
or artifacts; release workflows use it as a fail-closed precondition.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def versions(root: Path = ROOT) -> dict[str, str]:
    tauri = json.loads((root / "app/src-tauri/tauri.conf.json").read_text())
    package = json.loads((root / "app/package.json").read_text())
    cargo_text = (root / "app/src-tauri/Cargo.toml").read_text()
    cargo_match = re.search(
        r'(?ms)^\[package\]\s.*?^version\s*=\s*"([^"]+)"', cargo_text
    )
    if cargo_match is None:
        raise ValueError("Cargo package version is missing")
    return {
        "tauri": str(tauri["version"]),
        "npm": str(package["version"]),
        "cargo": cargo_match.group(1),
    }


def verify(tag: str | None = None, root: Path = ROOT) -> str:
    found = versions(root)
    unique = set(found.values())
    if len(unique) != 1:
        details = ", ".join(f"{name}={value}" for name, value in found.items())
        raise ValueError(f"release versions do not match: {details}")
    version = unique.pop()
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError(f"release version is not valid SemVer: {version}")
    if tag is not None and tag != f"v{version}":
        raise ValueError(f"release tag {tag!r} must equal 'v{version}'")
    return version


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", help="optional release tag, including the leading v")
    args = parser.parse_args()
    try:
        version = verify(args.tag)
    except (KeyError, OSError, ValueError) as error:
        print(error, file=sys.stderr)
        return 1
    print(version)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
