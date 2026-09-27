import { expect, test, type Page, type TestInfo } from "@playwright/test";

type Platform = "macos" | "windows";

async function installTauriMock(page: Page, platform: Platform): Promise<void> {
  await page.addInitScript((selectedPlatform) => {
    const callbacks = new Map<number, (...args: unknown[]) => void>();
    let callbackId = 1;
    const app = {
      app_version: "2.1.1-beta.9",
      build_revision: "0123456789abcdef0123456789abcdef01234567",
      platform: selectedPlatform,
      architecture: selectedPlatform === "macos" ? "aarch64" : "x86_64",
      paste_shortcut: selectedPlatform === "macos" ? "Command+V" : "Ctrl+V",
    };
    const permissions = selectedPlatform === "macos"
      ? {
          microphone: "available",
          accessibility: "available",
          input_monitoring: "available",
          screen_capture: "checked-on-use",
        }
      : {
          microphone: "checked-on-use",
          accessibility: "not-required",
          input_monitoring: "not-required",
          screen_capture: "available",
        };
    const hotkeys = {
      read: "Control+Alt+Command+KeyU",
      dictate: "Control+Alt+Command+KeyI",
      snip: "Control+Alt+Command+KeyP",
    };

    Object.assign(window, {
      __TAURI_INTERNALS__: {
        callbacks,
        transformCallback(callback: (...args: unknown[]) => void) {
          const id = callbackId++;
          callbacks.set(id, callback);
          return id;
        },
        unregisterCallback(id: number) {
          callbacks.delete(id);
        },
        runCallback(id: number, data: unknown) {
          callbacks.get(id)?.(data);
        },
        convertFileSrc(path: string) {
          return path;
        },
        async invoke(command: string) {
          if (command.startsWith("plugin:event|")) return 1;
          if (command.startsWith("plugin:opener|")) return null;
          switch (command) {
            case "engine_status":
              return { status: "ok", voices: 54, stt_ready: true, stt_warm: false };
            case "system_check":
              return {
                app,
                engine: { status: "ok" },
                permissions,
                hotkeys,
                microphones: [],
                setup: { stage: "complete" },
                offline_ready: true,
              };
            case "hotkeys":
              return {
                ...hotkeys,
                bindings: Object.fromEntries(
                  Object.entries(hotkeys).map(([slot, label]) => [
                    slot,
                    { label, registered: true, configurable: true },
                  ]),
                ),
              };
            case "storage_status":
              return {
                engine_bytes: 0,
                config_bytes: 0,
                legacy_runtime_bytes: 0,
                shared_stt_cache_bytes: 0,
              };
            case "get_prefs":
              return {
                voice: "af_heart",
                speed: 1,
                cue_enabled: true,
                cue_volume: 0.22,
                live_preview: true,
              };
            case "launch_at_login_status":
              return true;
            case "microphone_devices":
              return [];
            case "list_voices":
              return ["af_heart"];
            case "dictation_status":
              return { state: "idle" };
            default:
              return null;
          }
        },
      },
    });
  }, platform);
}

async function capture(page: Page, testInfo: TestInfo, name: string): Promise<void> {
  await page.screenshot({
    path: testInfo.outputPath(`${name}.png`),
    fullPage: true,
  });
}

for (const platform of ["macos", "windows"] as const) {
  test(`settings contract — ${platform}`, async ({ page }, testInfo) => {
    await installTauriMock(page, platform);
    await page.goto("/");
    await expect(page.locator("body")).toHaveAttribute("data-platform", platform);
    await expect(page.locator("#app-version")).toContainText("2.1.1-beta.9 · 01234567");
    await expect(page.locator("#setup")).toBeHidden();
    await expect(page.locator("#key-read")).toHaveText(
      platform === "macos" ? "⌃⌥⌘U" : "Ctrl+Alt+Win+U",
    );
    const horizontalOverflow = await page.evaluate(
      () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(horizontalOverflow).toBeLessThanOrEqual(0);
    await capture(page, testInfo, `settings-${platform}`);
  });
}

for (const mode of ["playing", "starting", "recording", "transcribing"] as const) {
  test(`player state — ${mode}`, async ({ page }, testInfo) => {
    await installTauriMock(page, "windows");
    await page.setViewportSize({ width: 152, height: 50 });
    await page.goto("/player.html");
    await page.evaluate((playerMode) => {
      const api = window as typeof window & {
        __kokoroSetPlayerMode?: (value: string) => void;
      };
      api.__kokoroSetPlayerMode?.(playerMode);
    }, mode);
    await expect(page.locator("body")).toHaveClass(new RegExp(mode === "playing" ? "^$" : mode));
    const bar = await page.locator("#bar").boundingBox();
    expect(bar).not.toBeNull();
    expect(bar!.x).toBeGreaterThanOrEqual(0);
    expect(bar!.x + bar!.width).toBeLessThanOrEqual(152);
    await capture(page, testInfo, `player-${mode}`);
  });
}

test("player notice remains readable", async ({ page }, testInfo) => {
  await installTauriMock(page, "windows");
  await page.setViewportSize({ width: 360, height: 64 });
  await page.goto("/player.html");
  await page.evaluate(() => {
    const api = window as typeof window & {
      __kokoroShowNotice?: (value: string) => void;
    };
    api.__kokoroShowNotice?.("Copied. Press Ctrl+V to paste.");
  });
  await expect(page.locator("#notice-label")).toHaveText("Copied. Press Ctrl+V to paste.");
  await expect(page.locator("#notice-label")).toBeInViewport();
  await capture(page, testInfo, "player-notice-windows");
});
