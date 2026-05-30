#!/usr/bin/env python3
"""
Benchmark: UI assistant turn p95 latency gate.

Hits POST /v1/query/sessions + POST /v1/query/sessions/{id}/turns for each
question and reports p50/p95/p99 latency. A turn only counts as successful
when the response has assistant text, meets any configured verifier gate, and
has the configured minimum grounding references.

Environment variables
---------------------
IRONRAG_API_BASE_URL   Base URL of the API (default: http://localhost:19000)
IRONRAG_API_TOKEN      Bearer token (preferred when set)
IRONRAG_LOGIN          Login for cookie-session auth (default: admin)
IRONRAG_PROBE_PASSWORD Password for cookie-session auth when no bearer token is set
IRONRAG_LIBRARY_ID     Library UUID to query against (required)
IRONRAG_BENCH_QUESTIONS
                       Path to question file (default: scripts/bench/grounded-queries.md)

Exit codes
----------
0  p95 latency is within the SLO gate (≤ 28 000 ms)
1  p95 latency breaches the gate, a quality gate fails, or required env vars
   are missing

SLO gate reference: CLAUDE.md §6 Concurrency — "p95 ≤ 30 s" for a full turn;
this script uses a 28 000 ms gate (28 s) to match WALL_CLOCK_DEADLINE in
mcp_agent/turn.rs, with a 2 s margin before the constitution threshold.
"""

from __future__ import annotations

import argparse
import http.cookiejar
import json
import os
import pathlib
import re
import statistics
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from typing import Any
from urllib import error as urllib_error
from urllib import request as urllib_request

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

DEFAULT_BASE_URL = "http://localhost:19000"
DEFAULT_QUESTIONS_PATH = "scripts/bench/grounded-queries.md"

# Constitution gate: CLAUDE.md §6 Concurrency — p95 ≤ 30 s per turn.
# We enforce 28 s here to match WALL_CLOCK_DEADLINE in mcp_agent/turn.rs,
# providing a 2 s margin before the constitution threshold.
P95_GATE_MS = 28_000
DEFAULT_EXPECTED_VERIFICATION: str | None = None
DEFAULT_MIN_REFERENCES = 1
DEFAULT_MIN_ANSWER_CHARS = 1
UUID_PATTERN = re.compile(
    r"^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-"
    r"[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$"
)
REQUEST_ERROR_TYPES = (
    urllib_error.URLError,
    urllib_error.HTTPError,
    OSError,
    KeyError,
    json.JSONDecodeError,
)
AUTH_ERROR_TYPES = (RuntimeError, *REQUEST_ERROR_TYPES)


@dataclass(frozen=True)
class TurnQuality:
    answer_chars: int
    verification_state: str | None
    reference_count: int
    missing_required: tuple[str, ...] = ()
    forbidden_found: tuple[str, ...] = ()


@dataclass(frozen=True)
class BenchmarkQuestion:
    question: str
    case_id: str | None = None
    require_all: tuple[str, ...] = ()
    forbid_any: tuple[str, ...] = ()
    expected_verification: str | None = None
    min_references: int | None = None
    min_answer_chars: int | None = None


# ---------------------------------------------------------------------------
# HTTP helpers (stdlib only — no third-party deps beyond `requests` fallback)
# ---------------------------------------------------------------------------

def _headers(auth_headers: dict[str, str]) -> dict[str, str]:
    headers = {
        "Content-Type": "application/json",
        "Accept": "application/json",
    }
    headers.update(auth_headers)
    return headers


def _cookie_auth_header(base_url: str, login: str, password: str) -> str:
    """Authenticate through the UI session endpoint and return a Cookie header."""
    jar = http.cookiejar.CookieJar()
    opener = urllib_request.build_opener(urllib_request.HTTPCookieProcessor(jar))
    url = base_url.rstrip("/") + "/v1/iam/session/login"
    data = json.dumps({"login": login, "password": password}).encode()
    req = urllib_request.Request(
        url,
        data=data,
        headers={"Content-Type": "application/json", "Accept": "application/json"},
        method="POST",
    )
    with opener.open(req, timeout=30) as resp:
        body = json.loads(resp.read().decode())
    if not body.get("sessionId"):
        raise RuntimeError("login response did not include sessionId")
    cookie_header = "; ".join(f"{cookie.name}={cookie.value}" for cookie in jar)
    if not cookie_header:
        raise RuntimeError("login response did not set a session cookie")
    return cookie_header


