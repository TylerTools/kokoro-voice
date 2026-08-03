#!/usr/bin/env bash
# kokoro-voice installer — macOS (and Linux for the service + clients).
#
# Idempotent: safe to re-run. Every step checks before it acts, so a partial
# install can be repaired by running this again.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG_DIR="${HOME}/.config/kokoro"
TOKEN_FILE="${CONFIG_DIR}/token"
PLIST_LABEL="com.tylertools.kokoro-voice"
PLIST="${HOME}/Library/LaunchAgents/${PLIST_LABEL}.plist"
PORT="${KOKORO_PORT:-8123}"
MODEL_BASE="https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0"

say()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m !\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }
ok()   { printf '\033[1;32m ✓\033[0m %s\n' "$*"; }

IS_MAC=false
[[ "$(uname -s)" == "Darwin" ]] && IS_MAC=true

# ── 1. uv ────────────────────────────────────────────────────────────────────
say "Checking for uv"
if ! command -v uv >/dev/null 2>&1; then
    export PATH="${HOME}/.local/bin:${PATH}"
fi
if ! command -v uv >/dev/null 2>&1; then
    say "Installing uv (manages Python without touching the system one)"
    curl -LsSf https://astral.sh/uv/install.sh | sh
    export PATH="${HOME}/.local/bin:${PATH}"
fi
command -v uv >/dev/null 2>&1 || die "uv install failed; see https://docs.astral.sh/uv/"
ok "uv $(uv --version | awk '{print $2}')"

# ── 2. environment ───────────────────────────────────────────────────────────
say "Creating the Python environment"
[[ -d "${HERE}/.venv" ]] || uv venv --python 3.12 "${HERE}/.venv"
export VIRTUAL_ENV="${HERE}/.venv"
uv pip install --quiet -r "${HERE}/requirements.txt"
if $IS_MAC && [[ "$(uname -m)" == "arm64" ]]; then
    uv pip install --quiet -r "${HERE}/requirements-macos.txt"
    # mlx-whisper declares torch, but only imports it in torch_whisper.py — the
    # weight-CONVERSION path, which we never take. Verified by checking that
    # `torch` never enters sys.modules across `import mlx_whisper` or a real
    # transcribe() call. Dropping it saves ~480MB per install.
    if [[ -d "${HERE}/.venv/lib/python3.12/site-packages/torch" ]]; then
        uv pip uninstall --quiet torch 2>/dev/null || true
        ok "Removed unused torch (~480MB; conversion-only dependency)"
    fi
    ok "Installed with MLX (Apple GPU) speech-to-text"
else
    warn "Not Apple silicon — installing the CPU speech-to-text engine."
    warn "See PORTING-WINDOWS.md §3; benchmark int8 vs float32 on this CPU."
    uv pip install --quiet -r "${HERE}/requirements-windows.txt"
fi
ok "Environment ready ($(du -sh "${HERE}/.venv" | cut -f1))"

# ── 3. models ────────────────────────────────────────────────────────────────
# Not redistributed — fetched from upstream. Sizes are checked because a
# truncated download fails much later and far less obviously.
say "Downloading models (~500MB, first run only)"
mkdir -p "${HERE}/models"
fetch_model() {
    local name="$1" expected="$2" path="${HERE}/models/$1"
    if [[ -f "$path" ]]; then
        local actual; actual=$(wc -c <"$path" | tr -d ' ')
        if [[ "$actual" == "$expected" ]]; then ok "$name already present"; return; fi
        warn "$name is $actual bytes, expected $expected — refetching"
    fi
    curl -fL --progress-bar -o "$path" "${MODEL_BASE}/${name}"
    local actual; actual=$(wc -c <"$path" | tr -d ' ')
    [[ "$actual" == "$expected" ]] || die "$name downloaded $actual bytes, expected $expected"
    ok "$name"
}
fetch_model "kokoro-v1.0.fp16.onnx" 177464787
fetch_model "voices-v1.0.bin"        28214398

# ── 4. auth token ────────────────────────────────────────────────────────────
say "Setting up the auth token"
mkdir -p "${CONFIG_DIR}"; chmod 700 "${CONFIG_DIR}"
if [[ -s "${TOKEN_FILE}" ]]; then
    ok "Token already exists (left untouched)"
else
    "${HERE}/.venv/bin/python" -c "import secrets; print(secrets.token_urlsafe(32))" >"${TOKEN_FILE}"
    ok "Generated a new token"
