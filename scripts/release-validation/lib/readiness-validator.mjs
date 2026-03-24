export function validateReadinessCoherence(readiness) {
  const issues = [];
  const textReady = readiness?.textReady === true;
  const vectorReady = readiness?.vectorReady === true;
  const graphReady = readiness?.graphReady === true;

  if (graphReady && (!vectorReady || !textReady)) {
    issues.push("graphReady requires vectorReady and textReady");
  }
  if (vectorReady && !textReady) {
    issues.push("vectorReady requires textReady");
  }

  const textState = readiness?.textState;
  const vectorState = readiness?.vectorState;
  const graphState = readiness?.graphState;
  if (textReady && textState && String(textState).toLowerCase() !== "ready") {
    issues.push(`textReady true but textState is ${textState}`);
  }
  if (vectorReady && vectorState && String(vectorState).toLowerCase() !== "ready") {
    issues.push(`vectorReady true but vectorState is ${vectorState}`);
  }
  if (graphReady && graphState && String(graphState).toLowerCase() !== "ready") {
    issues.push(`graphReady true but graphState is ${graphState}`);
  }

  return {
    coherent: issues.length === 0,
    issues,
  };
}
