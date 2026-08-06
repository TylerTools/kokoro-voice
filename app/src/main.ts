import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { listen } from "@tauri-apps/api/event";

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

  // Not installed is a first-run state, not a failure — show setup, not an error.
  const setup = document.getElementById("setup") as HTMLElement;
  if (h.status === "not-installed") {
    setup.hidden = false;
    statusEl.classList.add("status--warn");
    statusText.textContent = "Setup needed";
    detail.textContent = "Download the voices to get started.";
    return;
  }
  setup.hidden = true;

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

// Live setup progress from Rust.
listen<{ pct: number; message: string }>("setup-progress", (e) => {
  const wrap = document.getElementById("bar-wrap") as HTMLElement;
  const bar = document.getElementById("bar") as HTMLElement;
  const msg = document.getElementById("setup-msg") as HTMLElement;
  wrap.hidden = false;
  bar.style.width = `${e.payload.pct}%`;
  msg.textContent = e.payload.message;
});

document.getElementById("setup-go")?.addEventListener("click", async (ev) => {
  const btn = ev.currentTarget as HTMLButtonElement;
  const msg = document.getElementById("setup-msg") as HTMLElement;
  btn.disabled = true;
  btn.textContent = "Installing…";
  try {
    await invoke("setup_engine");
    msg.textContent = "Done. Starting up…";
  } catch (e) {
    // Setup is resumable, so say so rather than leaving a dead end.
    msg.textContent = `${e} — press Retry to pick up where it stopped.`;
    btn.disabled = false;
    btn.textContent = "Retry";
    return;
  }
  setTimeout(refresh, 1500);
});

// Show the real hotkeys rather than hardcoding them in the markup.
invoke<Record<string, string>>("hotkeys").then((hk) => {
  const set = (id: string, v: string) => {
    const el = document.getElementById(id);
    if (el) el.textContent = pretty(v);
  };
  set("key-read", hk.read);
  set("key-dictate", hk.dictate);
  set("key-snip", hk.snip);
});

// ── voice & speed ───────────────────────────────────────────────────────────
const SPEEDS = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

async function initPrefs() {
  const prefs = await invoke<{ voice: string; speed: number }>("get_prefs");

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
const MOD_GLYPH: Record<string, string> = {
  Control: "\u2303", Alt: "\u2325", Shift: "\u21e7", Command: "\u2318",
};

function pretty(accel: string): string {
  return accel
    .split("+")
    .map((p) => MOD_GLYPH[p] ?? p.replace(/^Key/, "").replace(/^Digit/, ""))
    .join("");
}

function accelFrom(e: KeyboardEvent): string | null {
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Control");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (e.metaKey) mods.push("Command");
  const code = e.code;
  // A modifier on its own is not a shortcut the OS can register.
  if (/^(Control|Alt|Shift|Meta)(Left|Right)$/.test(code)) return null;
  // Function keys are valid with no modifier; anything else needs one.
  if (mods.length === 0 && !/^F\d+$/.test(code)) return null;
  return [...mods, code].join("+");
}

document.querySelectorAll<HTMLButtonElement>("button[data-rec]").forEach((btn) => {
  btn.addEventListener("click", () => {
    const slot = btn.dataset.rec!;
    const original = btn.textContent;
    btn.textContent = "Press keys…";
    btn.classList.add("recording");

    const finish = () => {
      window.removeEventListener("keydown", onKey, true);
      btn.textContent = original;
      btn.classList.remove("recording");
    };

    const onKey = async (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.code === "Escape") { finish(); return; }
      const accel = accelFrom(e);
      if (!accel) return;   // still waiting for a full combination
      finish();
      try {
        await invoke("set_hotkey", { slot, accelerator: accel });
        const kbd = document.getElementById(`key-${slot}`);
        if (kbd) kbd.textContent = pretty(accel);
      } catch (err) {
        const kbd = document.getElementById(`key-${slot}`);
        if (kbd) kbd.textContent = String(err);
      }
    };
    window.addEventListener("keydown", onKey, true);
  });
});

initPrefs();
refresh();
setInterval(refresh, 4000);
