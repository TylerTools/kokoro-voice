#!/usr/bin/env python3
"""Fail unless macOS and Windows release manifests form one artifact pair."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


EXPECTED = {"macos": "aarch64", "windows": "x86_64"}


def verify_pair(paths: list[Path]) -> dict[str, dict[str, object]]:
    manifests: dict[str, dict[str, object]] = {}
    for path in paths:
        value = json.loads(path.read_text())
        if value.get("schema_version") != 1:
            raise ValueError(f"unsupported manifest schema in {path}")
        platform = value.get("platform")
        if platform not in EXPECTED:
            raise ValueError(f"unexpected platform in {path}: {platform}")
        if platform in manifests:
            raise ValueError(f"duplicate {platform} manifest")
        if value.get("architecture") != EXPECTED[platform]:
            raise ValueError(f"wrong {platform} architecture")
        if not value.get("signing_identity"):
            raise ValueError(f"missing {platform} signing identity")
        artifacts = value.get("artifacts")
        if not isinstance(artifacts, list) or not artifacts:
            raise ValueError(f"missing {platform} artifacts")
        manifests[platform] = value
    if set(manifests) != set(EXPECTED):
        missing = sorted(set(EXPECTED) - set(manifests))
        raise ValueError(f"missing release manifests: {', '.join(missing)}")
    for key in ("product", "version", "tag", "revision"):
        values = {str(manifest.get(key)) for manifest in manifests.values()}
        if len(values) != 1:
            raise ValueError(f"paired manifest mismatch for {key}")
    return manifests


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", nargs="+", type=Path)
    args = parser.parse_args()
    manifests = verify_pair(args.manifest)
    mac = manifests["macos"]
    print(f"paired {mac['tag']} at {mac['revision']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

