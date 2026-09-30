/**
 * Interactive first-run lessons in the settings webview. The operating system
 * and Rust host remain authoritative for permission, shortcut, and dictation
 * results; this view never awards success from an animation or saved setting.
 */
import { listen } from "@tauri-apps/api/event";
import type { SetupStep } from "./setup_flow";
import {
  canAdvance,
  emptyEvidence,
  isPracticeStep,
  nextStep,
  previousStep,
  TOUR_STEPS,
  type TourEvidence,
  type TourStep,
} from "./onboarding_flow";

type HotkeySlot = "read" | "dictate" | "snip";
type DictationState = "starting" | "recording" | "transcribing" | "completed" | "clipboard-fallback" | "timed-out" | "device-unavailable" | "permission-denied" | string;

const COMPLETE_KEY = "hereword-tour-complete-v1";
const PENDING_KEY = "hereword-tour-pending-v1";
const VOICE_KEY = "hereword-tour-voice-on";

const setupSpoken: Record<Exclude<SetupStep, "complete">, string> = {
  download: "First, HereWord downloads its local speech models. They stay on this computer. Choose Download and finish setup to begin.",
  "engine-starting": "HereWord is starting the local speech engine. This can take a moment. Setup will continue when it is ready.",
  microphone: "Allow HereWord to use the microphone. Dictation needs this permission. Setup will continue after you approve it.",
  accessibility: "Allow HereWord in macOS Accessibility. This lets it read selected text and place dictated words where you are working.",
  "input-monitoring": "Allow HereWord in Input Monitoring so your shortcuts work across apps. If macOS asks, quit and reopen HereWord. Setup will resume.",
  shortcuts: "HereWord is checking that all three shortcuts are registered. When they are ready, we will practice each one together.",
};

const lessons: Record<TourStep, { title: string; instruction: string; spoken: string }> = {
  welcome: {
    title: "Learn HereWord by trying it",
    instruction: "This short tour teaches you to Read, Dictate, and Snip. Each instruction is shown and spoken. You can turn the voice off or finish later at any time.",
    spoken: "Welcome to HereWord. We'll practice reading, dictation, and snipping together. Each step tells you what to do and waits for you to try it.",
  },
  read: {
    title: "Read selected words",
    instruction: "Select the sample text, then press your Read shortcut. You can keep the suggested keys or change them below. Confirm only when you hear the words.",
    spoken: "First, let's read. Select the sample text, then press your Read shortcut. If you hear the words, choose I heard it.",
  },
  dictate: {
    title: "Speak and see your words",
    instruction: "Focus the practice box. Hold your Dictate shortcut while you speak, then release it. This step passes when your words appear in the box.",
    spoken: "Now let's dictate. Focus the practice box, hold your Dictate shortcut, say a short sentence, then release the keys. Your words should appear in the box.",
  },
  snip: {
    title: "Read text from the screen",
    instruction: "Press your Snip shortcut, then draw a box around the sample words below. Confirm only after you hear them read aloud.",
    spoken: "Last, let's snip. Press your Snip shortcut and draw a box around the sample words. When you hear them, choose I heard it.",
  },
  finish: {
    title: "You're ready to use HereWord",
    instruction: "You tested all three actions. You can replay this tour any time from Settings, and you can change your shortcuts there too.",
    spoken: "Great work. You've tested reading, dictation, and snipping. You can replay this tour at any time from HereWord Settings.",
  },
};

