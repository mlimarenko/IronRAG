const CANONICAL_STAGE_ORDER = [
  "extract_content",
  "chunk_content",
  "embed_chunk",
  "extract_graph",
];

export function validateStageOrder(stageEvents) {
  const violations = [];
  if (!Array.isArray(stageEvents)) {
    return { valid: false, violations: ["stageEvents must be an array"] };
  }

  let lastIndex = -1;
  const seen = new Set();
  for (const event of stageEvents) {
    const stageName = event?.stageName;
    const index = CANONICAL_STAGE_ORDER.indexOf(stageName);
    if (index === -1) {
      violations.push(`unknown stage: ${String(stageName)}`);
      continue;
    }
    if (index < lastIndex) {
      violations.push(`out-of-order stage: ${stageName}`);
    }
    if (seen.has(stageName)) {
      violations.push(`duplicate stage event: ${stageName}`);
    }
    seen.add(stageName);
    lastIndex = Math.max(lastIndex, index);
  }

  let highestSeenIndex = -1;
  for (const stageName of CANONICAL_STAGE_ORDER) {
    if (seen.has(stageName)) {
      highestSeenIndex = Math.max(highestSeenIndex, CANONICAL_STAGE_ORDER.indexOf(stageName));
    }
  }
  for (let i = 0; i < highestSeenIndex; i += 1) {
    const stageName = CANONICAL_STAGE_ORDER[i];
    if (!seen.has(stageName)) {
      violations.push(`missing prerequisite stage: ${stageName}`);
    }
  }

  return {
    valid: violations.length === 0,
    violations,
  };
}
