function pushBlockingIssue(target, id, rationale) {
  target.push({ id, rationale });
}

export function computeVerdict(report, config) {
  const blockingIssueDetails = [];
  const totalFiles = report.files.length;
  const graphReadyCount = report.files.filter((item) => item.readiness?.graphReady === true).length;
  const billingVerifiedCount = report.files.filter((item) => item.billing?.costStatus === 200).length;
  const formatPassRate = totalFiles > 0 ? graphReadyCount / totalFiles : 0;

  if (formatPassRate < config.formatPassRateThreshold) {
    pushBlockingIssue(
      blockingIssueDetails,
      "FORMAT_PASS_RATE_BELOW_THRESHOLD",
      `format pass rate ${formatPassRate.toFixed(3)} below threshold ${config.formatPassRateThreshold}`,
    );
  }
  if (!report.graph.semanticPass) {
    pushBlockingIssue(
      blockingIssueDetails,
      "GRAPH_SEMANTIC_THRESHOLD_FAILED",
      "graph semantic markers below threshold",
    );
  }
  if (!report.mcp.pass) {
    pushBlockingIssue(
      blockingIssueDetails,
      "MCP_WORKFLOW_FAILED",
      "mcp workflow validation failed",
    );
  }
  if (billingVerifiedCount < totalFiles) {
    pushBlockingIssue(
      blockingIssueDetails,
      "BILLING_VISIBILITY_FAILED",
      `billing cost visibility failed for ${totalFiles - billingVerifiedCount} file(s)`,
    );
  }
  if (report.graph.sanityIssues?.length > 0) {
    pushBlockingIssue(
      blockingIssueDetails,
      "GRAPH_SANITY_ISSUES",
      `graph has ${report.graph.sanityIssues.length} sanity issue(s): ${report.graph.sanityIssues.slice(0, 3).join("; ")}`,
    );
  }
  if (report.graph.isolationIssues?.length > 0) {
    pushBlockingIssue(
      blockingIssueDetails,
      "GRAPH_ISOLATION_VIOLATION",
      `graph ownership isolation violated: ${report.graph.isolationIssues.join("; ")}`,
    );
  }
  if (report.sla && !report.sla.pass) {
    pushBlockingIssue(
      blockingIssueDetails,
      "SLA_BREACH",
      `SLA breaches: ${report.sla.breaches.join("; ")}`,
    );
  }
  const blockingIssues = blockingIssueDetails.map((item) => item.rationale);

  return {
    verdict: blockingIssueDetails.length === 0 ? "pass" : "blocked",
    formatPassRate,
    graphReadyCount,
    billingVerifiedCount,
    blockingIssues,
    blockingIssueDetails,
  };
}
