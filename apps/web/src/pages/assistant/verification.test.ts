import { describe, expect, it } from "vitest";
import { mapAssistantVerificationState } from "./verification";

describe("mapAssistantVerificationState", () => {
  it("keeps not_run as a neutral state instead of coercing it to failed", () => {
    expect(mapAssistantVerificationState("not_run")).toBe("not_run");
  });

  it("preserves canonical verification states", () => {
    expect(mapAssistantVerificationState("verified")).toBe("passed");
    expect(mapAssistantVerificationState("insufficient_evidence")).toBe(
      "insufficient_evidence",
    );
    expect(mapAssistantVerificationState("failed")).toBe("failed");
  });
});
