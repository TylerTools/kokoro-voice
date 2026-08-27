#!/bin/zsh
# Promote a verified Stable build. Candidate bundles are intentionally rejected.
set -euo pipefail

script_dir=${0:A:h}
exec /usr/bin/python3 "${script_dir}/release_manager.py" promote "$@"
