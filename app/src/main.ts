/**
 * Kokoro Voice settings control plane.
 *
 * This frontend renders engine state and captures user input, but Rust owns
 * process lifecycle, shortcut meaning/registration, permissions, and durable
 * preferences. Keep platform policy out of this file; send captured facts to
 * the backend and render its authoritative result.
 */
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { listen } from "@tauri-apps/api/event";

type Health = {
  status: string;
  voices?: number;
  stt_ready?: boolean;
  auth_required?: boolean;
};

type HotkeySlot = "read" | "dictate" | "snip";

type DictationState =
  | "starting"
  | "recording"
  | "transcribing"
  | "completed"
  | "cancelled"
  | "permission-denied"
  | "device-unavailable"
  | "timed-out"
  | "live-typing"
  | "clipboard-fallback"
  | "cancelled-by-user";

function isHotkeySlot(value: string | undefined): value is HotkeySlot {
  return value === "read" || value === "dictate" || value === "snip";
}

const statusEl = document.getElementById("status") as HTMLDivElement;
const statusText = document.getElementById("status-text") as HTMLSpanElement;
const detail = document.getElementById("detail") as HTMLSpanElement;

/**
 * Poll the engine and describe it in plain language.
 *
 * The engine takes a few seconds to warm the speech model after launch, so
 * "starting" is a real, expected state rather than an error — saying "down"
 * during normal startup would train people to ignore the indicator.
 */
async function refresh(): Promise<void> {
  let h: Health;
  try {
    h = (await invoke("engine_status")) as Health;
  } catch {
    h = { status: "down" };
  }

  statusEl.classList.remove("status--ok", "status--warn", "status--down", "status--unknown");

  // Not installed is a first-run state, not a failure — show setup, not an error.
  const setup = document.getElementById("setup") as HTMLElement;
  if (h.status === "not-installed") {
    setup.hidden = false;
    statusEl.classList.add("status--warn");
    statusText.textContent = "Setup needed";
    detail.textContent = "Download the voices to get started.";
    return;
  }
  setup.hidden = true;

  if (h.status === "ok" && h.stt_ready) {
    statusEl.classList.add("status--ok");
    statusText.textContent = "Ready";
    detail.textContent = `${h.voices ?? 0} voices · speech recognition ready`;
  } else if (h.status === "ok") {
    statusEl.classList.add("status--warn");
    statusText.textContent = "Almost ready";
    detail.textContent = "Reading works now; speech recognition is still warming up.";
  } else if (h.status === "starting") {
    statusEl.classList.add("status--warn");
    statusText.textContent = "Starting…";
    detail.textContent = "Loading the voices. This takes a few seconds after launch.";
  } else {
    statusEl.classList.add("status--down");
    statusText.textContent = "Not running";
    detail.textContent = "The engine isn't responding. Quit and reopen Kokoro Voice.";
  }
}

// Buttons declare which Rust command they call, so adding one is a markup
// change rather than another event listener.
document.querySelectorAll<HTMLButtonElement>("button[data-cmd]").forEach((btn) => {
  btn.addEventListener("click", async () => {
    const cmd = btn.dataset.cmd!;
    const argId = btn.dataset.arg;
    const original = btn.textContent;
    try {
      if (argId) {
        const input = document.getElementById(argId) as HTMLInputElement;
        await invoke(cmd, { text: input.value });
      } else {
        await invoke(cmd);
      }
      btn.textContent = "✓";
      setTimeout(() => (btn.textContent = original), 900);
    } catch (e) {
      btn.textContent = "failed";
      detail.textContent = String(e);
      setTimeout(() => (btn.textContent = original), 1600);
    }
  });
});

