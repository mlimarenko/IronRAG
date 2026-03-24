import test from "node:test";
import assert from "node:assert/strict";

import { computeVerdict } from "../lib/verdict-reducer.mjs";

function baseConfig() {
  return {
    formatPassRateThreshold: 0.95,
  };
}

function passingReport() {
  return {
    files: [
      { readiness: { graphReady: true }, billing: { costStatus: 200 } },
      { readiness: { graphReady: true }, billing: { costStatus: 200 } },
    ],
    graph: { semanticPass: true },
    mcp: { pass: true },
  };
}

test("computeVerdict returns pass when all gates pass", () => {
  const verdict = computeVerdict(passingReport(), baseConfig());
  assert.equal(verdict.verdict, "pass");
  assert.equal(verdict.graphReadyCount, 2);
  assert.equal(verdict.billingVerifiedCount, 2);
  assert.equal(verdict.blockingIssues.length, 0);
});

test("computeVerdict blocks when format threshold is missed", () => {
  const report = passingReport();
  report.files[1].readiness.graphReady = false;

  const verdict = computeVerdict(report, baseConfig());
  assert.equal(verdict.verdict, "blocked");
  assert.equal(verdict.graphReadyCount, 1);
  assert.ok(verdict.blockingIssues.some((item) => item.includes("format pass rate")));
});

test("computeVerdict blocks when graph semantics, mcp, and billing fail", () => {
  const report = passingReport();
  report.graph.semanticPass = false;
  report.mcp.pass = false;
  report.files[1].billing.costStatus = 500;

  const verdict = computeVerdict(report, baseConfig());
  assert.equal(verdict.verdict, "blocked");
  assert.ok(verdict.blockingIssues.includes("graph semantic markers below threshold"));
  assert.ok(verdict.blockingIssues.includes("mcp workflow validation failed"));
  assert.ok(verdict.blockingIssues.some((item) => item.includes("billing cost visibility failed")));
  const ids = verdict.blockingIssueDetails.map((item) => item.id);
  assert.ok(ids.includes("GRAPH_SEMANTIC_THRESHOLD_FAILED"));
  assert.ok(ids.includes("MCP_WORKFLOW_FAILED"));
  assert.ok(ids.includes("BILLING_VISIBILITY_FAILED"));
});

test("computeVerdict handles empty file list as blocked", () => {
  const report = {
    files: [],
    graph: { semanticPass: true },
    mcp: { pass: true },
  };

  const verdict = computeVerdict(report, baseConfig());
  assert.equal(verdict.verdict, "blocked");
  assert.equal(verdict.formatPassRate, 0);
  assert.equal(verdict.graphReadyCount, 0);
  assert.equal(verdict.billingVerifiedCount, 0);
});
