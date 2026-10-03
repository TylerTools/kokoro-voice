/**
 * HereWord settings control plane.
 *
 * This frontend renders engine state and captures user input, but Rust owns
 * process lifecycle, shortcut meaning/registration, permissions, and durable
 * preferences. Keep platform policy out of this file; send captured facts to
 * the backend and render its authoritative result.
 */
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { listen } from "@tauri-apps/api/event";
import { initOnboarding } from "./onboarding";
import {
  nextSetupStep,
  permissionReady,
  permissionRecoveryStep,
  readinessStatus,
  shortcutsRegistered,
  type EngineHealth,
  type PermissionState,
  type SetupReport,
  type SetupStep,
} from "./setup_flow";
import {
  formatShortcut,
  permissionSettingsName,
  setupIntro,
  versionLabel,
  type AppInfo,
} from "./platform_ui";

type Health = EngineHealth;
let lastHealth: Health = { status: "starting" };

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
const SHORTCUT_SETUP_MESSAGE = "Finish setup to enable shortcuts. HereWord checks approvals automatically.";

const updateStatus = document.getElementById("update-status")!;
const checkUpdateButton = document.getElementById("check-update") as HTMLButtonElement;
const installUpdateButton = document.getElementById("install-update") as HTMLButtonElement;
const updateProgress = document.getElementById("update-progress") as HTMLProgressElement;

checkUpdateButton.addEventListener("click", async () => {
  checkUpdateButton.disabled = true;
  installUpdateButton.hidden = true;
  updateStatus.textContent = "Checking for updates…";
  try {
    const result = await invoke<{ configured: boolean; version: string | null }>("check_for_update");
    updateStatus.textContent = !result.configured ? "Updates aren't available in this build."
      : result.version ? `HereWord ${result.version} is available.` : "You're up to date.";
    installUpdateButton.hidden = !result.version;
  } catch (error) {
    updateStatus.textContent = String(error);
  } finally {
    checkUpdateButton.disabled = false;
  }
});

installUpdateButton.addEventListener("click", async () => {
  checkUpdateButton.disabled = true;
  installUpdateButton.disabled = true;
  updateProgress.hidden = false;
  updateProgress.removeAttribute("value");
  updateStatus.textContent = "Downloading and verifying the update…";
  try {
    await invoke("install_update");
    updateStatus.textContent = "Installing the update. HereWord will restart shortly.";
  } catch (error) {
    updateStatus.textContent = String(error);
    checkUpdateButton.disabled = false;
    installUpdateButton.disabled = false;
    updateProgress.hidden = true;
  }
});

listen<{ received: number; total: number | null }>("update-progress", ({ payload }) => {
  if (payload.total) {
    updateProgress.max = payload.total;
    updateProgress.value = payload.received;
  }
});
listen<string>("update-failed", ({ payload }) => {
  updateStatus.textContent = payload;
  checkUpdateButton.disabled = false;
  installUpdateButton.disabled = false;
  updateProgress.hidden = true;
});
let currentAppInfo: AppInfo = {
  app_version: "",
  build_revision: "development",
  platform: "unknown",
  architecture: "",
  paste_shortcut: "the paste shortcut",
};

function renderAppIdentity(info: AppInfo): void {
  currentAppInfo = info;
  document.body.dataset.platform = info.platform;
  const intro = document.getElementById("setup-intro");
  if (intro) intro.textContent = setupIntro(info);
  const version = document.getElementById("app-version");
  if (version) version.textContent = versionLabel(info);
}

/**
 * Poll the engine and describe it in plain language.
 *
 * TTS readiness gates startup. STT may intentionally be cold because its
 * memory is reclaimed after inactivity; cold is ready on demand, not degraded.
 */
async function refresh(): Promise<void> {
  let h: Health;
  try {
    h = (await invoke("engine_status")) as Health;
  } catch {
    h = { status: "down" };
  }

  lastHealth = h;
  renderReadiness();
}

function renderReadiness(): void {
  const status = readinessStatus(lastHealth, setupReport);
  statusEl.classList.remove("status--ok", "status--warn", "status--down", "status--unknown");
  statusEl.classList.add(`status--${status.level}`);
  statusText.textContent = status.label;
  statusEl.title = status.title;
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
      btn.textContent = "Done";
      setTimeout(() => (btn.textContent = original), 900);
    } catch (e) {
      btn.textContent = "failed";
      detail.textContent = String(e);
      setTimeout(() => (btn.textContent = original), 1600);
    }
  });
});

document.getElementById("export-diagnostics")?.addEventListener("click", async () => {
  const path = await invoke<string>("export_diagnostics");
  detail.textContent = `Diagnostics saved to ${path}`;
});

