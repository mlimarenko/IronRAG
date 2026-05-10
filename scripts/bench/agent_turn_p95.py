#!/usr/bin/env python3
"""
Benchmark: UI assistant turn p95 latency gate.

Hits POST /v1/query/sessions + POST /v1/query/sessions/{id}/turns for each
question and reports p50/p95/p99 latency.

Environment variables
---------------------
IRONRAG_API_BASE_URL   Base URL of the API (default: http://localhost:19000)
IRONRAG_API_TOKEN      Bearer token (required)
IRONRAG_LIBRARY_ID     Library UUID to query against (required)
IRONRAG_BENCH_QUESTIONS
                       Path to question file (default: scripts/bench/grounded-queries.md)

Exit codes
----------
0  p95 latency is within the SLO gate (≤ 25 000 ms)
1  p95 latency breaches the gate, or required env vars are missing

SLO gate reference: CLAUDE.md §6 Concurrency — "p95 ≤ 30 s" for a full turn;
this script uses a 25 000 ms gate (25 s) to give a 5 s margin before the
constitution threshold.
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import Any
from urllib import error as urllib_error
from urllib import request as urllib_request

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

DEFAULT_BASE_URL = "http://localhost:19000"
DEFAULT_QUESTIONS_PATH = "scripts/bench/grounded-queries.md"

# Constitution gate: CLAUDE.md §6 Concurrency — p95 ≤ 30 s per turn.
# We enforce 25 s here to provide a 5-second margin before the hard limit.
P95_GATE_MS = 25_000


# ---------------------------------------------------------------------------
# HTTP helpers (stdlib only — no third-party deps beyond `requests` fallback)
# ---------------------------------------------------------------------------

def _headers(token: str) -> dict[str, str]:
    return {
        "Authorization": f"Bearer {token}",
        "Content-Type": "application/json",
        "Accept": "application/json",
    }


def _post(base_url: str, path: str, token: str, body: dict[str, Any]) -> dict[str, Any]:
    """Perform a POST request; raise urllib_error.URLError / HTTPError on failure."""
    url = base_url.rstrip("/") + path
    data = json.dumps(body).encode()
    req = urllib_request.Request(url, data=data, headers=_headers(token), method="POST")
    with urllib_request.urlopen(req, timeout=120) as resp:
        return json.loads(resp.read().decode())


# ---------------------------------------------------------------------------
# Core benchmark logic
# ---------------------------------------------------------------------------

def load_questions(path: str) -> list[str]:
    """Load questions from a markdown file; skip blank lines and # comments."""
    questions: list[str] = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            stripped = line.strip()
            if stripped and not stripped.startswith("#"):
                questions.append(stripped)
    return questions


def run_turn(
    base_url: str,
    token: str,
    library_id: str,
    question: str,
    top_k: int | None,
) -> tuple[float, str | None]:
    """
    Execute one complete turn (session creation + turn POST).

    Returns (elapsed_ms, error_message_or_None).
    """
    t_start = time.perf_counter()
    try:
        session_body: dict[str, Any] = {"libraryId": library_id}
        session = _post(base_url, "/v1/query/sessions", token, session_body)
        session_id = session["id"]

        turn_body: dict[str, Any] = {"contentText": question}
        if top_k is not None:
            turn_body["topK"] = top_k

        _post(base_url, f"/v1/query/sessions/{session_id}/turns", token, turn_body)
    except (urllib_error.URLError, urllib_error.HTTPError, KeyError, json.JSONDecodeError) as exc:
        elapsed_ms = (time.perf_counter() - t_start) * 1000.0
        return elapsed_ms, str(exc)

    elapsed_ms = (time.perf_counter() - t_start) * 1000.0
    return elapsed_ms, None


def percentile(data: list[float], p: int) -> float:
    """Return the p-th percentile (inclusive method) from a sorted list."""
    if len(data) == 1:
        return data[0]
    # statistics.quantiles returns n-1 cut points; we need the p-th percentile
    # via the 'inclusive' method, which matches the constitution definition.
    qs = statistics.quantiles(sorted(data), n=100, method="inclusive")
    # quantiles(n=100) returns 99 values: index 0 = P1, index 98 = P99
    idx = max(0, min(p - 1, len(qs) - 1))
    return qs[idx]


