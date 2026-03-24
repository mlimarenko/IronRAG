import { verifyBillingForAttempt } from "./billing-client.mjs";

export function resolveExecutionOwner(fileResult) {
  return {
    executionKind: "ingest_attempt",
    executionId: fileResult?.attemptId ?? null,
  };
}

export async function verifyBillingForFiles(apiBase, cookie, fileResults) {
  const results = [];
  for (const fileResult of fileResults) {
    const owner = resolveExecutionOwner(fileResult);
    if (!owner.executionId) {
      results.push({ ...fileResult, billing: null });
      continue;
    }
    const billing = await verifyBillingForAttempt(apiBase, cookie, owner.executionId);
    results.push({
      ...fileResult,
      billing,
    });
  }
  return results;
}