invoke<{
  engine_bytes: number;
  config_bytes: number;
  legacy_runtime_bytes: number;
  shared_stt_cache_bytes: number;
}>("storage_status").then((storage) => {
  const el = document.getElementById("storage-detail");
  if (el) {
    const owned = (storage.engine_bytes + storage.config_bytes) / 1_000_000_000;
    const shared = storage.shared_stt_cache_bytes / 1_000_000_000;
    const legacy = storage.legacy_runtime_bytes / 1_000_000;
    el.textContent = `${owned.toFixed(2)} GB app data + ${shared.toFixed(2)} GB shared speech model${
      legacy > 0 ? `; ${legacy.toFixed(1)} MB legacy temporary audio` : ""
    }`;
  }
});

document.getElementById("remove-local-data")?.addEventListener("click", async () => {
  if (!window.confirm("Remove the downloaded runtime and models from this computer?")) return;
  const removePreferences = (document.getElementById("remove-preferences") as HTMLInputElement).checked;
  await invoke("remove_local_data", { removePreferences });
  window.location.reload();
});

// One guided setup transaction. Rust reports the platform's permission facts;
// this shared UI renders only the steps that apply to that platform.
const GUIDED_SETUP_KEY = "kokoro-guided-setup-active";
const setupCard = document.getElementById("setup") as HTMLElement;
const setupButton = document.getElementById("setup-go") as HTMLButtonElement;
const setupMessage = document.getElementById("setup-msg") as HTMLElement;
const setupCancel = document.getElementById("setup-cancel") as HTMLButtonElement;
let guidedSetupActive = localStorage.getItem(GUIDED_SETUP_KEY) === "1";
let setupReport: SetupReport | null = null;
let requestedStep: SetupStep | null = null;
let setupRefreshInFlight = false;
let setupEffectInFlight = false;
const onboarding = initOnboarding();

function setSetupActive(active: boolean): void {
  guidedSetupActive = active;
  if (active) localStorage.setItem(GUIDED_SETUP_KEY, "1");
  else localStorage.removeItem(GUIDED_SETUP_KEY);
}

function renderPermissionState(id: string, state: PermissionState | undefined): void {
  const element = document.getElementById(id);
  if (!element) return;
  const ready = permissionReady(state);
  element.textContent = state === "checked-on-use" ? "On first use" : ready ? "Ready" : "Needs approval";
  element.classList.toggle("setup-state--ready", ready);
  element.classList.toggle("setup-state--needed", !ready);
}

function renderPermissionRow(id: string, state: PermissionState | undefined): void {
  const element = document.getElementById(id);
  if (element) element.hidden = state === "not-required";
}

function renderSetup(report: SetupReport): SetupStep {
  renderAppIdentity(report.app);
  const step = nextSetupStep(report);
  renderReadiness();
  const recovery = document.getElementById("setup-recovery") as HTMLDetailsElement;
  const recoveryPane = permissionRecoveryStep(report);
  recovery.hidden = recoveryPane === null;
  const recoveryLabel = document.getElementById("setup-recovery-pane");
  if (recoveryLabel) recoveryLabel.textContent = recoveryPane ?? "";
  const engineReady = report.offline_ready && report.engine.status === "ok" && report.engine.tts_ready !== false;
  const engineState = document.getElementById("setup-state-engine") as HTMLElement;
  engineState.textContent = engineReady ? "Ready" : report.offline_ready ? "Starting…" : "Not installed";
  engineState.classList.toggle("setup-state--ready", engineReady);
  engineState.classList.toggle("setup-state--needed", !engineReady);
  renderPermissionState("setup-state-microphone", report.permissions.microphone);
  renderPermissionState("setup-state-accessibility", report.permissions.accessibility);
  renderPermissionState("setup-state-input", report.permissions.input_monitoring);
  if (permissionReady(report.permissions.input_monitoring) && !shortcutsRegistered(report)) {
    const inputState = document.getElementById("setup-state-input") as HTMLElement;
    inputState.textContent = "Not registered";
    inputState.classList.remove("setup-state--ready");
    inputState.classList.add("setup-state--needed");
  }
  renderPermissionRow("setup-row-microphone", report.permissions.microphone);
  renderPermissionRow("setup-row-accessibility", report.permissions.accessibility);
  renderPermissionRow("setup-row-input", report.permissions.input_monitoring);

  setupCard.hidden = step === "complete";
  setupButton.hidden = step === "complete";
  setupButton.disabled = step === "engine-starting" || setupEffectInFlight;
  const labels: Record<SetupStep, string> = {
    download: "Download & finish setup",
    "engine-starting": "Starting…",
    microphone: "Allow Microphone",
    accessibility: "Continue in Accessibility",
    "input-monitoring": "Continue in Input Monitoring",
    shortcuts: "Check shortcuts",
    complete: "Setup complete",
  };
  setupButton.textContent = labels[step];
  if (step === "complete") {
    setSetupActive(false);
    setupMessage.textContent = "";
  } else if (!guidedSetupActive && !setupEffectInFlight) {
    setupMessage.textContent = "HereWord continues as soon as each approval is on.";
  }
  if (step === "shortcuts") {
    setupMessage.textContent = "Permissions are approved. HereWord is checking shortcuts. If this continues, quit and reopen HereWord; setup will resume and check again.";
  }
  onboarding.onSetupStep(step, guidedSetupActive);
  return step;
}

