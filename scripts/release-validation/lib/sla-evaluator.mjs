function toNumber(value) {
  const num = Number(value);
  return Number.isFinite(num) ? num : null;
}

function getStageEvents(fileResult) {
  if (Array.isArray(fileResult?.stageEvents)) return fileResult.stageEvents;
  if (Array.isArray(fileResult?.detail?.data?.stageEvents)) return fileResult.detail.data.stageEvents;
  if (Array.isArray(fileResult?.detail?.data?.stages)) return fileResult.detail.data.stages;
  return [];
}

export function evaluateStageSla(fileResults, slaConfig) {
  const breaches = [];
  const stageSlaMs = slaConfig?.stageSlaMs ?? {};
  const durations = [];

  for (const fileResult of Array.isArray(fileResults) ? fileResults : []) {
    const totalDurationMs = toNumber(fileResult?.durationMs);
    if (totalDurationMs != null) {
      durations.push(totalDurationMs);
    }

    if (toNumber(stageSlaMs.total) != null && totalDurationMs != null && totalDurationMs > stageSlaMs.total) {
      breaches.push(
        `${fileResult?.fileName ?? "unknown"} exceeded total SLA (${totalDurationMs}ms > ${stageSlaMs.total}ms)`,
      );
    }

    for (const stageEvent of getStageEvents(fileResult)) {
      const stageName = stageEvent?.stageName ?? stageEvent?.name ?? null;
      const stageDuration = toNumber(stageEvent?.durationMs ?? stageEvent?.duration_ms);
      const stageLimit = stageName ? toNumber(stageSlaMs[stageName]) : null;
      if (stageName && stageDuration != null && stageLimit != null && stageDuration > stageLimit) {
        breaches.push(
          `${fileResult?.fileName ?? "unknown"} stage ${stageName} exceeded SLA (${stageDuration}ms > ${stageLimit}ms)`,
        );
      }
    }
  }

  const avgDurationMs =
    durations.length > 0 ? Math.round(durations.reduce((sum, value) => sum + value, 0) / durations.length) : 0;

  return {
    avgDurationMs,
    breaches,
    pass: breaches.length === 0,
  };
}
