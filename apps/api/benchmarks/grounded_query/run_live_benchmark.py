#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import mimetypes
import os
import re
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import requests


DEFAULT_POLL_INTERVAL_SECONDS = 5.0
DEFAULT_WAIT_TIMEOUT_SECONDS = 900.0
DEFAULT_QUERY_TOP_K = 8
DEFAULT_SUITE_MATRIX = [
    "api_baseline_suite.json",
    "workflow_strict_suite.json",
    "layout_noise_suite.json",
    "graph_multihop_suite.json",
    "multiformat_surface_suite.json",
]


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def lower_text(value: str | None) -> str:
    return (value or "").casefold()


def canonical_match_text(value: str | None) -> str:
    normalized = lower_text(value)
    normalized = re.sub(r"[\u2010-\u2015\u2212]+", " ", normalized)
    normalized = re.sub(r"\s+", " ", normalized)
    return normalized.strip()


def contains_all(haystack: str | None, needles: list[str]) -> bool:
    normalized = canonical_match_text(haystack)
    return all(canonical_match_text(needle) in normalized for needle in needles)


def contains_any(haystack: str | None, needles: list[str]) -> bool:
    normalized = canonical_match_text(haystack)
    return any(canonical_match_text(needle) in normalized for needle in needles)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run live grounded QA benchmarks against a IronRAG deployment."
    )
    parser.add_argument(
        "--base-url",
        default=os.environ.get("IRONRAG_BENCHMARK_BASE_URL"),
        help="IronRAG API base URL including /v1.",
    )
    parser.add_argument(
        "--suite",
        action="append",
        help="Path to one benchmark suite JSON. Can be provided multiple times.",
    )
    parser.add_argument(
        "--workspace-id",
        default=os.environ.get("IRONRAG_BENCHMARK_WORKSPACE_ID"),
        help="Workspace UUID where the benchmark library should live.",
    )
    parser.add_argument(
        "--library-id",
        help="Reuse an existing library instead of creating a fresh one.",
    )
    parser.add_argument(
        "--library-name",
        help="Display name for a freshly created library.",
    )
    parser.add_argument(
        "--session-cookie",
        default=os.environ.get("IRONRAG_SESSION_COOKIE"),
        help="Value of ironrag_ui_session cookie.",
    )
    parser.add_argument(
        "--wait-timeout-seconds",
        type=float,
        default=DEFAULT_WAIT_TIMEOUT_SECONDS,
        help="Maximum time to wait for readiness / quiet pipeline.",
    )
    parser.add_argument(
        "--poll-interval-seconds",
        type=float,
        default=DEFAULT_POLL_INTERVAL_SECONDS,
        help="Polling interval for ops state.",
    )
    parser.add_argument(
        "--query-top-k",
        type=int,
        default=DEFAULT_QUERY_TOP_K,
        help="topK value for grounded answer requests.",
    )
    parser.add_argument(
        "--output",
        help="Optional path to write the final matrix JSON.",
    )
    parser.add_argument(
        "--output-dir",
        help="Optional directory to write one JSON file per suite plus matrix.result.json.",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Exit non-zero if any strict-blocking suite fails.",
    )
    parser.add_argument(
        "--skip-upload",
        action="store_true",
        help="Reuse an existing corpus in --library-id and skip document uploads.",
    )
    parser.add_argument(
        "--canonicalize-reused-library",
        action="store_true",
        help="Wait until a reused library becomes quiet and query-ready before benchmarking.",
    )
    parser.add_argument(
        "--upload-only",
        action="store_true",
        help="Create or reuse a library, upload corpus documents, wait for readiness, print summary JSON, and exit without running QA cases.",
    )
    return parser.parse_args()


@dataclass
class BenchmarkCase:
    case_id: str
    question: str
    search_query: str
    expected_documents_contains: list[str]
    search_required_all: list[str]
    answer_required_all: list[str]
    answer_required_any: list[str]
    answer_forbidden_any: list[str]
    min_chunk_reference_count: int
    min_prepared_segment_reference_count: int
    min_technical_fact_reference_count: int
    min_entity_reference_count: int
    min_relation_reference_count: int
    allowed_verification_states: list[str]


