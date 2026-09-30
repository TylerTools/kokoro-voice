import { expect, test, type Page, type TestInfo } from "@playwright/test";

type Platform = "macos" | "windows";

async function installTauriMock(page: Page, platform: Platform, denied?: "accessibility" | "input_monitoring", registered = true, offlineReady = true): Promise<void> {
  await page.addInitScript(({ selectedPlatform, deniedPermission, initialRegistered, initialOfflineReady }) => {
    const callbacks = new Map<number, (...args: unknown[]) => void>();
    const eventHandlers = new Map<string, number[]>();
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
      permissions, registered: initialRegistered, checksFail: false, pauseOtherMedia: false, prefsFail: false, offlineReady: initialOfflineReady, hiddenWindow: false,
      emit(event: string, payload: unknown) {
        for (const handler of eventHandlers.get(event) ?? []) {
          callbacks.get(handler)?.({ event, id: 1, payload });
        }
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
        metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" }, windows: [{ label: "main" }], webviews: [{ label: "main" }] },
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
        async invoke(command: string, args?: Record<string, unknown>) {
          if (command === "plugin:event|listen") {
            const event = String(args?.event);
            eventHandlers.set(event, [...(eventHandlers.get(event) ?? []), Number(args?.handler)]);
            return 1;
          }
          if (command.startsWith("plugin:event|")) return 1;
          if (command === "plugin:window|hide") { state.hiddenWindow = true; return null; }
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
                offline_ready: state.offlineReady,
              };
            case "hotkeys":
              return shortcutReport();
            case "set_hotkey": {
              const slot = String(args?.slot) as keyof typeof hotkeys;
              const accelerator = String(args?.accelerator);
              hotkeys[slot] = accelerator;
              return { accelerator };
            }
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
  }, { selectedPlatform: platform, deniedPermission: denied, initialRegistered: registered, initialOfflineReady: offlineReady });
}

async function installNarrationMock(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const played: string[] = [];
    const paused: string[] = [];
    Object.assign(window, { __tourPlayed: played, __tourPaused: paused });
    Object.defineProperty(window, "Audio", {
      configurable: true,
      value: class {
        src: string;
        onended: (() => void) | null = null;
        onerror: (() => void) | null = null;
        constructor(src: string) { this.src = src; }
        play() {
          played.push(this.src);
          Object.assign(window, { __tourLastAudio: this });
          return Promise.resolve();
        }
        pause() { paused.push(this.src); }
      },
    });
  });
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

test("guided tour waits for practice evidence", async ({ page }, testInfo) => {
  await installTauriMock(page, "macos");
  await page.addInitScript(() => localStorage.setItem("hereword-tour-voice-on", "0"));
  await page.goto("/");
  await expect(page.locator("#tour")).toBeHidden();
  await page.locator("#tour-launch").click();
  await expect(page.locator("#tour-title")).toHaveText("Read selected words");
  await expect(page.locator("#tour-progress")).toHaveText("Step 1 of 3");
  await expect(page.locator(".shortcut-row").first()).toBeHidden();
  await expect(page.locator(".advanced")).toBeHidden();
  await expect(page.locator("#tour-next")).toBeDisabled();
  await page.locator("#tour-select-text").click();
  await page.evaluate(() => (window as unknown as { __setupTest: { emit: (event: string, payload: unknown) => void } }).__setupTest.emit("hotkey-triggered", "read"));
  await expect(page.locator("#tour-heard")).toBeEnabled();
  await page.locator("#tour-heard").click();
  await expect(page.locator("#tour-next")).toBeEnabled();
  await capture(page, testInfo, "tour-read");
  await page.emulateMedia({ colorScheme: "dark", reducedMotion: "reduce" });
  await capture(page, testInfo, "tour-read-dark-reduced-motion");
  expect(await page.locator(".tour-demo-key").evaluate((el) => getComputedStyle(el).animationName)).toBe("none");
  expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBeLessThanOrEqual(0);
  await page.locator("#tour-next").click();
  await expect(page.locator("#tour-title")).toHaveText("Speak and see your words");
  await page.locator("#tour-focus-dictate").click();
  await page.evaluate(() => (window as unknown as { __setupTest: { emit: (event: string, payload: unknown) => void } }).__setupTest.emit("hotkey-triggered", "dictate"));
  await page.locator("#tour-dictate-text").fill("HereWord hears me");
  await page.evaluate(() => (window as unknown as { __setupTest: { emit: (event: string, payload: unknown) => void } }).__setupTest.emit("dictation-state", { state: "completed" }));
  await expect(page.locator("#tour-next")).toBeEnabled();
  await page.locator("#tour-next").click();
  await expect(page.locator("#tour-title")).toHaveText("Read text from the screen");
  await expect(page.locator("#tour-heard")).toBeDisabled();
  await page.evaluate(() => (window as unknown as { __setupTest: { emit: (event: string, payload: unknown) => void } }).__setupTest.emit("hotkey-triggered", "snip"));
  await page.locator("#tour-heard").click();
  await expect(page.locator("#tour-title")).toHaveText("You're all set");
  await expect(page.locator("#tour-progress")).toHaveText("All set");
  await page.locator("#tour-next").click();
  await expect(page.locator("#tour")).toBeHidden();
  await expect(page.locator(".shortcut-row").first()).toBeVisible();
  expect(await page.evaluate(() => localStorage.getItem("hereword-tour-complete-v1"))).toBe("1");
});