async function advanceGuidedSetup(report: SetupReport): Promise<void> {
  if (!guidedSetupActive || setupEffectInFlight) return;
  const step = nextSetupStep(report);
  if (step === "download" || step === "engine-starting" || step === "complete" || requestedStep === step) return;
  setupEffectInFlight = true;
  // Keep this latch until system_check advances or the user explicitly retries.
  // A permission API can report ready before the next setup check catches up.
  requestedStep = step;
  try {
    if (step === "microphone") {
      setupMessage.textContent = "Allow HereWord to use the microphone. Setup will continue automatically.";
      const result = await invoke<{ available?: boolean; requested?: boolean }>("retry_permission", { capability: "microphone" });
      if (!result.available && !result.requested) {
        await openUrl("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone");
      } else if (result.available) {
        window.setTimeout(() => { void refreshSetup(true); }, 0);
      }
    } else if (step === "accessibility") {
      setupMessage.textContent = "Turn on HereWord in Accessibility. This page will continue automatically.";
      const result = await invoke<{ available?: boolean }>("retry_permission", { capability: "accessibility" });
      if (!result.available) {
        await openUrl("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
      } else {
        window.setTimeout(() => { void refreshSetup(true); }, 0);
      }
    } else if (step === "input-monitoring") {
      setupMessage.textContent = "Turn on HereWord in Input Monitoring, then choose Quit & Reopen if macOS asks. Setup resumes and checks the new process automatically.";
      const result = await invoke<{ available?: boolean }>("retry_permission", { capability: "input-monitoring" });
      if (!result.available) {
        await openUrl("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent");
      } else {
        window.setTimeout(() => { void refreshSetup(true); }, 0);
      }
    }
  } catch (error) {
    requestedStep = null;
    setupMessage.textContent = `Setup could not continue: ${error}`;
  } finally {
    setupEffectInFlight = false;
    if (setupReport) renderSetup(setupReport);
  }
}

async function refreshSetup(advance = guidedSetupActive): Promise<void> {
  if (setupRefreshInFlight) return;
  setupRefreshInFlight = true;
  try {
    setupReport = await invoke<SetupReport>("system_check");
    const wasGuided = guidedSetupActive;
    const step = renderSetup(setupReport);
    if (step === "complete" && (wasGuided || detail.textContent === SHORTCUT_SETUP_MESSAGE)) {
      detail.textContent = "Setup complete.";
    } else if (advance) {
      await advanceGuidedSetup(setupReport);
    }
  } catch (error) {
    setupReport = null;
    renderReadiness();
    setupCard.hidden = false;
    setupButton.hidden = false;
    setupButton.disabled = false;
    setupButton.textContent = "Check setup";
    setupMessage.textContent = `HereWord could not verify setup: ${error}`;
  } finally {
    setupRefreshInFlight = false;
  }
}

// Live engine-install progress from Rust.
listen<{ pct: number; message: string }>("setup-progress", (e) => {
  const wrap = document.getElementById("bar-wrap") as HTMLElement;
  const bar = document.getElementById("bar") as HTMLElement;
  const msg = document.getElementById("setup-msg") as HTMLElement;
  wrap.hidden = false;
  bar.style.width = `${e.payload.pct}%`;
  msg.textContent = e.payload.message;
});

listen<boolean>("microphone-permission-changed", () => {
  void refreshSetup(true);
});

document.getElementById("setup-go")?.addEventListener("click", async (ev) => {
  const btn = ev.currentTarget as HTMLButtonElement;
  requestedStep = null;
  setSetupActive(true);
  if (!setupReport) await refreshSetup(false);
  if (!setupReport) return;
  const step = nextSetupStep(setupReport);
  if (step !== "download") {
    await advanceGuidedSetup(setupReport);
    return;
  }
  btn.disabled = true;
  setupCancel.hidden = false;
  btn.textContent = "Installing…";
  try {
    await invoke("setup_engine");
    setupMessage.textContent = "Models installed. Starting HereWord…";
  } catch (e) {
    // Setup is resumable, so say so rather than leaving a dead end.
    setupMessage.textContent = `${e} — press Retry to pick up where it stopped.`;
    btn.disabled = false;
    setupCancel.hidden = true;
    btn.textContent = "Retry";
    return;
  }
  setupCancel.hidden = true;
  requestedStep = null;
  await refreshSetup(true);
});

document.getElementById("setup-cancel")?.addEventListener("click", async (ev) => {
  (ev.currentTarget as HTMLButtonElement).disabled = true;
  await invoke("cancel_setup");
  setSetupActive(false);
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
  detail.textContent = `${event.payload} shortcut verified — it reached HereWord.`;
  awaitingHotkeyVerification = null;
});

async function refreshHotkeys(): Promise<void> {
  const hk = await invoke<HotkeyResponse>("hotkeys");
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
    detail.textContent = SHORTCUT_SETUP_MESSAGE;
  }
}

