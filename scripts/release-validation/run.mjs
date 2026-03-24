#!/usr/bin/env node
import fs from "node:fs/promises";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { loadJson, createSession } from "./lib/http-client.mjs";
import { uploadDocument, pollIngestJob, normalizeIngestResult } from "./lib/ingest-client.mjs";
import { fetchEntities, fetchRelations, searchDocuments } from "./lib/knowledge-client.mjs";
import { verifyBillingForAttempt } from "./lib/billing-client.mjs";
import { runMcpWorkflow } from "./lib/mcp-client.mjs";
import { evaluateGraphSemantics } from "./lib/semantic-evaluator.mjs";
import { validateStageOrder } from "./lib/stage-validator.mjs";
import { validateReadinessCoherence } from "./lib/readiness-validator.mjs";
import { checkRelationSanity, checkOwnershipIsolation } from "./lib/graph-sanity.mjs";
import { evaluateStageSla } from "./lib/sla-evaluator.mjs";
import {
  validateCapabilities,
  validateInitialize,
  validateToolsList,
  validateSearch,
  validateUpload,
  validateReadback,
} from "./lib/mcp-validator.mjs";
import { computeVerdict } from "./lib/verdict-reducer.mjs";
import { toMarkdown } from "./lib/report-format.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

function parseArgs(argv) {
  const args = {
    config: path.join(__dirname, "config.json"),
    formats: null,
    libraryId: process.env.RUSTRAG_RELEASE_LIBRARY_ID ?? null,
    login: process.env.RUSTRAG_RELEASE_LOGIN ?? "admin",
    password: process.env.RUSTRAG_RELEASE_PASSWORD ?? "rustrag",
    runDir: process.env.RUSTRAG_RELEASE_RUN_DIR ?? null,
    fixturesDir: process.env.RUSTRAG_RELEASE_FIXTURES_DIR ?? null,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (token === "--config") args.config = argv[i + 1], (i += 1);
    else if (token === "--library-id") args.libraryId = argv[i + 1], (i += 1);
    else if (token === "--formats") args.formats = argv[i + 1], (i += 1);
    else if (token === "--run-dir") args.runDir = argv[i + 1], (i += 1);
    else if (token === "--fixtures-dir") args.fixturesDir = argv[i + 1], (i += 1);
    else if (token === "--login") args.login = argv[i + 1], (i += 1);
    else if (token === "--password") args.password = argv[i + 1], (i += 1);
  }
  return args;
}

