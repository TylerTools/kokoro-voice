#!/bin/zsh
# Build the Stable artifact. This script never installs or launches the bundle.
set -euo pipefail

script_dir=${0:A:h}
repo_root=${script_dir:h:h}

identity=${KOKORO_CODESIGN_IDENTITY:-${APPLE_SIGNING_IDENTITY:-}}
if [[ -z "${identity}" ]]; then
  if [[ "${KOKORO_ALLOW_AD_HOC_STABLE:-0}" != "1" ]]; then
    print -u2 "Stable builds require KOKORO_CODESIGN_IDENTITY or APPLE_SIGNING_IDENTITY."
    print -u2 "A durable identity keeps macOS Accessibility and Input Monitoring grants across updates."
    print -u2 "Use KOKORO_ALLOW_AD_HOC_STABLE=1 only for an intentional permission-reset test."
    exit 2
  fi
  identity="-"
fi

unset KOKORO_BUILD_CHANNEL
unset KOKORO_DISPLAY_NAME
unset KOKORO_APP_SUPPORT_DIR
unset KOKORO_CONFIG_DIR_NAME
unset KOKORO_DEFAULT_PORT
unset KOKORO_CLIENT_HOST
unset KOKORO_DIAGNOSTICS_FILE
unset KOKORO_TTS_CPU_MEM_ARENA

cd "${repo_root}/app"
APPLE_SIGNING_IDENTITY="${identity}" npm run tauri -- build --bundles app "$@"

bundle="${repo_root}/app/src-tauri/target/release/bundle/macos/HereWord.app"
if [[ "${identity}" == "-" ]]; then
  /usr/bin/codesign --force --deep --sign - "${bundle}"
fi
/usr/bin/codesign --verify --deep --strict "${bundle}"