def resolve_auth_headers(base_url: str, login: str) -> dict[str, str]:
    """Resolve benchmark auth headers from bearer token or cookie-session login."""
    token = os.environ.get("IRONRAG_API_TOKEN", "")
    if token:
        return {"Authorization": f"Bearer {token}"}

    password = os.environ.get("IRONRAG_PROBE_PASSWORD", "")
    if not password:
        raise RuntimeError("IRONRAG_API_TOKEN or IRONRAG_PROBE_PASSWORD is required")
    return {"Cookie": _cookie_auth_header(base_url, login, password)}


def _resolve_http_timeout_seconds(default: int) -> int:
    """Read `IRONRAG_HTTP_TIMEOUT` (seconds) with a sensible default.

    The canonical SLO sits at full-turn p95 ≤ 90 s, so the default budget
    has to cover a full agent turn (multiple tool-calls + planning + verify)
    with margin. Set the env var explicitly when running concurrency
    sweeps where head-of-line blocking may temporarily push individual
    request wallclocks beyond the typical p95.
    """
    raw = os.environ.get("IRONRAG_HTTP_TIMEOUT", "").strip()
    if not raw:
        return default
    try:
        value = int(raw)
        return value if value > 0 else default
    except ValueError:
        return default


def _post(
    base_url: str,
    path: str,
    auth_headers: dict[str, str],
    body: dict[str, Any],
) -> dict[str, Any]:
    """Perform a POST request; raise network or JSON errors on failure."""
    url = base_url.rstrip("/") + path
    data = json.dumps(body).encode()
    req = urllib_request.Request(url, data=data, headers=_headers(auth_headers), method="POST")
    timeout_seconds = _resolve_http_timeout_seconds(default=180)
    with urllib_request.urlopen(req, timeout=timeout_seconds) as resp:
        return json.loads(resp.read().decode())


# ---------------------------------------------------------------------------
# Core benchmark logic
# ---------------------------------------------------------------------------

def load_questions(path: str) -> list[BenchmarkQuestion]:
    """Load plain text questions or JSON case specs."""
    if pathlib.Path(path).suffix == ".json":
        payload = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
        raw_cases = payload.get("cases") if isinstance(payload, dict) else payload
        if not isinstance(raw_cases, list):
            raise ValueError("JSON question file must be a list or define a cases list")
        return [parse_question_case(case) for case in raw_cases]

    questions: list[BenchmarkQuestion] = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            stripped = line.strip()
            if stripped and not stripped.startswith("#"):
                questions.append(BenchmarkQuestion(question=stripped))
    return questions


def parse_question_case(case: Any) -> BenchmarkQuestion:
    if isinstance(case, str):
        return BenchmarkQuestion(question=case)
    if not isinstance(case, dict):
        raise ValueError("JSON question cases must be strings or objects")
    question = case.get("question")
    if not isinstance(question, str) or not question.strip():
        raise ValueError("JSON question case missing non-empty question")
    expected_verification = case.get("expectedVerification", case.get("assistantExpectedVerification"))
    if expected_verification is not None and not isinstance(expected_verification, str):
        raise ValueError("expectedVerification must be a string when provided")
    case_id = case.get("id")
    if case_id is not None and not isinstance(case_id, str):
        raise ValueError("id must be a string when provided")
    return BenchmarkQuestion(
        question=question.strip(),
        case_id=case_id or None,
        require_all=tuple(read_expectation_values(case, "requireAll", "assistantRequireAll")),
        forbid_any=tuple(read_expectation_values(case, "forbidAny", "assistantForbidAny")),
        expected_verification=expected_verification or None,
        min_references=read_optional_nonnegative_int(case, "minReferences", "assistantMinReferences"),
        min_answer_chars=read_optional_nonnegative_int(case, "minAnswerChars", "assistantMinAnswerChars"),
    )


def read_expectation_values(case: dict[str, Any], *keys: str) -> list[str]:
    for key in keys:
        value = case.get(key)
        if value is None:
            continue
        if isinstance(value, str):
            return split_expectation_list(value)
        if isinstance(value, list):
            expectations: list[str] = []
            for item in value:
                if not isinstance(item, str):
                    raise ValueError(f"{key} entries must be strings")
                stripped = item.strip()
                if stripped:
                    expectations.append(stripped)
            return expectations
        raise ValueError(f"{key} must be a string or list")
    return []


