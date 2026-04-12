import type { VerificationState } from "@/types";

export function mapAssistantVerificationState(
  apiState: string,
): VerificationState {
  const map: Record<string, VerificationState> = {
    verified: "passed",
    partially_supported: "partially_supported",
    conflicting: "conflicting",
    insufficient_evidence: "insufficient_evidence",
    failed: "failed",
    not_run: "not_run",
  };
  return map[apiState] ?? "failed";
}