class BenchmarkClient:
    def __init__(self, base_url: str, session_cookie: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.http = requests.Session()
        self.http.cookies.set("ironrag_ui_session", session_cookie, path="/")

    def get_json(self, path: str, **kwargs: Any) -> Any:
        response = self.http.get(f"{self.base_url}{path}", timeout=120, **kwargs)
        response.raise_for_status()
        return response.json()

    def post_json(self, path: str, payload: dict[str, Any], **kwargs: Any) -> Any:
        response = self.http.post(
            f"{self.base_url}{path}",
            json=payload,
            timeout=300,
            **kwargs,
        )
        response.raise_for_status()
        return response.json()

    def post_multipart(self, path: str, fields: dict[str, str], file_path: Path) -> Any:
        mime_type = mimetypes.guess_type(file_path.name)[0] or "application/octet-stream"
        with file_path.open("rb") as handle:
            files = {"file": (file_path.name, handle, mime_type)}
            response = self.http.post(
                f"{self.base_url}{path}",
                data=fields,
                files=files,
                timeout=300,
            )
        response.raise_for_status()
        return response.json()


def load_suite(path: Path) -> tuple[list[Path], list[BenchmarkCase], dict[str, Any]]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    documents = []
    for item in payload["documents"]:
        candidate = Path(item)
        if not candidate.is_absolute():
            candidate = (path.parent / candidate).resolve()
        documents.append(candidate)

    cases = [
        BenchmarkCase(
            case_id=item["id"],
            question=item["question"],
            search_query=item.get("searchQuery", item["question"]),
            expected_documents_contains=item.get("expectedDocumentsContains", []),
            search_required_all=item.get("searchRequiredAll", []),
            answer_required_all=item.get("answerRequiredAll", []),
            answer_required_any=item.get("answerRequiredAny", []),
            answer_forbidden_any=item.get("answerForbiddenAny", []),
            min_chunk_reference_count=item.get("minChunkReferenceCount", 0),
            min_prepared_segment_reference_count=item.get(
                "minPreparedSegmentReferenceCount", 0
            ),
            min_technical_fact_reference_count=item.get("minTechnicalFactReferenceCount", 0),
            min_entity_reference_count=item.get("minEntityReferenceCount", 0),
            min_relation_reference_count=item.get("minRelationReferenceCount", 0),
            allowed_verification_states=item.get("allowedVerificationStates", ["verified"]),
        )
        for item in payload["cases"]
    ]
    return documents, cases, payload


def default_suite_paths() -> list[Path]:
    base = Path(__file__).resolve().parent
    return [base / item for item in DEFAULT_SUITE_MATRIX]


def create_library(client: BenchmarkClient, workspace_id: str, library_name: str) -> dict[str, Any]:
    return client.post_json(
        f"/catalog/workspaces/{workspace_id}/libraries",
        {
            "displayName": library_name,
            "description": "Neutral benchmark corpus for grounded QA evaluation",
        },
    )


def upload_documents(
    client: BenchmarkClient,
    library_id: str,
    document_paths: list[Path],
) -> list[dict[str, Any]]:
    uploads: list[dict[str, Any]] = []
    for document_path in document_paths:
        uploads.append(
            client.post_multipart(
                "/content/documents/upload",
                {"library_id": library_id},
                document_path,
            )
        )
    return uploads


def snapshot_library_state(
    client: BenchmarkClient,
    library_id: str,
    started_monotonic: float,
) -> dict[str, Any]:
    snapshot = client.get_json(f"/ops/libraries/{library_id}")
    state = snapshot["state"]
    return {
        "elapsedSeconds": round(time.monotonic() - started_monotonic, 3),
        "queueDepth": state["queue_depth"],
        "runningAttempts": state["running_attempts"],
        "readableDocumentCount": state["readable_document_count"],
        "degradedState": state["degraded_state"],
        "knowledgeGenerationState": state["knowledge_generation_state"],
        "latestKnowledgeGenerationId": state["latest_knowledge_generation_id"],
    }


def fetch_library_summary(client: BenchmarkClient, library_id: str) -> dict[str, Any]:
    return client.get_json(f"/knowledge/libraries/{library_id}/summary")


def fetch_topology_counts(client: BenchmarkClient, library_id: str) -> dict[str, int]:
    topology = client.get_json(f"/knowledge/libraries/{library_id}/graph-topology")
    return {
        "documents": len(topology.get("documents", [])),
        "entities": len(topology.get("entities", [])),
        "relations": len(topology.get("relations", [])),
        "documentLinks": len(topology.get("documentLinks", [])),
    }


def wait_for_library_state(
    client: BenchmarkClient,
    library_id: str,
    minimum_readable_count: int,
    poll_interval_seconds: float,
    wait_timeout_seconds: float,
) -> tuple[list[dict[str, Any]], float | None, float | None]:
    timeline: list[dict[str, Any]] = []
    started = time.monotonic()
    readable_elapsed: float | None = None

    while True:
        point = snapshot_library_state(client, library_id, started)
        timeline.append(point)

        if readable_elapsed is None and point["readableDocumentCount"] >= minimum_readable_count:
            readable_elapsed = point["elapsedSeconds"]

        if readable_elapsed is not None and point["queueDepth"] == 0 and point["runningAttempts"] == 0:
            return timeline, readable_elapsed, point["elapsedSeconds"]

        if point["elapsedSeconds"] >= wait_timeout_seconds:
            return timeline, readable_elapsed, None

        time.sleep(poll_interval_seconds)


def create_query_session(client: BenchmarkClient, workspace_id: str, library_id: str) -> dict[str, Any]:
    return client.post_json(
        "/query/sessions",
        {
            "workspaceId": workspace_id,
            "libraryId": library_id,
            "title": f"Benchmark {utc_now_iso()}",
        },
    )


def summarize_search_hits(search_payload: dict[str, Any]) -> tuple[list[dict[str, Any]], str]:
    summaries: list[dict[str, Any]] = []
    chunk_texts: list[str] = []

    for hit in search_payload.get("documentHits", []):
        document = hit.get("document", {})
        chunk_summaries = []
        for chunk in hit.get("chunkHits", []):
            content = chunk.get("content_text") or chunk.get("contentText") or ""
            chunk_texts.append(content)
            chunk_summaries.append(
                {
                    "chunkId": chunk.get("chunk_id") or chunk.get("chunkId"),
                    "score": chunk.get("score") or chunk.get("lexicalScore"),
                    "contentPreview": content[:600],
                }
            )
        summaries.append(
            {
                "title": document.get("title") or document.get("fileName"),
                "score": hit.get("score"),
                "chunkHits": chunk_summaries,
            }
        )

    return summaries, "\n".join(chunk_texts)


def run_case(
    client: BenchmarkClient,
    library_id: str,
    session_id: str,
    case: BenchmarkCase,
    query_top_k: int,
) -> dict[str, Any]:
    search_payload = client.get_json(
        f"/knowledge/libraries/{library_id}/search/documents",
        params={
            "query": case.search_query,
            "limit": 3,
            "chunkHitLimitPerDocument": 3,
            "evidenceSampleLimit": 0,
        },
    )
    search_summaries, aggregated_chunk_text = summarize_search_hits(search_payload)
    top_document_title = search_summaries[0]["title"] if search_summaries else None
    top_document_ok = (
        True
        if not case.expected_documents_contains
        else any(
            lower_text(needle) in lower_text(top_document_title)
            for needle in case.expected_documents_contains
        )
    )
    retrieval_contains_required = contains_all(aggregated_chunk_text, case.search_required_all)

    answer_started = time.monotonic()
    turn_payload = client.post_json(
        f"/query/sessions/{session_id}/turns",
        {"contentText": case.question, "topK": query_top_k, "includeDebug": True},
    )
    answer_latency_ms = round((time.monotonic() - answer_started) * 1000.0, 1)
    response_turn = turn_payload.get("responseTurn", {})
    execution = turn_payload.get("execution", {})
    answer_text = response_turn.get("contentText") or response_turn.get("content_text") or ""
    execution_id = execution.get("id") or execution.get("executionId")
    execution_detail = client.get_json(f"/query/executions/{execution_id}")

    answer_has_required = contains_all(answer_text, case.answer_required_all) and (
        True if not case.answer_required_any else contains_any(answer_text, case.answer_required_any)
    )
    answer_has_forbidden = contains_any(answer_text, case.answer_forbidden_any)

    chunk_reference_count = len(execution_detail.get("chunkReferences", []))
    prepared_segment_reference_count = len(execution_detail.get("preparedSegmentReferences", []))
    technical_fact_reference_count = len(execution_detail.get("technicalFactReferences", []))
    entity_reference_count = len(execution_detail.get("entityReferences", []))
    relation_reference_count = len(execution_detail.get("relationReferences", []))
    verification_state = execution_detail.get("verificationState") or "not_run"
    verification_warnings = execution_detail.get("verificationWarnings", [])

    graph_usage_pass = (
        chunk_reference_count >= case.min_chunk_reference_count
        and entity_reference_count >= case.min_entity_reference_count
        and relation_reference_count >= case.min_relation_reference_count
    )
    structured_evidence_pass = (
        prepared_segment_reference_count >= case.min_prepared_segment_reference_count
        and technical_fact_reference_count >= case.min_technical_fact_reference_count
    )
    verification_pass = verification_state in case.allowed_verification_states
    strict_case_pass = (
        top_document_ok
        and retrieval_contains_required
        and answer_has_required
        and not answer_has_forbidden
        and graph_usage_pass
        and structured_evidence_pass
        and verification_pass
    )

    return {
        "caseId": case.case_id,
        "question": case.question,
        "searchQuery": case.search_query,
        "topSearchDocumentTitle": top_document_title,
        "topSearchDocumentOk": top_document_ok,
        "retrievalContainsRequired": retrieval_contains_required,
        "answerHasRequired": answer_has_required,
        "answerHasForbidden": answer_has_forbidden,
        "answerPass": answer_has_required and not answer_has_forbidden,
        "graphUsagePass": graph_usage_pass,
        "structuredEvidencePass": structured_evidence_pass,
        "verificationState": verification_state,
        "verificationWarnings": verification_warnings,
        "verificationPass": verification_pass,
        "strictCasePass": strict_case_pass,
        "searchResultCount": len(search_summaries),
        "searchResults": search_summaries,
        "answerLatencyMs": answer_latency_ms,
        "answer": answer_text,
        "executionId": execution_id,
        "executionState": execution.get("executionState") or execution.get("execution_state"),
        "chunkReferenceCount": chunk_reference_count,
        "preparedSegmentReferenceCount": prepared_segment_reference_count,
        "technicalFactReferenceCount": technical_fact_reference_count,
        "entityReferenceCount": entity_reference_count,
        "relationReferenceCount": relation_reference_count,
        "minChunkReferenceCount": case.min_chunk_reference_count,
        "minPreparedSegmentReferenceCount": case.min_prepared_segment_reference_count,
        "minTechnicalFactReferenceCount": case.min_technical_fact_reference_count,
        "minEntityReferenceCount": case.min_entity_reference_count,
        "minRelationReferenceCount": case.min_relation_reference_count,
        "allowedVerificationStates": case.allowed_verification_states,
    }


def build_summary(case_results: list[dict[str, Any]]) -> dict[str, Any]:
    total = len(case_results)
    top_doc_pass = sum(1 for item in case_results if item["topSearchDocumentOk"])
    retrieval_pass = sum(1 for item in case_results if item["retrievalContainsRequired"])
    answer_pass = sum(1 for item in case_results if item["answerPass"])
    graph_usage_pass = sum(1 for item in case_results if item["graphUsagePass"])
    structured_evidence_pass = sum(1 for item in case_results if item["structuredEvidencePass"])
    verification_pass = sum(1 for item in case_results if item["verificationPass"])
    strict_case_pass = sum(1 for item in case_results if item["strictCasePass"])
    forbidden_failures = [item["caseId"] for item in case_results if item["answerHasForbidden"]]
    verification_failures = [item["caseId"] for item in case_results if not item["verificationPass"]]
    return {
        "totalCases": total,
        "topDocumentPassCount": top_doc_pass,
        "retrievalPassCount": retrieval_pass,
        "answerPassCount": answer_pass,
        "graphUsagePassCount": graph_usage_pass,
        "structuredEvidencePassCount": structured_evidence_pass,
        "verificationPassCount": verification_pass,
        "strictCasePassCount": strict_case_pass,
        "topDocumentPassRate": round(top_doc_pass / total, 3) if total else 0.0,
        "retrievalPassRate": round(retrieval_pass / total, 3) if total else 0.0,
        "answerPassRate": round(answer_pass / total, 3) if total else 0.0,
        "graphUsagePassRate": round(graph_usage_pass / total, 3) if total else 0.0,
        "structuredEvidencePassRate": round(structured_evidence_pass / total, 3) if total else 0.0,
        "verificationPassRate": round(verification_pass / total, 3) if total else 0.0,
        "strictCasePassRate": round(strict_case_pass / total, 3) if total else 0.0,
        "forbiddenAnswerFailures": forbidden_failures,
        "verificationFailures": verification_failures,
    }


def build_matrix_summary(suite_results: list[dict[str, Any]]) -> dict[str, Any]:
    total_suites = len(suite_results)
    strict_blocking_suites = sum(1 for suite in suite_results if suite["strictBlocking"])
    strict_blocking_suites_passed = sum(
        1
        for suite in suite_results
        if suite["strictBlocking"]
        and suite["summary"]["strictCasePassCount"] == suite["summary"]["totalCases"]
    )
    total_cases = sum(suite["summary"]["totalCases"] for suite in suite_results)
    strict_case_pass_count = sum(suite["summary"]["strictCasePassCount"] for suite in suite_results)
    failing_suites = [
        suite["suite"]["suiteId"]
        for suite in suite_results
        if suite["strictBlocking"]
        and suite["summary"]["strictCasePassCount"] != suite["summary"]["totalCases"]
    ]
    return {
        "totalSuites": total_suites,
        "strictBlockingSuites": strict_blocking_suites,
        "strictBlockingSuitesPassed": strict_blocking_suites_passed,
        "totalCases": total_cases,
        "strictCasePassCount": strict_case_pass_count,
        "strictCasePassRate": round(strict_case_pass_count / total_cases, 3)
        if total_cases
        else 0.0,
        "failingSuites": failing_suites,
    }


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    if not args.base_url:
        print(
            "IronRAG base URL is required via --base-url or IRONRAG_BENCHMARK_BASE_URL.",
            file=sys.stderr,
        )
        return 2
    if not args.workspace_id:
        print(
            "IronRAG workspace id is required via --workspace-id or IRONRAG_BENCHMARK_WORKSPACE_ID.",
            file=sys.stderr,
        )
        return 2
    if not args.session_cookie:
        print(
            "IRONRAG session cookie is required via --session-cookie or IRONRAG_SESSION_COOKIE.",
            file=sys.stderr,
        )
        return 2
    if args.skip_upload and not args.library_id:
        print("--skip-upload requires --library-id.", file=sys.stderr)
        return 2
    if args.upload_only and args.skip_upload:
        print("--upload-only cannot be combined with --skip-upload.", file=sys.stderr)
        return 2

    suite_paths = [Path(item).resolve() for item in (args.suite or default_suite_paths())]
    suite_payloads = []
    all_documents: list[Path] = []
    missing_paths: list[str] = []
    for suite_path in suite_paths:
        documents, cases, payload = load_suite(suite_path)
        suite_payloads.append((suite_path, documents, cases, payload))
        for document_path in documents:
            if document_path not in all_documents:
                all_documents.append(document_path)
            if not document_path.exists():
                missing_paths.append(str(document_path))
    if missing_paths and not args.skip_upload:
        print(
            json.dumps(
                {"error": "missing_documents", "paths": sorted(set(missing_paths))},
                ensure_ascii=False,
                indent=2,
            ),
            file=sys.stderr,
        )
        return 2

    client = BenchmarkClient(args.base_url, args.session_cookie)
    created_library = None
    library_id = args.library_id
    if not library_id:
        library_name = args.library_name or f"Grounded Benchmark {datetime.now().strftime('%H%M%S')}"
        created_library = create_library(client, args.workspace_id, library_name)
        library_id = created_library["id"]

    uploads = [] if args.skip_upload else upload_documents(client, library_id, all_documents)
    minimum_readable_count = len(all_documents) if not args.skip_upload else 1
    timeline, answer_ready_seconds, quiet_seconds = wait_for_library_state(
        client,
        library_id,
        minimum_readable_count,
        args.poll_interval_seconds,
        args.wait_timeout_seconds,
    )
    if args.skip_upload and not args.canonicalize_reused_library:
        answer_ready_seconds = 0.0
        quiet_seconds = 0.0

    library_summary = fetch_library_summary(client, library_id)
    topology_counts = fetch_topology_counts(client, library_id)

    if args.upload_only:
        payload = {
            "generatedAt": utc_now_iso(),
            "mode": "upload_only",
            "workspaceId": args.workspace_id,
            "library": created_library or {"id": library_id},
            "uploadedDocumentCount": len(uploads),
            "uploadedDocumentPaths": [str(path) for path in all_documents],
            "pipeline": {
                "answerReadySeconds": answer_ready_seconds,
                "quietSeconds": quiet_seconds,
                "timeline": timeline,
            },
            "librarySummary": library_summary,
            "topologyCounts": topology_counts,
            "suitePaths": [str(path) for path in suite_paths],
        }
        if args.output:
            write_json(Path(args.output).resolve(), payload)
        if args.output_dir:
            write_json(Path(args.output_dir).resolve() / "upload.result.json", payload)
        print(json.dumps(payload, ensure_ascii=False, indent=2))
        return 0

    suite_results = []
    for suite_path, _documents, cases, payload in suite_payloads:
        session = create_query_session(client, args.workspace_id, library_id)
        case_results = [run_case(client, library_id, session["id"], case, args.query_top_k) for case in cases]
        suite_result = {
            "generatedAt": utc_now_iso(),
            "suite": {
                "suiteId": payload.get("suiteId"),
                "description": payload.get("description"),
                "path": str(suite_path),
            },
            "strictBlocking": bool(payload.get("strictBlocking", True)),
            "workspaceId": args.workspace_id,
            "library": created_library or {"id": library_id},
            "querySessionId": session["id"],
            "topologyCounts": topology_counts,
            "librarySummary": library_summary,
            "timing": {
                "answerReadySeconds": answer_ready_seconds,
                "pipelineQuietSeconds": quiet_seconds,
                "pollIntervalSeconds": args.poll_interval_seconds,
                "waitTimeoutSeconds": args.wait_timeout_seconds,
            },
            "summary": build_summary(case_results),
            "cases": case_results,
        }
        suite_results.append(suite_result)

    matrix_result = {
        "generatedAt": utc_now_iso(),
        "suiteMatrix": [str(path) for path in suite_paths],
        "workspaceId": args.workspace_id,
        "library": created_library or {"id": library_id},
        "uploads": [
            {
                "documentId": item["document"]["document"]["id"],
                "fileName": item["document"]["fileName"],
                "jobId": item["mutation"].get("jobId"),
                "mutationId": item["mutation"]["mutation"]["id"],
            }
            for item in uploads
        ],
        "timing": {
            "answerReadySeconds": answer_ready_seconds,
            "pipelineQuietSeconds": quiet_seconds,
            "pollIntervalSeconds": args.poll_interval_seconds,
            "waitTimeoutSeconds": args.wait_timeout_seconds,
        },
        "opsTimeline": timeline,
        "topologyCounts": topology_counts,
        "librarySummary": library_summary,
        "summary": build_matrix_summary(suite_results),
        "suites": suite_results,
    }

    if args.output:
        write_json(Path(args.output), matrix_result)
    if args.output_dir:
        output_dir = Path(args.output_dir)
        write_json(output_dir / "matrix.result.json", matrix_result)
        for suite_result in suite_results:
            suite_path = Path(suite_result["suite"]["path"])
            write_json(output_dir / f"{suite_path.stem}.result.json", suite_result)

    print(json.dumps(matrix_result, ensure_ascii=False, indent=2))

    if args.strict and matrix_result["summary"]["strictBlockingSuites"] != matrix_result["summary"]["strictBlockingSuitesPassed"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