listen<{ session: string; state: DictationState }>("dictation-state", (e) => {
  const messages: Record<DictationState, string> = {
    starting: "Opening microphone…",
    recording: "Listening…",
    transcribing: "Transcribing locally…",
    completed: "Dictation complete.",
    cancelled: "Dictation cancelled.",
    "permission-denied": `Microphone permission denied. Open ${permissionSettingsName(currentAppInfo)}.`,
    "device-unavailable": "The selected microphone is unavailable.",
    "timed-out": "The microphone did not open in time.",
    "live-typing": "Typing…",
    "clipboard-fallback": `Copied. Press ${currentAppInfo.paste_shortcut} to paste.`,
    "cancelled-by-user": "Dictation cancelled.",
  };
  detail.textContent = messages[e.payload.state];
});

// ── voice & speed ───────────────────────────────────────────────────────────
const SPEEDS = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

async function initPrefs() {
  const prefs = await invoke<{ voice: string; speed: number; cue_enabled?: boolean; cue_volume?: number; live_preview?: boolean; pause_other_media?: boolean; media_mode?: "off" | "pause" | "duck" }>("get_prefs");

  const cueEnabled = document.getElementById("cue-enabled") as HTMLInputElement;
  const cueVolume = document.getElementById("cue-volume") as HTMLInputElement;
  const cueVolumeLabel = document.getElementById("cue-volume-label") as HTMLOutputElement;
  const cueVolumeRow = document.getElementById("cue-volume-row") as HTMLElement;
  cueEnabled.checked = prefs.cue_enabled !== false;
  cueVolumeRow.hidden = !cueEnabled.checked;
  cueVolume.value = String(Math.round((prefs.cue_volume ?? 0.22) * 100));
  cueVolumeLabel.value = `${cueVolume.value}%`;
  cueEnabled.onchange = () => {
    cueVolumeRow.hidden = !cueEnabled.checked;
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
  const mediaMode = document.getElementById("media-mode") as HTMLSelectElement;
  let savedMediaMode = prefs.media_mode ?? (prefs.pause_other_media === true ? "pause" : "off");
  mediaMode.value = savedMediaMode;
  mediaMode.onchange = async () => {
    const selected = mediaMode.value;
    mediaMode.disabled = true;
    try {
      await invoke("set_prefs", { mediaMode: selected });
      savedMediaMode = selected as typeof savedMediaMode;
    } catch {
      mediaMode.value = savedMediaMode;
      detail.textContent = "Couldn’t save the media setting. Try again.";
    } finally {
      mediaMode.disabled = false;
    }
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
    const unavailable = document.createElement("option");
    unavailable.value = savedMicrophone;
    unavailable.textContent = `${savedMicrophone} (unavailable)`;
    unavailable.selected = true;
    mic.appendChild(unavailable);
    detail.textContent = "Preferred microphone is unavailable; dictation uses the system default until it reconnects.";
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
const MODIFIER_ORDER = ["Control", "Alt", "Shift", "Command"];

function pretty(accel: string): string {
  return formatShortcut(accel, currentAppInfo.platform);
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
        window.dispatchEvent(new CustomEvent("hereword-hotkey-saved", { detail: { slot } }));
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

async function bootstrap(): Promise<void> {
  await refreshSetup();
  await refreshHotkeys();
}

initPrefs();
refresh();
void bootstrap();
setInterval(() => {
  if (document.visibilityState === "visible") {
    void refresh();
    void refreshSetup();
  }
}, 4000);
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") {
    void refresh();
    void refreshSetup();
  }
});
window.addEventListener("focus", () => { void refreshSetup(); });
