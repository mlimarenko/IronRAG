#!/usr/bin/env python3
"""Legacy PG-vs-baseline entry point backed by the canonical release comparator."""

from __future__ import annotations

import argparse
import json
import math
import os
import shlex
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Sequence


REPO_ROOT = Path(__file__).resolve().parents[2]
GROUNDING_DIR = REPO_ROOT / "apps/api/benchmarks/grounded_query"
RUNNER_PATH = GROUNDING_DIR / "run_live_benchmark.py"
sys.path.insert(0, str(GROUNDING_DIR))

import compare_benchmarks as canonical  # noqa: E402
import run_live_benchmark as runner  # noqa: E402


DEFAULT_BASELINE_DIR = REPO_ROOT / ".omc/research/arango-audit/W10a-run-20260602T072914Z"
DEFAULT_OUTPUT_ROOT = REPO_ROOT / ".omc/research/pg-vs-arango"
DEFAULT_API_BASE_URL = "http://127.0.0.1:19500"
DEFAULT_MAX_AGENT_TURN_P95_MS = 90_000.0
AGENT_RESULT_FILE = "agent_turn_p95.result.json"


def utc_stamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def read_json(path: Path) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError(f"expected a JSON object: {path}")
    return payload


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, allow_nan=False) + "\n",
        encoding="utf-8",
    )


def normalize_api_base_url(value: str) -> str:
    normalized = value.rstrip("/")
    return normalized if normalized.endswith("/v1") else f"{normalized}/v1"


