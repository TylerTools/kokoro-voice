import { describe, expect, it } from "vitest";
import { nextSetupStep, permissionReady, type SetupReport } from "./setup_flow";

const base: SetupReport = {
  app: {
    app_version: "2.1.1-beta.9",
    build_revision: "development",
    platform: "windows",
    architecture: "x86_64",
    paste_shortcut: "Ctrl+V",
  },
  engine: { status: "ok" },
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
});

