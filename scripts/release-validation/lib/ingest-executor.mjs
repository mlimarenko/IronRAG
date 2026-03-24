import { pollIngestJob, uploadDocument } from "./ingest-client.mjs";

function stateFromPollResponse(response) {
  return (
    response?.data?.job?.queue_state ??
    response?.data?.latestAttempt?.attempt_state ??
    response?.data?.mutation?.mutation_state ??
    "unknown"
  );
}

async function pollWithGuards(apiBase, cookie, jobId, config) {
  const maxWaitMs = Number(config.maxPollAttempts) * Number(config.pollIntervalMs);
  const stalledThresholdMs = Math.floor(maxWaitMs * 0.6);
  const startedAtMs = Date.now();
  let waitedMs = 0;
  let sameStateMs = 0;
  let lastState = null;
  let attempts = 0;
  let stalled = false;
  let timedOut = false;
  let detail = null;

  while (attempts < config.maxPollAttempts && waitedMs < maxWaitMs) {
    attempts += 1;
    const polled = await pollIngestJob(apiBase, cookie, jobId, config.pollIntervalMs, 1);
    detail = polled;
    const state = stateFromPollResponse(polled);
    if (state === lastState) {
      sameStateMs += config.pollIntervalMs;
    } else {
      sameStateMs = 0;
      lastState = state;
    }
    waitedMs = Date.now() - startedAtMs;

    if (sameStateMs > stalledThresholdMs) {
      stalled = true;
      break;
    }

    if (state === "completed" || state === "failed") {
      break;
    }
  }

  if (waitedMs >= maxWaitMs && stateFromPollResponse(detail) !== "completed") {
    timedOut = true;
  }

  return { detail, attempts, stalled, timedOut, maxWaitMs };
}

function toTimeline(fileName, startedAtMs, endedAtMs, upload, detail, pollMeta) {
  return {
    fileName,
    startedAt: new Date(startedAtMs).toISOString(),
    endedAt: new Date(endedAtMs).toISOString(),
    durationMs: Math.max(0, endedAtMs - startedAtMs),
    upload,
    detail,
    poll: pollMeta,
  };
}

export async function executeIngestionMatrix(
  apiBase,
  cookie,
  libraryId,
  fixturesDir,
  fixtures,
  config,
) {
  const timelines = [];
  for (const fixture of fixtures) {
    const startedAtMs = Date.now();
    const upload = await uploadDocument(apiBase, cookie, libraryId, fixturesDir, fixture);
    const jobId = upload?.data?.mutation?.jobId ?? null;
    let detail = null;
    let pollMeta = {
      attempts: 0,
      stalled: false,
      timedOut: false,
      maxWaitMs: Number(config.maxPollAttempts) * Number(config.pollIntervalMs),
    };

    if (jobId) {
      const pollResult = await pollWithGuards(apiBase, cookie, jobId, config);
      detail = pollResult.detail;
      pollMeta = {
        attempts: pollResult.attempts,
        stalled: pollResult.stalled,
        timedOut: pollResult.timedOut,
        maxWaitMs: pollResult.maxWaitMs,
      };
    } else {
      detail = {
        ok: false,
        status: upload?.status ?? null,
        error: "missing_job_id",
      };
      pollMeta.stalled = false;
      pollMeta.timedOut = false;
    }

    const endedAtMs = Date.now();
    timelines.push(toTimeline(fixture.fileName, startedAtMs, endedAtMs, upload, detail, pollMeta));
  }
  return timelines;
}

function extractNegativeFailureReason(upload, detail, pollMeta) {
  const uploadStatus = upload?.status ?? null;
  const detailStatus = detail?.status ?? null;
  const queueState = detail?.data?.job?.queue_state ?? null;
  const failureCode =
    detail?.data?.latestAttempt?.failure_code ??
    detail?.data?.mutation?.failure_code ??
    upload?.data?.mutation?.mutation?.failure_code ??
    null;
  const failureMessage =
    detail?.data?.latestAttempt?.failure_message ??
    detail?.data?.latestAttempt?.failure_reason ??
    detail?.error ??
    upload?.error ??
    null;

  if (!upload?.ok) return `upload_failed:${uploadStatus}`;
  if (pollMeta.stalled) return "ingest_stalled";
  if (pollMeta.timedOut) return "ingest_timeout";
  if (queueState === "failed") return `ingest_failed:${failureCode ?? "unknown_code"}`;
  if (failureMessage) return String(failureMessage);
  if (detailStatus && detailStatus >= 400) return `detail_failed:${detailStatus}`;
  return "unexpected_success";
}

export async function executeNegativeFixtures(
  apiBase,
  cookie,
  libraryId,
  fixturesDir,
  negativeFixtures,
  config,
) {
  const results = [];
  for (const fixture of negativeFixtures) {
    const startedAtMs = Date.now();
    const upload = await uploadDocument(apiBase, cookie, libraryId, fixturesDir, fixture);
    const jobId = upload?.data?.mutation?.jobId ?? null;
    let detail = null;
    let pollMeta = {
      attempts: 0,
      stalled: false,
      timedOut: false,
      maxWaitMs: Number(config.maxPollAttempts) * Number(config.pollIntervalMs),
    };

    if (jobId) {
      const pollResult = await pollWithGuards(apiBase, cookie, jobId, config);
      detail = pollResult.detail;
      pollMeta = {
        attempts: pollResult.attempts,
        stalled: pollResult.stalled,
        timedOut: pollResult.timedOut,
        maxWaitMs: pollResult.maxWaitMs,
      };
    }

    const endedAtMs = Date.now();
    results.push({
      fileName: fixture.fileName,
      startedAt: new Date(startedAtMs).toISOString(),
      endedAt: new Date(endedAtMs).toISOString(),
      durationMs: Math.max(0, endedAtMs - startedAtMs),
      upload,
      detail,
      poll: pollMeta,
      failureReason: extractNegativeFailureReason(upload, detail, pollMeta),
    });
  }
  return results;
}
