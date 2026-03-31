#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import requests


DEFAULT_BASE_URL = "http://127.0.0.1:19000/v1"
DEFAULT_POLL_INTERVAL_SECONDS = 5.0
DEFAULT_WAIT_TIMEOUT_SECONDS = 900.0
DEFAULT_QUERY_TOP_K = 8


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
        description="Run a live grounded QA benchmark against a RustRAG deployment."
    )
    parser.add_argument(
        "--base-url",
        default=os.environ.get("RUSTRAG_BASE_URL", DEFAULT_BASE_URL),
        help="RustRAG API base URL including /v1.",
    )
    parser.add_argument(
        "--suite",
        default=str(Path(__file__).with_name("grad_api_suite.json")),
        help="Path to the benchmark suite JSON.",
    )
    parser.add_argument(
        "--workspace-id",
        required=True,
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
        default=os.environ.get("RUSTRAG_SESSION_COOKIE"),
        help="Value of rustrag_ui_session cookie.",
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
        help="Optional path to write the full benchmark result JSON.",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Exit non-zero if any answer or retrieval assertion fails.",
    )
    parser.add_argument(
        "--skip-upload",
        action="store_true",
        help="Reuse an existing corpus in --library-id and skip document uploads.",
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
    min_entity_reference_count: int
    min_relation_reference_count: int


class BenchmarkClient:
    def __init__(self, base_url: str, session_cookie: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.http = requests.Session()
        self.http.cookies.set(
            "rustrag_ui_session",
            session_cookie,
            domain="127.0.0.1",
            path="/",
        )

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

    def post_multipart(
        self,
        path: str,
        fields: dict[str, str],
        file_path: Path,
    ) -> Any:
        with file_path.open("rb") as handle:
            files = {
                "file": (
                    file_path.name,
                    handle,
                    "application/pdf",
                )
            }
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
    documents = [Path(item) for item in payload["documents"]]
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
            min_entity_reference_count=item.get("minEntityReferenceCount", 0),
            min_relation_reference_count=item.get("minRelationReferenceCount", 0),
        )
        for item in payload["cases"]
    ]
    return documents, cases, payload


def create_library(
    client: BenchmarkClient,
    workspace_id: str,
    library_name: str,
) -> dict[str, Any]:
    return client.post_json(
        f"/catalog/workspaces/{workspace_id}/libraries",
        {
            "displayName": library_name,
            "description": "Periodic benchmark corpus for grounded QA evaluation",
        },
    )


def upload_documents(
    client: BenchmarkClient,
    library_id: str,
    document_paths: list[Path],
) -> list[dict[str, Any]]:
    uploads = []
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


def poll_until_answer_ready(
    client: BenchmarkClient,
    library_id: str,
    expected_readable_count: int,
    poll_interval_seconds: float,
    wait_timeout_seconds: float,
) -> tuple[list[dict[str, Any]], float | None, float]:
    timeline: list[dict[str, Any]] = []
    started = time.monotonic()

    while True:
        point = snapshot_library_state(client, library_id, started)
        timeline.append(point)
        if point["readableDocumentCount"] >= expected_readable_count:
            return timeline, point["elapsedSeconds"], started
        if point["elapsedSeconds"] >= wait_timeout_seconds:
            return timeline, None, started
        time.sleep(poll_interval_seconds)


def poll_until_pipeline_quiet(
    client: BenchmarkClient,
    library_id: str,
    started_monotonic: float,
    existing_timeline: list[dict[str, Any]],
    poll_interval_seconds: float,
    wait_timeout_seconds: float,
) -> float | None:
    seen_elapsed = {item["elapsedSeconds"] for item in existing_timeline}

    while True:
        point = snapshot_library_state(client, library_id, started_monotonic)
        if point["elapsedSeconds"] not in seen_elapsed:
            existing_timeline.append(point)
            seen_elapsed.add(point["elapsedSeconds"])

        if point["queueDepth"] == 0 and point["runningAttempts"] == 0:
            return point["elapsedSeconds"]
        if point["elapsedSeconds"] >= wait_timeout_seconds:
            return None
        time.sleep(poll_interval_seconds)


