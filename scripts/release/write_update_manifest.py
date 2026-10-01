#!/usr/bin/env python3
"""Owns the signed update feed; never publishes releases or handles private keys.

Only paired artifacts from the release draft may enter the feed. Their detached
signatures are verified by the installed app before installation.
"""

import argparse
import datetime
import json
from pathlib import Path
from urllib.parse import quote


def manifest(version: str, repository: str, directory: Path) -> dict:
    if len(repository.split("/")) != 2 or any(
        not part or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.-" for c in part)
        for part in repository.split("/")
    ):
        raise ValueError("invalid GitHub repository")
    if not version or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.+-" for c in version):
        raise ValueError("invalid release version")
    platforms = {}
    for target, pattern in (("darwin-aarch64", "*.app.tar.gz"), ("windows-x86_64", "*.exe")):
        artifacts = list(directory.glob(pattern))
        if len(artifacts) != 1:
            raise ValueError(f"expected exactly one {target} update artifact")
        artifact = artifacts[0]
        signature = artifact.with_name(artifact.name + ".sig").read_text().strip()
        if not signature:
            raise ValueError(f"missing {target} update signature")
        platforms[target] = {
            "signature": signature,
            "url": f"https://github.com/{repository}/releases/download/{quote('v' + version, safe='')}/{quote(artifact.name, safe='')}",
        }
    return {
        "version": version,
        "notes": f"HereWord {version}",
        "pub_date": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "platforms": platforms,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.write_text(json.dumps(manifest(args.version, args.repository, args.artifacts), indent=2) + "\n")