export function initOnboarding() {
  const card = document.getElementById("tour") as HTMLElement;
  const launch = document.getElementById("tour-launch") as HTMLButtonElement;
  const title = document.getElementById("tour-title") as HTMLElement;
  const instruction = document.getElementById("tour-instruction") as HTMLElement;
  const progress = document.getElementById("tour-progress") as HTMLElement;
  const demo = document.getElementById("tour-demo") as HTMLElement;
  const demoSelection = demo.querySelector(".tour-demo-selection") as HTMLElement;
  const demoKey = demo.querySelector(".tour-demo-key") as HTMLElement;
  const practice = document.getElementById("tour-practice") as HTMLElement;
  const shortcut = document.getElementById("tour-shortcut") as HTMLElement;
  const feedback = document.getElementById("tour-feedback") as HTMLElement;
  const readPractice = document.getElementById("tour-read-practice") as HTMLElement;
  const dictatePractice = document.getElementById("tour-dictate-practice") as HTMLElement;
  const snipPractice = document.getElementById("tour-snip-practice") as HTMLElement;
  const readText = document.getElementById("tour-read-text") as HTMLTextAreaElement;
  const dictateText = document.getElementById("tour-dictate-text") as HTMLTextAreaElement;
  const heard = document.getElementById("tour-heard") as HTMLButtonElement;
  const change = document.getElementById("tour-change") as HTMLButtonElement;
  const back = document.getElementById("tour-back") as HTMLButtonElement;
  const next = document.getElementById("tour-next") as HTMLButtonElement;
  const narration = document.getElementById("tour-narration") as HTMLButtonElement;
  const hearAgain = document.getElementById("tour-hear-again") as HTMLButtonElement;
  const setupHear = document.getElementById("setup-hear") as HTMLButtonElement;
  const setupVoiceToggle = document.getElementById("setup-voice-toggle") as HTMLButtonElement;

  let step: TourStep = "welcome";
  let evidence: TourEvidence = emptyEvidence();
  let voiceOn = localStorage.getItem(VOICE_KEY) !== "0";
  let dictationTargetArmed = false;
  let dictationBefore = "";
  let opened = false;
  let currentSetupStep: SetupStep = "download";
  let lastSpokenSetupStep: SetupStep | null = null;

  function stopNarration() {
    window.speechSynthesis?.cancel();
  }

  function speakText(text: string, force = false) {
    stopNarration();
    if (!voiceOn && !force) return;
    if (!window.speechSynthesis || typeof SpeechSynthesisUtterance === "undefined") {
      feedback.textContent = "Spoken guidance is unavailable here. The complete instruction is shown above.";
      return;
    }
    const utterance = new SpeechSynthesisUtterance(text);
    utterance.rate = 0.94;
    utterance.lang = document.documentElement.lang || "en-US";
    window.speechSynthesis.speak(utterance);
  }

  function speak(force = false) {
    speakText(lessons[step].spoken, force);
  }

  function renderVoiceButtons() {
    const label = voiceOn ? "Voice on" : "Voice off";
    narration.textContent = label;
    narration.setAttribute("aria-pressed", String(voiceOn));
    setupVoiceToggle.textContent = label;
    setupVoiceToggle.setAttribute("aria-pressed", String(voiceOn));
  }

  function render() {
    const lesson = lessons[step];
    title.textContent = lesson.title;
    instruction.textContent = lesson.instruction;
    progress.textContent = `Step ${TOUR_STEPS.indexOf(step) + 1} of ${TOUR_STEPS.length}`;
    demo.className = `tour-demo tour-demo--${step}`;
    practice.hidden = !isPracticeStep(step);
    readPractice.hidden = step !== "read";
    dictatePractice.hidden = step !== "dictate";
    snipPractice.hidden = step !== "snip";
    heard.hidden = step !== "read" && step !== "snip";
    heard.disabled = step === "read" ? !evidence.readShortcut : !evidence.snipShortcut;
    change.hidden = !isPracticeStep(step);
    back.hidden = step === "welcome" || step === "finish";
    next.disabled = !canAdvance(step, evidence);
    next.textContent = step === "welcome" ? "Start practice" : step === "finish" ? "Done" : "Next";
    renderVoiceButtons();
    if (isPracticeStep(step)) {
      const label = document.getElementById(`key-${step}`)?.textContent?.trim() || "your saved shortcut";
      shortcut.textContent = `${step[0].toUpperCase()}${step.slice(1)} shortcut: ${label}`;
      demoKey.textContent = label;
    } else {
      shortcut.textContent = "";
      demoKey.textContent = "Press shortcut";
    }
    demoSelection.textContent = step === "dictate" ? "Speak words"
      : step === "snip" ? "Draw around text"
      : step === "finish" ? "All three passed" : "Select words";
  }

  function go(to: TourStep) {
    step = to;
    feedback.textContent = "";
    if (step === "dictate") {
      dictateText.value = "";
      dictationTargetArmed = false;
    }
    render();
    speak();
    card.scrollIntoView({ behavior: "smooth", block: "start" });
    title.focus();
  }

  function start() {
    if (opened) return;
    opened = true;
    evidence = emptyEvidence();
    card.hidden = false;
    localStorage.removeItem(PENDING_KEY);
    go("welcome");
    title.focus();
  }

  function close() {
    stopNarration();
    opened = false;
    card.hidden = true;
    launch.focus();
  }

  launch.addEventListener("click", start);
  document.getElementById("tour-close")?.addEventListener("click", close);
  function toggleVoice() {
    voiceOn = !voiceOn;
    localStorage.setItem(VOICE_KEY, voiceOn ? "1" : "0");
    renderVoiceButtons();
    if (voiceOn && opened) speak();
    else if (voiceOn && currentSetupStep !== "complete") speakText(setupSpoken[currentSetupStep]);
    else stopNarration();
  }
  narration.addEventListener("click", toggleVoice);
  setupVoiceToggle.addEventListener("click", toggleVoice);
  hearAgain.addEventListener("click", () => speak(true));
  setupHear.addEventListener("click", () => {
    if (currentSetupStep !== "complete") speakText(setupSpoken[currentSetupStep], true);
  });
  back.addEventListener("click", () => go(previousStep(step)));
  next.addEventListener("click", () => {
    if (!canAdvance(step, evidence)) return;
    if (step === "finish") {
      localStorage.setItem(COMPLETE_KEY, "1");
      close();
      return;
    }
    go(nextStep(step));
  });
  change.addEventListener("click", () => {
    if (!isPracticeStep(step)) return;
    stopNarration();
    const button = document.querySelector<HTMLButtonElement>(`button[data-rec="${step}"]`);
    if (!button || button.hidden) {
      feedback.textContent = "This shortcut cannot be changed on this device. Use the shown keys to practice.";
      return;
    }
    feedback.textContent = "Press your preferred keys, then press them once more to verify they work.";
    button.scrollIntoView({ behavior: "smooth", block: "center" });
    button.click();
  });
  window.addEventListener("hereword-hotkey-saved", (event) => {
    const slot = (event as CustomEvent<{ slot: HotkeySlot }>).detail?.slot;
    if (!opened || slot !== step) return;
    feedback.textContent = "Shortcut saved. Press the new keys in this step to make sure they work.";
    render();
    card.scrollIntoView({ behavior: "smooth", block: "start" });
  });
  document.getElementById("tour-select-text")?.addEventListener("click", () => {
    stopNarration();
    readText.focus();
    readText.select();
    feedback.textContent = "Text selected. Press your Read shortcut now.";
  });
  document.getElementById("tour-focus-dictate")?.addEventListener("click", () => {
    stopNarration();
    dictateText.focus();
    feedback.textContent = "Hold your Dictate shortcut, speak, then release it.";
  });
  heard.addEventListener("click", () => {
    if (step === "read" && evidence.readShortcut) evidence.readHeard = true;
    if (step === "snip" && evidence.snipShortcut) evidence.snipHeard = true;
    feedback.textContent = "Great. This action worked for you.";
    render();
  });

  void listen<HotkeySlot>("hotkey-triggered", ({ payload }) => {
    if (!opened || payload !== step) return;
    stopNarration();
    if (step === "read") {
      evidence.readShortcut = true;
      feedback.textContent = "Read shortcut reached HereWord. Confirm when you hear the sample text.";
    } else if (step === "dictate") {
      dictationTargetArmed = document.activeElement === dictateText;
      dictationBefore = dictateText.value;
      feedback.textContent = dictationTargetArmed
        ? "Listening. Speak now, then release the shortcut."
        : "Focus the practice box before dictating so your words appear here.";
    } else if (step === "snip") {
      evidence.snipShortcut = true;
      feedback.textContent = "Draw around the sample words. Confirm when you hear them.";
    }
    render();
  });

  void listen<{ state: DictationState }>("dictation-state", ({ payload }) => {
    if (!opened || step !== "dictate" || !dictationTargetArmed) return;
    if (payload.state === "completed") {
      window.setTimeout(() => {
        if (step !== "dictate") return;
        if (dictateText.value.trim() && dictateText.value !== dictationBefore) {
          evidence.dictationInserted = true;
          feedback.textContent = "Your words appeared in the practice box. Dictation passed.";
        } else {
          feedback.textContent = "Dictation finished, but no words reached this box. Focus it and try again.";
        }
        render();
      }, 150);
    } else if (["clipboard-fallback", "timed-out", "device-unavailable", "permission-denied"].includes(payload.state)) {
      feedback.textContent = "The words did not reach the practice box. Focus it and try again.";
    }
  });

  return {
    onSetupStep(setupStep: SetupStep, guidedSetupActive: boolean) {
      launch.disabled = setupStep !== "complete";
      currentSetupStep = setupStep;
      const activeRow = setupStep === "download" || setupStep === "engine-starting" ? "setup-row-engine"
        : setupStep === "microphone" ? "setup-row-microphone"
        : setupStep === "accessibility" ? "setup-row-accessibility"
        : setupStep === "input-monitoring" || setupStep === "shortcuts" ? "setup-row-input" : "";
      for (const id of ["setup-row-engine", "setup-row-microphone", "setup-row-accessibility", "setup-row-input"]) {
        document.getElementById(id)?.classList.toggle("setup-row--current", id === activeRow);
      }
      renderVoiceButtons();
      if (setupStep !== "complete" && guidedSetupActive && setupStep !== lastSpokenSetupStep && !opened) {
        lastSpokenSetupStep = setupStep;
        speakText(setupSpoken[setupStep]);
      }
      if (setupStep === "download" && !localStorage.getItem(COMPLETE_KEY)) {
        localStorage.setItem(PENDING_KEY, "1");
      } else if (setupStep === "complete" && localStorage.getItem(PENDING_KEY) === "1" && !opened) {
        start();
      }
    },
  };
}
