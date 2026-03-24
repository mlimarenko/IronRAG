import fs from "node:fs";
import path from "node:path";
import { requestJson } from "./http-client.mjs";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export async function uploadDocument(apiBase, cookie, libraryId, fixtureDir, fixture) {
  const filePath = path.join(fixtureDir, fixture.fileName);
  const bytes = fs.readFileSync(filePath);
  const formData = new FormData();
  formData.append("library_id", libraryId);
  formData.append("file", new Blob([bytes], { type: fixture.mimeType }), fixture.fileName);
  const response = await requestJson(apiBase, cookie, "/content/documents/upload", {
    method: "POST",
    body: formData,
  });
  return response;
}

export async function pollIngestJob(apiBase, cookie, jobId, pollIntervalMs, maxPollAttempts) {
  let last = null;
  for (let i = 0; i < maxPollAttempts; i += 1) {
    await sleep(pollIntervalMs);
    const response = await requestJson(apiBase, cookie, `/ingest/jobs/${jobId}`);
    last = response;
    if (!response.ok) {
      continue;
    }
    const queueState = response.data?.job?.queue_state;
    if (queueState === "completed" || queueState === "failed") {
      return response;
    }
  }
  return last;
}

export function normalizeIngestResult(fixture, uploadResponse, detailResponse, startedAtMs, endedAtMs) {
  const mutation = uploadResponse.data?.mutation?.mutation ?? null;
  const detail = detailResponse?.data ?? null;
  const latestAttempt = detail?.latestAttempt ?? null;
  return {
    fileName: fixture.fileName,
    mimeType: fixture.mimeType,
    uploadStatus: uploadResponse.status,
    mutationId: mutation?.id ?? null,
    mutationState: mutation?.mutation_state ?? null,
    jobId: uploadResponse.data?.mutation?.jobId ?? null,
    documentId: uploadResponse.data?.document?.document?.id ?? null,
    attemptId: latestAttempt?.id ?? null,
    queueState: detail?.job?.queue_state ?? null,
    attemptState: latestAttempt?.attempt_state ?? null,
    failureCode: latestAttempt?.failure_code ?? mutation?.failure_code ?? null,
    readiness: detail?.readiness ?? null,
    durationMs: Math.max(0, endedAtMs - startedAtMs),
  };
}