async function ensureRunPaths(runDirArg) {
  if (runDirArg) {
    await fs.mkdir(path.join(runDirArg, "artifacts"), { recursive: true });
    await fs.mkdir(path.join(runDirArg, "fixtures"), { recursive: true });
    return {
      runId: path.basename(runDirArg),
      runDir: runDirArg,
      fixturesDir: path.join(runDirArg, "fixtures"),
      artifactsDir: path.join(runDirArg, "artifacts"),
    };
  }
  const bootstrapScript = path.join(__dirname, "bootstrap.sh");
  const result = spawnSync(bootstrapScript, [], { encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(`bootstrap failed: ${result.stderr || result.stdout}`);
  }
  return JSON.parse(result.stdout);
}

function maybeGenerateFixtures(fixturesDir) {
  const script = path.join(__dirname, "generate-fixtures.py");
  const result = spawnSync(script, ["--output-dir", fixturesDir], { encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(`fixture generation failed: ${result.stderr || result.stdout}`);
  }
}

function runSqlDiagnostics(artifactsDir) {
  const files = ["stage-flow.sql", "stage-latency.sql", "billing-consistency.sql"];
  const outputs = {};
  for (const file of files) {
    const sqlPath = path.join(__dirname, "sql", file);
    const output = spawnSync(
      "docker",
      [
        "compose",
        "exec",
        "-T",
        "postgres",
        "psql",
        "-U",
        "postgres",
        "-d",
        "rustrag",
        "-f",
        sqlPath,
      ],
      { encoding: "utf8" },
    );
    const content = (output.stdout || "") + (output.stderr || "");
    outputs[file] = {
      exitCode: output.status ?? 1,
      output: content.trim(),
    };
  }
  return fs.writeFile(
    path.join(artifactsDir, "sql-diagnostics.json"),
    JSON.stringify(outputs, null, 2),
    "utf8",
  );
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const config = await loadJson(args.config);
  const apiBase = process.env.RUSTRAG_RELEASE_API_BASE ?? config.apiBase;
  if (!args.libraryId) {
    throw new Error("library id is required (use --library-id or RUSTRAG_RELEASE_LIBRARY_ID)");
  }

  const paths = await ensureRunPaths(args.runDir);
  const fixturesDir = args.fixturesDir ?? paths.fixturesDir;
  await fs.mkdir(fixturesDir, { recursive: true });
  maybeGenerateFixtures(fixturesDir);

  const selectedFormats = args.formats
    ? new Set(args.formats.split(",").map((item) => item.trim()).filter(Boolean))
    : null;
  const fixtures = config.fixtures.filter(
    (fixture) => !selectedFormats || selectedFormats.has(fixture.fileName),
  );

  const cookie = await createSession(apiBase, args.login, args.password);
  const report = {
    runId: paths.runId,
    generatedAt: new Date().toISOString(),
    apiBase,
    libraryId: args.libraryId,
    files: [],
    graph: {},
    mcp: {},
    verdict: {},
  };

  for (const fixture of fixtures) {
    const startedAt = Date.now();
    const upload = await uploadDocument(apiBase, cookie, args.libraryId, fixturesDir, fixture);
    const jobId = upload.data?.mutation?.jobId ?? null;
    const detail = jobId
      ? await pollIngestJob(apiBase, cookie, jobId, config.pollIntervalMs, config.maxPollAttempts)
      : null;
    const endedAt = Date.now();
    const normalized = normalizeIngestResult(fixture, upload, detail, startedAt, endedAt);
    normalized.detail = detail;
    normalized.stageEvents = detail?.data?.stageEvents ?? detail?.data?.stages ?? null;
    normalized.stageOrder = validateStageOrder(
      Array.isArray(normalized.stageEvents) ? normalized.stageEvents : [],
    );
    if (normalized.attemptId) {
      normalized.billing = await verifyBillingForAttempt(apiBase, cookie, normalized.attemptId);
    } else {
      normalized.billing = null;
    }
    report.files.push(normalized);
    if (normalized.readiness) {
      normalized.readinessCoherence = validateReadinessCoherence(normalized.readiness);
    }
    // eslint-disable-next-line no-console
    console.log(
      `${fixture.fileName}: graphReady=${normalized.readiness?.graphReady === true} billing=${normalized.billing?.costStatus ?? "n/a"} durationMs=${normalized.durationMs}`,
    );
  }

  const [entities, relations, directSearch] = await Promise.all([
    fetchEntities(apiBase, cookie, args.libraryId, config.searchLimit),
    fetchRelations(apiBase, cookie, args.libraryId, config.searchLimit),
    searchDocuments(apiBase, cookie, args.libraryId, "Acme graph budget", 5),
  ]);
  const semantic = evaluateGraphSemantics(
    entities.items,
    config.expectedSemanticTerms,
    config.graphSemanticMinMatchedTerms,
  );
  report.graph = {
    entitiesCount: entities.items.length,
    relationsCount: relations.items.length,
    matchedTerms: semantic.matchedTerms,
    semanticPass: semantic.semanticPass,
    directSearchStatus: directSearch.status,
    directSearchHitCount: directSearch.data?.documentHits?.length ?? 0,
  };
  const graphSanity = checkRelationSanity(relations.items);
  const graphIsolation = checkOwnershipIsolation(entities.items, relations.items, args.libraryId);
  report.graph.sanityIssues = graphSanity.issues;
  report.graph.isolationIssues = graphIsolation.issues;

  const mcp = await runMcpWorkflow(apiBase, cookie, args.libraryId);
  report.mcp = {
    capabilitiesStatus: mcp.capabilities.status,
    initializeOk: Boolean(mcp.initialize.data?.result) && !mcp.initialize.data?.error,
    toolsListOk: Array.isArray(mcp.toolsList.data?.result?.tools),
    searchOk: mcp.searchDocuments.data?.result?.isError === false,
    uploadOk: mcp.uploadDocuments.data?.result?.isError === false,
    mutationStatusOk: mcp.mutationStatus ? mcp.mutationStatus.data?.result?.isError === false : false,
    readOk: mcp.readDocument ? mcp.readDocument.data?.result?.isError === false : false,
    searchHitCount: mcp.searchDocuments.data?.result?.structuredContent?.hits?.length ?? 0,
  };
  report.mcp.pass =
    report.mcp.capabilitiesStatus === 200 &&
    report.mcp.initializeOk &&
    report.mcp.toolsListOk &&
    report.mcp.searchOk &&
    report.mcp.uploadOk &&
    report.mcp.mutationStatusOk &&
    report.mcp.readOk;
  report.mcp.validators = {
    capabilities: validateCapabilities(mcp.capabilities),
    initialize: validateInitialize(mcp.initialize),
    toolsList: validateToolsList(mcp.toolsList),
    search: validateSearch(mcp.searchDocuments),
    upload: validateUpload(mcp.uploadDocuments),
    readback: mcp.readDocument
      ? validateReadback(mcp.readDocument)
      : { pass: false, issues: ["readDocument not executed"] },
  };

  report.sla = evaluateStageSla(report.files, config.stageSlaMs);
  report.verdict = computeVerdict(report, config);

  await runSqlDiagnostics(paths.artifactsDir);
  const jsonPath = path.join(paths.artifactsDir, "release-validation-report.json");
  const mdPath = path.join(paths.artifactsDir, "release-validation-report.md");
  const verdictPath = path.join(paths.artifactsDir, "release-validation-verdict.json");
  const verdictArtifact = {
    runId: report.runId,
    generatedAt: report.generatedAt,
    libraryId: report.libraryId,
    verdict: report.verdict.verdict,
    formatPassRate: report.verdict.formatPassRate,
    graphReadyCount: report.verdict.graphReadyCount,
    billingVerifiedCount: report.verdict.billingVerifiedCount,
    blockingIssues: report.verdict.blockingIssues,
    blockingIssueDetails: report.verdict.blockingIssueDetails ?? [],
  };
  await fs.writeFile(jsonPath, JSON.stringify(report, null, 2), "utf8");
  await fs.writeFile(mdPath, toMarkdown(report), "utf8");
  await fs.writeFile(verdictPath, JSON.stringify(verdictArtifact, null, 2), "utf8");

  // eslint-disable-next-line no-console
  console.log(jsonPath);
  // eslint-disable-next-line no-console
  console.log(mdPath);
  // eslint-disable-next-line no-console
  console.log(verdictPath);

  if (report.verdict.verdict !== "pass") {
    process.exitCode = 2;
  }
}

main().catch((error) => {
  // eslint-disable-next-line no-console
  console.error(error);
  process.exitCode = 1;
});
