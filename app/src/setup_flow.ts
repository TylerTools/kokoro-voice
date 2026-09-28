/**
 * Pure setup-flow policy for the settings UI.
 *
 * This module decides the next user-visible setup step. It does not request OS
 * permissions or own engine lifecycle; those effects remain in the Tauri
 * composition root so the frontend cannot mistake a prompt for a grant.
 */

export type PermissionState = "available" | "required" | "not-required" | "checked-on-use";

export type EngineHealth = {
  status?: string;
  voices?: number;
  tts_ready?: boolean;
  stt_ready?: boolean;
  stt_warm?: boolean;
};

export type SetupReport = {
  app: import("./platform_ui").AppInfo;
  engine: EngineHealth;
  hotkeys: { bindings: Record<string, { registered: boolean }> };
  permissions: {
    accessibility?: PermissionState;
    input_monitoring?: PermissionState;
    microphone?: PermissionState;
    screen_capture?: PermissionState;
  };
  offline_ready: boolean;
};

export type SetupStep =
  | "download"
  | "engine-starting"
  | "microphone"
  | "accessibility"
  | "input-monitoring"
  | "shortcuts"
  | "complete";

export function permissionReady(state: PermissionState | undefined): boolean {
  // Windows asks for microphone permission when capture first begins. That is
  // a valid ready state, not an incomplete macOS-style settings transaction.
  return state === "available" || state === "not-required" || state === "checked-on-use";
}

export function shortcutsRegistered(report: SetupReport): boolean {
  const bindings = report.hotkeys?.bindings;
  return ["read", "dictate", "snip"].every((slot) => bindings?.[slot]?.registered === true);
}

export function nextSetupStep(report: SetupReport): SetupStep {
  if (!report.offline_ready || report.engine.status === "not-installed") return "download";
  if (report.engine.status !== "ok" || report.engine.tts_ready === false) return "engine-starting";
  if (!permissionReady(report.permissions.microphone)) return "microphone";
  if (!permissionReady(report.permissions.accessibility)) return "accessibility";
  if (!permissionReady(report.permissions.input_monitoring)) return "input-monitoring";
  if (!shortcutsRegistered(report)) return "shortcuts";
  return "complete";
}

export function readinessStatus(engine: EngineHealth, report: SetupReport | null): {
  label: string; level: "ok" | "warn" | "down"; title: string;
} {
  if (engine.status === "not-installed") {
    return { label: "Setup incomplete", level: "warn", title: "Finish setup to enable HereWord." };
  }
  if (engine.status === "ok") {
    if (!report) return { label: "Checking setup…", level: "warn", title: "Checking permissions and shortcuts." };
    if (nextSetupStep(report) !== "complete") {
      return { label: "Setup incomplete", level: "warn", title: "Speech models are installed. Finish setup to enable permissions and shortcuts." };
    }
    if (engine.tts_ready === false) {
      return { label: "Starting…", level: "warn", title: "Loading local voices." };
    }
    if (!engine.stt_ready) {
      return { label: "Dictation retrying", level: "warn", title: "Reading works. Dictation will retry when used." };
    }
    return {
      label: "Ready", level: "ok",
      title: engine.stt_warm
        ? `${engine.voices ?? 0} voices · speech recognition ready`
        : `${engine.voices ?? 0} voices · speech recognition ready on demand`,
    };
  }
  if (engine.status === "starting") return { label: "Starting…", level: "warn", title: "Loading local voices." };
  return { label: "Not running", level: "down", title: "Quit and reopen HereWord." };
}

export function permissionRecoveryStep(report: SetupReport): "Accessibility" | "Input Monitoring" | null {
  if (report.app.platform !== "macos") return null;
  const step = nextSetupStep(report);
  return step === "accessibility" ? "Accessibility" : step === "input-monitoring" ? "Input Monitoring" : null;
}
