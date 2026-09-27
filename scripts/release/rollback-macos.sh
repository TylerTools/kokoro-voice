#!/bin/zsh
# Restore the last verified Stable app and its compatibility-sensitive settings.
set -euo pipefail

script_dir=${0:A:h}
exec /usr/bin/python3 "${script_dir}/release_manager.py" rollback "$@"
