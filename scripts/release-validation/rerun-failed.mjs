#!/usr/bin/env node
import fs from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

async function main() {
  const reportPath = process.argv[2];
  if (!reportPath) {
    throw new Error("usage: rerun-failed.mjs <report-json-path>");
  }
  const raw = await fs.readFile(reportPath, "utf8");
  const report = JSON.parse(raw);
  const failed = report.files
    .filter((item) => item.readiness?.graphReady !== true || item.billing?.costStatus !== 200)
    .map((item) => item.fileName);

  if (failed.length === 0) {
    // eslint-disable-next-line no-console
    console.log("no failed files");
    return;
  }

  const runScript = path.join(__dirname, "run.mjs");
  const result = spawnSync(
    "node",
    [runScript, "--library-id", report.libraryId, "--formats", failed.join(",")],
    { encoding: "utf8" },
  );
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  if ((result.status ?? 1) !== 0) {
    process.exitCode = result.status ?? 1;
    return;
  }

  const rerunReportPath =
    result.stdout
      .split("\n")
      .map((line) => line.trim())
      .find((line) => line.endsWith("release-validation-report.json")) ?? null;
  if (!rerunReportPath) {
    throw new Error("rerun succeeded but release-validation-report.json path was not found in output");
  }

  const rerunRaw = await fs.readFile(rerunReportPath, "utf8");
  const rerun = JSON.parse(rerunRaw);
  const previousByName = new Map(report.files.map((item) => [item.fileName, item]));
  const rerunByName = new Map(rerun.files.map((item) => [item.fileName, item]));
  const supersededFiles = failed.map((fileName) => ({
    fileName,
    previous: previousByName.get(fileName) ?? null,
    rerun: rerunByName.get(fileName) ?? null,
  }));
  const merged = {
    sourceReportPath: reportPath,
    rerunReportPath,
    createdAt: new Date().toISOString(),
    libraryId: report.libraryId,
    originalRun: {
      runId: report.runId,
      generatedAt: report.generatedAt,
      verdict: report.verdict?.verdict ?? null,
    },
    rerunRun: {
      runId: rerun.runId,
      generatedAt: rerun.generatedAt,
      verdict: rerun.verdict?.verdict ?? null,
    },
    rerunSelection: failed,
    supersededFiles,
    unchangedFileCount: Math.max(0, report.files.length - supersededFiles.length),
  };
  const mergePath = path.join(path.dirname(reportPath), "release-validation-rerun-merge.json");
  await fs.writeFile(mergePath, JSON.stringify(merged, null, 2), "utf8");
  // eslint-disable-next-line no-console
  console.log(mergePath);
  process.exitCode = 0;
}

main().catch((error) => {
  // eslint-disable-next-line no-console
  console.error(error);
  process.exitCode = 1;
});