def _valid_non_negative_number(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = float(value)
    return number if math.isfinite(number) and number >= 0.0 else None


def _validate_agent_result(label: str, payload: dict[str, Any] | None) -> list[str]:
    if payload is None:
        return [label]
    invalid: list[str] = []
    for key in ("p50_ms", "p95_ms", "p99_ms"):
        if _valid_non_negative_number(payload.get(key)) is None:
            invalid.append(f"{label}.{key}")
    for key in ("runs", "successes", "failures"):
        value = payload.get(key)
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            invalid.append(f"{label}.{key}")
    if not invalid:
        if payload["runs"] <= 0 or payload["successes"] <= 0:
            invalid.append(f"{label}.sampleCount")
        if payload["successes"] + payload["failures"] != payload["runs"]:
            invalid.append(f"{label}.counters")
    if not isinstance(payload.get("gate_passed"), bool):
        invalid.append(f"{label}.gate_passed")
    return invalid


def build_agent_turn_gate(
    baseline_dir: Path,
    candidate_dir: Path,
    *,
    max_regression_percent: float = canonical.DEFAULT_MAX_LATENCY_REGRESSION_PERCENT,
    max_candidate_p95_ms: float = DEFAULT_MAX_AGENT_TURN_P95_MS,
) -> dict[str, Any]:
    baseline_path = baseline_dir / AGENT_RESULT_FILE
    candidate_path = candidate_dir / AGENT_RESULT_FILE
    baseline = read_json(baseline_path) if baseline_path.exists() else None
    candidate = read_json(candidate_path) if candidate_path.exists() else None
    invalid_artifacts = [
        *_validate_agent_result("baseline", baseline),
        *_validate_agent_result("candidate", candidate),
    ]

    baseline_p95 = _valid_non_negative_number(
        baseline.get("p95_ms") if baseline is not None else None
    )
    candidate_p95 = _valid_non_negative_number(
        candidate.get("p95_ms") if candidate is not None else None
    )
    regression_percent = (
        canonical.latency_regression_percent(baseline_p95, candidate_p95)
        if baseline_p95 is not None and candidate_p95 is not None
        else None
    )
    relative_passed = (
        regression_percent is not None
        and regression_percent <= max_regression_percent + 1e-9
    )
    absolute_passed = (
        candidate_p95 is not None
        and candidate_p95 <= max_candidate_p95_ms + 1e-9
    )
    candidate_gate_passed = (
        candidate is not None and candidate.get("gate_passed") is True
    )
    candidate_has_no_failures = (
        candidate is not None and candidate.get("failures") == 0
    )
    return {
        "required": True,
        "invalidArtifacts": sorted(invalid_artifacts),
        "baselineP95Ms": baseline_p95,
        "candidateP95Ms": candidate_p95,
        "regressionPercent": regression_percent,
        "maxRegressionPercent": max_regression_percent,
        "relativePassed": relative_passed,
        "maxCandidateP95Ms": max_candidate_p95_ms,
        "absolutePassed": absolute_passed,
        "candidateGatePassed": candidate_gate_passed,
        "candidateHasNoFailures": candidate_has_no_failures,
        "passed": (
            not invalid_artifacts
            and relative_passed
            and absolute_passed
            and candidate_gate_passed
            and candidate_has_no_failures
        ),
    }


def build_release_report(
    baseline_dir: Path,
    candidate_dir: Path,
    *,
    max_latency_regression_percent: float,
    max_candidate_p50_ms: float,
    max_candidate_p95_ms: float,
    max_agent_turn_p95_ms: float,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    baseline_matrix = canonical.load_matrix(baseline_dir)
    candidate_matrix = canonical.load_matrix(candidate_dir)
    grounded = canonical.build_comparison_report(
        baseline_matrix,
        candidate_matrix,
        max_latency_regression_percent=max_latency_regression_percent,
        max_candidate_p50_ms=max_candidate_p50_ms,
        max_candidate_p95_ms=max_candidate_p95_ms,
    )
    agent_turn = build_agent_turn_gate(
        baseline_dir,
        candidate_dir,
        max_regression_percent=max_latency_regression_percent,
        max_candidate_p95_ms=max_agent_turn_p95_ms,
    )
    combined = {
        "generatedAt": utc_now_iso(),
        "baselineDir": str(baseline_dir),
        "candidateDir": str(candidate_dir),
        "grounded": grounded,
        "agentTurn": agent_turn,
        "passed": grounded["passed"] and agent_turn["passed"],
    }
    return baseline_matrix, candidate_matrix, combined


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Run or compare a PG candidate using the canonical grounded release gate "
            "plus the required agent-turn verdict."
        )
    )
    parser.add_argument(
        "--api-base-url",
        default=os.environ.get("IRONRAG_API_BASE_URL", DEFAULT_API_BASE_URL),
    )
    parser.add_argument("--baseline-dir", type=Path, default=DEFAULT_BASELINE_DIR)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument(
        "--candidate-dir",
        type=Path,
        help="Compare existing artifacts instead of running the live grounded harness.",
    )
    parser.add_argument("--suite", action="append", type=Path)
    parser.add_argument(
        "--workspace-id",
        default=os.environ.get("IRONRAG_BENCHMARK_WORKSPACE_ID"),
    )
    parser.add_argument("--library-id")
    parser.add_argument("--library-name")
    parser.add_argument("--skip-upload", action="store_true")
    parser.add_argument("--canonicalize-reused-library", action="store_true")
    parser.add_argument("--wait-timeout-seconds", type=float, default=900.0)
    parser.add_argument("--poll-interval-seconds", type=float, default=5.0)
    parser.add_argument("--query-top-k", type=int, default=8)
    parser.add_argument(
        "--max-latency-regression-percent",
        type=float,
        default=canonical.DEFAULT_MAX_LATENCY_REGRESSION_PERCENT,
    )
    parser.add_argument(
        "--max-candidate-p50-ms",
        type=float,
        default=canonical.DEFAULT_MAX_CANDIDATE_P50_MS,
    )
    parser.add_argument(
        "--max-candidate-p95-ms",
        type=float,
        default=canonical.DEFAULT_MAX_CANDIDATE_P95_MS,
    )
    parser.add_argument(
        "--max-agent-turn-p95-ms",
        type=float,
        default=DEFAULT_MAX_AGENT_TURN_P95_MS,
    )
    parser.add_argument("--keep-going-on-runner-error", action="store_true")
    return parser


def resolve_suite_paths(args: argparse.Namespace) -> list[Path]:
    if args.suite:
        return [path.resolve() for path in args.suite]
    return [(GROUNDING_DIR / item).resolve() for item in runner.DEFAULT_SUITE_MATRIX]


def default_output_dir() -> Path:
    return DEFAULT_OUTPUT_ROOT / f"W10b-run-{utc_stamp()}"


