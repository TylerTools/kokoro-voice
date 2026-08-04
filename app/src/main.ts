import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";

type Health = {
  status: string;
  voices?: number;
  stt_ready?: boolean;
  auth_required?: boolean;
};

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

document.getElementById("open-privacy")?.addEventListener("click", () => {
  openUrl("x-apple.systempreferences:com.apple.preference.security?Privacy");
});

refresh();
setInterval(refresh, 4000);
