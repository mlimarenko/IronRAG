#!/usr/bin/env python3
"""Run a release-style suite over the canonical agent surface probe."""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any


DEFAULT_BASE_URL = "http://127.0.0.1:19000"
DEFAULT_LOGIN = "admin"
DEFAULT_PASSWORD = "rustrag123"
SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parents[1]
DEFAULT_SUITE_PATH = SCRIPT_DIR / "agent-surface-suite.json"
DEFAULT_REPORTS_DIR = REPO_ROOT / "tmp" / "agent-surface-suite"
PROBE_SCRIPT_PATH = SCRIPT_DIR / "profile-agent-surfaces.py"


@dataclass(frozen=True)
class CaseResult:
    case_id: str
    exit_code: int
    report_path: str | None
    failed_checks: list[str]
    stderr_summary: str

    @property
    def status(self) -> str:
        if self.exit_code == 0:
            return "pass"
        if self.exit_code == 2:
            return "fail"
        return "error"


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run a multi-scenario suite over the canonical agent surface probe."
    )
    parser.add_argument("--suite-path", default=str(DEFAULT_SUITE_PATH))
    parser.add_argument("--base-url", default=DEFAULT_BASE_URL)
    parser.add_argument("--login", default=DEFAULT_LOGIN)
    parser.add_argument("--password", default=DEFAULT_PASSWORD)
    parser.add_argument("--library-id", required=True)
    parser.add_argument("--workspace-id")
    parser.add_argument("--mcp-token")
    parser.add_argument("--reports-dir", default=str(DEFAULT_REPORTS_DIR))
    parser.add_argument("--output-path")
    return parser.parse_args(argv)


def parse_failed_checks(stderr: str) -> list[str]:
    marker = "agent surface probe failed release gate:"
    for line in stderr.splitlines():
        if marker not in line:
            continue
        _, values = line.split(marker, 1)
        return [value.strip() for value in values.split(",") if value.strip()]
    return []


def build_case_command(
    *,
    base_url: str,
    login: str,
    password: str,
    library_id: str,
    workspace_id: str | None,
    mcp_token: str | None,
    case: dict[str, Any],
    report_path: pathlib.Path,
) -> list[str]:
    command = [
        sys.executable,
        str(PROBE_SCRIPT_PATH),
        "--base-url",
        base_url,
        "--login",
        login,
        "--password",
        password,
        "--library-id",
        library_id,
        "--output-path",
        str(report_path),
    ]
    if workspace_id:
        command.extend(["--workspace-id", workspace_id])
    if mcp_token:
        command.extend(["--mcp-token", mcp_token])

    scalar_fields = {
        "entityQuery": "--entity-query",
        "documentQuery": "--document-query",
        "documentLimit": "--document-limit",
        "graphLimit": "--graph-limit",
        "readLength": "--read-length",
        "question": "--question",
        "sseRuns": "--sse-runs",
        "graphMinEntities": "--graph-min-entities",
        "graphMinRelations": "--graph-min-relations",
        "graphMinDocuments": "--graph-min-documents",
        "entitySearchMinHits": "--entity-search-min-hits",
        "searchMinHits": "--search-min-hits",
        "searchMinReadableHits": "--search-min-readable-hits",
        "readMinContentChars": "--read-min-content-chars",
        "readMinReferences": "--read-min-references",
        "assistantMinReferences": "--assistant-min-references",
        "assistantExpectedVerification": "--assistant-expected-verification",
        "assistantMaxToolStarts": "--assistant-max-tool-starts",
        "expectedSearchTopLabel": "--expected-search-top-label",
        "communityMinCount": "--community-min-count",
        "maxToolLatencyMs": "--max-tool-latency-ms",
        "maxFirstDeltaMs": "--max-first-delta-ms",
        "maxCompletedMs": "--max-completed-ms",
        "timeoutSeconds": "--timeout-seconds",
    }
    for key, flag in scalar_fields.items():
        value = case.get(key)
        if value is None:
            continue
        command.extend([flag, str(value)])

    list_fields = {
        "assistantRequireAll": "--assistant-require-all",
        "assistantForbidAny": "--assistant-forbid-any",
    }
    for key, flag in list_fields.items():
        values = case.get(key) or []
        if values:
            command.extend([flag, ",".join(str(value) for value in values)])

    return command


