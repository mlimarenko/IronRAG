#!/usr/bin/env python3
"""Run W10b grounded-query benchmarks and compare PG results to W10a Arango."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]
RUNNER_PATH = REPO_ROOT / "apps/api/benchmarks/grounded_query/run_live_benchmark.py"
GROUNDING_SUITE_DIR = REPO_ROOT / "apps/api/benchmarks/grounded_query"
DEFAULT_BASELINE_DIR = (
    REPO_ROOT / ".omc/research/arango-audit/W10a-run-20260602T072914Z"
)
DEFAULT_OUTPUT_ROOT = REPO_ROOT / ".omc/research/pg-vs-arango"
DEFAULT_API_BASE_URL = "http://127.0.0.1:19500"
DEFAULT_P95_BUDGET_MS = 90_000.0
DEFAULT_SUITE_FILES = [
    "api_baseline_suite.json",
    "workflow_strict_suite.json",
    "layout_noise_suite.json",
    "graph_multihop_suite.json",
    "technical_contract_suite.json",
]
QUALITY_CHECKS = [
    ("topDoc", "topSearchDocumentOk"),
    ("retrieval", "retrievalContainsRequired"),
    ("answer", "answerPass"),
    ("graph", "graphUsagePass"),
    ("entityLabels", "entityReferenceLabelsPass"),
    ("relationText", "relationReferenceTextPass"),
    ("structured", "structuredEvidencePass"),
    ("verification", "verificationPass"),
    ("strict", "strictCasePass"),
]


def utc_stamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def read_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def normalize_api_base_url(value: str) -> str:
    normalized = value.rstrip("/")
    if normalized.endswith("/v1"):
        return normalized
    return f"{normalized}/v1"


def percentile(values: list[float], percentile_value: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    rank = (len(ordered) - 1) * percentile_value
    lower = int(rank)
    upper = min(lower + 1, len(ordered) - 1)
    weight = rank - lower
    return ordered[lower] * (1.0 - weight) + ordered[upper] * weight


def fmt_ms(value: float | None) -> str:
    if value is None:
        return "-"
    return f"{value:,.0f}ms"


def fmt_delta(value: float | None) -> str:
    if value is None:
        return "-"
    sign = "+" if value >= 0 else ""
    return f"{sign}{value:,.0f}ms"


def fmt_count_delta(value: int) -> str:
    sign = "+" if value >= 0 else ""
    return f"{sign}{value}"


def bool_value(item: dict[str, Any], key: str) -> bool:
    return bool(item.get(key))


def case_latencies(cases: list[dict[str, Any]]) -> list[float]:
    latencies: list[float] = []
    for case in cases:
        value = case.get("answerLatencyMs")
        if isinstance(value, int | float):
            latencies.append(float(value))
    return latencies


def suite_file_for_suite_path(path: Path) -> str:
    return f"{path.stem}.result.json"


def default_output_dir() -> Path:
    return DEFAULT_OUTPUT_ROOT / f"W10b-run-{utc_stamp()}"


def load_suite_result_files(result_dir: Path) -> dict[str, dict[str, Any]]:
    suites: dict[str, dict[str, Any]] = {}
    for path in sorted(result_dir.glob("*.result.json")):
        data = read_json(path)
        if isinstance(data.get("cases"), list) and isinstance(data.get("suite"), dict):
            suites[path.name] = data
    return suites


def summarize_suite(
    result_name: str,
    baseline: dict[str, Any],
    candidate: dict[str, Any] | None,
    p95_budget_ms: float,
) -> dict[str, Any]:
    baseline_cases = baseline.get("cases", [])
    candidate_cases = candidate.get("cases", []) if candidate else []
    baseline_by_id = {case.get("caseId"): case for case in baseline_cases}
    candidate_by_id = {case.get("caseId"): case for case in candidate_cases}
    all_case_ids = sorted(
        case_id
        for case_id in set(baseline_by_id) | set(candidate_by_id)
        if isinstance(case_id, str)
    )

    baseline_failed = {
        case_id
        for case_id, case in baseline_by_id.items()
        if isinstance(case_id, str) and not bool_value(case, "strictCasePass")
    }
    candidate_failed = {
        case_id
        for case_id, case in candidate_by_id.items()
        if isinstance(case_id, str) and not bool_value(case, "strictCasePass")
    }
    missing_candidate_cases = set(baseline_by_id) - set(candidate_by_id)
    missing_candidate_cases = {case_id for case_id in missing_candidate_cases if isinstance(case_id, str)}
    new_failure_ids = sorted((candidate_failed - baseline_failed) | missing_candidate_cases)

    case_diffs: list[dict[str, Any]] = []
    quality_regression_count = 0
    quality_fix_count = 0
    for case_id in all_case_ids:
        baseline_case = baseline_by_id.get(case_id, {})
        candidate_case = candidate_by_id.get(case_id, {})
        check_diffs = []
        for label, key in QUALITY_CHECKS:
            before = bool_value(baseline_case, key)
            after = bool_value(candidate_case, key)
            if before and not after:
                quality_regression_count += 1
            if not before and after:
                quality_fix_count += 1
            if before != after:
                check_diffs.append({"check": label, "baseline": before, "candidate": after})

        baseline_latency = baseline_case.get("answerLatencyMs")
        candidate_latency = candidate_case.get("answerLatencyMs")
        latency_delta = (
            float(candidate_latency) - float(baseline_latency)
            if isinstance(baseline_latency, int | float)
            and isinstance(candidate_latency, int | float)
            else None
        )
        if check_diffs or case_id in new_failure_ids:
            case_diffs.append(
                {
                    "caseId": case_id,
                    "baselineStrictPass": bool_value(baseline_case, "strictCasePass"),
                    "candidateStrictPass": bool_value(candidate_case, "strictCasePass"),
                    "baselineFailedChecks": baseline_case.get("failedChecks", []),
                    "candidateFailedChecks": candidate_case.get("failedChecks", []),
                    "qualityCheckDiffs": check_diffs,
                    "baselineAnswerLatencyMs": baseline_latency,
                    "candidateAnswerLatencyMs": candidate_latency,
                    "answerLatencyDeltaMs": latency_delta,
                }
            )

    baseline_latencies = case_latencies(baseline_cases)
    candidate_latencies = case_latencies(candidate_cases)
    baseline_p50 = percentile(baseline_latencies, 0.50)
    baseline_p95 = percentile(baseline_latencies, 0.95)
    candidate_p50 = percentile(candidate_latencies, 0.50)
    candidate_p95 = percentile(candidate_latencies, 0.95)
    p50_delta = (
        candidate_p50 - baseline_p50
        if candidate_p50 is not None and baseline_p50 is not None
        else None
    )
    p95_delta = (
        candidate_p95 - baseline_p95
        if candidate_p95 is not None and baseline_p95 is not None
        else None
    )

    latency_pass = candidate_p95 is not None and candidate_p95 <= p95_budget_ms
    no_new_failures = not new_failure_ids
    verdict = "PASS" if no_new_failures and latency_pass else "REGRESSION"
    suite_id = baseline.get("suite", {}).get("suiteId") or result_name.removesuffix(".result.json")

    return {
        "resultFile": result_name,
        "suiteId": suite_id,
        "totalCases": len(all_case_ids),
        "baselinePassCount": len(baseline_cases) - len(baseline_failed),
        "baselineFailCount": len(baseline_failed),
        "candidatePassCount": len(candidate_cases) - len(candidate_failed),
        "candidateFailCount": len(candidate_failed) + len(missing_candidate_cases),
        "failCountDelta": len(candidate_failed) + len(missing_candidate_cases) - len(baseline_failed),
        "baselineP50Ms": baseline_p50,
        "candidateP50Ms": candidate_p50,
        "p50DeltaMs": p50_delta,
        "baselineP95Ms": baseline_p95,
        "candidateP95Ms": candidate_p95,
        "p95DeltaMs": p95_delta,
        "p95BudgetMs": p95_budget_ms,
        "latencyPass": latency_pass,
        "noNewFailures": no_new_failures,
        "newFailureIds": new_failure_ids,
        "qualityRegressionCount": quality_regression_count,
        "qualityFixCount": quality_fix_count,
        "caseDiffs": case_diffs,
        "verdict": verdict,
    }


def load_agent_turn_summary(result_dir: Path) -> dict[str, Any] | None:
    path = result_dir / "agent_turn_p95.result.json"
    if path.exists():
        return read_json(path)
    return None


def summarize_agent_turn(
    baseline_dir: Path,
    candidate_dir: Path,
) -> dict[str, Any] | None:
    baseline = load_agent_turn_summary(baseline_dir)
    candidate = load_agent_turn_summary(candidate_dir)
    if not baseline and not candidate:
        return None
    baseline_p95 = baseline.get("p95_ms") if baseline else None
    candidate_p95 = candidate.get("p95_ms") if candidate else None
    delta = (
        float(candidate_p95) - float(baseline_p95)
        if isinstance(baseline_p95, int | float) and isinstance(candidate_p95, int | float)
        else None
    )
    gate_passed = bool(candidate.get("gate_passed")) if candidate else None
    return {
        "baselinePresent": baseline is not None,
        "candidatePresent": candidate is not None,
        "baselineP95Ms": baseline_p95,
        "candidateP95Ms": candidate_p95,
        "p95DeltaMs": delta,
        "candidateGatePassed": gate_passed,
        "note": (
            "agent_turn_p95 is compared when candidate output contains "
            "agent_turn_p95.result.json; this wrapper runs grounded-query suites only."
        ),
    }


def print_table(rows: list[dict[str, Any]], agent_turn: dict[str, Any] | None) -> None:
    print("W10b PostgreSQL vs W10a Arango grounded-query comparison")
    print()
    headers = [
        "suite",
        "strict",
        "failΔ",
        "p50Δ",
        "p95Δ",
        "p95",
        "qualityΔ",
        "verdict",
    ]
    table_rows = []
    for row in rows:
        strict = (
            f"{row['baselinePassCount']}/{row['totalCases']} -> "
            f"{row['candidatePassCount']}/{row['totalCases']}"
        )
        quality_delta = f"-{row['qualityRegressionCount']} +{row['qualityFixCount']}"
        table_rows.append(
            [
                str(row["suiteId"]),
                strict,
                fmt_count_delta(int(row["failCountDelta"])),
                fmt_delta(row["p50DeltaMs"]),
                fmt_delta(row["p95DeltaMs"]),
                f"{fmt_ms(row['candidateP95Ms'])} <= {fmt_ms(row['p95BudgetMs'])}",
                quality_delta,
                str(row["verdict"]),
            ]
        )

    widths = [
        max(len(headers[idx]), *(len(row[idx]) for row in table_rows))
        for idx in range(len(headers))
    ]
    print("  ".join(headers[idx].ljust(widths[idx]) for idx in range(len(headers))))
    print("  ".join("-" * widths[idx] for idx in range(len(headers))))
    for row in table_rows:
        print("  ".join(row[idx].ljust(widths[idx]) for idx in range(len(headers))))

    new_failure_rows = [row for row in rows if row["newFailureIds"]]
    if new_failure_rows:
        print()
        print("New strict failures")
        for row in new_failure_rows:
            print(f"  {row['suiteId']}: {', '.join(row['newFailureIds'])}")

    quality_rows = [
        row
        for row in rows
        if any(
            any(not diff["candidate"] for diff in case["qualityCheckDiffs"])
            for case in row["caseDiffs"]
        )
    ]
    if quality_rows:
        print()
        print("Answer-quality regressions")
        for row in quality_rows:
            for case in row["caseDiffs"]:
                regressed_checks = [
                    diff["check"]
                    for diff in case["qualityCheckDiffs"]
                    if diff["baseline"] and not diff["candidate"]
                ]
                if regressed_checks:
                    print(
                        f"  {row['suiteId']}::{case['caseId']}: "
                        f"{', '.join(regressed_checks)} "
                        f"latencyΔ={fmt_delta(case['answerLatencyDeltaMs'])}"
                    )

    if agent_turn:
        print()
        if agent_turn["candidatePresent"]:
            print(
                "Agent turn p95: "
                f"{fmt_ms(agent_turn['candidateP95Ms'])} "
                f"delta={fmt_delta(agent_turn['p95DeltaMs'])} "
                f"gate={agent_turn['candidateGatePassed']}"
            )
        else:
            print("Agent turn p95: baseline present; candidate result not generated by this wrapper")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Run the W10b live grounded-query suite against PostgreSQL and compare "
            "the results to the captured W10a Arango baseline."
        )
    )
    parser.add_argument(
        "--api-base-url",
        default=os.environ.get("IRONRAG_API_BASE_URL", DEFAULT_API_BASE_URL),
        help=(
            "Target API base URL. /v1 is appended when omitted "
            f"(default: env IRONRAG_API_BASE_URL or {DEFAULT_API_BASE_URL})."
        ),
    )
    parser.add_argument(
        "--baseline-dir",
        type=Path,
        default=DEFAULT_BASELINE_DIR,
        help=f"Directory containing W10a *.result.json files (default: {DEFAULT_BASELINE_DIR}).",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        help="Directory for the fresh PG result set (default: .omc/research/pg-vs-arango/W10b-run-<UTC>).",
    )
    parser.add_argument(
        "--candidate-dir",
        type=Path,
        help="Compare an existing candidate result directory instead of running the live harness.",
    )
    parser.add_argument(
        "--suite",
        action="append",
        type=Path,
        help="Suite JSON to run. May be repeated. Defaults to the W10a suite set.",
    )
    parser.add_argument(
        "--workspace-id",
        default=os.environ.get("IRONRAG_BENCHMARK_WORKSPACE_ID"),
        help="Workspace UUID passed through to run_live_benchmark.py.",
    )
    parser.add_argument(
        "--library-id",
        help="Reuse an existing benchmark library instead of creating a fresh one.",
    )
    parser.add_argument(
        "--library-name",
        help="Display name for a fresh benchmark library.",
    )
    parser.add_argument(
        "--session-cookie",
        default=os.environ.get("IRONRAG_SESSION_COOKIE"),
        help="Value of ironrag_ui_session cookie passed to run_live_benchmark.py.",
    )
    parser.add_argument(
        "--skip-upload",
        action="store_true",
        help="Pass --skip-upload to run_live_benchmark.py; requires --library-id.",
    )
    parser.add_argument(
        "--canonicalize-reused-library",
        action="store_true",
        help="Wait for quiet/query-ready state when --skip-upload reuses a library.",
    )
    parser.add_argument(
        "--wait-timeout-seconds",
        type=float,
        default=900.0,
        help="Readiness timeout passed to run_live_benchmark.py.",
    )
    parser.add_argument(
        "--poll-interval-seconds",
        type=float,
        default=5.0,
        help="Readiness poll interval passed to run_live_benchmark.py.",
    )
    parser.add_argument(
        "--query-top-k",
        type=int,
        default=8,
        help="topK passed to grounded answer requests.",
    )
    parser.add_argument(
        "--p95-budget-ms",
        type=float,
        default=DEFAULT_P95_BUDGET_MS,
        help=(
            "Per-suite answerLatencyMs p95 budget used by the regression gate "
            f"(default: {DEFAULT_P95_BUDGET_MS:.0f}, the turn p95 SLO)."
        ),
    )
    parser.add_argument(
        "--keep-going-on-runner-error",
        action="store_true",
        help="Attempt comparison even if run_live_benchmark.py exits non-zero.",
    )
    return parser


def resolve_suite_paths(args: argparse.Namespace) -> list[Path]:
    if args.suite:
        return [path.resolve() for path in args.suite]
    return [(GROUNDING_SUITE_DIR / item).resolve() for item in DEFAULT_SUITE_FILES]


def command_for_log(command: list[str]) -> str:
    redacted: list[str] = []
    redact_next = False
    for item in command:
        if redact_next:
            redacted.append("<redacted>")
            redact_next = False
            continue
        redacted.append(item)
        if item == "--session-cookie":
            redact_next = True
    return " ".join(redacted)


def run_live_harness(args: argparse.Namespace, suite_paths: list[Path], output_dir: Path) -> int:
    command = [
        sys.executable,
        str(RUNNER_PATH),
        "--base-url",
        normalize_api_base_url(args.api_base_url),
        "--output-dir",
        str(output_dir),
        "--workspace-id",
        str(args.workspace_id),
        "--session-cookie",
        str(args.session_cookie),
        "--wait-timeout-seconds",
        str(args.wait_timeout_seconds),
        "--poll-interval-seconds",
        str(args.poll_interval_seconds),
        "--query-top-k",
        str(args.query_top_k),
    ]
    for suite_path in suite_paths:
        command.extend(["--suite", str(suite_path)])
    if args.library_id:
        command.extend(["--library-id", args.library_id])
    if args.library_name:
        command.extend(["--library-name", args.library_name])
    if args.skip_upload:
        command.append("--skip-upload")
    if args.canonicalize_reused_library:
        command.append("--canonicalize-reused-library")

    output_dir.mkdir(parents=True, exist_ok=True)
    print(f"Running live benchmark: {command_for_log(command)}", file=sys.stderr)
    completed = subprocess.run(command, cwd=REPO_ROOT, check=False)
    return completed.returncode


def validate_args(args: argparse.Namespace) -> int:
    if args.candidate_dir:
        return 0
    missing = []
    if not args.workspace_id:
        missing.append("--workspace-id or IRONRAG_BENCHMARK_WORKSPACE_ID")
    if not args.session_cookie:
        missing.append("--session-cookie or IRONRAG_SESSION_COOKIE")
    if args.skip_upload and not args.library_id:
        missing.append("--library-id when --skip-upload is used")
    if missing:
        print("Missing required live benchmark input:", file=sys.stderr)
        for item in missing:
            print(f"  {item}", file=sys.stderr)
        return 2
    return 0


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    validation_status = validate_args(args)
    if validation_status:
        return validation_status

    baseline_dir = args.baseline_dir.resolve()
    if not baseline_dir.exists():
        print(f"baseline directory not found: {baseline_dir}", file=sys.stderr)
        return 2

    suite_paths = resolve_suite_paths(args)
    for suite_path in suite_paths:
        if not suite_path.exists():
            print(f"suite not found: {suite_path}", file=sys.stderr)
            return 2

    if args.candidate_dir:
        candidate_dir = args.candidate_dir.resolve()
    else:
        candidate_dir = (args.output_dir or default_output_dir()).resolve()
        runner_status = run_live_harness(args, suite_paths, candidate_dir)
        if runner_status and not args.keep_going_on_runner_error:
            print(f"run_live_benchmark.py failed with exit code {runner_status}", file=sys.stderr)
            return runner_status

    baseline_suites = load_suite_result_files(baseline_dir)
    candidate_suites = load_suite_result_files(candidate_dir)
    if not baseline_suites:
        print(f"no suite result files found in baseline directory: {baseline_dir}", file=sys.stderr)
        return 2
    if not candidate_suites:
        print(f"no suite result files found in candidate directory: {candidate_dir}", file=sys.stderr)
        return 2

    expected_result_files = [suite_file_for_suite_path(path) for path in suite_paths]
    rows = []
    missing_baseline = []
    for result_file in expected_result_files:
        baseline = baseline_suites.get(result_file)
        if baseline is None:
            missing_baseline.append(result_file)
            continue
        rows.append(
            summarize_suite(
                result_file,
                baseline,
                candidate_suites.get(result_file),
                args.p95_budget_ms,
            )
        )
    if missing_baseline:
        print(
            f"baseline is missing expected suite result(s): {', '.join(missing_baseline)}",
            file=sys.stderr,
        )
        return 2

    agent_turn = summarize_agent_turn(baseline_dir, candidate_dir)
    comparison = {
        "generatedAt": utc_now_iso(),
        "baselineDir": str(baseline_dir),
        "candidateDir": str(candidate_dir),
        "apiBaseUrl": normalize_api_base_url(args.api_base_url),
        "p95BudgetMs": args.p95_budget_ms,
        "suites": rows,
        "agentTurnP95": agent_turn,
        "verdict": "PASS" if all(row["verdict"] == "PASS" for row in rows) else "REGRESSION",
    }
    write_json(candidate_dir / "pg_vs_arango_comparison.result.json", comparison)
    print_table(rows, agent_turn)
    print()
    print(f"Candidate results: {candidate_dir}")
    print(f"Comparison JSON: {candidate_dir / 'pg_vs_arango_comparison.result.json'}")
    return 0 if comparison["verdict"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