document.getElementById("open-accessibility")?.addEventListener("click", async () => {
  await invoke("retry_permission", { capability: "accessibility" });
  await openUrl("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
});

document.getElementById("open-input-monitoring")?.addEventListener("click", async () => {
  await invoke("retry_permission", { capability: "input-monitoring" });
  await openUrl("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent");
});

document.getElementById("export-diagnostics")?.addEventListener("click", async () => {
  const path = await invoke<string>("export_diagnostics");
  detail.textContent = `Diagnostics saved to ${path}`;
});

document.getElementById("run-system-check")?.addEventListener("click", async (event) => {
  const button = event.currentTarget as HTMLButtonElement;
  button.disabled = true;
  detail.textContent = "Checking engine, permissions, hotkeys, and microphone…";
  try {
    const report = await invoke<Record<string, unknown>>("system_check");
    const permissions = report.permissions as Record<string, string>;
    const engine = report.engine as Record<string, string>;
    if (engine.status !== "ok") {
      detail.textContent = "The speech engine is not ready. Export diagnostics for details.";
    } else if (permissions.accessibility !== "available") {
      detail.textContent = "Accessibility is off. Enable Kokoro Voice, then run this check again.";
    } else if (permissions.input_monitoring !== "available") {
      detail.textContent = "Input Monitoring is off. Enable Kokoro Voice, then run this check again.";
    } else {
      detail.textContent = "System check passed. Shortcuts are listening.";
    }
  } catch (error) {
    detail.textContent = `System check failed: ${error}`;
  } finally {
    button.disabled = false;
  }
});

invoke<{ engine_bytes: number }>("storage_status").then((storage) => {
  const el = document.getElementById("storage-detail");
  if (el) el.textContent = `${(storage.engine_bytes / 1_000_000_000).toFixed(2)} GB of downloaded local data`;
});

document.getElementById("remove-local-data")?.addEventListener("click", async () => {
  if (!window.confirm("Remove the downloaded runtime and models from this computer?")) return;
  const removePreferences = (document.getElementById("remove-preferences") as HTMLInputElement).checked;
  await invoke("remove_local_data", { removePreferences });
  window.location.reload();
});

// Live setup progress from Rust.
listen<{ pct: number; message: string }>("setup-progress", (e) => {
  const wrap = document.getElementById("bar-wrap") as HTMLElement;
  const bar = document.getElementById("bar") as HTMLElement;
  const msg = document.getElementById("setup-msg") as HTMLElement;
  wrap.hidden = false;
  bar.style.width = `${e.payload.pct}%`;
  msg.textContent = e.payload.message;
});

document.getElementById("setup-go")?.addEventListener("click", async (ev) => {
  const btn = ev.currentTarget as HTMLButtonElement;
  const msg = document.getElementById("setup-msg") as HTMLElement;
  btn.disabled = true;
  const cancel = document.getElementById("setup-cancel") as HTMLButtonElement;
  cancel.hidden = false;
  btn.textContent = "Installing…";
  try {
    await invoke("setup_engine");
    msg.textContent = "Done. Starting up…";
  } catch (e) {
    // Setup is resumable, so say so rather than leaving a dead end.
    msg.textContent = `${e} — press Retry to pick up where it stopped.`;
    btn.disabled = false;
    cancel.hidden = true;
    btn.textContent = "Retry";
    return;
  }
  cancel.hidden = true;
  setTimeout(refresh, 1500);
});

document.getElementById("setup-cancel")?.addEventListener("click", async (ev) => {
  (ev.currentTarget as HTMLButtonElement).disabled = true;
  await invoke("cancel_setup");
});

// Show the real hotkeys rather than hardcoding them in the markup.
type HotkeyResponse = Record<HotkeySlot, string> & {
  bindings: Record<HotkeySlot, { label: string; registered: boolean; configurable: boolean }>;
};

type HotkeyCapture = {
  slot: HotkeySlot;
  accelerator: string;
  kind: "modifier-gesture" | "registered-shortcut";
};

// A shortcut is only proven after the runtime reports that the physical key
// reached an action adapter. Saving and OS registration are necessary but are
// not presented as end-to-end success.
let awaitingHotkeyVerification: HotkeySlot | null = null;
listen<HotkeySlot>("hotkey-triggered", (event) => {
  if (event.payload !== awaitingHotkeyVerification) return;
  detail.textContent = `${event.payload} shortcut verified — it reached Kokoro.`;
  awaitingHotkeyVerification = null;
});

invoke<HotkeyResponse>("hotkeys").then((hk) => {
  const set = (id: string, v: string) => {
    const el = document.getElementById(id);
    if (el) el.textContent = pretty(v);
  };
  set("key-read", hk.read);
  set("key-dictate", hk.dictate);
  set("key-snip", hk.snip);
  document.querySelectorAll<HTMLButtonElement>("button[data-rec]").forEach((btn) => {
    const slot = btn.dataset.rec;
    const binding = isHotkeySlot(slot) ? hk.bindings[slot] : undefined;
    btn.hidden = binding ? !binding.configurable : false;
  });
  if (Object.values(hk.bindings).some((binding) => !binding.registered)) {
    detail.textContent = "Shortcuts are off. Run System Check and enable the permission it names.";
  }
});

let testingDictation = false;
listen<{ session: string; state: DictationState }>("dictation-state", (e) => {
  if (e.payload.state === "starting") {
    testingDictation = document.activeElement?.id === "dictation-test";
  }
  const messages: Record<DictationState, string> = {
    starting: "Opening microphone…",
    recording: "Listening…",
    transcribing: "Transcribing locally…",
    completed: "Dictation inserted and copied to the clipboard.",
    cancelled: "Dictation cancelled.",
    "permission-denied": "Microphone permission denied. Open Privacy & Security.",
    "device-unavailable": "The selected microphone is unavailable.",
    "timed-out": "The microphone did not open in time.",
    "live-typing": "Typing the local transcript…",
    "clipboard-fallback": "The transcript was copied because the target could not be verified.",
    "cancelled-by-user": "Dictation cancelled.",
  };
  detail.textContent = messages[e.payload.state];
});

listen<string>("dictated", async () => {
  const test = document.getElementById("dictation-test") as HTMLTextAreaElement;
  if (!testingDictation || !test || !test.value.trim()) return;
  testingDictation = false;
  const status = document.getElementById("dictation-test-status") as HTMLElement;
  await invoke("record_capability", { capability: "dictation-insertion", passed: true });
  await invoke("set_prefs", { livePreview: true });
  const toggle = document.getElementById("live-preview") as HTMLInputElement;
  if (toggle) toggle.checked = true;
  status.textContent = "Dictation passed. Live typing is enabled.";
});

// ── voice & speed ───────────────────────────────────────────────────────────
const SPEEDS = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

async function initPrefs() {
  const prefs = await invoke<{ voice: string; speed: number; cue_enabled?: boolean; cue_volume?: number; live_preview?: boolean }>("get_prefs");

  const cueEnabled = document.getElementById("cue-enabled") as HTMLInputElement;
  const cueVolume = document.getElementById("cue-volume") as HTMLInputElement;
  const cueVolumeLabel = document.getElementById("cue-volume-label") as HTMLOutputElement;
  cueEnabled.checked = prefs.cue_enabled !== false;
  cueVolume.value = String(Math.round((prefs.cue_volume ?? 0.22) * 100));
  cueVolumeLabel.value = `${cueVolume.value}%`;
  cueEnabled.onchange = () => {
    void invoke("set_prefs", { cueEnabled: cueEnabled.checked });
  };
  cueVolume.oninput = () => {
    cueVolumeLabel.value = `${cueVolume.value}%`;
  };
  cueVolume.onchange = () => {
    void invoke("set_prefs", { cueVolume: Number(cueVolume.value) / 100 });
  };
  const livePreview = document.getElementById("live-preview") as HTMLInputElement;
  livePreview.checked = prefs.live_preview !== false;
  livePreview.onchange = () => {
    void invoke("set_prefs", { livePreview: livePreview.checked });
  };
  const launchAtLogin = document.getElementById("launch-at-login") as HTMLInputElement;
  launchAtLogin.checked = await invoke<boolean>("launch_at_login_status");
  launchAtLogin.onchange = async () => {
    launchAtLogin.checked = await invoke<boolean>("set_launch_at_login", {
      enabled: launchAtLogin.checked,
    });
  };

  const microphones = await invoke<Array<{ id: number; name: string; default: boolean }>>("microphone_devices");
  const mic = document.getElementById("microphone") as HTMLSelectElement;
  mic.innerHTML = '<option value="">System default</option>';
  const savedMicrophone = String((prefs as Record<string, unknown>).microphone_device ?? "");
  for (const device of microphones) {
    const option = document.createElement("option");
    option.value = device.name;
    option.textContent = device.name + (device.default ? " (default)" : "");
    option.selected = savedMicrophone === option.value;
    mic.appendChild(option);
  }
  if (savedMicrophone && !microphones.some((device) => device.name === savedMicrophone)) {
    mic.value = "";
    await invoke("set_microphone", { device: null });
    detail.textContent = "Saved microphone is unavailable; using the system default.";
  }
  mic.onchange = () => { void invoke("set_microphone", { device: mic.value || null }); };

  // Voices come from the engine, so this only populates once it is up. Retry
  // rather than leaving an empty dropdown if setup is still running.
  const voices = await invoke<string[]>("list_voices");
  const sel = document.getElementById("voice") as HTMLSelectElement;
  if (!voices.length) {
    setTimeout(initPrefs, 5000);
    return;
  }
  sel.innerHTML = "";
  for (const v of voices) {
    const o = document.createElement("option");
    o.value = v;
    o.textContent = v;
    o.selected = v === prefs.voice;
    sel.appendChild(o);
  }
  sel.addEventListener("change", () => invoke("set_prefs", { voice: sel.value }));

  const chips = document.getElementById("speeds") as HTMLElement;
  chips.innerHTML = "";
  for (const sp of SPEEDS) {
    const b = document.createElement("button");
    b.className = "chip" + (Math.abs(sp - prefs.speed) < 0.01 ? " on" : "");
    b.textContent = `${sp}x`;
    b.addEventListener("click", async () => {
      await invoke("set_prefs", { speed: sp });
      chips.querySelectorAll(".chip").forEach((c) => c.classList.remove("on"));
      b.classList.add("on");
    });
    chips.appendChild(b);
  }
}

// ── hotkey recorder ─────────────────────────────────────────────────────────
// Captures a REAL key press in this window, so the binding is whatever actually
// arrives — after any KVM has translated it. e.code is used rather than e.key
// because a KVM can rewrite the produced character while the physical key code
// survives.
const MOD_GLYPH: Record<string, string> = {
  Control: "\u2303", Alt: "\u2325", Shift: "\u21e7", Command: "\u2318",
};
const MODIFIER_ORDER = ["Control", "Alt", "Shift", "Command"];

function pretty(accel: string): string {
  return accel
    .split("+")
    .map((p) => MOD_GLYPH[p] ?? p.replace(/^Key/, "").replace(/^Digit/, ""))
    .join("");
}

function accelFrom(e: KeyboardEvent, observedModifiers: ReadonlySet<string>): string | null {
  const modifiers = new Set(observedModifiers);
  if (e.ctrlKey) modifiers.add("Control");
  if (e.altKey) modifiers.add("Alt");
  if (e.shiftKey) modifiers.add("Shift");
  if (e.metaKey) modifiers.add("Command");
  const code = e.code;
  // A modifier on its own is not a shortcut the OS can register.
  if (/^(Control|Alt|Shift|Meta)(Left|Right)$/.test(code)) return null;
  // Function keys are valid with no modifier; anything else needs one.
  if (modifiers.size === 0 && !/^F\d+$/.test(code)) return null;
  return [...MODIFIER_ORDER.filter((modifier) => modifiers.has(modifier)), code].join("+");
}

let hotkeyRecorderActive = false;
document.querySelectorAll<HTMLButtonElement>("button[data-rec]").forEach((btn) => {
  btn.addEventListener("click", async () => {
    if (hotkeyRecorderActive) return;
    const slotValue = btn.dataset.rec;
    if (!isHotkeySlot(slotValue)) {
      detail.textContent = "Shortcut recorder is missing a valid action slot.";
      return;
    }
    hotkeyRecorderActive = true;
    const slot = slotValue;
    const original = btn.textContent;
    const recorderButtons = document.querySelectorAll<HTMLButtonElement>("button[data-rec]");
    recorderButtons.forEach((button) => { button.disabled = true; });
    try {
      await invoke("begin_hotkey_recording");
    } catch (err) {
      hotkeyRecorderActive = false;
      recorderButtons.forEach((button) => { button.disabled = false; });
      detail.textContent = String(err);
      return;
    }
    recorderButtons.forEach((button) => { button.disabled = button !== btn; });
    btn.textContent = "Press keys…";
    btn.classList.add("recording");
    const pressedModifiers = new Set<string>();
    let finished = false;
    let captureCommitted = false;
    let recorderTimeout: number | undefined;

    const finish = async (resumeAdapters: boolean) => {
      if (finished) return;
      finished = true;
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("keyup", onKeyUp, true);
      if (recorderTimeout !== undefined) window.clearTimeout(recorderTimeout);
      btn.textContent = original;
      btn.classList.remove("recording");
      hotkeyRecorderActive = false;
      recorderButtons.forEach((button) => { button.disabled = false; });
      if (resumeAdapters) {
        try {
          await invoke("end_hotkey_recording");
        } catch (err) {
          detail.textContent = String(err);
        }
      }
    };

    const saveCaptured = async (accel: string) => {
      if (finished || captureCommitted) return;
      captureCommitted = true;
      try {
        const saved = await invoke<HotkeyCapture>("set_hotkey", { slot, accelerator: accel });
        await finish(false); // set_hotkey already resumed and updated the platform controller.
        const current = await invoke<HotkeyResponse>("hotkeys");
        const currentLabel = current[slot];
        const kbd = document.getElementById(`key-${slot}`);
        if (kbd) kbd.textContent = pretty(currentLabel);
        awaitingHotkeyVerification = slot;
        detail.textContent = `${pretty(saved.accelerator)} registered for ${slot}. Press it now to verify.`;
      } catch (err) {
        await finish(true); // Validation failed before commit; restore the active adapters.
        const kbd = document.getElementById(`key-${slot}`);
        if (kbd) kbd.textContent = String(err);
        detail.textContent = String(err);
      }
    };

    const modifierName = (e: KeyboardEvent): string | null => {
      // Deskflow can rewrite `key` (on this Mac, physical Shift arrives as
      // CapsLock) while preserving the hardware-oriented `code`. Prefer code,
      // and accept Deskflow's CapsLock translation only inside this recorder.
      if (/^Control/.test(e.code) || e.key === "Control") return "Control";
      if (/^Alt/.test(e.code) || e.key === "Alt") return "Alt";
      if (/^Shift/.test(e.code) || e.code === "CapsLock" || e.key === "Shift") return "Shift";
      if (/^(Meta|OS)/.test(e.code) || e.key === "Meta" || e.key === "OS" || e.key === "Super") return "Command";
      return null;
    };

    const onKey = async (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (finished || captureCommitted) return;
      if (e.code === "Escape") { await finish(true); return; }
      const modifier = modifierName(e);
      if (modifier) {
        pressedModifiers.add(modifier);
        return;
      }
      const accel = accelFrom(e, pressedModifiers);
      if (!accel) return;   // still waiting for a full combination
      await saveCaptured(accel);
    };

    const onKeyUp = async (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (finished || captureCommitted) return;
      if (!modifierName(e) || pressedModifiers.size < 2) return;
      const accel = MODIFIER_ORDER.filter((modifier) => pressedModifiers.has(modifier)).join("+");
      await saveCaptured(accel);
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKeyUp, true);
    // Avoid leaving shortcuts suspended forever if the recorder is abandoned,
    // but do not cancel on window blur: macOS/Deskflow can briefly report a
    // blur while a modifier chord is being delivered.
    recorderTimeout = window.setTimeout(() => { void finish(true); }, 15_000);
  });
});

initPrefs();
refresh();
setInterval(refresh, 4000);