def run_live_harness(
    args: argparse.Namespace,
    suite_paths: Sequence[Path],
    output_dir: Path,
) -> int:
    command = [
        sys.executable,
        str(RUNNER_PATH),
        "--base-url",
        normalize_api_base_url(args.api_base_url),
        "--output-dir",
        str(output_dir),
        "--workspace-id",
        str(args.workspace_id),
        "--wait-timeout-seconds",
        str(args.wait_timeout_seconds),
        "--poll-interval-seconds",
        str(args.poll_interval_seconds),
        "--query-top-k",
        str(args.query_top_k),
        "--strict",
    ]
    for suite_path in suite_paths:
        command.extend(("--suite", str(suite_path)))
    if args.library_id:
        command.extend(("--library-id", args.library_id))
    if args.library_name:
        command.extend(("--library-name", args.library_name))
    if args.skip_upload:
        command.append("--skip-upload")
    if args.canonicalize_reused_library:
        command.append("--canonicalize-reused-library")
    output_dir.mkdir(parents=True, exist_ok=True)
    print(f"Running live benchmark: {shlex.join(command)}", file=sys.stderr)
    return subprocess.run(command, cwd=REPO_ROOT, check=False).returncode


def validate_numeric_args(parser: argparse.ArgumentParser, args: argparse.Namespace) -> None:
    for name in (
        "max_latency_regression_percent",
        "max_candidate_p50_ms",
        "max_candidate_p95_ms",
        "max_agent_turn_p95_ms",
    ):
        value = getattr(args, name)
        is_zero_disallowed = (
            name != "max_latency_regression_percent"
            and math.isclose(value, 0.0, rel_tol=0.0, abs_tol=0.0)
        )
        if not math.isfinite(value) or value < 0.0 or is_zero_disallowed:
            parser.error(f"--{name.replace('_', '-')} must be a positive finite number")


def resolve_candidate_dir(
    parser: argparse.ArgumentParser,
    args: argparse.Namespace,
) -> tuple[Path, int | None]:
    if args.candidate_dir:
        return args.candidate_dir.resolve(), None
    if not args.workspace_id:
        parser.error("--workspace-id or IRONRAG_BENCHMARK_WORKSPACE_ID is required")
    try:
        runner.resolve_session_cookie()
    except ValueError as error:
        parser.error(str(error))
    suite_paths = resolve_suite_paths(args)
    missing_suites = [str(path) for path in suite_paths if not path.exists()]
    if missing_suites:
        parser.error(f"suite file not found: {', '.join(missing_suites)}")
    if args.skip_upload and not args.library_id:
        parser.error("--skip-upload requires --library-id")
    candidate_dir = (args.output_dir or default_output_dir()).resolve()
    runner_status = run_live_harness(args, suite_paths, candidate_dir)
    if runner_status and not args.keep_going_on_runner_error:
        return candidate_dir, runner_status
    return candidate_dir, None


def compare_artifacts(
    args: argparse.Namespace,
    baseline_dir: Path,
    candidate_dir: Path,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    return build_release_report(
        baseline_dir,
        candidate_dir,
        max_latency_regression_percent=args.max_latency_regression_percent,
        max_candidate_p50_ms=args.max_candidate_p50_ms,
        max_candidate_p95_ms=args.max_candidate_p95_ms,
        max_agent_turn_p95_ms=args.max_agent_turn_p95_ms,
    )


def print_and_write_report(
    baseline_matrix: dict[str, Any],
    candidate_matrix: dict[str, Any],
    report: dict[str, Any],
    candidate_dir: Path,
) -> int:
    canonical.print_comparison_report(
        baseline_matrix,
        candidate_matrix,
        report["grounded"],
    )
    agent_turn = report["agentTurn"]
    print("\n--- Agent Turn Gate ---")
    print(json.dumps(agent_turn, ensure_ascii=False, indent=2, allow_nan=False))
    print("\nCOMBINED OVERALL: " + ("PASS" if report["passed"] else "FAIL"))
    output_path = candidate_dir / "pg_vs_arango_comparison.result.json"
    write_json(output_path, report)
    print(f"Comparison JSON: {output_path}")
    return 0 if report["passed"] else 1


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    validate_numeric_args(parser, args)
    baseline_dir = args.baseline_dir.resolve()
    if not baseline_dir.exists():
        parser.error(f"baseline directory not found: {baseline_dir}")

    candidate_dir, runner_status = resolve_candidate_dir(parser, args)
    if runner_status is not None:
        return runner_status
    try:
        baseline_matrix, candidate_matrix, report = compare_artifacts(
            args,
            baseline_dir,
            candidate_dir,
        )
    except (OSError, ValueError) as error:
        print(f"benchmark artifact error: {error}", file=sys.stderr)
        return 2
    return print_and_write_report(
        baseline_matrix,
        candidate_matrix,
        report,
        candidate_dir,
    )


if __name__ == "__main__":
    raise SystemExit(main())
