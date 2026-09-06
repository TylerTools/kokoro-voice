#!/bin/zsh
# Build the reusable side-by-side Candidate bundle without installing it.
set -euo pipefail

script_dir=${0:A:h}
repo_root=${script_dir:h:h}

export KOKORO_BUILD_CHANNEL=candidate
export KOKORO_DISPLAY_NAME="HereWord Candidate"
export KOKORO_APP_SUPPORT_DIR="Kokoro Voice Candidate"
export KOKORO_CONFIG_DIR_NAME="kokoro-voice-candidate"
export KOKORO_DEFAULT_PORT="8126"
export KOKORO_CLIENT_HOST="127.0.0.1:8126"
export KOKORO_DIAGNOSTICS_FILE="kokoro-voice-candidate-diagnostics.json"
export KOKORO_TTS_CPU_MEM_ARENA="1"

cd "${repo_root}/app"
npm run tauri -- build --bundles app --config src-tauri/tauri.candidate.conf.json "$@"

bundle="${repo_root}/app/src-tauri/target/release/bundle/macos/HereWord Candidate.app"
identity=${KOKORO_CODESIGN_IDENTITY:--}
if [[ "${identity}" == "-" ]]; then
  /usr/bin/codesign --force --deep --sign - "${bundle}"
else
  /usr/bin/codesign --force --deep --options runtime --timestamp --sign "${identity}" "${bundle}"
fi
/usr/bin/codesign --verify --deep --strict "${bundle}"
