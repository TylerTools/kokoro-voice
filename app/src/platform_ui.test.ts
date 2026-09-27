import { describe, expect, it } from "vitest";
import {
  formatShortcut,
  permissionSettingsName,
  setupIntro,
  versionLabel,
  type AppInfo,
} from "./platform_ui";

const mac: AppInfo = {
  app_version: "2.1.1-beta.9",
  build_revision: "0123456789abcdef",
  platform: "macos",
  architecture: "aarch64",
  paste_shortcut: "Command+V",
};

const windows: AppInfo = {
  ...mac,
  platform: "windows",
  architecture: "x86_64",
  paste_shortcut: "Ctrl+V",
};

describe("platform presentation policy", () => {
  it("keeps the Apple setup language on macOS", () => {
    expect(setupIntro(mac)).toContain("macOS access");
    expect(permissionSettingsName(mac)).toBe("Privacy & Security");
  });

  it("does not send Windows users into Apple settings", () => {
    expect(setupIntro(windows)).toContain("Windows asks");
    expect(setupIntro(windows)).not.toContain("macOS");
    expect(permissionSettingsName(windows)).toBe("system settings");
  });

  it("renders the same stored accelerator in native notation", () => {
    expect(formatShortcut("Control+Command+KeyU", "macos")).toBe("⌃⌘U");
    expect(formatShortcut("Control+Command+KeyU", "windows")).toBe("Ctrl+Win+U");
    expect(formatShortcut("F7", "windows")).toBe("F7");
  });

  it("shows both semantic version and immutable revision", () => {
    expect(versionLabel(mac)).toBe("HereWord 2.1.1-beta.9 · 01234567");
    expect(versionLabel({ ...mac, build_revision: "development" })).toBe(
      "HereWord 2.1.1-beta.9 · development",
    );
  });
});

