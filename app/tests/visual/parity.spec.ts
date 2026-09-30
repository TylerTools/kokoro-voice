import { expect, test, type Page, type TestInfo } from "@playwright/test";

type Platform = "macos" | "windows";

async function installTauriMock(page: Page, platform: Platform, denied?: "accessibility" | "input_monitoring", registered = true): Promise<void> {
  await page.addInitScript(({ selectedPlatform, deniedPermission, initialRegistered }) => {
    const callbacks = new Map<number, (...args: unknown[]) => void>();
    const eventHandlers = new Map<string, number>();
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
    if (deniedPermission && selectedPlatform === "macos") permissions[deniedPermission] = "required";
    const state = {
      permissions, registered: initialRegistered, checksFail: false,
      pauseOtherMedia: false, prefsFail: false,
      emit(event: string, payload: unknown) {
        const id = eventHandlers.get(event);
        if (id) callbacks.get(id)?.({ event, payload });
      },
    };
    const shortcutReport = () => ({
      ...hotkeys,
      bindings: Object.fromEntries(Object.entries(hotkeys).map(([slot, label]) => [
        slot, { label, registered: state.registered, configurable: true },
      ])),
    });

    Object.assign(window, {
      __setupTest: state,
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
        async invoke(command: string, args?: { capability?: string; event?: string; handler?: number }) {
          if (command === "plugin:event|listen") {
            if (args?.event && args.handler) eventHandlers.set(args.event, args.handler);
            return 1;
          }
          if (command.startsWith("plugin:event|")) return null;
          if (command.startsWith("plugin:opener|")) return null;
          switch (command) {
            case "engine_status":
              return { status: "ok", voices: 54, stt_ready: true, stt_warm: false };
            case "system_check":
              if (state.checksFail) throw new Error("System check unavailable");
              return {
                app,
                engine: { status: "ok" },
                permissions,
                hotkeys: shortcutReport(),
                microphones: [],
                setup: { stage: "complete" },
                offline_ready: true,
              };
            case "hotkeys":
              return shortcutReport();
            case "retry_permission":
              return { available: args?.capability === "input-monitoring"
                ? permissions.input_monitoring === "available"
                : args?.capability === "accessibility" ? permissions.accessibility === "available" : true };
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
                pause_other_media: state.pauseOtherMedia,
              };
            case "set_prefs":
              if (state.prefsFail) throw new Error("Could not save preferences");
              if (args && "pauseOtherMedia" in args) state.pauseOtherMedia = Boolean(args.pauseOtherMedia);
              return null;
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
  }, { selectedPlatform: platform, deniedPermission: denied, initialRegistered: registered });
}

test("practice tour waits for each real action and the person's confirmation", async ({ page }, testInfo) => {
  await installTauriMock(page, "macos");
  await page.goto("/");
  await expect(page.locator("#tour-launch")).toBeVisible();
  await page.locator("#tour-open").click();
  await expect(page.locator("#tour-title")).toHaveText("Make the shortcuts yours");
  await expect(page.locator("#tour-next")).toBeDisabled();
  await page.locator("#tour-confirm").click();
  await page.locator("#tour-next").click();
  await expect(page.locator("#tour-title")).toHaveText("Read selected text");
  await expect(page.locator("#tour-confirm")).toBeDisabled();
  await page.evaluate(() => {
    (window as unknown as { __setupTest: { emit: (event: string, payload: unknown) => void } })
      .__setupTest.emit("practice-action", "read");
  });
  await expect(page.locator("#tour-confirm")).toBeEnabled();
  await page.locator("#tour-confirm").click();
  await page.locator("#tour-next").click();
  await expect(page.locator("#tour-title")).toHaveText("Dictate a short sentence");
  await page.evaluate(() => {
    (window as unknown as { __setupTest: { emit: (event: string, payload: unknown) => void } })
      .__setupTest.emit("dictation-state", { session: "practice", state: "completed" });
  });
  await expect(page.locator("#tour-confirm")).toBeEnabled();
  await page.locator("#tour-confirm").click();
  await page.locator("#tour-next").click();
  await expect(page.locator("#tour-title")).toHaveText("Snip and read");
  await capture(page, testInfo, "practice-tour-snip");
  await page.evaluate(() => {
    (window as unknown as { __setupTest: { emit: (event: string, payload: unknown) => void } })
      .__setupTest.emit("practice-action", "snip");
  });
  await page.locator("#tour-confirm").click();
  await page.locator("#tour-next").click();
  await expect(page.locator("#tour")).toBeHidden();
  await expect(page.locator("#tour-open")).toHaveText("Repeat practice tour");
});

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
    await expect(page.locator("#pause-other-media")).not.toBeChecked();
    await page.locator("#pause-other-media").check();
    expect(await page.evaluate(() => (window as unknown as { __setupTest: { pauseOtherMedia: boolean } }).__setupTest.pauseOtherMedia)).toBe(true);
    await page.evaluate(() => { (window as unknown as { __setupTest: { prefsFail: boolean } }).__setupTest.prefsFail = true; });
    await page.locator("#pause-other-media").click();
    await expect(page.locator("#pause-other-media")).toBeChecked();
    await expect(page.locator("#detail")).toContainText("Couldn’t save the media setting");
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

for (const denied of ["accessibility", "input_monitoring"] as const) {
  test(`permission recovery survives reopen — ${denied}`, async ({ page }) => {
    await installTauriMock(page, "macos", denied, false);
    await page.goto("/");
    await expect(page.locator("#status-text")).toHaveText("Setup incomplete");
    await page.locator("#setup-recovery summary").click();
    await expect(page.locator("#setup-recovery")).toContainText("/Applications/HereWord.app");
    await expect(page.locator("#setup-recovery")).toContainText("Quit & Reopen");
    await page.locator("#setup-go").click();
    await page.reload();
    await expect(page.locator("#setup")).toBeVisible();
    await expect(page.locator("#status-text")).toHaveText("Setup incomplete");
    await page.evaluate(() => {
      const state = (window as unknown as { __setupTest: { permissions: Record<string, string>; registered: boolean } }).__setupTest;
      state.permissions.accessibility = "available";
      state.permissions.input_monitoring = "available";
      state.registered = true;
      window.dispatchEvent(new Event("focus"));
    });
    await expect(page.locator("#setup")).toBeHidden();
    await expect(page.locator("#status-text")).toHaveText("Ready");
    await expect(page.locator("#setup-recovery")).toBeHidden();
    expect(await page.evaluate(() => localStorage.getItem("kokoro-guided-setup-active"))).toBeNull();
  });
}

test("approved permissions still require shortcut registration", async ({ page }) => {
  await installTauriMock(page, "macos", undefined, false);
  await page.goto("/");
  await expect(page.locator("#status-text")).toHaveText("Setup incomplete");
  await expect(page.locator("#setup-go")).toHaveText("Check shortcuts");
  await expect(page.locator("#setup-state-input")).toHaveText("Not registered");
  await expect(page.locator("#setup-recovery")).toBeHidden();
  await page.waitForTimeout(4200);
  await expect(page.locator("#status-text")).toHaveText("Setup incomplete");
  await page.evaluate(() => {
    (window as unknown as { __setupTest: { registered: boolean } }).__setupTest.registered = true;
    window.dispatchEvent(new Event("focus"));
  });
  await expect(page.locator("#status-text")).toHaveText("Ready");
  await expect(page.locator("#setup")).toBeHidden();
  await expect(page.locator("#detail")).toHaveText("Setup complete.");
});

test("failed system check clears stale Ready status", async ({ page }) => {
  await installTauriMock(page, "macos");
  await page.goto("/");
  await expect(page.locator("#status-text")).toHaveText("Ready");
  await page.evaluate(() => {
    (window as unknown as { __setupTest: { checksFail: boolean } }).__setupTest.checksFail = true;
    window.dispatchEvent(new Event("focus"));
  });
  await expect(page.locator("#status-text")).toHaveText("Checking setup…");
  await expect(page.locator("#setup")).toBeVisible();
  await expect(page.locator("#setup-go")).toHaveText("Check setup");
});

for (const mode of ["playing", "starting", "recording", "transcribing"] as const) {
  test(`player state — ${mode}`, async ({ page }, testInfo) => {
    await installTauriMock(page, "windows");
    await page.setViewportSize({ width: 205, height: 50 });
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
    expect(bar!.x + bar!.width).toBeLessThanOrEqual(205);
    await expect(page.getByRole("button", { name: "Move player" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Close popup" })).toBeVisible();
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
  await expect(page.getByRole("button", { name: "Close popup" })).toBeVisible();
  await capture(page, testInfo, "player-notice-windows");
});

test("player drag and close send the matching native commands", async ({ page }) => {
  await installTauriMock(page, "macos");
  await page.setViewportSize({ width: 205, height: 50 });
  await page.goto("/player.html");
  await page.evaluate(() => {
    const bridge = (window as typeof window & {
      __TAURI_INTERNALS__: {
        invoke: (command: string, args?: unknown) => Promise<unknown>;
      };
      __playerCalls?: Array<{ command: string; args?: unknown }>;
    }).__TAURI_INTERNALS__;
    const original = bridge.invoke.bind(bridge);
    (window as typeof window & { __playerCalls?: Array<{ command: string; args?: unknown }> }).__playerCalls = [];
    bridge.invoke = async (command, args) => {
      (window as typeof window & { __playerCalls: Array<{ command: string; args?: unknown }> }).__playerCalls.push({ command, args });
      return original(command, args);
    };
  });
  const handle = await page.getByRole("button", { name: "Move player" }).boundingBox();
  expect(handle).not.toBeNull();
  await page.mouse.move(handle!.x + 15, handle!.y + 25);
  await page.mouse.down();
  await page.mouse.move(handle!.x + 35, handle!.y + 25, { steps: 4 });
  await page.mouse.up();
  await expect.poll(() => page.evaluate(() =>
    (window as typeof window & { __playerCalls: Array<{ command: string }> }).__playerCalls
      .some((call) => call.command === "save_player_position"),
  )).toBe(true);
  await page.getByRole("button", { name: "Close popup" }).click();
  const calls = await page.evaluate(() =>
    (window as typeof window & { __playerCalls: Array<{ command: string; args?: unknown }> }).__playerCalls,
  );
  expect(calls.some((call) => call.command === "move_player_by")).toBe(true);
  expect(calls).toContainEqual({ command: "close_player_popup", args: { notice: false } });
  await page.evaluate(() => {
    (window as typeof window & { __kokoroShowNotice?: (message: string) => void })
      .__kokoroShowNotice?.("Nothing selected");
  });
  await page.getByRole("button", { name: "Close popup" }).click();
  const noticeCalls = await page.evaluate(() =>
    (window as typeof window & { __playerCalls: Array<{ command: string; args?: unknown }> }).__playerCalls,
  );
  expect(noticeCalls).toContainEqual({ command: "close_player_popup", args: { notice: true } });
});