fi
chmod 600 "${TOKEN_FILE}"

# ── 5. service ───────────────────────────────────────────────────────────────
if $IS_MAC; then
    say "Installing the launchd service"
    mkdir -p "${HOME}/Library/LaunchAgents" "${HERE}/logs"
    # Written from a template so paths follow the clone location rather than
    # being baked in. Binds LOOPBACK ONLY — see the README security section.
    cat >"${PLIST}" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>${PLIST_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>${HERE}/.venv/bin/python</string>
        <string>-m</string><string>uvicorn</string>
        <string>server:app</string>
        <!-- Loopback only. Do NOT change to 0.0.0.0: that publishes an
             unauthenticated-by-default service to every network you join.
             To serve another machine, bind that ONE interface. -->
        <string>--host</string><string>127.0.0.1</string>
        <string>--port</string><string>${PORT}</string>
    </array>
    <key>WorkingDirectory</key><string>${HERE}</string>
    <!-- Serve model weights from the local cache only. Without this the model
         hub is contacted on every load to resolve "latest", which breaks the
         offline guarantee and lets an upstream change swap the weights. -->
    <key>EnvironmentVariables</key>
    <dict><key>HF_HUB_OFFLINE</key><string>1</string></dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
    <key>ThrottleInterval</key><integer>10</integer>
    <key>StandardOutPath</key><string>${HERE}/logs/kokoro.out.log</string>
    <key>StandardErrorPath</key><string>${HERE}/logs/kokoro.err.log</string>
    <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
PLIST_EOF
    plutil -lint "${PLIST}" >/dev/null || die "generated plist is invalid"
    # Retire any pre-rename agent. It binds the same port, so leaving it loaded
    # makes the new service fail to start with a confusing "address in use".
    for legacy in com.tylertools.kokoro-tts; do
        if launchctl print "gui/$(id -u)/${legacy}" >/dev/null 2>&1; then
            warn "Removing legacy service ${legacy} (same port)"
            launchctl bootout "gui/$(id -u)/${legacy}" 2>/dev/null || true
            rm -f "${HOME}/Library/LaunchAgents/${legacy}.plist"
        fi
    done
    launchctl bootout "gui/$(id -u)/${PLIST_LABEL}" 2>/dev/null || true
    sleep 1
    launchctl bootstrap "gui/$(id -u)" "${PLIST}"
    ok "Service installed and started"
else
    warn "Not macOS — start the service yourself:"
    warn "  cd ${HERE} && HF_HUB_OFFLINE=1 .venv/bin/python -m uvicorn server:app --host 127.0.0.1 --port ${PORT}"
fi

# ── 6. verify ────────────────────────────────────────────────────────────────
# An installer that reports success without proving it is worse than no
# installer, so this waits for a real answer from the running service.
if $IS_MAC; then
    say "Verifying (the speech model takes a moment to warm up)"
    for i in $(seq 1 60); do
        if curl -fsS -m 3 "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1; then break; fi
        sleep 2
        [[ $i -eq 60 ]] && die "Service did not come up. Check ${HERE}/logs/kokoro.err.log"
    done
    HEALTH=$(curl -fsS -m 5 "http://127.0.0.1:${PORT}/health")
    echo "    ${HEALTH}"
    case "$HEALTH" in
        *'"auth_required":true'*) ok "Auth is on" ;;
        *) warn "Auth is NOT on — check ${TOKEN_FILE}" ;;
    esac
    TOKEN=$(cat "${TOKEN_FILE}")
    CODE=$(curl -s -o /dev/null -w '%{http_code}' -m 30 \
        -X POST "http://127.0.0.1:${PORT}/speak" \
        -H 'Content-Type: application/json' -H "Authorization: Bearer ${TOKEN}" \
        -d '{"text":"Installation complete."}')
    [[ "$CODE" == "200" ]] && ok "Synthesis works" || die "Synthesis returned HTTP ${CODE}"
fi

echo
say "Done."
echo "  Service:  http://127.0.0.1:${PORT}"
echo "  Token:    ${TOKEN_FILE}"
echo "  Logs:     ${HERE}/logs/"
echo
echo "  Try it:   ${HERE}/.venv/bin/python client/speak.py --text 'hello there'"
if $IS_MAC; then
echo
echo "  For the hotkeys and mini player, install Hammerspoon and point it at"
echo "  hosts/macos/init.lua — see hosts/macos/README.md"
fi
