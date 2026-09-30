/**
 * Owns the optional practice tour, never shortcut registration or action results.
 * Backend events prove that an action reached HereWord; the person confirms the
 * audible or visible result before a lesson is marked complete.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Action = "read" | "dictate" | "snip";

const lessons: Array<{
  title: string;
  instruction: string;
  narration: string;
  action?: Action;
  prompt: string;
  confirmation: string;
}> = [
  {
    title: "Make the shortcuts yours",
    instruction: "These are the keys HereWord will use. Change any shortcut below, or keep the defaults. The next three steps let you try each one.",
    narration: "First, look at the Read, Dictate, and Snip shortcuts. Use Change shortcut if you want different keys. Then choose My shortcuts are set.",
    prompt: "Choose My shortcuts are set when the keys look right.",
    confirmation: "My shortcuts are set",
  },
  {
    title: "Read selected text",
    instruction: "Select the sample sentence below, then press your Read shortcut. HereWord should speak it aloud. Press the shortcut again with no new selection to pause or resume.",
    narration: "Select the sample sentence. Press your Read shortcut. Listen for HereWord to read it aloud. Press the shortcut again with no new selection to pause or resume.",
    action: "read",
    prompt: "Waiting for Read to find selected text…",
    confirmation: "I heard the sentence",
  },
  {
    title: "Dictate a short sentence",
    instruction: "Click the empty field below. Hold your Dictate shortcut while saying a short sentence, then release. Wait for the words to appear.",
    narration: "Click the empty field. Hold your Dictate shortcut while you speak a short sentence, then release. Wait for the words to appear before confirming.",
    action: "dictate",
    prompt: "Waiting for a completed dictation…",
    confirmation: "My words appeared",
  },
  {
    title: "Snip and read",
    instruction: "Press your Snip shortcut, then drag a box around the sample below. HereWord should recognize and read it aloud.",
    narration: "Press your Snip shortcut. Drag a box around the sample words. Listen for HereWord to read them aloud.",
    action: "snip",
    prompt: "Waiting for Snip to recognize text…",
    confirmation: "I heard the snip",
  },
];

export function createPracticeTour() {
  const launch = document.getElementById("tour-launch") as HTMLElement;
  const panel = document.getElementById("tour") as HTMLElement;
  const openButton = document.getElementById("tour-open") as HTMLButtonElement;
  const closeButton = document.getElementById("tour-close") as HTMLButtonElement;
  const title = document.getElementById("tour-title") as HTMLElement;
  const progress = document.getElementById("tour-progress") as HTMLElement;
  const instruction = document.getElementById("tour-instruction") as HTMLElement;
  const practice = document.getElementById("tour-practice") as HTMLElement;
  const result = document.getElementById("tour-result") as HTMLElement;
  const demo = document.getElementById("tour-demo") as HTMLElement;
  const demoText = demo.querySelector(".tour-demo-text") as HTMLElement;
  const key = document.getElementById("tour-key") as HTMLElement;
  const hear = document.getElementById("tour-hear") as HTMLButtonElement;
  const change = document.getElementById("tour-change") as HTMLButtonElement;
  const confirm = document.getElementById("tour-confirm") as HTMLButtonElement;
  const next = document.getElementById("tour-next") as HTMLButtonElement;

  let ready = false;
  let opened = false;
  let index = 0;
  let actionObserved = false;
  let confirmed = false;
  let shortcutTriggered = false;

  function shortcutLabel(action: Action): string {
    return document.getElementById(`key-${action}`)?.textContent?.trim() || "your shortcut";
  }

  function render(): void {
    launch.hidden = !ready || opened;
    panel.hidden = !ready || !opened;
    openButton.textContent = localStorage.getItem("hereword-practice-complete") === "1"
      ? "Repeat practice tour" : "Start practice tour";
    if (!opened) return;
    const lesson = lessons[index];
    progress.textContent = `Step ${index + 1} of ${lessons.length}`;
    title.textContent = lesson.title;
    instruction.textContent = lesson.instruction;
    demo.dataset.action = lesson.action ?? "shortcuts";
    demoText.textContent = {
      shortcuts: "Choose your keys",
      read: "Select • Press • Hear",
      dictate: "Hold • Speak • Release",
      snip: "Press • Drag • Hear",
    }[lesson.action ?? "shortcuts"];
    key.textContent = lesson.action ? shortcutLabel(lesson.action) : "Your keys";
    result.textContent = confirmed ? "Verified with your confirmation." : lesson.prompt;
    confirm.textContent = lesson.confirmation;
    confirm.hidden = confirmed;
    confirm.disabled = Boolean(lesson.action) && !actionObserved;
    next.disabled = !confirmed;
    next.textContent = index === lessons.length - 1 ? "Finish tour" : "Next step";
    change.hidden = !lesson.action;
    change.textContent = lesson.action ? `Change ${lesson.action} shortcut` : "Change shortcut";
    if (index === 0) {
      practice.innerHTML = '<div class="tour-key-list"><span>Read <kbd id="tour-key-read"></kbd></span><span>Dictate <kbd id="tour-key-dictate"></kbd></span><span>Snip <kbd id="tour-key-snip"></kbd></span></div>';
      for (const action of ["read", "dictate", "snip"] as const) {
        (document.getElementById(`tour-key-${action}`) as HTMLElement).textContent = shortcutLabel(action);
      }
    } else if (lesson.action === "read") {
      practice.innerHTML = '<textarea id="tour-read-sample" rows="2" spellcheck="false" aria-label="Select this sentence to practice Read">HereWord can read the words you select.</textarea>';
    } else if (lesson.action === "dictate") {
      practice.innerHTML = '<textarea id="tour-dictate-sample" rows="2" placeholder="Your dictated words will appear here" aria-label="Practice dictation here"></textarea>';
    } else {
      practice.innerHTML = '<div class="tour-snip-sample">HereWord can read text in a picture.</div>';
    }
  }

  function resetStep(): void {
    actionObserved = false;
    confirmed = false;
    shortcutTriggered = false;
    render();
  }

  openButton.addEventListener("click", () => {
    opened = true;
    index = 0;
    resetStep();
    panel.scrollIntoView({ behavior: "smooth", block: "start" });
  });
  closeButton.addEventListener("click", () => {
    opened = false;
    render();
  });
  hear.addEventListener("click", async () => {
    hear.disabled = true;
    try {
      await invoke("stop_speaking");
      await invoke("speak_text", { text: lessons[index].narration });
    } catch {
      result.textContent = "Spoken guidance could not start. The written steps are still here.";
    } finally {
      hear.disabled = false;
    }
  });
  change.addEventListener("click", () => {
    const action = lessons[index].action;
    const button = action && document.querySelector<HTMLButtonElement>(`button[data-rec="${action}"]`);
    if (!button || button.hidden) {
      result.textContent = "This shortcut is set by your system and cannot be changed here.";
      return;
    }
    button.scrollIntoView({ behavior: "smooth", block: "center" });
    button.click();
  });
  confirm.addEventListener("click", () => {
    if (lessons[index].action && !actionObserved) return;
    confirmed = true;
    render();
  });
  next.addEventListener("click", () => {
    if (!confirmed) return;
    if (index === lessons.length - 1) {
      localStorage.setItem("hereword-practice-complete", "1");
      opened = false;
      render();
      return;
    }
    index += 1;
    resetStep();
    panel.scrollIntoView({ behavior: "smooth", block: "start" });
  });

  void listen<Action>("practice-action", ({ payload }) => {
    if (!opened || lessons[index].action !== payload) return;
    actionObserved = true;
    result.textContent = payload === "read"
      ? "Read found your selection. Did you hear it?"
      : "Snip recognized the text. Did you hear it?";
    confirm.disabled = false;
  });

  return {
    setReady(value: boolean) {
      if (ready === value) return;
      ready = value;
      render();
    },
    refreshShortcut() {
      if (opened) render();
    },
    onHotkey(action: Action) {
      if (!opened || lessons[index].action !== action) return;
      shortcutTriggered = true;
      if (!actionObserved) result.textContent = `${action} shortcut reached HereWord. Waiting for the action…`;
    },
    onDictationState(state: string) {
      if (!opened || lessons[index].action !== "dictate") return;
      if (state === "completed") {
        actionObserved = true;
        result.textContent = "Dictation finished. Did your words appear in the field?";
        confirm.disabled = false;
      } else if (state === "clipboard-fallback") {
        result.textContent = "Words were copied, but this field could not be verified. Paste them or try again in another text field.";
      } else if (state === "device-unavailable" || state === "permission-denied" || state === "timed-out") {
        result.textContent = "The microphone did not open. Check the microphone and setup permissions, then try again.";
      } else if (state === "recording" && shortcutTriggered) {
        result.textContent = "Listening. Speak while holding the shortcut, then release.";
      }
    },
  };
}
