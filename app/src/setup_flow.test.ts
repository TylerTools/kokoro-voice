import { describe, expect, it } from "vitest";
import { nextSetupStep, permissionReady, permissionRecoveryStep, readinessStatus, type SetupReport } from "./setup_flow";

const base: SetupReport = {
  app: {
    app_version: "2.1.1-beta.9",
    build_revision: "development",
    platform: "windows",
    architecture: "x86_64",
    paste_shortcut: "Ctrl+V",
  },
  engine: { status: "ok" },
  hotkeys: { bindings: { read: { registered: true }, dictate: { registered: true }, snip: { registered: true } } },
  permissions: {
    microphone: "checked-on-use",
    accessibility: "not-required",
    input_monitoring: "not-required",
    screen_capture: "available",
  },
  offline_ready: true,
};

describe("setup flow", () => {
  it("accepts Windows permission-on-use as ready", () => {
    expect(permissionReady("checked-on-use")).toBe(true);
    expect(nextSetupStep(base)).toBe("complete");
  });

  it("still gates an explicit macOS permission requirement", () => {
    const mac: SetupReport = {
      ...base,
      app: { ...base.app, platform: "macos", architecture: "aarch64", paste_shortcut: "Command+V" },
      permissions: { ...base.permissions, microphone: "required" },
    };
    expect(nextSetupStep(mac)).toBe("microphone");
  });

  it("keeps model installation ahead of permission work", () => {
    expect(nextSetupStep({ ...base, offline_ready: false })).toBe("download");
  });

  it("requires registration of every action after permissions pass", () => {
    const report = { ...base, hotkeys: { bindings: { ...base.hotkeys.bindings, snip: { registered: false } } } };
    expect(nextSetupStep(report)).toBe("shortcuts");
    expect(readinessStatus({ status: "ok", stt_ready: true }, report).label).toBe("Setup incomplete");
  });

  it("does not claim readiness from engine health before system check", () => {
    expect(readinessStatus({ status: "ok", stt_ready: true }, null).label).toBe("Checking setup…");
  });

  it("keeps an unready speech engine in setup", () => {
    expect(nextSetupStep({ ...base, engine: { status: "ok", tts_ready: false } })).toBe("engine-starting");
  });

  it("shows macOS recovery for the currently denied permission only", () => {
    const mac = { ...base, app: { ...base.app, platform: "macos" as const }, permissions: { ...base.permissions, accessibility: "required" as const, input_monitoring: "required" as const } };
    expect(permissionRecoveryStep(mac)).toBe("Accessibility");
    expect(readinessStatus({ status: "ok", stt_ready: true }, mac).label).toBe("Setup incomplete");
    expect(permissionRecoveryStep({ ...mac, permissions: { ...mac.permissions, accessibility: "available" } })).toBe("Input Monitoring");
    expect(permissionRecoveryStep(base)).toBeNull();
  });

  it("reports Ready only after setup completes while cold dictation remains available", () => {
    expect(readinessStatus({ status: "ok", stt_ready: true, stt_warm: false }, base).label).toBe("Ready");
    expect(readinessStatus({ status: "ok", stt_ready: false }, base).label).toBe("Dictation retrying");
  });
});