def read_optional_nonnegative_int(case: dict[str, Any], *keys: str) -> int | None:
    for key in keys:
        value = case.get(key)
        if value is None:
            continue
        if not isinstance(value, int) or isinstance(value, bool):
            raise ValueError(f"{key} must be an integer")
        if value < 0:
            raise ValueError(f"{key} must be non-negative")
        return value
    return None


def summarize_turn_quality(payload: dict[str, Any]) -> TurnQuality:
    response_turn = payload.get("responseTurn")
    response_turn = response_turn if isinstance(response_turn, dict) else {}
    answer_text = response_turn.get("contentText")
    answer_chars = len(answer_text.strip()) if isinstance(answer_text, str) else 0
    verification_state = payload.get("verificationState")
    reference_count = 0
    for key in (
        "chunkReferences",
        "preparedSegmentReferences",
        "technicalFactReferences",
        "entityReferences",
        "relationReferences",
    ):
        references = payload.get(key)
        if isinstance(references, list):
            reference_count += len(references)
    return TurnQuality(
        answer_chars=answer_chars,
        verification_state=verification_state if isinstance(verification_state, str) else None,
        reference_count=reference_count,
    )


def split_expectation_list(value: str | None) -> list[str]:
    if not value:
        return []
    return [part.strip() for part in value.split(",") if part.strip()]


def validate_turn_quality(
    payload: dict[str, Any],
    *,
    expected_verification: str | None,
    min_references: int,
    min_answer_chars: int,
    require_all: list[str],
    forbid_any: list[str],
) -> tuple[TurnQuality, str | None]:
    quality = summarize_turn_quality(payload)
    response_turn = payload.get("responseTurn")
    response_turn = response_turn if isinstance(response_turn, dict) else {}
    answer_text = response_turn.get("contentText")
    answer_text = answer_text if isinstance(answer_text, str) else ""
    folded_answer = answer_text.casefold()
    missing_required = tuple(term for term in require_all if term.casefold() not in folded_answer)
    forbidden_found = tuple(term for term in forbid_any if term.casefold() in folded_answer)
    quality = TurnQuality(
        answer_chars=quality.answer_chars,
        verification_state=quality.verification_state,
        reference_count=quality.reference_count,
        missing_required=missing_required,
        forbidden_found=forbidden_found,
    )
    failures: list[str] = []
    if quality.answer_chars < min_answer_chars:
        failures.append(f"answer_chars={quality.answer_chars}<min={min_answer_chars}")
    if expected_verification and quality.verification_state != expected_verification:
        failures.append(
            f"verification={quality.verification_state!r} expected={expected_verification!r}"
        )
    if quality.reference_count < min_references:
        failures.append(f"references={quality.reference_count}<min={min_references}")
    if missing_required:
        failures.append(f"missing_required={list(missing_required)!r}")
    if forbidden_found:
        failures.append(f"forbidden_found={list(forbidden_found)!r}")
    return quality, "; ".join(failures) if failures else None


