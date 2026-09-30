// Development-only Tauri stand-in for the browser onboarding preview.
// This file is copied into dist by scripts/preview-onboarding.sh and is not
// included in the packaged app.
(() => {
  for (const key of ["hereword-tour-complete-v1", "hereword-tour-pending-v1", "kokoro-guided-setup-active"]) {
    localStorage.removeItem(key);
  }
  const callbacks = new Map();
  const handlers = new Map();
  let callbackId = 1;
  let offlineReady = false;
  const hotkeys = {
    read: "Control+Alt+Command+KeyU",
    dictate: "Control+Alt+Command+KeyI",
    snip: "Control+Alt+Command+KeyP",
  };
  const permissions = {
    microphone: "available",
    accessibility: "available",
    input_monitoring: "available",
    screen_capture: "checked-on-use",
  };
  const app = {
    app_version: "2.1.1-beta.27-preview",
    build_revision: "0c4e133000000000000000000000000000000000",
    platform: "macos",
    architecture: "aarch64",
    paste_shortcut: "Command+V",
  };
  const emit = (event, payload) => {
    for (const id of handlers.get(event) || []) callbacks.get(id)?.({ event, id: 1, payload });
  };
  const hotkeyReport = () => ({
    ...hotkeys,
    bindings: Object.fromEntries(Object.entries(hotkeys).map(([slot, label]) => [
      slot, { label, registered: true, configurable: true },
    ])),
  });
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" }, windows: [{ label: "main" }], webviews: [{ label: "main" }] },
    transformCallback(callback) { const id = callbackId++; callbacks.set(id, callback); return id; },
    unregisterCallback(id) { callbacks.delete(id); },
    runCallback(id, data) { callbacks.get(id)?.(data); },
    convertFileSrc(path) { return path; },
    async invoke(command, args = {}) {
      if (command === "plugin:event|listen") {
        handlers.set(args.event, [...(handlers.get(args.event) || []), args.handler]);
        return 1;
      }
      if (command.startsWith("plugin:event|") || command.startsWith("plugin:opener|")) return null;
      if (command === "plugin:window|hide") {
        document.getElementById("preview-status").textContent = "Preview complete. HereWord would now close this setup window and remain in the menu bar.";
        return null;
      }
      switch (command) {
        case "engine_status": return { status: offlineReady ? "ok" : "starting", voices: offlineReady ? 54 : 0, stt_ready: offlineReady, stt_warm: false };
        case "system_check": return {
          app,
          engine: { status: offlineReady ? "ok" : "starting", tts_ready: offlineReady },
          permissions,
          hotkeys: hotkeyReport(),
          microphones: [],
          setup: { stage: offlineReady ? "complete" : "download" },
          offline_ready: offlineReady,
        };
        case "setup_engine": offlineReady = true; return null;
        case "hotkeys": return hotkeyReport();
        case "set_hotkey": {
          hotkeys[args.slot] = args.accelerator;
          return { slot: args.slot, accelerator: args.accelerator, kind: "registered-shortcut" };
        }
        case "retry_permission": return { available: true };
        case "storage_status": return { engine_bytes: 0, config_bytes: 0, legacy_runtime_bytes: 0, shared_stt_cache_bytes: 0 };
        case "get_prefs": return { voice: "af_heart", speed: 1, cue_enabled: true, cue_volume: 0.22, live_preview: true, pause_other_media: false };
        case "list_voices": return ["af_heart"];
        case "launch_at_login_status": return false;
        case "microphone_devices": return [];
        case "dictation_status": return { state: "idle" };
        default: return null;
      }
    },
  };

  document.addEventListener("DOMContentLoaded", () => {
    const layout = document.createElement("style");
    layout.textContent = "body { width: min(100%, 720px); margin: 0 auto; }";
    document.head.append(layout);
    const panel = document.createElement("div");
    panel.id = "preview-panel";
    panel.style.cssText = "margin:0 0 14px;padding:12px 15px;border:1px solid #bb735f;border-radius:12px;background:#fff5ee;color:#322a25;box-shadow:0 4px 14px #0001;display:flex;align-items:center;gap:10px;flex-wrap:wrap";
    panel.innerHTML = '<strong>Interactive preview</strong><span id="preview-status">Demo controls stand in for physical shortcuts. Click the setup button to begin; your installed app is unchanged.</span><button id="preview-action" type="button" hidden></button>';
    document.querySelector("header").after(panel);
    const action = document.getElementById("preview-action");
    const status = document.getElementById("preview-status");
    action.addEventListener("click", () => {
      const title = document.getElementById("tour-title").textContent;
      if (title === "Read selected words") {
        document.getElementById("tour-select-text").click();
        emit("hotkey-triggered", "read");
        status.textContent = "Demo Read event sent. Click ‘I heard it’, then Next.";
        void new Audio("/onboarding/demo-read.wav").play();
      } else if (title === "Speak and see your words") {
        document.getElementById("tour-focus-dictate").click();
        emit("hotkey-triggered", "dictate");
        document.getElementById("tour-dictate-text").value = "I can use HereWord.";
        emit("dictation-state", { state: "completed" });
        status.textContent = "Demo dictation inserted text. Click Next.";
      } else if (title === "Read text from the screen") {
        emit("hotkey-triggered", "snip");
        status.textContent = "Demo Snip event sent. Click ‘I heard it’ to finish.";
        void new Audio("/onboarding/demo-snip.wav").play();
      }
    });
    const update = () => {
      const title = document.getElementById("tour-title")?.textContent;
      const visible = !document.getElementById("tour")?.hidden;
      const labels = {
        "Read selected words": "Play sample / simulate Read",
        "Speak and see your words": "Simulate Dictate",
        "Read text from the screen": "Play sample / simulate Snip",
      };
      action.hidden = !visible || !labels[title];
      action.textContent = labels[title] || "";
    };
    new MutationObserver(update).observe(document.getElementById("tour"), { attributes: true, subtree: true, childList: true });
    update();
  });
})();
