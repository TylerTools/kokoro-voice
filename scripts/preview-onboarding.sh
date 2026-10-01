#!/bin/zsh
# Serve the production UI with a development-only browser simulation.
set -euo pipefail
script_dir=${0:A:h}
repo_root=${script_dir:h}
export PATH="${HOME}/.local/node/bin:${PATH}"
cd "${repo_root}/app"
npm run build
cp tests/manual/onboarding-preview.js dist/onboarding-preview.js
cp tests/manual/onboarding-audio/*.wav dist/onboarding/
python3 - <<'PY'
from pathlib import Path
path = Path('dist/index.html')
text = path.read_text()
needle = '<script type="module" crossorigin src="/assets/main-'
assert needle in text
path.write_text(text.replace(needle, '<script src="/onboarding-preview.js"></script>\n    ' + needle, 1))
PY
exec python3 -m http.server 8765 --bind 127.0.0.1 --directory dist
