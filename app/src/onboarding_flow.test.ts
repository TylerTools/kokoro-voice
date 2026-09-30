import { describe, expect, it } from "vitest";
import { canAdvance, emptyEvidence, nextStep, previousStep } from "./onboarding_flow";

describe("guided tour progress", () => {
  it("requires a real Read shortcut and audible confirmation", () => {
    const evidence = emptyEvidence();
    expect(canAdvance("read", evidence)).toBe(false);
    evidence.readShortcut = true;
    expect(canAdvance("read", evidence)).toBe(false);
    evidence.readHeard = true;
    expect(canAdvance("read", evidence)).toBe(true);
  });

  it("requires successful insertion for dictation", () => {
    const evidence = emptyEvidence();
    expect(canAdvance("dictate", evidence)).toBe(false);
    evidence.dictationInserted = true;
    expect(canAdvance("dictate", evidence)).toBe(true);
  });

  it("requires a Snip shortcut and audible confirmation", () => {
    const evidence = emptyEvidence();
    evidence.snipHeard = true;
    expect(canAdvance("snip", evidence)).toBe(false);
    evidence.snipShortcut = true;
    expect(canAdvance("snip", evidence)).toBe(true);
  });

  it("keeps navigation within the tour", () => {
    expect(previousStep("welcome")).toBe("welcome");
    expect(nextStep("welcome")).toBe("read");
    expect(nextStep("snip")).toBe("finish");
    expect(nextStep("finish")).toBe("finish");
  });
});