def fetch_topology_counts(client: BenchmarkClient, library_id: str) -> dict[str, int]:
    topology = client.get_json(f"/knowledge/libraries/{library_id}/graph-topology")
    return {
        "documents": len(topology.get("documents", [])),
        "entities": len(topology.get("entities", [])),
        "relations": len(topology.get("relations", [])),
        "documentLinks": len(topology.get("documentLinks", [])),
    }


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
                "title": document.get("title"),
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
        {
            "contentText": case.question,
            "topK": query_top_k,
            "includeDebug": True,
        },
    )
    answer_latency_ms = round((time.monotonic() - answer_started) * 1000.0, 1)
    response_turn = turn_payload.get("responseTurn", {})
    execution = turn_payload.get("execution", {})
    answer_text = response_turn.get("contentText") or response_turn.get("content_text") or ""
    execution_id = execution.get("id") or execution.get("executionId")
    execution_detail = client.get_json(f"/query/executions/{execution_id}")

    answer_has_required = contains_all(answer_text, case.answer_required_all) and (
        True
        if not case.answer_required_any
        else contains_any(answer_text, case.answer_required_any)
    )
    answer_has_forbidden = contains_any(answer_text, case.answer_forbidden_any)
    chunk_reference_count = len(execution_detail.get("chunkReferences", []))
    entity_reference_count = len(execution_detail.get("entityReferences", []))
    relation_reference_count = len(execution_detail.get("relationReferences", []))
    graph_usage_pass = (
        chunk_reference_count >= case.min_chunk_reference_count
        and entity_reference_count >= case.min_entity_reference_count
        and relation_reference_count >= case.min_relation_reference_count
    )
    strict_case_pass = (
        top_document_ok
        and retrieval_contains_required
        and answer_has_required
        and not answer_has_forbidden
        and graph_usage_pass
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
        "strictCasePass": strict_case_pass,
        "searchResultCount": len(search_summaries),
        "searchResults": search_summaries,
        "answerLatencyMs": answer_latency_ms,
        "answer": answer_text,
        "executionId": execution_id,
        "executionState": execution.get("executionState") or execution.get("execution_state"),
        "chunkReferenceCount": chunk_reference_count,
        "entityReferenceCount": entity_reference_count,
        "relationReferenceCount": relation_reference_count,
        "minChunkReferenceCount": case.min_chunk_reference_count,
        "minEntityReferenceCount": case.min_entity_reference_count,
        "minRelationReferenceCount": case.min_relation_reference_count,
    }


def build_summary(case_results: list[dict[str, Any]]) -> dict[str, Any]:
    total = len(case_results)
    top_doc_pass = sum(1 for item in case_results if item["topSearchDocumentOk"])
    retrieval_pass = sum(1 for item in case_results if item["retrievalContainsRequired"])
    answer_pass = sum(1 for item in case_results if item["answerPass"])
    graph_usage_pass = sum(1 for item in case_results if item["graphUsagePass"])
    strict_case_pass = sum(1 for item in case_results if item["strictCasePass"])
    forbidden_failures = [
        item["caseId"] for item in case_results if item["answerHasForbidden"]
    ]
    return {
        "totalCases": total,
        "topDocumentPassCount": top_doc_pass,
        "retrievalPassCount": retrieval_pass,
        "answerPassCount": answer_pass,
        "graphUsagePassCount": graph_usage_pass,
        "strictCasePassCount": strict_case_pass,
        "topDocumentPassRate": round(top_doc_pass / total, 3) if total else 0.0,
        "retrievalPassRate": round(retrieval_pass / total, 3) if total else 0.0,
        "answerPassRate": round(answer_pass / total, 3) if total else 0.0,
        "graphUsagePassRate": round(graph_usage_pass / total, 3) if total else 0.0,
        "strictCasePassRate": round(strict_case_pass / total, 3) if total else 0.0,
        "forbiddenAnswerFailures": forbidden_failures,
    }


def main() -> int:
    args = parse_args()
    if not args.session_cookie:
        print("RUSTRAG session cookie is required via --session-cookie or RUSTRAG_SESSION_COOKIE.", file=sys.stderr)
        return 2
    if args.skip_upload and not args.library_id:
        print("--skip-upload requires --library-id.", file=sys.stderr)
        return 2

    suite_path = Path(args.suite)
    document_paths, cases, suite_payload = load_suite(suite_path)
    missing_paths = [str(path) for path in document_paths if not path.exists()]
    if missing_paths and not args.skip_upload:
        print(json.dumps({"error": "missing_documents", "paths": missing_paths}, ensure_ascii=False, indent=2), file=sys.stderr)
        return 2

    client = BenchmarkClient(args.base_url, args.session_cookie)
    created_library = None
    library_id = args.library_id
    if not library_id:
        library_name = args.library_name or f"Agent Benchmark {datetime.now().strftime('%H%M%S')}"
        created_library = create_library(client, args.workspace_id, library_name)
        library_id = created_library["id"]

    uploads = [] if args.skip_upload else upload_documents(client, library_id, document_paths)
    timeline, answer_ready_seconds, started_monotonic = poll_until_answer_ready(
        client,
        library_id,
        len(document_paths),
        args.poll_interval_seconds,
        args.wait_timeout_seconds,
    )
    session = create_query_session(client, args.workspace_id, library_id)
    case_results = [
        run_case(client, library_id, session["id"], case, args.query_top_k)
        for case in cases
    ]
    quiet_seconds = poll_until_pipeline_quiet(
        client,
        library_id,
        started_monotonic,
        timeline,
        args.poll_interval_seconds,
        args.wait_timeout_seconds,
    )
    topology_counts = fetch_topology_counts(client, library_id)
    summary = build_summary(case_results)

    result = {
        "generatedAt": utc_now_iso(),
        "suite": {
            "suiteId": suite_payload.get("suiteId"),
            "description": suite_payload.get("description"),
            "path": str(suite_path),
        },
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
        "querySessionId": session["id"],
        "summary": summary,
        "cases": case_results,
    }

    output = json.dumps(result, ensure_ascii=False, indent=2)
    if args.output:
        Path(args.output).write_text(output + "\n", encoding="utf-8")
    print(output)

    if args.strict and summary["strictCasePassCount"] != summary["totalCases"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
