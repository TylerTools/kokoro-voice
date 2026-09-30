/**
 * Guided lesson progress. Only observed shortcut and dictation events or an
 * explicit audible confirmation can complete a practice step. No dictated
 * words or selected text are persisted here.
 */
export type TourStep = "welcome" | "read" | "dictate" | "snip" | "finish";
export type PracticeStep = "read" | "dictate" | "snip";

export type TourEvidence = {
  readShortcut: boolean;
  readHeard: boolean;
  dictationInserted: boolean;
  snipShortcut: boolean;
  snipHeard: boolean;
};

export const TOUR_STEPS: TourStep[] = ["welcome", "read", "dictate", "snip", "finish"];

export function emptyEvidence(): TourEvidence {
  return {
    readShortcut: false,
    readHeard: false,
    dictationInserted: false,
    snipShortcut: false,
    snipHeard: false,
  };
}

export function canAdvance(step: TourStep, evidence: TourEvidence): boolean {
  switch (step) {
    case "welcome": return true;
    case "read": return evidence.readShortcut && evidence.readHeard;
    case "dictate": return evidence.dictationInserted;
    case "snip": return evidence.snipShortcut && evidence.snipHeard;
    case "finish": return true;
  }
}

export function nextStep(step: TourStep): TourStep {
  return TOUR_STEPS[Math.min(TOUR_STEPS.indexOf(step) + 1, TOUR_STEPS.length - 1)];
}

export function previousStep(step: TourStep): TourStep {
  return TOUR_STEPS[Math.max(TOUR_STEPS.indexOf(step) - 1, 0)];
}

export function isPracticeStep(step: TourStep): step is PracticeStep {
  return step === "read" || step === "dictate" || step === "snip";
}
