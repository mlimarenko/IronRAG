export function toMarkdown(report) {
  const issueIds = new Set((report.verdict.blockingIssueDetails ?? []).map((item) => item.id));
  const totalCost = report.files
    .map((item) => Number(item.billing?.totalCost ?? 0))
    .reduce((sum, value) => sum + (Number.isFinite(value) ? value : 0), 0);
  const avgDurationMs =
    report.files.length > 0
      ? Math.round(
          report.files.map((item) => item.durationMs ?? 0).reduce((sum, value) => sum + value, 0) /
            report.files.length,
        )
      : 0;
  const storyStatus = [
    { id: "US1 Ingestion Reliability", pass: !issueIds.has("FORMAT_PASS_RATE_BELOW_THRESHOLD") },
    { id: "US2 Graph Quality", pass: !issueIds.has("GRAPH_SEMANTIC_THRESHOLD_FAILED") },
    { id: "US3 Billing + Performance", pass: !issueIds.has("BILLING_VISIBILITY_FAILED") },
    { id: "US4 MCP Agent Usability", pass: !issueIds.has("MCP_WORKFLOW_FAILED") },
  ];
  const lines = [];
  lines.push(`# Release Validation Report`);
  lines.push("");
  lines.push(`- Run ID: \`${report.runId}\``);
  lines.push(`- Generated at: \`${report.generatedAt}\``);
  lines.push(`- Library: \`${report.libraryId}\``);
  lines.push(`- Verdict: **${report.verdict.verdict.toUpperCase()}**`);
  lines.push("");
  lines.push("## Aggregate");
  lines.push("");
  lines.push(`- Total files: ${report.files.length}`);
  lines.push(`- Graph-ready files: ${report.verdict.graphReadyCount}`);
  lines.push(`- Billing verified files: ${report.verdict.billingVerifiedCount}`);
  lines.push(`- Format pass rate: ${report.verdict.formatPassRate.toFixed(3)}`);
  lines.push(`- Total billed cost (USD): ${totalCost.toFixed(8)}`);
  lines.push(`- Avg duration per file (ms): ${avgDurationMs}`);
  lines.push("");
  lines.push("## Story Status");
  lines.push("");
  for (const story of storyStatus) {
    lines.push(`- ${story.id}: ${story.pass ? "PASS" : "BLOCKED"}`);
  }
  lines.push("");
  lines.push("## Format Matrix");
  lines.push("");
  lines.push("| File | graphReady | billingStatus | durationMs | readinessCoherent |");
  lines.push("|------|-----------|---------------|------------|-------------------|");
  for (const item of report.files) {
    const coherent = item.readinessCoherence?.coherent ?? "n/a";
    lines.push(
      `| ${item.fileName} | ${item.readiness?.graphReady === true} | ${item.billing?.costStatus ?? "n/a"} | ${item.durationMs} | ${coherent} |`,
    );
  }
  lines.push("");
  lines.push("## Graph");
  lines.push("");
  lines.push(`- Entities: ${report.graph.entitiesCount}`);
  lines.push(`- Relations: ${report.graph.relationsCount}`);
  lines.push(`- Matched semantic terms: ${report.graph.matchedTerms.join(", ")}`);
  lines.push(`- Semantic pass: ${report.graph.semanticPass}`);
  lines.push("");
  lines.push("## Graph Quality");
  lines.push("");
  if (report.graph.sanityIssues?.length > 0) {
    lines.push("### Sanity issues");
    for (const issue of report.graph.sanityIssues) {
      lines.push(`- ${issue}`);
    }
  } else {
    lines.push("- No graph sanity issues detected.");
  }
  if (report.graph.isolationIssues?.length > 0) {
    lines.push("### Isolation issues");
    for (const issue of report.graph.isolationIssues) {
      lines.push(`- ${issue}`);
    }
  } else {
    lines.push("- No ownership isolation violations.");
  }
  lines.push("");
  lines.push("## MCP");
  lines.push("");
  lines.push(`- MCP pass: ${report.mcp.pass}`);
  lines.push(`- MCP search hits: ${report.mcp.searchHitCount}`);
  lines.push("");
  lines.push("## MCP Workflow Detail");
  lines.push("");
  if (report.mcp.validators) {
    const steps = ["capabilities", "initialize", "toolsList", "search", "upload", "readback"];
    for (const step of steps) {
      const v = report.mcp.validators[step];
      if (v) {
        lines.push(`- ${step}: ${v.pass ? "PASS" : "FAIL"}${v.issues?.length ? " — " + v.issues.join("; ") : ""}`);
      }
    }
  } else {
    lines.push("- No structured MCP validation results available.");
  }
  lines.push("");
  lines.push("## Cost and SLA");
  lines.push("");
  lines.push(`- Total billed cost (USD): ${totalCost.toFixed(8)}`);
  lines.push(`- Average file processing duration (ms): ${avgDurationMs}`);
  if (report.sla) {
    lines.push(`- SLA pass: ${report.sla.pass}`);
    lines.push(`- Average duration (ms): ${report.sla.avgDurationMs}`);
    if (report.sla.breaches.length > 0) {
      lines.push("- SLA breaches:");
      for (const breach of report.sla.breaches) {
        lines.push(`  - ${breach}`);
      }
    }
  }
  lines.push("");
  lines.push("## File Results");
  lines.push("");
  for (const item of report.files) {
    lines.push(
      `- \`${item.fileName}\`: graphReady=${item.readiness?.graphReady === true}, billingStatus=${item.billing?.costStatus ?? "n/a"}, durationMs=${item.durationMs}`,
    );
  }
  if (report.verdict.blockingIssues.length > 0) {
    lines.push("");
    lines.push("## Blocking Issues");
    lines.push("");
    for (const issue of report.verdict.blockingIssues) {
      lines.push(`- ${issue}`);
    }
  }
  lines.push("");
  lines.push("## Next Actions");
  lines.push("");
  if (report.verdict.verdict === "pass") {
    lines.push("- Proceed with release go/no-go sign-off and artifact publication.");
    lines.push("- Archive report JSON/MD/verdict artifacts with release checkpoint docs.");
  } else {
    lines.push("- Address blocking issues in priority order (format, graph, billing, MCP).");
    lines.push("- Re-run failed segments with `rerun-failed.mjs`, then execute one full validation rerun.");
  }
  lines.push("");
  return lines.join("\n");
}
