#!/usr/bin/env python3
"""
Benchmark: UI assistant turn p95 latency gate.

Hits POST /v1/query/libraries/{libraryId}/sessions for session creation, then
POST /v1/query/sessions/{id}/turns for each question and reports p50/p95/p99
latency. A turn only counts as successful when the response has assistant
text, meets any configured verifier gate, and has the configured minimum
grounding references.

Environment variables
---------------------
IRONRAG_API_BASE_URL   Base URL of the API (default: http://localhost:19000)
IRONRAG_API_TOKEN      Bearer token (preferred when set)
IRONRAG_LOGIN          Login for cookie-session auth (default: admin)
IRONRAG_PROBE_PASSWORD Password for cookie-session auth when no bearer token is set
IRONRAG_LIBRARY_ID     Library UUID to query against (required)
IRONRAG_BENCH_QUESTIONS
                       Path to a JSON file with a top-level `cases` array
                       (default: scripts/bench/grounded-queries.json)

Exit codes
----------
0  p95 latency is within the configured gate (default hard SLO: ≤ 90 000 ms)
1  p95 latency breaches the gate, a quality gate fails, or required env vars
   are missing

SLO model: 90 s is the hard full-turn availability ceiling. Release and canary
workflows intentionally pass stricter 30 s and 25 s targets respectively. The
relative baseline/candidate gate remains mandatory even when the hard SLO passes.
"""

from __future__ import annotations

import argparse
import hashlib
import http.cookiejar
import json
import math
import os
import pathlib
import re
import secrets
import sys
import threading
import time
import uuid
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any
from urllib import error as urllib_error
from urllib import request as urllib_request


REPOSITORY_ROOT = pathlib.Path(__file__).resolve().parents[2]
if str(REPOSITORY_ROOT) not in sys.path:
    sys.path.insert(0, str(REPOSITORY_ROOT))

from apps.api.benchmarks.lifecycle_state import canonical_turn_payload, load_collector_document
from apps.api.benchmarks.tls_pinning import pinned_urllib_opener

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

DEFAULT_BASE_URL = "http://localhost:19000"
DEFAULT_QUESTIONS_PATH = "scripts/bench/grounded-queries.json"

# Hard full-turn availability SLO. Release/canary commands override this with
# stricter targets; keeping the hard ceiling here prevents silent conflicts
# between the generic tool and environment-specific rollout policy.
P95_GATE_MS = 90_000
MIN_SUSTAINED_CONCURRENCY_RATIO = 0.8
EXPERIMENT_SCHEMA_VERSION = 1
EXPERIMENT_ROLES = ("baseline", "candidate")
SEQUENCE_POSITIONS = ("A", "B")
DEFAULT_EXPECTED_VERIFICATION: str | None = None
DEFAULT_MIN_REFERENCES = 1
DEFAULT_MIN_ANSWER_CHARS = 1
JSON_MEDIA_TYPE = "application/json"


class NoRedirectHandler(urllib_request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: ANN001
        return None
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
    ValueError,
)
AUTH_ERROR_TYPES = (RuntimeError, *REQUEST_ERROR_TYPES)


@dataclass(frozen=True)
class TurnQuality:
    answer_chars: int
    verification_state: str | None
    disposition: str | None
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


@dataclass(frozen=True)
class HttpJsonResponse:
    payload: dict[str, Any]
    status: int
    request_id: str | None
    trace_id: str | None


@dataclass(frozen=True)
class TurnExecution:
    total_ms: float
    session_create_ms: float
    turn_ms: float | None
    quality: TurnQuality | None
    failure_category: str | None
    error: str | None
    response_status: int | None
    request_id: str | None
    trace_id: str | None
    execution_id: str | None
    raw_response: dict[str, Any] | None = None
    quality_policy: dict[str, Any] | None = None


@dataclass(frozen=True)
class ScheduledQuestion:
    spec: BenchmarkQuestion
    repetition: int
    sample_id: str
    schedule_ordinal: int
    session_id: str | None = None


@dataclass(frozen=True)
class TurnExpectations:
    top_k: int | None
    expected_verification: str | None
    min_references: int
    min_answer_chars: int
    require_all: list[str]
    forbid_any: list[str]