def run_case(
    *,
    base_url: str,
    login: str,
    password: str,
    library_id: str,
    workspace_id: str | None,
    mcp_token: str | None,
    reports_dir: pathlib.Path,
    case: dict[str, Any],
) -> CaseResult:
    case_id = str(case["id"])
    report_path = reports_dir / f"{case_id}.md"
    command = build_case_command(
        base_url=base_url,
        login=login,
        password=password,
        library_id=library_id,
        workspace_id=workspace_id,
        mcp_token=mcp_token,
        case=case,
        report_path=report_path,
    )
    completed = subprocess.run(command, capture_output=True, text=True, check=False)
    stdout_lines = [line.strip() for line in completed.stdout.splitlines() if line.strip()]
    parsed_report_path = stdout_lines[-1] if stdout_lines else None
    stderr_summary = " ".join(line.strip() for line in completed.stderr.splitlines() if line.strip())
    return CaseResult(
        case_id=case_id,
        exit_code=completed.returncode,
        report_path=parsed_report_path,
        failed_checks=parse_failed_checks(completed.stderr),
        stderr_summary=stderr_summary,
    )


def render_suite_report(
    *,
    output_path: pathlib.Path,
    suite_path: pathlib.Path,
    suite_data: dict[str, Any],
    results: list[CaseResult],
) -> None:
    generated_at = datetime.now(timezone.utc).isoformat()
    passed = sum(1 for result in results if result.status == "pass")
    failed = sum(1 for result in results if result.status == "fail")
    errored = sum(1 for result in results if result.status == "error")
    report = f"""# Agent surface suite report

- Generated at: {generated_at}
- Suite: `{suite_data.get("suiteId", "unknown")}`
- Suite path: `{suite_path}`
- Description: {suite_data.get("description", "n/a")}

## Summary

| Cases | Passed | Failed | Errors |
|---:|---:|---:|---:|
| {len(results)} | {passed} | {failed} | {errored} |

## Cases

| Case | Status | Failed checks | Probe report |
|---|---|---|---|
"""
    for result in results:
        failed_checks = ", ".join(result.failed_checks) if result.failed_checks else "n/a"
        report_path = result.report_path or "n/a"
        report += f"| `{result.case_id}` | {result.status} | {failed_checks} | `{report_path}` |\n"
    report += "\n## Error details\n\n"
    for result in results:
        if not result.stderr_summary:
            continue
        report += f"- `{result.case_id}`: {result.stderr_summary}\n"
    output_path.write_text(report, encoding="utf-8")


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    suite_path = pathlib.Path(args.suite_path)
    suite_data = json.loads(suite_path.read_text(encoding="utf-8"))
    cases = suite_data.get("cases")
    if not isinstance(cases, list) or not cases:
        raise SystemExit("suite must define a non-empty 'cases' list")

    reports_dir = pathlib.Path(args.reports_dir)
    reports_dir.mkdir(parents=True, exist_ok=True)
    output_path = (
        pathlib.Path(args.output_path)
        if args.output_path
        else reports_dir / "suite-report.md"
    )

    results = [
        run_case(
            base_url=args.base_url,
            login=args.login,
            password=args.password,
            library_id=args.library_id,
            workspace_id=args.workspace_id,
            mcp_token=args.mcp_token,
            reports_dir=reports_dir,
            case=case,
        )
        for case in cases
    ]
    render_suite_report(
        output_path=output_path,
        suite_path=suite_path,
        suite_data=suite_data,
        results=results,
    )
    print(output_path)
    return 0 if all(result.exit_code == 0 for result in results) else 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
