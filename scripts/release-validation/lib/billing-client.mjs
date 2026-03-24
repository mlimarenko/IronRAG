import { requestJson } from "./http-client.mjs";

export async function fetchExecutionCost(apiBase, cookie, executionKind, executionId) {
  return requestJson(apiBase, cookie, `/billing/executions/${executionKind}/${executionId}`);
}

export async function fetchProviderCalls(apiBase, cookie, executionKind, executionId) {
  return requestJson(
    apiBase,
    cookie,
    `/billing/executions/${executionKind}/${executionId}/provider-calls`,
  );
}

export async function fetchCharges(apiBase, cookie, executionKind, executionId) {
  return requestJson(apiBase, cookie, `/billing/executions/${executionKind}/${executionId}/charges`);
}

export async function verifyBillingForAttempt(apiBase, cookie, attemptId) {
  const executionKind = "ingest_attempt";
  const [cost, providerCalls, charges] = await Promise.all([
    fetchExecutionCost(apiBase, cookie, executionKind, attemptId),
    fetchProviderCalls(apiBase, cookie, executionKind, attemptId),
    fetchCharges(apiBase, cookie, executionKind, attemptId),
  ]);
  return {
    executionKind,
    executionId: attemptId,
    costStatus: cost.status,
    callsStatus: providerCalls.status,
    chargesStatus: charges.status,
    totalCost: cost.data?.total_cost ?? null,
    currencyCode: cost.data?.currency_code ?? null,
    providerCallCount: Array.isArray(providerCalls.data) ? providerCalls.data.length : null,
    chargeCount: Array.isArray(charges.data) ? charges.data.length : null,
    zeroCostObserved:
      (cost.status === 404 && Array.isArray(providerCalls.data) && providerCalls.data.length === 0) ||
      (cost.status === 200 && Number(cost.data?.total_cost ?? "0") === 0),
  };
}