def stable_json_sha256(payload: Any) -> str:
    encoded = json.dumps(
        payload,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


# ---------------------------------------------------------------------------
# HTTP helpers (stdlib only — no third-party deps beyond `requests` fallback)
# ---------------------------------------------------------------------------

def _headers(
    auth_headers: dict[str, str],
    *,
    request_id: str,
    traceparent: str,
) -> dict[str, str]:
    headers = {
        "Content-Type": JSON_MEDIA_TYPE,
        "Accept": JSON_MEDIA_TYPE,
        "x-request-id": request_id,
        "traceparent": traceparent,
    }
    headers.update(auth_headers)
    return headers


def _cookie_auth_header(base_url: str, login: str, password: str) -> str:
    """Authenticate through the UI session endpoint and return a Cookie header."""
    jar = http.cookiejar.CookieJar()
    opener = pinned_urllib_opener(
        os.environ.get("IRONRAG_API_TLS_CERTIFICATE_SHA256", ""),
        urllib_request.HTTPCookieProcessor(jar),
        NoRedirectHandler(),
    )
    url = api_url(base_url, "/v1/iam/sessions")
    data = json.dumps({"login": login, "password": password}).encode()
    req = urllib_request.Request(
        url,
        data=data,
        headers={"Content-Type": JSON_MEDIA_TYPE, "Accept": JSON_MEDIA_TYPE},
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

    The defined SLO sits at full-turn p95 ≤ 90 s, so the default budget
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


def api_url(base_url: str, path: str) -> str:
    """Join an API-root URL and path without ever duplicating the /v1 prefix."""
    root = base_url.rstrip("/")
    normalized_path = "/" + path.lstrip("/")
    if root.endswith("/v1") and normalized_path.startswith("/v1/"):
        normalized_path = normalized_path.removeprefix("/v1")
    return root + normalized_path


def _post(
    base_url: str,
    path: str,
    auth_headers: dict[str, str],
    body: dict[str, Any],
) -> HttpJsonResponse:
    """Perform a POST request; raise network or JSON errors on failure."""
    url = api_url(base_url, path)
    data = json.dumps(body).encode()
    request_id = str(uuid.uuid4())
    trace_id = secrets.token_hex(16)
    traceparent = f"00-{trace_id}-{secrets.token_hex(8)}-01"
    req = urllib_request.Request(
        url,
        data=data,
        headers=_headers(
            auth_headers,
            request_id=request_id,
            traceparent=traceparent,
        ),
        method="POST",
    )
    timeout_seconds = _resolve_http_timeout_seconds(default=180)
    with pinned_urllib_opener(
        os.environ.get("IRONRAG_API_TLS_CERTIFICATE_SHA256", ""), NoRedirectHandler()
    ).open(
        req, timeout=timeout_seconds
    ) as resp:
        payload = json.loads(resp.read().decode())
        if not isinstance(payload, dict):
            raise ValueError("response JSON must be an object")
        return HttpJsonResponse(
            payload=payload,
            status=resp.status,
            request_id=request_id,
            trace_id=trace_id,
        )


def _get(
    base_url: str,
    path: str,
    auth_headers: dict[str, str],
) -> HttpJsonResponse:
    request_id = str(uuid.uuid4())
    trace_id = secrets.token_hex(16)
    traceparent = f"00-{trace_id}-{secrets.token_hex(8)}-01"
    req = urllib_request.Request(
        api_url(base_url, path),
        headers=_headers(
            auth_headers,
            request_id=request_id,
            traceparent=traceparent,
        ),
        method="GET",
    )
    with pinned_urllib_opener(
        os.environ.get("IRONRAG_API_TLS_CERTIFICATE_SHA256", ""), NoRedirectHandler()
    ).open(
        req, timeout=_resolve_http_timeout_seconds(default=30)
    ) as resp:
        payload = json.loads(resp.read().decode())
        if not isinstance(payload, dict):
            raise ValueError("response JSON must be an object")
        return HttpJsonResponse(
            payload=payload,
            status=resp.status,
            request_id=request_id,
            trace_id=trace_id,
        )


# ---------------------------------------------------------------------------
# Core benchmark logic
# ---------------------------------------------------------------------------

def load_questions(path: str) -> list[BenchmarkQuestion]:
    """Load benchmark cases from the defined JSON `cases` envelope."""
    if pathlib.Path(path).suffix != ".json":
        raise ValueError("question file must be a JSON file with a cases envelope")
    payload = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
    if not isinstance(payload, dict) or not isinstance(payload.get("cases"), list):
        raise ValueError("JSON question file must be an object with a cases list")
    return [parse_question_case(case) for case in payload["cases"]]


ALLOWED_CASE_FIELDS = frozenset(
    {
        "id",
        "question",
        "assistantRequireAll",
        "assistantForbidAny",
        "assistantExpectedVerification",
        "assistantMinReferences",
        "assistantMinAnswerChars",
    }
)


def parse_question_case(case: Any) -> BenchmarkQuestion:
    if not isinstance(case, dict):
        raise ValueError("JSON question cases must be objects")
    unknown_fields = sorted(set(case) - ALLOWED_CASE_FIELDS)
    if unknown_fields:
        raise ValueError(f"JSON question case has unknown fields: {', '.join(unknown_fields)}")
    question = case.get("question")
    if not isinstance(question, str) or not question.strip():
        raise ValueError("JSON question case missing non-empty question")
    expected_verification = case.get("assistantExpectedVerification")
    if expected_verification is not None and not isinstance(expected_verification, str):
        raise ValueError("expectedVerification must be a string when provided")
    case_id = case.get("id")
    if case_id is not None and not isinstance(case_id, str):
        raise ValueError("id must be a string when provided")
    return BenchmarkQuestion(
        question=question.strip(),
        case_id=case_id or None,
        require_all=tuple(read_expectation_values(case, "assistantRequireAll")),
        forbid_any=tuple(read_expectation_values(case, "assistantForbidAny")),
        expected_verification=expected_verification or None,
        min_references=read_optional_nonnegative_int(case, "assistantMinReferences"),
        min_answer_chars=read_optional_nonnegative_int(case, "assistantMinAnswerChars"),
    )


def _expectation_values(value: Any, key: str) -> list[str]:
    if isinstance(value, str):
        return split_expectation_list(value)
    if isinstance(value, list):
        expectations = []
        for item in value:
            if not isinstance(item, str):
                raise ValueError(f"{key} entries must be strings")
            stripped = item.strip()
            if stripped:
                expectations.append(stripped)
        return expectations
    raise ValueError(f"{key} must be a string or list")


def read_expectation_values(case: dict[str, Any], key: str) -> list[str]:
    value = case.get(key)
    return _expectation_values(value, key) if value is not None else []


def read_optional_nonnegative_int(case: dict[str, Any], key: str) -> int | None:
    value = case.get(key)
    if value is None:
        return None
    if not isinstance(value, int) or isinstance(value, bool):
        raise ValueError(f"{key} must be an integer")
    if value < 0:
        raise ValueError(f"{key} must be non-negative")
    return value


def summarize_turn_quality(
    payload: dict[str, Any], *, question: str | None, library_id: str | None,
    workspace_id: str | None
) -> TurnQuality:
    canonical = canonical_turn_payload(
        payload,
        expected_question=question,
        expected_library_id=library_id,
        expected_workspace_id=workspace_id,
    )
    answer_text = canonical["answerText"]
    return TurnQuality(
        answer_chars=len(answer_text.strip()),
        verification_state=canonical["verificationState"],
        disposition=canonical["answerDisposition"],
        reference_count=canonical["referenceCount"],
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
    question: str | None = None,
    library_id: str | None = None,
    workspace_id: str | None = None,
) -> tuple[TurnQuality, str | None]:
    quality = summarize_turn_quality(
        payload, question=question, library_id=library_id, workspace_id=workspace_id
    )
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
        disposition=quality.disposition,
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
    # A raw verificationState of "not_run" is not itself a failure: ordinary
    # prose under Moderate/Lenient verification_level() has no typed semantic
    # verification result and is still FactualReady. Only answerDisposition,
    # the policy's own terminal-readiness contract (verification_policy.rs),
    # says whether the turn produced a usable answer.
    if quality.disposition == "non_terminal":
        failures.append(f"disposition={quality.disposition!r} is non_terminal")
    if quality.reference_count < min_references:
        failures.append(f"references={quality.reference_count}<min={min_references}")
    if missing_required:
        failures.append(f"missing_required={list(missing_required)!r}")
    if forbidden_found:
        failures.append(f"forbidden_found={list(forbidden_found)!r}")
    return quality, "; ".join(failures) if failures else None


def failure_category(error: BaseException) -> str:
    if isinstance(error, urllib_error.HTTPError):
        if error.code == 429:
            return "http_429"
        if error.code >= 500:
            return "http_5xx"
        return "http_4xx"
    if isinstance(error, (TimeoutError, urllib_error.URLError)) and "timed out" in str(
        error
    ).casefold():
        return "timeout"
    if isinstance(error, json.JSONDecodeError):
        return "decode"
    if isinstance(error, (KeyError, ValueError)):
        return "schema"
    return "transport"


def run_turn(
    base_url: str,
    auth_headers: dict[str, str],
    library_id: str,
    question: str,
    expectations: TurnExpectations,
    workspace_id: str | None = None,
    session_id: str | None = None,
) -> TurnExecution:
    """
    Execute one complete turn (session creation + turn POST).

    Returns one typed sample with separately measured session and turn latency.
    """
    t_start = time.perf_counter()
    session_create_ms = 0.0
    try:
        if session_id is None:
            session = _post(
                base_url,
                f"/v1/query/libraries/{library_id}/sessions",
                auth_headers,
                {},
            )
            session_create_ms = (time.perf_counter() - t_start) * 1000.0
            session_id = session.payload["id"]

        turn_body: dict[str, Any] = {"contentText": question}
        if expectations.top_k is not None:
            turn_body["topK"] = expectations.top_k

        turn_started = time.perf_counter()
        turn = _post(base_url, f"/v1/query/sessions/{session_id}/turns", auth_headers, turn_body)
        turn_ms = (time.perf_counter() - turn_started) * 1000.0
        quality, quality_error = validate_turn_quality(
            turn.payload,
            expected_verification=expectations.expected_verification,
            min_references=expectations.min_references,
            min_answer_chars=expectations.min_answer_chars,
            require_all=expectations.require_all,
            forbid_any=expectations.forbid_any,
            question=question,
            library_id=library_id,
            workspace_id=workspace_id,
        )
    except REQUEST_ERROR_TYPES as exc:
        return TurnExecution(
            total_ms=(time.perf_counter() - t_start) * 1000.0,
            session_create_ms=session_create_ms,
            turn_ms=None,
            quality=None,
            failure_category=failure_category(exc),
            error=str(exc),
            response_status=getattr(exc, "code", None),
            request_id=None,
            trace_id=None,
            execution_id=None,
        )

    return TurnExecution(
        total_ms=(time.perf_counter() - t_start) * 1000.0,
        session_create_ms=session_create_ms,
        turn_ms=turn_ms,
        quality=quality,
        failure_category="answer_quality" if quality_error else None,
        error=quality_error,
        response_status=turn.status,
        request_id=turn.request_id,
        trace_id=turn.trace_id,
        execution_id=canonical_turn_payload(
            turn.payload,
            expected_question=question,
            expected_library_id=library_id,
            expected_workspace_id=workspace_id,
        )["executionId"],
        raw_response=turn.payload,
        quality_policy={
            "expectedVerification": expectations.expected_verification,
            "minReferences": expectations.min_references,
            "minAnswerChars": expectations.min_answer_chars,
            "requireAll": expectations.require_all,
            "forbidAny": expectations.forbid_any,
        },
    )


def percentile(data: list[float], p: int) -> float:
    """Return the conservative nearest-rank percentile for a finite sample."""
    if not data:
        raise ValueError("percentile requires at least one sample")
    if p < 0 or p > 100:
        raise ValueError("percentile must be between 0 and 100")
    ordered = sorted(data)
    if p == 0:
        return ordered[0]
    rank = math.ceil((p / 100.0) * len(ordered))
    return ordered[rank - 1]


def _case_quality_thresholds(
    spec: BenchmarkQuestion,
    expected_verification: str | None,
    min_references: int,
    min_answer_chars: int,
) -> tuple[str | None, int, int]:
    return (
        spec.expected_verification
        if spec.expected_verification is not None
        else expected_verification,
        max(min_references, spec.min_references)
        if spec.min_references is not None
        else min_references,
        max(min_answer_chars, spec.min_answer_chars)
        if spec.min_answer_chars is not None
        else min_answer_chars,
    )


def _run_question(
    spec: BenchmarkQuestion,
    base_url: str,
    auth_headers: dict[str, str],
    library_id: str,
    expectations: TurnExpectations,
    workspace_id: str | None,
    session_id: str | None = None,
) -> tuple[BenchmarkQuestion, TurnExecution]:
    spec_expected_verification, spec_min_references, spec_min_answer_chars = _case_quality_thresholds(
        spec,
        expectations.expected_verification,
        expectations.min_references,
        expectations.min_answer_chars,
    )
    arguments = (
        base_url,
        auth_headers,
        library_id,
        spec.question,
        TurnExpectations(
            top_k=expectations.top_k,
            expected_verification=spec_expected_verification,
            min_references=spec_min_references,
            min_answer_chars=spec_min_answer_chars,
            require_all=[*expectations.require_all, *spec.require_all],
            forbid_any=[*expectations.forbid_any, *spec.forbid_any],
        ),
    )
    if workspace_id is not None:
        arguments = (*arguments, workspace_id)
    execution = (
        run_turn(*arguments, session_id=session_id)
        if session_id is not None
        else run_turn(*arguments)
    )
    return spec, execution


def _benchmark_entry(
    spec: BenchmarkQuestion,
    execution: TurnExecution,
    *,
    runtime_query_turn_max_concurrent: int | None = None,
) -> dict[str, Any]:
    entry: dict[str, Any] = {
        "question": spec.question,
        "elapsed_ms": round(execution.total_ms, 1),
        "session_create_ms": round(execution.session_create_ms, 1),
        "turn_ms": round(execution.turn_ms, 1) if execution.turn_ms is not None else None,
        "response_status": execution.response_status,
        "request_id": execution.request_id,
        "trace_id": execution.trace_id,
        "execution_id": execution.execution_id,
    }
    entry["queryTurnMaxConcurrent"] = runtime_query_turn_max_concurrent
    if spec.case_id:
        entry["case_id"] = spec.case_id
    if execution.quality is not None:
        quality = execution.quality
        entry.update(
            answer_chars=quality.answer_chars,
            verification_state=quality.verification_state,
            disposition=quality.disposition,
            reference_count=quality.reference_count,
        )
        if quality.missing_required:
            entry["missing_required"] = list(quality.missing_required)
        if quality.forbidden_found:
            entry["forbidden_found"] = list(quality.forbidden_found)
    if execution.raw_response is not None:
        entry["raw_response"] = execution.raw_response
    if execution.quality_policy is not None:
        entry["quality_policy"] = execution.quality_policy
    if execution.error:
        entry["failure_category"] = execution.failure_category
        entry["error"] = execution.error
    return entry


def _report_question_result(
    spec: BenchmarkQuestion,
    execution: TurnExecution,
) -> None:
    case_label = f"{spec.case_id}: " if spec.case_id else ""
    if execution.error:
        print(
            f"  FAIL  [{execution.total_ms:8.0f} ms]  {case_label}{spec.question[:60]}  — {execution.error}",
            file=sys.stderr,
        )
        return
    print(
        f"  OK    [{execution.total_ms:8.0f} ms]  refs={execution.quality.reference_count if execution.quality else 0:>3}  {case_label}{spec.question[:60]}",
        file=sys.stderr,
    )


def _empty_benchmark_result(questions: list[BenchmarkQuestion], failure_count: int, per_question: list[dict[str, Any]]) -> dict[str, Any]:
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


def _successful_benchmark_result(
    questions: list[BenchmarkQuestion], failure_count: int, per_question: list[dict[str, Any]], latencies_ms: list[float]
) -> dict[str, Any]:
    sorted_latencies = sorted(latencies_ms)
    return {
        "runs": len(questions),
        "successes": len(sorted_latencies),
        "failures": failure_count,
        "min_ms": round(sorted_latencies[0], 1),
        "p50_ms": round(percentile(sorted_latencies, 50), 1),
        "p95_ms": round(percentile(sorted_latencies, 95), 1),
        "p99_ms": round(percentile(sorted_latencies, 99), 1),
        "max_ms": round(sorted_latencies[-1], 1),
        "per_question": sorted(per_question, key=lambda entry: entry["elapsed_ms"], reverse=True),
    }


def run_benchmark(
    base_url: str,
    auth_headers: dict[str, str],
    library_id: str,
    questions: list[BenchmarkQuestion],
    concurrency: int,
    expectations: TurnExpectations,
    workspace_id: str | None = None,
) -> dict[str, Any]:
    """
    Run all questions (optionally in parallel) and return a results dict.
    """
    concurrency = normalize_concurrency(concurrency)
    per_question: list[dict[str, Any]] = []
    latencies_ms: list[float] = []
    failure_count = 0

    with ThreadPoolExecutor(max_workers=concurrency) as executor:
        futures = {
            executor.submit(
                _run_question,
                spec,
                base_url,
                auth_headers,
                library_id,
                expectations,
                workspace_id,
            ): spec
            for spec in questions
        }
        for future in as_completed(futures):
            spec, execution = future.result()
            entry = _benchmark_entry(spec, execution)
            _report_question_result(spec, execution)
            if execution.error:
                failure_count += 1
            else:
                latencies_ms.append(execution.total_ms)
            per_question.append(entry)

    if not latencies_ms:
        return _empty_benchmark_result(questions, failure_count, per_question)
    return _successful_benchmark_result(questions, failure_count, per_question, latencies_ms)


def normalize_concurrency(value: int) -> int:
    """Clamp concurrency to the minimum valid ThreadPoolExecutor worker count."""
    return max(1, value)


def question_definition(spec: BenchmarkQuestion) -> dict[str, Any]:
    return {
        "caseId": spec.case_id,
        "question": spec.question,
        "requireAll": list(spec.require_all),
        "forbidAny": list(spec.forbid_any),
        "expectedVerification": spec.expected_verification,
        "minReferences": spec.min_references,
        "minAnswerChars": spec.min_answer_chars,
    }


def build_expanded_schedule(
    questions: list[BenchmarkQuestion],
    repetitions: int,
    schedule_seed: str,
) -> tuple[list[ScheduledQuestion], str, str]:
    if repetitions <= 0:
        raise ValueError("repetitions must be positive")
    case_ids = [spec.case_id for spec in questions]
    if any(not case_id for case_id in case_ids):
        raise ValueError("experiment cases require non-empty ids")
    if len(case_ids) != len(set(case_ids)):
        raise ValueError("experiment case ids must be unique")
    question_set_sha256 = stable_json_sha256(
        [question_definition(spec) for spec in questions]
    )
    schedule_definitions = [
        (spec, repetition, f"{spec.case_id}#{repetition}")
        for repetition in range(1, repetitions + 1)
        for spec in questions
    ]
    schedule_definitions.sort(
        key=lambda item: hashlib.sha256(
            f"{schedule_seed}\0{item[2]}".encode("utf-8")
        ).digest()
    )
    schedule = [
        ScheduledQuestion(
            spec=spec,
            repetition=repetition,
            sample_id=sample_id,
            schedule_ordinal=ordinal,
        )
        for ordinal, (spec, repetition, sample_id) in enumerate(schedule_definitions)
    ]
    schedule_sha256 = stable_json_sha256([item.sample_id for item in schedule])
    return schedule, question_set_sha256, schedule_sha256


class ConcurrencyTimeline:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self.events: list[tuple[int, int]] = []
        self.active = 0
        self.maximum = 0

    def start(self, started_ns: int) -> None:
        with self._lock:
            if self.events:
                raise RuntimeError("concurrency timeline already started")
            self.events.append((started_ns, 0))

    def enter(self, observed_ns: int) -> None:
        with self._lock:
            self.active += 1
            self.maximum = max(self.maximum, self.active)
            self.events.append((observed_ns, self.active))

    def leave(self, observed_ns: int) -> None:
        with self._lock:
            self.active -= 1
            self.events.append((observed_ns, self.active))

    def close(self, completed_ns: int) -> None:
        with self._lock:
            if self.active != 0:
                raise RuntimeError("cannot close a concurrency timeline with active requests")
            self.events.append((completed_ns, 0))

    def ratio_at_target(self, started_ns: int, completed_ns: int, target: int) -> float:
        if completed_ns <= started_ns:
            return 0.0
        active = 0
        previous_ns = started_ns
        at_target_ns = 0
        for observed_at, next_active in sorted(self.events):
            bounded = min(max(observed_at, started_ns), completed_ns)
            if active >= target:
                at_target_ns += max(0, bounded - previous_ns)
            previous_ns = bounded
            active = next_active
        if active >= target:
            at_target_ns += max(0, completed_ns - previous_ns)
        return round(at_target_ns / (completed_ns - started_ns), 6)


def precreated_schedule(
    *,
    base_url: str,
    auth_headers: dict[str, str],
    library_id: str,
    schedule: list[ScheduledQuestion],
) -> list[ScheduledQuestion]:
    """Open one query session per scheduled turn before the run starts.

    Session creation is a request of its own; doing it inside the timed run
    would charge its latency to the turn under measurement.
    """
    prepared: list[ScheduledQuestion] = []
    for item in schedule:
        session = _post(
            base_url,
            f"/v1/query/libraries/{library_id}/sessions",
            auth_headers,
            {},
        )
        session_id = session.payload.get("id")
        if not isinstance(session_id, str) or not session_id:
            raise ValueError("session precreation returned no id")
        prepared.append(
            ScheduledQuestion(
                spec=item.spec,
                repetition=item.repetition,
                sample_id=item.sample_id,
                schedule_ordinal=item.schedule_ordinal,
                session_id=session_id,
            )
        )
    return prepared


def sustained_run_summary(
    *,
    samples: list[dict[str, Any]],
    timeline: "ConcurrencyTimeline",
    started_holder: dict[str, Any],
    completed_ns: int,
    completed_utc: str,
    concurrency: int,
) -> dict[str, Any]:
    """Reduce the collected samples to the artifact the experiment publishes."""
    started_ns = started_holder["monotonic_ns"]
    duration_ns = completed_ns - started_ns
    duration_seconds = duration_ns / 1_000_000_000.0
    samples.sort(key=lambda item: item["sample_id"])
    successful = [sample for sample in samples if "error" not in sample]
    failed = [sample for sample in samples if "error" in sample]
    failure_reason_counts: dict[str, int] = {}
    for sample in failed:
        category = str(sample.get("failure_category") or "unknown")
        failure_reason_counts[category] = failure_reason_counts.get(category, 0) + 1
    latencies = [float(sample["elapsed_ms"]) for sample in successful]
    latency_summary = {
        "sample_count": len(latencies),
        "p50_ms": round(percentile(latencies, 50), 1) if latencies else None,
        "p95_ms": round(percentile(latencies, 95), 1) if latencies else None,
        "p99_ms": round(percentile(latencies, 99), 1) if latencies else None,
    }
    return {
        "attempted": len(samples),
        "successful": len(successful),
        "failed": len(failed),
        "duration_seconds": round(duration_seconds, 6),
        "duration_ns": duration_ns,
        "started_at": started_holder["utc"],
        "completed_at": completed_utc,
        "attempted_throughput": round(len(samples) / duration_seconds, 6),
        "successful_throughput": round(len(successful) / duration_seconds, 6),
        "target_concurrency": concurrency,
        "observed_max_concurrency": timeline.maximum,
        "sustained_concurrency_ratio": timeline.ratio_at_target(
            started_ns, completed_ns, concurrency
        ),
        "failure_reason_counts": failure_reason_counts,
        "latency": latency_summary,
        "concurrency_events": [
            {
                "offset_ns": observed_at - started_ns,
                "offset_ms": round((observed_at - started_ns) / 1_000_000.0, 3),
                "active": active,
            }
            for observed_at, active in sorted(timeline.events)
        ],
        "samples": samples,
    }


def run_sustained_benchmark(
    *,
    base_url: str,
    auth_headers: dict[str, str],
    library_id: str,
    schedule: list[ScheduledQuestion],
    concurrency: int,
    expectations: TurnExpectations,
    workspace_id: str | None = None,
    barrier_timeout_seconds: float = 30.0,
    runtime_query_turn_max_concurrent: int | None = None,
) -> dict[str, Any]:
    concurrency = normalize_concurrency(concurrency)
    if len(schedule) < concurrency:
        raise ValueError("scheduled turns must be at least requested concurrency")
    if len(schedule) % concurrency != 0:
        raise ValueError("scheduled turns must divide evenly across workers")

    schedule = precreated_schedule(
        base_url=base_url,
        auth_headers=auth_headers,
        library_id=library_id,
        schedule=schedule,
    )

    worker_queues = [schedule[index::concurrency] for index in range(concurrency)]
    timeline = ConcurrencyTimeline()
    started_holder: dict[str, Any] = {}

    def mark_start() -> None:
        started_holder["monotonic_ns"] = time.perf_counter_ns()
        started_holder["utc"] = datetime.now(timezone.utc).isoformat()
        timeline.start(started_holder["monotonic_ns"])

    barrier = threading.Barrier(concurrency + 1, action=mark_start)

    def run_worker(items: list[ScheduledQuestion]) -> list[dict[str, Any]]:
        barrier.wait(timeout=barrier_timeout_seconds)
        rows: list[dict[str, Any]] = []
        for item in items:
            sample_started_ns = time.perf_counter_ns()
            timeline.enter(sample_started_ns)
            try:
                call_arguments = (
                    item.spec,
                    base_url,
                    auth_headers,
                    library_id,
                    expectations,
                )
                if workspace_id is not None:
                    call_arguments = (*call_arguments, workspace_id)
                execution = _run_question(
                    *call_arguments,
                    session_id=item.session_id,
                )[1]
            finally:
                sample_completed_ns = time.perf_counter_ns()
                timeline.leave(sample_completed_ns)
            entry = _benchmark_entry(
                item.spec,
                execution,
                runtime_query_turn_max_concurrent=runtime_query_turn_max_concurrent,
            )
            entry.update(
                sample_id=item.sample_id,
                schedule_ordinal=item.schedule_ordinal,
                repetition=item.repetition,
                started_offset_ns=(
                    sample_started_ns - started_holder["monotonic_ns"]
                ),
                completed_offset_ns=(
                    sample_completed_ns - started_holder["monotonic_ns"]
                ),
                started_offset_ms=round(
                    (sample_started_ns - started_holder["monotonic_ns"]) / 1_000_000.0,
                    3,
                ),
                completed_offset_ms=round(
                    (sample_completed_ns - started_holder["monotonic_ns"])
                    / 1_000_000.0,
                    3,
                ),
            )
            rows.append(entry)
        return rows

    with ThreadPoolExecutor(max_workers=concurrency) as executor:
        futures = [executor.submit(run_worker, items) for items in worker_queues]
        barrier.wait(timeout=barrier_timeout_seconds)
        samples = [sample for future in futures for sample in future.result()]
    completed_ns = time.perf_counter_ns()
    timeline.close(completed_ns)
    return sustained_run_summary(
        samples=samples,
        timeline=timeline,
        started_holder=started_holder,
        completed_ns=completed_ns,
        completed_utc=datetime.now(timezone.utc).isoformat(),
        concurrency=concurrency,
    )


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
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                handle.write(f"{summary_json}\n")
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, path)
        except BaseException:
            temporary.unlink(missing_ok=True)
            raise
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


def is_sha256(value: str | None) -> bool:
    if not isinstance(value, str):
        return False
    normalized = value.removeprefix("sha256:")
    return len(normalized) == 64 and all(character in "0123456789abcdef" for character in normalized)


def normalized_sha256(value: str) -> str:
    return value.removeprefix("sha256:")


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
    parser.add_argument("--experiment-id")
    parser.add_argument("--artifact-role", choices=EXPERIMENT_ROLES)
    parser.add_argument("--pair-id")
    parser.add_argument("--round-id")
    parser.add_argument("--sequence-position", choices=SEQUENCE_POSITIONS)
    parser.add_argument("--repetitions", type=positive_int_arg, default=1)
    parser.add_argument("--data-state-file", type=pathlib.Path)
    parser.add_argument("--workspace-id")
    return parser


def validate_experiment_args(args: argparse.Namespace) -> list[str]:
    if not args.experiment_id:
        return []
    invalid = [
        name
        for name in ("artifact_role", "pair_id", "round_id", "sequence_position")
        if not isinstance(getattr(args, name), str) or not getattr(args, name).strip()
    ]
    if args.top_k is None:
        invalid.append("top_k")
    if args.data_state_file is None:
        invalid.append("data_state_file")
    if not isinstance(args.workspace_id, str) or not is_uuid(args.workspace_id):
        invalid.append("workspace_id")
    if not args.output_path:
        invalid.append("output_path")
    return sorted(invalid)


def build_experiment_artifact(
    *,
    args: argparse.Namespace,
    questions_path: pathlib.Path,
    question_set_sha256: str,
    schedule_sha256: str,
    result: dict[str, Any],
    data_state_identity: dict[str, Any],
    runtime_version: dict[str, Any],
) -> dict[str, Any]:
    artifact = {
        "schemaVersion": EXPERIMENT_SCHEMA_VERSION,
        "experimentId": args.experiment_id,
        "artifactRole": args.artifact_role,
        "pairId": args.pair_id,
        "roundId": args.round_id,
        "sequencePosition": args.sequence_position,
        "workspaceId": args.workspace_id,
        "runtimeIdentity": {
            "artifactSha256": runtime_version.get("binarySha256"),
            "resolvedConfigSha256": runtime_version.get("resolvedConfigSha256"),
            "environmentSha256": stable_json_sha256(
                {
                    key: runtime_version.get(key)
                    for key in ("service", "environment", "role", "runtimeResourcesSha256")
                }
            ),
            "queryTurnMaxConcurrent": runtime_version.get("queryTurnMaxConcurrent"),
            "versionEndpoint": runtime_version,
        },
        "dataIdentity": data_state_identity,
        "workloadIdentity": {
            "sourceSha256": hashlib.sha256(questions_path.read_bytes()).hexdigest(),
            "effectiveQuestionSetSha256": question_set_sha256,
            "expandedScheduleSha256": schedule_sha256,
            "repetitions": args.repetitions,
            "scheduledTurns": result["attempted"],
            "targetConcurrency": result["target_concurrency"],
            "topK": args.top_k,
            "timeoutSeconds": _resolve_http_timeout_seconds(default=180),
            "retryCount": 0,
            "loadModel": "closed_loop_start_barrier",
        },
        "acceptancePolicy": {
            "p95GateMs": args.p95_gate_ms,
            "minSustainedConcurrencyRatio": MIN_SUSTAINED_CONCURRENCY_RATIO,
            "expectedVerification": getattr(args, "expected_verification", None),
            "minReferences": getattr(args, "min_references", DEFAULT_MIN_REFERENCES),
            "minAnswerChars": getattr(args, "min_answer_chars", DEFAULT_MIN_ANSWER_CHARS),
            "requireAll": split_expectation_list(getattr(args, "require_all", None)),
            "forbidAny": split_expectation_list(getattr(args, "forbid_any", None)),
        },
        "summary": {
            key: value
            for key, value in result.items()
            if key not in {"samples", "concurrency_events"}
        },
        "concurrencyEvents": result["concurrency_events"],
        "samples": result["samples"],
    }
    artifact["gatePassed"] = (
        artifact["summary"]["failed"] == 0
        and artifact["summary"]["observed_max_concurrency"]
        == artifact["summary"]["target_concurrency"]
        and artifact["summary"]["sustained_concurrency_ratio"]
        >= MIN_SUSTAINED_CONCURRENCY_RATIO
        and artifact["summary"]["latency"]["p95_ms"] is not None
        and artifact["summary"]["latency"]["p95_ms"] <= args.p95_gate_ms
    )
    artifact["artifactFingerprintSha256"] = stable_json_sha256(artifact)
    return artifact


RUNTIME_IDENTITY_FIELDS = (
    "service",
    "version",
    "environment",
    "role",
    "deploymentId",
    "binarySha256",
    "resolvedConfigSha256",
    "runtimeResourcesSha256",
    "redisEndpointSha256",
    "redisServiceSha256",
    "databaseTlsLeafSha256",
    "redisTlsLeafSha256",
)

RUNTIME_ATTESTATION_DIGESTS = (
    "binarySha256",
    "resolvedConfigSha256",
    "runtimeResourcesSha256",
    "redisEndpointSha256",
    "redisServiceSha256",
    "databaseTlsLeafSha256",
    "redisTlsLeafSha256",
)


def experiment_binding_error(
    *,
    runtime_version: dict[str, Any],
    data_state_identity: dict[str, Any],
    library_id: str,
    artifact_role: str,
) -> str | None:
    """Name the first reason this runtime may not carry the experiment."""
    if data_state_identity.get("role") != artifact_role:
        return "data-state role does not match --artifact-role"
    if data_state_identity.get("libraryId") != library_id:
        return "data-state libraryId does not match IRONRAG_LIBRARY_ID"
    state_deployment_id = data_state_identity["databaseIdentity"]["deploymentId"]
    if runtime_version.get("deploymentId") != state_deployment_id:
        return "runtime deploymentId does not match data-state collector"
    if any(
        not isinstance(runtime_version.get(key), str) or not runtime_version[key]
        for key in RUNTIME_IDENTITY_FIELDS
    ):
        return "runtime /v1/version identity is incomplete"
    if not all(
        is_sha256(runtime_version.get(key)) for key in RUNTIME_ATTESTATION_DIGESTS
    ):
        return "runtime attestation digests are invalid"
    resources_sha256 = stable_json_sha256(runtime_version.get("runtimeResources"))
    if resources_sha256 != runtime_version.get("runtimeResourcesSha256"):
        return "runtime resource attestation is stale"
    return None


def run_experiment_mode(
    *,
    args: argparse.Namespace,
    base_url: str,
    auth_headers: dict[str, str],
    library_id: str,
    questions: list[BenchmarkQuestion],
    questions_path: str,
    concurrency: int,
) -> int:
    """Run the pinned experiment variant, which publishes an artifact instead of a table."""
    try:
        _, data_state_identity = load_collector_document(args.data_state_file.resolve())
        runtime_version = _get(base_url, "/v1/version", auth_headers).payload
    except REQUEST_ERROR_TYPES as error:
        print(f"ERROR: cannot bind experiment to runtime/data state: {error}", file=sys.stderr)
        return 1
    binding_error = experiment_binding_error(
        runtime_version=runtime_version,
        data_state_identity=data_state_identity,
        library_id=library_id,
        artifact_role=args.artifact_role,
    )
    if binding_error is not None:
        print(f"ERROR: {binding_error}", file=sys.stderr)
        return 1
    schedule_seed = "\0".join(
        (args.experiment_id, args.pair_id, args.round_id, str(concurrency))
    )
    try:
        schedule, question_set_sha256, schedule_sha256 = build_expanded_schedule(
            questions,
            args.repetitions,
            schedule_seed,
        )
        experiment_result = run_sustained_benchmark(
            base_url=base_url,
            auth_headers=auth_headers,
            library_id=library_id,
            schedule=schedule,
            concurrency=concurrency,
            expectations=TurnExpectations(
                top_k=args.top_k,
                expected_verification=args.expected_verification or None,
                min_references=args.min_references,
                min_answer_chars=args.min_answer_chars,
                require_all=split_expectation_list(args.require_all),
                forbid_any=split_expectation_list(args.forbid_any),
            ),
            workspace_id=args.workspace_id,
            runtime_query_turn_max_concurrent=runtime_version.get(
                "queryTurnMaxConcurrent"
            ),
        )
    except (ValueError, threading.BrokenBarrierError) as error:
        print(f"ERROR: experiment did not start: {error}", file=sys.stderr)
        return 1
    artifact = build_experiment_artifact(
        args=args,
        questions_path=pathlib.Path(questions_path),
        question_set_sha256=question_set_sha256,
        schedule_sha256=schedule_sha256,
        result=experiment_result,
        data_state_identity=data_state_identity,
        runtime_version=runtime_version,
    )
    write_summary_json(artifact, args.output_path)
    print(f"wrote private experiment artifact: {args.output_path}", file=sys.stderr)
    return 0 if artifact["gatePassed"] else 1


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    base_url = os.environ.get("IRONRAG_API_BASE_URL", DEFAULT_BASE_URL).rstrip("/")
    login = os.environ.get("IRONRAG_LOGIN", "admin")
    library_id = os.environ.get("IRONRAG_LIBRARY_ID", "")
    questions_path = os.environ.get("IRONRAG_BENCH_QUESTIONS", DEFAULT_QUESTIONS_PATH)

    library_error = ""
    if not library_id:
        library_error = "IRONRAG_LIBRARY_ID is not set"
    elif not is_uuid(library_id):
        library_error = "IRONRAG_LIBRARY_ID must be a UUID"
    if library_error:
        print(f"ERROR: {library_error}", file=sys.stderr)
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

    invalid_experiment_args = validate_experiment_args(args)
    if invalid_experiment_args:
        print(
            "ERROR: invalid experiment arguments: " + ", ".join(invalid_experiment_args),
            file=sys.stderr,
        )
        return 1

    try:
        auth_headers = resolve_auth_headers(base_url, login)
    except AUTH_ERROR_TYPES as exc:
        print(f"ERROR: failed to authenticate benchmark client: {exc}", file=sys.stderr)
        parser.print_usage(sys.stderr)
        return 1

    concurrency = normalize_concurrency(args.concurrency)
    if args.experiment_id:
        return run_experiment_mode(
            args=args,
            base_url=base_url,
            auth_headers=auth_headers,
            library_id=library_id,
            questions=questions,
            questions_path=questions_path,
            concurrency=concurrency,
        )

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
        expectations=TurnExpectations(
            top_k=args.top_k,
            expected_verification=args.expected_verification or None,
            min_references=args.min_references,
            min_answer_chars=args.min_answer_chars,
            require_all=split_expectation_list(args.require_all),
            forbid_any=split_expectation_list(args.forbid_any),
        ),
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
