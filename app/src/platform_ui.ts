/**
 * Shared platform presentation policy.
 *
 * Rust reports platform and build facts; this module turns those facts into
 * labels without duplicating the settings or player UI. Native behavior stays
 * in Rust adapters, so a wording change here cannot weaken an OS boundary.
 */

export type DesktopPlatform = "macos" | "windows" | "linux" | "unknown";

export type AppInfo = {
  app_version: string;
  build_revision: string;
  platform: DesktopPlatform;
  architecture: string;
  paste_shortcut: string;
};

export function setupIntro(info: AppInfo): string {
  if (info.platform === "macos") {
    return "Download the local models, then allow the required macOS access. HereWord opens each setting and checks it automatically.";
  }
  if (info.platform === "windows") {
    return "Download the local models. Windows asks for microphone access the first time it is needed.";
  }
  return "Download the local models to finish setting up HereWord on this computer.";
}

export function permissionSettingsName(info: AppInfo): string {
  return info.platform === "macos" ? "Privacy & Security" : "system settings";
}

export function formatShortcut(accelerator: string, platform: DesktopPlatform): string {
  const macGlyph: Record<string, string> = {
    Control: "\u2303",
    Alt: "\u2325",
    Shift: "\u21e7",
    Command: "\u2318",
  };
  const windowsLabel: Record<string, string> = {
    Control: "Ctrl",
    Alt: "Alt",
    Shift: "Shift",
    Command: "Win",
  };
  const pieces = accelerator
    .split("+")
    .filter(Boolean)
    .map((piece) => {
      if (platform === "macos") return macGlyph[piece] ?? normalizeKey(piece);
      return windowsLabel[piece] ?? normalizeKey(piece);
    });
  return platform === "macos" ? pieces.join("") : pieces.join("+");
}

function normalizeKey(value: string): string {
  return value.replace(/^Key/, "").replace(/^Digit/, "");
}

export function versionLabel(info: AppInfo): string {
  const revision = info.build_revision === "development"
    ? "development"
    : info.build_revision.slice(0, 8);
  return `HereWord ${info.app_version} · ${revision}`;
}