def run_turn(
    base_url: str,
    auth_headers: dict[str, str],
    library_id: str,
    question: str,
    top_k: int | None,
    expected_verification: str | None,
    min_references: int,
    min_answer_chars: int,
    require_all: list[str],
    forbid_any: list[str],
) -> tuple[float, TurnQuality | None, str | None]:
    """
    Execute one complete turn (session creation + turn POST).

    Returns (elapsed_ms, quality_or_None, error_message_or_None).
    """
    t_start = time.perf_counter()
    try:
        session_body: dict[str, Any] = {"libraryId": library_id}
        session = _post(base_url, "/v1/query/sessions", auth_headers, session_body)
        session_id = session["id"]

        turn_body: dict[str, Any] = {"contentText": question}
        if top_k is not None:
            turn_body["topK"] = top_k

        turn = _post(base_url, f"/v1/query/sessions/{session_id}/turns", auth_headers, turn_body)
        quality, quality_error = validate_turn_quality(
            turn,
            expected_verification=expected_verification,
            min_references=min_references,
            min_answer_chars=min_answer_chars,
            require_all=require_all,
            forbid_any=forbid_any,
        )
    except REQUEST_ERROR_TYPES as exc:
        elapsed_ms = (time.perf_counter() - t_start) * 1000.0
        return elapsed_ms, None, str(exc)

    elapsed_ms = (time.perf_counter() - t_start) * 1000.0
    return elapsed_ms, quality, quality_error


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
    auth_headers: dict[str, str],
    library_id: str,
    questions: list[BenchmarkQuestion],
    concurrency: int,
    top_k: int | None,
    expected_verification: str | None,
    min_references: int,
    min_answer_chars: int,
    require_all: list[str],
    forbid_any: list[str],
) -> dict[str, Any]:
    """
    Run all questions (optionally in parallel) and return a results dict.
    """
    concurrency = normalize_concurrency(concurrency)
    per_question: list[dict[str, Any]] = []
    latencies_ms: list[float] = []
    failure_count = 0

    def _task(spec: BenchmarkQuestion) -> tuple[BenchmarkQuestion, float, TurnQuality | None, str | None]:
        spec_expected_verification = (
            spec.expected_verification
            if spec.expected_verification is not None
            else expected_verification
        )
        spec_min_references = (
            max(min_references, spec.min_references)
            if spec.min_references is not None
            else min_references
        )
        spec_min_answer_chars = (
            max(min_answer_chars, spec.min_answer_chars)
            if spec.min_answer_chars is not None
            else min_answer_chars
        )
        elapsed_ms, quality, err = run_turn(
            base_url,
            auth_headers,
            library_id,
            spec.question,
            top_k,
            spec_expected_verification,
            spec_min_references,
            spec_min_answer_chars,
            [*require_all, *spec.require_all],
            [*forbid_any, *spec.forbid_any],
        )
        return spec, elapsed_ms, quality, err

    with ThreadPoolExecutor(max_workers=concurrency) as executor:
        futures = {executor.submit(_task, spec): spec for spec in questions}
        for future in as_completed(futures):
            spec, elapsed_ms, quality, err = future.result()
            entry: dict[str, Any] = {
                "question": spec.question,
                "elapsed_ms": round(elapsed_ms, 1),
            }
            if spec.case_id:
                entry["case_id"] = spec.case_id
            case_label = f"{spec.case_id}: " if spec.case_id else ""
            if quality is not None:
                entry["answer_chars"] = quality.answer_chars
                entry["verification_state"] = quality.verification_state
                entry["reference_count"] = quality.reference_count
                if quality.missing_required:
                    entry["missing_required"] = list(quality.missing_required)
                if quality.forbidden_found:
                    entry["forbidden_found"] = list(quality.forbidden_found)
            if err:
                entry["error"] = err
                failure_count += 1
                print(
                    f"  FAIL  [{elapsed_ms:8.0f} ms]  {case_label}{spec.question[:60]}  — {err}",
                    file=sys.stderr,
                )
            else:
                latencies_ms.append(elapsed_ms)
                print(
                    f"  OK    [{elapsed_ms:8.0f} ms]  refs={quality.reference_count if quality else 0:>3}  {case_label}{spec.question[:60]}",
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


def normalize_concurrency(value: int) -> int:
    """Clamp concurrency to the minimum valid ThreadPoolExecutor worker count."""
    return max(1, value)


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


def write_summary_json(result: dict[str, Any], output_path: str | None) -> str:
    """Serialize the benchmark summary and optionally persist it as an artifact."""
    summary_json = json.dumps(result)
    if output_path:
        path = pathlib.Path(output_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"{summary_json}\n", encoding="utf-8")
    return summary_json


def add_gate_summary(result: dict[str, Any], p95_gate_ms: int) -> dict[str, Any]:
    """Attach explicit release-gate metadata to a benchmark result."""
    result = dict(result)
    p95_ms = result.get("p95_ms")
    successes = result.get("successes")
    failures = result.get("failures")
    result["p95_gate_ms"] = p95_gate_ms
    result["gate_passed"] = (
        isinstance(p95_ms, (int, float))
        and not isinstance(p95_ms, bool)
        and isinstance(successes, int)
        and not isinstance(successes, bool)
        and successes > 0
        and isinstance(failures, int)
        and not isinstance(failures, bool)
        and failures == 0
        and p95_ms <= p95_gate_ms
    )
    return result


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def non_negative_int_arg(value: str) -> int:
    """Parse a CLI integer that must not silently disable a quality gate."""
    try:
        parsed = int(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("must be an integer") from exc
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be non-negative")
    return parsed


def positive_int_arg(value: str) -> int:
    """Parse a CLI integer that must be safe to send as a retrieval limit."""
    parsed = non_negative_int_arg(value)
    if parsed == 0:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def is_uuid(value: str) -> bool:
    return bool(UUID_PATTERN.fullmatch(value))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="agent_turn_p95.py",
        description=(
            "Benchmark IronRAG UI assistant turn latency.\n"
            "Measures p50/p95/p99 across N questions; exits 1 if p95 > 28 000 ms.\n\n"
            "Required env vars: IRONRAG_LIBRARY_ID plus IRONRAG_API_TOKEN or "
            "IRONRAG_PROBE_PASSWORD\n"
            "Optional env vars: IRONRAG_API_BASE_URL, IRONRAG_LOGIN, "
            "IRONRAG_BENCH_QUESTIONS"
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
        type=positive_int_arg,
        default=None,
        metavar="N",
        help="Override top-K retrieval parameter sent to the API (default: API default)",
    )
    parser.add_argument(
        "--question-limit",
        type=positive_int_arg,
        default=None,
        metavar="N",
        help="Run only the first N questions (default: all)",
    )
    parser.add_argument(
        "--expected-verification",
        default=DEFAULT_EXPECTED_VERIFICATION,
        help=(
            "Required verificationState for each successful turn. Pass an empty "
            "string to disable this gate (default: disabled)."
        ),
    )
    parser.add_argument(
        "--min-references",
        type=non_negative_int_arg,
        default=DEFAULT_MIN_REFERENCES,
        metavar="N",
        help="Minimum grounding references required per successful turn (default: 1)",
    )
    parser.add_argument(
        "--min-answer-chars",
        type=non_negative_int_arg,
        default=DEFAULT_MIN_ANSWER_CHARS,
        metavar="N",
        help="Minimum non-whitespace answer length required per successful turn (default: 1)",
    )
    parser.add_argument(
        "--require-all",
        default="",
        help="Comma-separated literals that every successful answer must include",
    )
    parser.add_argument(
        "--forbid-any",
        default="",
        help="Comma-separated literals that every successful answer must not include",
    )
    parser.add_argument(
        "--p95-gate-ms",
        type=non_negative_int_arg,
        default=P95_GATE_MS,
        metavar="N",
        help=f"p95 latency gate in milliseconds (default: {P95_GATE_MS})",
    )
    parser.add_argument(
        "--output-path",
        default=None,
        metavar="PATH",
        help="Optional path to write the JSON benchmark summary",
    )
    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    base_url = os.environ.get("IRONRAG_API_BASE_URL", DEFAULT_BASE_URL).rstrip("/")
    login = os.environ.get("IRONRAG_LOGIN", "admin")
    library_id = os.environ.get("IRONRAG_LIBRARY_ID", "")
    questions_path = os.environ.get("IRONRAG_BENCH_QUESTIONS", DEFAULT_QUESTIONS_PATH)

    errors: list[str] = []
    if not library_id:
        errors.append("IRONRAG_LIBRARY_ID is not set")
    elif not is_uuid(library_id):
        errors.append("IRONRAG_LIBRARY_ID must be a UUID")
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
    except ValueError as exc:
        print(f"ERROR: invalid question file {questions_path}: {exc}", file=sys.stderr)
        return 1

    if not questions:
        print("ERROR: no questions loaded from question file", file=sys.stderr)
        return 1

    if args.question_limit is not None:
        questions = questions[: args.question_limit]

    try:
        auth_headers = resolve_auth_headers(base_url, login)
    except AUTH_ERROR_TYPES as exc:
        print(f"ERROR: failed to authenticate benchmark client: {exc}", file=sys.stderr)
        parser.print_usage(sys.stderr)
        return 1

    concurrency = normalize_concurrency(args.concurrency)
    print(
        f"Running {len(questions)} questions  concurrency={concurrency}"
        f"  base_url={base_url}  library_id={library_id}",
        file=sys.stderr,
    )
    print("", file=sys.stderr)

    result = run_benchmark(
        base_url=base_url,
        auth_headers=auth_headers,
        library_id=library_id,
        questions=questions,
        concurrency=concurrency,
        top_k=args.top_k,
        expected_verification=args.expected_verification or None,
        min_references=args.min_references,
        min_answer_chars=args.min_answer_chars,
        require_all=split_expectation_list(args.require_all),
        forbid_any=split_expectation_list(args.forbid_any),
    )

    p95_gate_ms = args.p95_gate_ms
    result = add_gate_summary(result, p95_gate_ms)
    print_summary_table(result, p95_gate_ms)

    summary_json = write_summary_json(result, args.output_path)
    # Write JSON summary to stdout for callers that pipe the final line.
    print(summary_json)

    return 0 if result["gate_passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