def run_benchmark(
    base_url: str,
    token: str,
    library_id: str,
    questions: list[str],
    concurrency: int,
    top_k: int | None,
) -> dict[str, Any]:
    """
    Run all questions (optionally in parallel) and return a results dict.
    """
    per_question: list[dict[str, Any]] = []
    latencies_ms: list[float] = []
    failure_count = 0

    def _task(q: str) -> tuple[str, float, str | None]:
        elapsed_ms, err = run_turn(base_url, token, library_id, q, top_k)
        return q, elapsed_ms, err

    with ThreadPoolExecutor(max_workers=concurrency) as executor:
        futures = {executor.submit(_task, q): q for q in questions}
        for future in as_completed(futures):
            q, elapsed_ms, err = future.result()
            entry: dict[str, Any] = {
                "question": q,
                "elapsed_ms": round(elapsed_ms, 1),
            }
            if err:
                entry["error"] = err
                failure_count += 1
                print(
                    f"  FAIL  [{elapsed_ms:8.0f} ms]  {q[:60]}  — {err}",
                    file=sys.stderr,
                )
            else:
                latencies_ms.append(elapsed_ms)
                print(
                    f"  OK    [{elapsed_ms:8.0f} ms]  {q[:60]}",
                    file=sys.stderr,
                )
            per_question.append(entry)

    if not latencies_ms:
        return {
            "runs": len(questions),
            "successes": 0,
            "failures": failure_count,
            "p50_ms": None,
            "p95_ms": None,
            "p99_ms": None,
            "min_ms": None,
            "max_ms": None,
            "per_question": per_question,
        }

    sorted_lat = sorted(latencies_ms)
    n = len(sorted_lat)

    result: dict[str, Any] = {
        "runs": len(questions),
        "successes": n,
        "failures": failure_count,
        "min_ms": round(sorted_lat[0], 1),
        "p50_ms": round(percentile(sorted_lat, 50), 1),
        "p95_ms": round(percentile(sorted_lat, 95), 1),
        "p99_ms": round(percentile(sorted_lat, 99), 1),
        "max_ms": round(sorted_lat[-1], 1),
        "per_question": sorted(per_question, key=lambda x: x["elapsed_ms"], reverse=True),
    }
    return result


def print_summary_table(result: dict[str, Any], gate_ms: int) -> None:
    """Print a human-readable summary table to stderr."""
    print("", file=sys.stderr)
    print("=" * 60, file=sys.stderr)
    print("  Agent turn latency summary", file=sys.stderr)
    print("=" * 60, file=sys.stderr)
    print(f"  Total runs  : {result['runs']}", file=sys.stderr)
    print(f"  Successes   : {result['successes']}", file=sys.stderr)
    print(f"  Failures    : {result['failures']}", file=sys.stderr)
    if result["p50_ms"] is not None:
        print(f"  min         : {result['min_ms']:>9.0f} ms", file=sys.stderr)
        print(f"  p50         : {result['p50_ms']:>9.0f} ms", file=sys.stderr)
        p95 = result["p95_ms"]
        gate_label = f"  [gate: ≤ {gate_ms} ms]"
        gate_status = "PASS" if p95 <= gate_ms else "BREACH"
        print(
            f"  p95         : {p95:>9.0f} ms  {gate_status}{gate_label}",
            file=sys.stderr,
        )
        print(f"  p99         : {result['p99_ms']:>9.0f} ms", file=sys.stderr)
        print(f"  max         : {result['max_ms']:>9.0f} ms", file=sys.stderr)
    else:
        print("  No successful runs — cannot compute latency stats.", file=sys.stderr)
    print("=" * 60, file=sys.stderr)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="agent_turn_p95.py",
        description=(
            "Benchmark IronRAG UI assistant turn latency.\n"
            "Measures p50/p95/p99 across N questions; exits 1 if p95 > 25 000 ms.\n\n"
            "Required env vars: IRONRAG_API_TOKEN, IRONRAG_LIBRARY_ID\n"
            "Optional env vars: IRONRAG_API_BASE_URL, IRONRAG_BENCH_QUESTIONS"
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--concurrency",
        type=int,
        default=1,
        metavar="N",
        help="Number of parallel sessions (default: 1)",
    )
    parser.add_argument(
        "--top-k",
        type=int,
        default=None,
        metavar="N",
        help="Override top-K retrieval parameter sent to the API (default: API default)",
    )
    parser.add_argument(
        "--question-limit",
        type=int,
        default=None,
        metavar="N",
        help="Run only the first N questions (default: all)",
    )
    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    base_url = os.environ.get("IRONRAG_API_BASE_URL", DEFAULT_BASE_URL).rstrip("/")
    token = os.environ.get("IRONRAG_API_TOKEN", "")
    library_id = os.environ.get("IRONRAG_LIBRARY_ID", "")
    questions_path = os.environ.get("IRONRAG_BENCH_QUESTIONS", DEFAULT_QUESTIONS_PATH)

    errors: list[str] = []
    if not token:
        errors.append("IRONRAG_API_TOKEN is not set")
    if not library_id:
        errors.append("IRONRAG_LIBRARY_ID is not set")
    if errors:
        for msg in errors:
            print(f"ERROR: {msg}", file=sys.stderr)
        parser.print_usage(sys.stderr)
        return 1

    try:
        questions = load_questions(questions_path)
    except FileNotFoundError:
        print(f"ERROR: question file not found: {questions_path}", file=sys.stderr)
        return 1

    if not questions:
        print("ERROR: no questions loaded from question file", file=sys.stderr)
        return 1

    if args.question_limit is not None and args.question_limit > 0:
        questions = questions[: args.question_limit]

    print(
        f"Running {len(questions)} questions  concurrency={args.concurrency}"
        f"  base_url={base_url}  library_id={library_id}",
        file=sys.stderr,
    )
    print("", file=sys.stderr)

    result = run_benchmark(
        base_url=base_url,
        token=token,
        library_id=library_id,
        questions=questions,
        concurrency=args.concurrency,
        top_k=args.top_k,
    )

    print_summary_table(result, P95_GATE_MS)

    # Write JSON summary to stdout
    print(json.dumps(result))

    if result["p95_ms"] is None:
        # All runs failed — treat as breach
        return 1

    return 0 if result["p95_ms"] <= P95_GATE_MS else 1


if __name__ == "__main__":
    sys.exit(main())
