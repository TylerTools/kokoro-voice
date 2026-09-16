#!/usr/bin/env python3
"""Create a non-secret manifest binding release artifacts to one revision."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from verify_version_sync import verify


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def build_manifest(
    *,
    platform: str,
    architecture: str,
    tag: str,
    revision: str,
    signing_identity: str,
    artifacts: list[Path],
) -> dict[str, object]:
    version = verify(tag)
    if platform not in {"macos", "windows"}:
        raise ValueError(f"unsupported release platform: {platform}")
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("release revision must be a full lowercase Git SHA")
    if not signing_identity.strip():
        raise ValueError("signing identity must not be empty")
    if not artifacts:
        raise ValueError("at least one artifact is required")
    entries = []
    for path in artifacts:
        if not path.is_file():
            raise ValueError(f"artifact does not exist: {path}")
        entries.append(
            {
                "name": path.name,
                "bytes": path.stat().st_size,
                "sha256": sha256(path),
            }
        )
    return {
        "schema_version": 1,
        "product": "HereWord",
        "version": version,
        "tag": tag,
        "revision": revision,
        "platform": platform,
        "architecture": architecture,
        "signing_identity": signing_identity,
        "artifacts": entries,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--platform", required=True)
    parser.add_argument("--architecture", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--signing-identity", required=True)
    parser.add_argument("--artifact", action="append", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    manifest = build_manifest(
        platform=args.platform,
        architecture=args.architecture,
        tag=args.tag,
        revision=args.revision,
        signing_identity=args.signing_identity,
        artifacts=args.artifact,
    )
    args.output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
