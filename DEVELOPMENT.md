# Isolated development

The validated Mac checkout is not a development environment. It may be running
the accessibility hotkeys and local engine directly from its Tauri debug build;
editing or switching that checkout can restart it.

Use a separate Git worktree and keep routine verification in unit tests,
GitHub Actions, Windows test hardware, or a disposable VM. Do not run a second
desktop instance on the daily-use Mac unless it is explicitly isolated.

An isolated macOS beta uses a different bundle identifier, engine directory,
configuration directory, and service port. It also disables autostart and global
hotkeys so it cannot steal the stable instance's input path:

```sh
cd app
KOKORO_ENGINE_ROOT="$HOME/Library/Application Support/Kokoro Voice Beta/engine" \
KOKORO_CONFIG_DIR="$HOME/.config/kokoro-beta" \
KOKORO_PORT=18123 \
KOKORO_DISABLE_AUTOSTART=1 \
KOKORO_DISABLE_HOTKEYS=1 \
npm run tauri -- dev --config src-tauri/tauri.beta.conf.json
```

This command deliberately uses separate model storage. Do not complete beta
setup on the daily-use Mac merely to run compilation or unit tests; that would
duplicate the model download. Use the existing dependency caches for builds and
the clean-machine release environments for first-run validation.

Never point `KOKORO_ENGINE_ROOT` or `KOKORO_CONFIG_DIR` at the stable instance
from a beta build. Never enable the production hotkeys in two running instances.