test("a shortcut can be changed within the single visible lesson", async ({ page }) => {
  await installTauriMock(page, "macos");
  await page.addInitScript(() => localStorage.setItem("hereword-tour-voice-on", "0"));
  await page.goto("/");
  await page.locator("#tour-launch").click();
  await expect(page.locator(".shortcut-row").first()).toBeHidden();
  await page.locator("#tour-change").click();
  await expect(page.locator("#tour-feedback")).toContainText("Press your preferred keys");
  await page.keyboard.press("Control+Alt+Meta+R");
  await expect(page.locator("#tour-shortcut")).toContainText("R");
  await expect(page.locator("#tour-feedback")).toContainText("Shortcut saved");
});

test("new installation opens the tour after setup, while ready installations wait", async ({ page }) => {
  await installTauriMock(page, "macos", undefined, true, false);
  await installNarrationMock(page);
  await page.goto("/");
  await expect(page.locator("#tour")).toBeHidden();
  await expect(page.locator("#setup")).toBeVisible();
  await expect(page.locator("#setup-row-engine")).toHaveClass(/setup-row--current/);
  await expect(page.locator(".setup-steps li:visible")).toHaveCount(1);
  await expect(page.locator(".shortcut-row").first()).toBeHidden();
  await page.locator("#setup-go").click();
  await expect.poll(() => page.evaluate(() => (window as unknown as { __tourPlayed: string[] }).__tourPlayed.length)).toBe(1);
  expect(await page.evaluate(() => (window as unknown as { __tourPlayed: string[] }).__tourPlayed[0])).toBe("/onboarding/setup-download.wav");
  await page.evaluate(() => {
    (window as unknown as { __setupTest: { offlineReady: boolean } }).__setupTest.offlineReady = true;
    window.dispatchEvent(new Event("focus"));
  });
  await expect(page.locator("#tour-title")).toHaveText("Read selected words");
  await expect(page.locator("#tour")).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as { __tourPlayed: string[] }).__tourPlayed)).toEqual(["/onboarding/setup-download.wav", "/onboarding/tour-read.wav"]);
});

test("first-run guide says all set and closes setup after the last practice", async ({ page }) => {
  await installTauriMock(page, "macos", undefined, true, false);
  await installNarrationMock(page);
  await page.goto("/");
  await page.locator("#setup-go").click();
  await page.evaluate(() => {
    (window as unknown as { __setupTest: { offlineReady: boolean } }).__setupTest.offlineReady = true;
    window.dispatchEvent(new Event("focus"));
  });
  await expect(page.locator("#tour-title")).toHaveText("Read selected words");
  await page.locator("#tour-select-text").click();
  await page.evaluate(() => (window as unknown as { __setupTest: { emit: (event: string, payload: unknown) => void } }).__setupTest.emit("hotkey-triggered", "read"));
  await page.locator("#tour-heard").click();
  await page.locator("#tour-next").click();
  await page.locator("#tour-focus-dictate").click();
  await page.evaluate(() => (window as unknown as { __setupTest: { emit: (event: string, payload: unknown) => void } }).__setupTest.emit("hotkey-triggered", "dictate"));
  await page.locator("#tour-dictate-text").fill("I can use HereWord");
  await page.evaluate(() => (window as unknown as { __setupTest: { emit: (event: string, payload: unknown) => void } }).__setupTest.emit("dictation-state", { state: "completed" }));
  await expect(page.locator("#tour-next")).toBeEnabled();
  await page.locator("#tour-next").click();
  await page.evaluate(() => (window as unknown as { __setupTest: { emit: (event: string, payload: unknown) => void } }).__setupTest.emit("hotkey-triggered", "snip"));
  await page.locator("#tour-heard").click();
  await expect(page.locator("#tour-title")).toHaveText("You're all set");
  expect(await page.evaluate(() => (window as unknown as { __tourLastAudio: { src: string } }).__tourLastAudio.src)).toBe("/onboarding/tour-finish.wav");
  await page.evaluate(() => (window as unknown as { __tourLastAudio: { onended: () => void } }).__tourLastAudio.onended());
  await expect(page.locator("#tour")).toBeHidden();
  await expect.poll(() => page.evaluate(() => (window as unknown as { __setupTest: { hiddenWindow: boolean } }).__setupTest.hiddenWindow)).toBe(true);
});

test("guided tour speaks each lesson and keeps captions visible when muted", async ({ page }) => {
  await installTauriMock(page, "macos");
  await installNarrationMock(page);
  await page.goto("/");
  await page.locator("#tour-launch").click();
  await expect(page.locator("#tour-instruction")).toContainText("Select the sample text");
  expect(await page.evaluate(() => (window as unknown as { __tourPlayed: string[] }).__tourPlayed.length)).toBe(1);
  await page.locator("#tour-hear-again").click();
  expect(await page.evaluate(() => (window as unknown as { __tourPlayed: string[] }).__tourPlayed.length)).toBe(2);
  expect(await page.evaluate(() => (window as unknown as { __tourPaused: string[] }).__tourPaused)).toEqual(["/onboarding/tour-read.wav"]);
  await page.locator("#tour-narration").click();
  await expect(page.locator("#tour-narration")).toHaveAttribute("aria-pressed", "false");
  expect(await page.evaluate(() => (window as unknown as { __tourPaused: string[] }).__tourPaused)).toEqual(["/onboarding/tour-read.wav", "/onboarding/tour-read.wav"]);
  await page.locator("#tour-hear-again").click();
  expect(await page.evaluate(() => (window as unknown as { __tourPlayed: string[] }).__tourPlayed.length)).toBe(3);
  await page.locator("#tour-close").click();
  expect(await page.evaluate(() => (window as unknown as { __tourPlayed: string[] }).__tourPlayed.length)).toBe(3);
  await expect(page.locator("#tour")).toBeHidden();
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
