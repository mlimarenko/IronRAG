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
RANK_METRIC_CUTOFFS = (1, 3, 5, 10)
RANK_METRIC_SEARCH_LIMIT = max(RANK_METRIC_CUTOFFS)
RANK_RELEVANCE_FILE_NAME = "rank_relevance.json"
RANK_TREND_FILE_NAME = "rank_metrics_trend.jsonl"
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


def first_answer_sentence(answer: str | None) -> str:
    text = (answer or "").strip()
    if not text:
        return ""
    first_line = next((line.strip() for line in text.splitlines() if line.strip()), "")
    match = re.search(r"(?<=[.!?。！？])\s+", first_line)
    if match:
        return first_line[: match.start()].strip()
    return first_line[:240].strip()


def references_contain_all_labels(references: list[dict[str, Any]], labels: list[str]) -> bool:
    if not labels:
        return True
    reference_text = "\n".join(
        str(reference.get("label") or reference.get("entityType") or "")
        for reference in references
    )
    return contains_all(reference_text, labels)


def relation_references_contain_all_text(
    references: list[dict[str, Any]], expected_text: list[str]
) -> bool:
    if not expected_text:
        return True
    reference_text = "\n".join(
        "\n".join(
            str(reference.get(field) or "")
            for field in ("predicate", "normalizedAssertion", "normalized_assertion")
        )
        for reference in references
    )
    return contains_all(reference_text, expected_text)


def append_failure_if(condition: bool, failures: list[str], failure_code: str) -> None:
    if condition:
        failures.append(failure_code)


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
    answer_opening_required_any: list[str]
    answer_forbidden_any: list[str]
    min_chunk_reference_count: int
    min_prepared_segment_reference_count: int
    min_technical_fact_reference_count: int
    min_entity_reference_count: int
    min_relation_reference_count: int
    expected_entity_reference_labels_contains: list[str]
    expected_relation_reference_text_contains: list[str]
    allowed_verification_states: list[str]
    relevant_documents: list[str]
    relevant_chunks: list[str]


class BenchmarkClient:
    def __init__(self, base_url: str, session_cookie: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.session_cookie = session_cookie
        self.http = self._new_session()

    def _new_session(self) -> requests.Session:
        http = requests.Session()
        http.cookies.set("ironrag_ui_session", self.session_cookie, path="/")
        return http

    def _refresh_session(self) -> None:
        self.http = self._new_session()

    def get_json(self, path: str, **kwargs: Any) -> Any:
        last_error: requests.ConnectionError | None = None
        for attempt in range(3):
            try:
                response = self.http.get(f"{self.base_url}{path}", timeout=120, **kwargs)
                response.raise_for_status()
                return response.json()
            except requests.ConnectionError as error:
                last_error = error
                self._refresh_session()
                if attempt < 2:
                    time.sleep(0.5 * (attempt + 1))
        assert last_error is not None
        raise last_error

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


def load_rank_relevance(path: Path) -> dict[str, Any]:
    relevance_path = path.parent / RANK_RELEVANCE_FILE_NAME
    if not relevance_path.exists():
        return {}
    payload = json.loads(relevance_path.read_text(encoding="utf-8"))
    return payload.get("suites", {})


def suite_case_rank_relevance(
    rank_relevance: dict[str, Any],
    suite_id: str | None,
    case_id: str,
) -> dict[str, Any]:
    if not suite_id:
        return {}
    suite_relevance = rank_relevance.get(suite_id, {})
    if not isinstance(suite_relevance, dict):
        return {}
    cases = suite_relevance.get("cases", {})
    if not isinstance(cases, dict):
        return {}
    value = cases.get(case_id, {})
    return value if isinstance(value, dict) else {}


def load_suite(path: Path) -> tuple[list[Path], list[BenchmarkCase], dict[str, Any]]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    suite_id = payload.get("suiteId")
    rank_relevance = load_rank_relevance(path)
    documents = []
    for item in payload["documents"]:
        candidate = Path(item)
        if not candidate.is_absolute():
            candidate = (path.parent / candidate).resolve()
        documents.append(candidate)

    cases = []
    for item in payload["cases"]:
        relevance = suite_case_rank_relevance(rank_relevance, suite_id, item["id"])
        cases.append(
            BenchmarkCase(
                case_id=item["id"],
                question=item["question"],
                search_query=item.get("searchQuery", item["question"]),
                expected_documents_contains=item.get("expectedDocumentsContains", []),
                search_required_all=item.get("searchRequiredAll", []),
                answer_required_all=item.get("answerRequiredAll", []),
                answer_required_any=item.get("answerRequiredAny", []),
                answer_opening_required_any=item.get("answerOpeningRequiredAny", []),
                answer_forbidden_any=item.get("answerForbiddenAny", []),
                min_chunk_reference_count=item.get("minChunkReferenceCount", 0),
                min_prepared_segment_reference_count=item.get(
                    "minPreparedSegmentReferenceCount", 0
                ),
                min_technical_fact_reference_count=item.get("minTechnicalFactReferenceCount", 0),
                min_entity_reference_count=item.get("minEntityReferenceCount", 0),
                min_relation_reference_count=item.get("minRelationReferenceCount", 0),
                expected_entity_reference_labels_contains=item.get(
                    "expectedEntityReferenceLabelsContains", []
                ),
                expected_relation_reference_text_contains=item.get(
                    "expectedRelationReferenceTextContains", []
                ),
                allowed_verification_states=item.get("allowedVerificationStates", ["verified"]),
                relevant_documents=item.get(
                    "relevantDocuments",
                    relevance.get(
                        "relevantDocuments",
                        item.get("expectedDocumentsContains", []),
                    ),
                ),
                relevant_chunks=item.get(
                    "relevantChunks",
                    relevance.get("relevantChunks", []),
                ),
            )
        )
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
        "queueDepth": state["queueDepth"],
        "runningAttempts": state["runningAttempts"],
        "readableDocumentCount": state["readableDocumentCount"],
        "degradedState": state["degradedState"],
        "knowledgeGenerationState": state["knowledgeGenerationState"],
        "latestKnowledgeGenerationId": state["latestKnowledgeGenerationId"],
    }


def fetch_library_summary(client: BenchmarkClient, library_id: str) -> dict[str, Any]:
    return client.get_json(f"/knowledge/libraries/{library_id}/summary")


def fetch_topology_counts(client: BenchmarkClient, library_id: str) -> dict[str, int]:
    response = client.http.get(
        f"{client.base_url}/knowledge/libraries/{library_id}/graph",
        headers={"Accept": "application/x-ndjson"},
        timeout=120,
    )
    response.raise_for_status()

    documents = 0
    entities = 0
    relations = 0
    document_links = 0
    for line in response.text.splitlines():
        payload = line.strip()
        if not payload:
            continue
        frame = json.loads(payload)
        section = frame.get("s")
        rows = frame.get("d")
        if section == "docs" and isinstance(rows, list):
            documents += len(rows)
        elif section == "nodes" and isinstance(rows, list):
            entities += len(rows)
        elif section == "edges" and isinstance(rows, list):
            relations += len(rows)
        elif section == "doc_links" and isinstance(rows, list):
            document_links += len(rows)

    return {
        "documents": documents,
        "entities": entities,
        "relations": relations,
        "documentLinks": document_links,
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


def stable_key(value: str | None) -> str:
    return canonical_match_text(Path(value).stem if value else "")


def document_stable_keys(document: dict[str, Any]) -> list[str]:
    candidates = [
        document.get("file_name"),
        document.get("fileName"),
        document.get("title"),
        document.get("external_key"),
        document.get("externalKey"),
    ]
    keys: list[str] = []
    for value in candidates:
        if not value:
            continue
        text = str(value)
        for candidate in (text, Path(text).name, Path(text).stem):
            key = stable_key(candidate)
            if key and key not in keys:
                keys.append(key)
    return keys


def primary_document_stable_key(document: dict[str, Any]) -> str | None:
    keys = document_stable_keys(document)
    return keys[0] if keys else None


def normalize_relevant_keys(values: list[str]) -> set[str]:
    keys = set()
    for value in values:
        key = stable_key(value)
        if key:
            keys.add(key)
    return keys


def compute_rank_metrics(
    ranked_ids: list[str],
    relevant_ids: set[str],
    cutoffs: tuple[int, ...] = RANK_METRIC_CUTOFFS,
) -> dict[str, Any] | None:
    relevant = {item for item in relevant_ids if item}
    if not relevant:
        return None
    ranked = [item for item in ranked_ids if item]
    first_rank = next(
        (index + 1 for index, item in enumerate(ranked) if item in relevant),
        None,
    )
    metrics: dict[str, Any] = {
        "rankedCount": len(ranked),
        "relevantCount": len(relevant),
        "firstRelevantRank": first_rank,
        "mrr": round(1.0 / first_rank, 6) if first_rank else 0.0,
    }
    for cutoff in cutoffs:
        metrics[f"hit@{cutoff}"] = any(item in relevant for item in ranked[:cutoff])
    return metrics


def compute_marker_rank_metrics(
    ranked_texts: list[str],
    relevant_markers: list[str],
    cutoffs: tuple[int, ...] = RANK_METRIC_CUTOFFS,
) -> dict[str, Any] | None:
    markers = [canonical_match_text(marker) for marker in relevant_markers if marker.strip()]
    if not markers:
        return None
    first_rank = None
    normalized_ranked = [canonical_match_text(item) for item in ranked_texts]
    for index, text in enumerate(normalized_ranked):
        if any(marker in text for marker in markers):
            first_rank = index + 1
            break
    metrics: dict[str, Any] = {
        "rankedCount": len(normalized_ranked),
        "relevantCount": len(markers),
        "firstRelevantRank": first_rank,
        "mrr": round(1.0 / first_rank, 6) if first_rank else 0.0,
    }
    for cutoff in cutoffs:
        metrics[f"hit@{cutoff}"] = any(
            any(marker in text for marker in markers) for text in normalized_ranked[:cutoff]
        )
    return metrics


def summarize_search_hits(
    search_payload: dict[str, Any],
) -> tuple[list[dict[str, Any]], str, list[str], list[str]]:
    summaries: list[dict[str, Any]] = []
    chunk_texts: list[str] = []
    ranked_document_keys: list[str] = []
    ranked_chunk_texts: list[str] = []

    for hit in search_payload.get("documentHits", []):
        document = hit.get("document", {})
        document_id = document.get("document_id") or document.get("documentId")
        file_name = document.get("file_name") or document.get("fileName")
        title = document.get("title") or file_name
        document_key = primary_document_stable_key(document)
        if document_key:
            ranked_document_keys.append(document_key)
        chunk_summaries = []
        for chunk in hit.get("chunkHits", []):
            content = (
                chunk.get("content_text")
                or chunk.get("contentText")
                or chunk.get("normalized_text")
                or chunk.get("normalizedText")
                or ""
            )
            chunk_texts.append(content)
            ranked_chunk_texts.append(content)
            chunk_summaries.append(
                {
                    "chunkId": chunk.get("chunk_id") or chunk.get("chunkId"),
                    "documentId": document_id,
                    "score": chunk.get("score") or chunk.get("lexicalScore"),
                    "contentPreview": content[:600],
                }
            )
        vector_chunk_summaries = []
        for chunk in hit.get("vectorChunkHits", []):
            vector_chunk_summaries.append(
                {
                    "chunkId": chunk.get("chunk_id") or chunk.get("chunkId"),
                    "documentId": document_id,
                    "score": chunk.get("score"),
                }
            )
        summaries.append(
            {
                "documentId": document_id,
                "fileName": file_name,
                "title": title,
                "stableKey": document_key,
                "score": hit.get("score"),
                "chunkHits": chunk_summaries,
                "vectorChunkHits": vector_chunk_summaries,
            }
        )

    return summaries, "\n".join(chunk_texts), ranked_document_keys, ranked_chunk_texts


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
            "limit": max(RANK_METRIC_SEARCH_LIMIT, query_top_k),
            "chunkHitLimitPerDocument": RANK_METRIC_SEARCH_LIMIT,
            "evidenceSampleLimit": 0,
        },
    )
    search_summaries, aggregated_chunk_text, ranked_document_keys, ranked_chunk_texts = (
        summarize_search_hits(search_payload)
    )
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
    document_rank_metrics = compute_rank_metrics(
        ranked_document_keys,
        normalize_relevant_keys(case.relevant_documents),
    )
    chunk_rank_metrics = compute_marker_rank_metrics(ranked_chunk_texts, case.relevant_chunks)
    rank_metrics = {
        key: value
        for key, value in {
            "documents": document_rank_metrics,
            "chunks": chunk_rank_metrics,
        }.items()
        if value is not None
    }

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
    answer_opening = first_answer_sentence(answer_text)
    answer_opening_has_required = (
        True
        if not case.answer_opening_required_any
        else contains_any(answer_opening, case.answer_opening_required_any)
    )
    answer_has_forbidden = contains_any(answer_text, case.answer_forbidden_any)

    chunk_reference_count = len(execution_detail.get("chunkReferences", []))
    prepared_segment_reference_count = len(execution_detail.get("preparedSegmentReferences", []))
    technical_fact_reference_count = len(execution_detail.get("technicalFactReferences", []))
    entity_references = execution_detail.get("entityReferences", [])
    relation_references = execution_detail.get("relationReferences", [])
    entity_reference_count = len(entity_references)
    relation_reference_count = len(relation_references)
    entity_reference_labels_pass = references_contain_all_labels(
        entity_references,
        case.expected_entity_reference_labels_contains,
    )
    relation_reference_text_pass = relation_references_contain_all_text(
        relation_references,
        case.expected_relation_reference_text_contains,
    )
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
    failed_checks: list[str] = []
    append_failure_if(not top_document_ok, failed_checks, "top_document")
    append_failure_if(not retrieval_contains_required, failed_checks, "retrieval_contains_required")
    append_failure_if(not answer_has_required, failed_checks, "answer_required")
    append_failure_if(
        not answer_opening_has_required,
        failed_checks,
        "answer_opening_required",
    )
    append_failure_if(answer_has_forbidden, failed_checks, "answer_forbidden")
    append_failure_if(chunk_reference_count < case.min_chunk_reference_count, failed_checks, "chunk_references")
    append_failure_if(
        prepared_segment_reference_count < case.min_prepared_segment_reference_count,
        failed_checks,
        "prepared_segment_references",
    )
    append_failure_if(
        technical_fact_reference_count < case.min_technical_fact_reference_count,
        failed_checks,
        "technical_fact_references",
    )
    append_failure_if(
        entity_reference_count < case.min_entity_reference_count,
        failed_checks,
        "entity_references",
    )
    append_failure_if(
        relation_reference_count < case.min_relation_reference_count,
        failed_checks,
        "relation_references",
    )
    append_failure_if(
        not entity_reference_labels_pass,
        failed_checks,
        "entity_reference_labels",
    )
    append_failure_if(
        not relation_reference_text_pass,
        failed_checks,
        "relation_reference_text",
    )
    append_failure_if(not verification_pass, failed_checks, "verification_state")
    strict_case_pass = (
        top_document_ok
        and retrieval_contains_required
        and answer_has_required
        and answer_opening_has_required
        and not answer_has_forbidden
        and graph_usage_pass
        and entity_reference_labels_pass
        and relation_reference_text_pass
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
        "answerOpening": answer_opening,
        "answerOpeningHasRequired": answer_opening_has_required,
        "answerHasForbidden": answer_has_forbidden,
        "answerPass": answer_has_required and answer_opening_has_required and not answer_has_forbidden,
        "graphUsagePass": graph_usage_pass,
        "entityReferenceLabelsPass": entity_reference_labels_pass,
        "relationReferenceTextPass": relation_reference_text_pass,
        "structuredEvidencePass": structured_evidence_pass,
        "verificationState": verification_state,
        "verificationWarnings": verification_warnings,
        "verificationPass": verification_pass,
        "strictCasePass": strict_case_pass,
        "failedChecks": failed_checks,
        "searchResultCount": len(search_summaries),
        "searchResults": search_summaries,
        "rankMetrics": rank_metrics,
        "relevantDocuments": case.relevant_documents,
        "relevantChunks": case.relevant_chunks,
        "answerLatencyMs": answer_latency_ms,
        "answer": answer_text,
        "executionId": execution_id,
        "executionState": execution.get("executionState") or execution.get("execution_state"),
        "chunkReferenceCount": chunk_reference_count,
        "preparedSegmentReferenceCount": prepared_segment_reference_count,
        "technicalFactReferenceCount": technical_fact_reference_count,
        "entityReferenceCount": entity_reference_count,
        "relationReferenceCount": relation_reference_count,
        "entityReferences": entity_references,
        "relationReferences": relation_references,
        "minChunkReferenceCount": case.min_chunk_reference_count,
        "minPreparedSegmentReferenceCount": case.min_prepared_segment_reference_count,
        "minTechnicalFactReferenceCount": case.min_technical_fact_reference_count,
        "minEntityReferenceCount": case.min_entity_reference_count,
        "minRelationReferenceCount": case.min_relation_reference_count,
        "expectedEntityReferenceLabelsContains": case.expected_entity_reference_labels_contains,
        "expectedRelationReferenceTextContains": case.expected_relation_reference_text_contains,
        "allowedVerificationStates": case.allowed_verification_states,
    }


def build_summary(case_results: list[dict[str, Any]]) -> dict[str, Any]:
    total = len(case_results)
    top_doc_pass = sum(1 for item in case_results if item["topSearchDocumentOk"])
    retrieval_pass = sum(1 for item in case_results if item["retrievalContainsRequired"])
    answer_pass = sum(1 for item in case_results if item["answerPass"])
    graph_usage_pass = sum(1 for item in case_results if item["graphUsagePass"])
    entity_label_pass = sum(1 for item in case_results if item["entityReferenceLabelsPass"])
    relation_text_pass = sum(1 for item in case_results if item["relationReferenceTextPass"])
    structured_evidence_pass = sum(1 for item in case_results if item["structuredEvidencePass"])
    verification_pass = sum(1 for item in case_results if item["verificationPass"])
    strict_case_pass = sum(1 for item in case_results if item["strictCasePass"])
    forbidden_failures = [item["caseId"] for item in case_results if item["answerHasForbidden"]]
    verification_failures = [item["caseId"] for item in case_results if not item["verificationPass"]]
    failure_reason_counts: dict[str, int] = {}
    for item in case_results:
        for failure_code in item.get("failedChecks", []):
            failure_reason_counts[failure_code] = failure_reason_counts.get(failure_code, 0) + 1
    rank_metrics = build_rank_metric_summary(case_results)
    return {
        "totalCases": total,
        "topDocumentPassCount": top_doc_pass,
        "retrievalPassCount": retrieval_pass,
        "answerPassCount": answer_pass,
        "graphUsagePassCount": graph_usage_pass,
        "entityReferenceLabelsPassCount": entity_label_pass,
        "relationReferenceTextPassCount": relation_text_pass,
        "structuredEvidencePassCount": structured_evidence_pass,
        "verificationPassCount": verification_pass,
        "strictCasePassCount": strict_case_pass,
        "topDocumentPassRate": round(top_doc_pass / total, 3) if total else 0.0,
        "retrievalPassRate": round(retrieval_pass / total, 3) if total else 0.0,
        "answerPassRate": round(answer_pass / total, 3) if total else 0.0,
        "graphUsagePassRate": round(graph_usage_pass / total, 3) if total else 0.0,
        "entityReferenceLabelsPassRate": round(entity_label_pass / total, 3) if total else 0.0,
        "relationReferenceTextPassRate": round(relation_text_pass / total, 3) if total else 0.0,
        "structuredEvidencePassRate": round(structured_evidence_pass / total, 3) if total else 0.0,
        "verificationPassRate": round(verification_pass / total, 3) if total else 0.0,
        "strictCasePassRate": round(strict_case_pass / total, 3) if total else 0.0,
        "forbiddenAnswerFailures": forbidden_failures,
        "verificationFailures": verification_failures,
        "failureReasonCounts": failure_reason_counts,
        "rankMetrics": rank_metrics,
    }


def average(values: list[float]) -> float:
    return round(sum(values) / len(values), 6) if values else 0.0


def build_rank_metric_summary(case_results: list[dict[str, Any]]) -> dict[str, Any]:
    summary: dict[str, Any] = {}
    for family in ("documents", "chunks"):
        cases = [
            item.get("rankMetrics", {}).get(family)
            for item in case_results
            if item.get("rankMetrics", {}).get(family)
        ]
        family_metrics = [item for item in cases if isinstance(item, dict)]
        metric_summary: dict[str, Any] = {
            "caseCount": len(family_metrics),
            "mrr": average([float(item.get("mrr", 0.0)) for item in family_metrics]),
        }
        for cutoff in RANK_METRIC_CUTOFFS:
            key = f"hit@{cutoff}"
            metric_summary[key] = average(
                [1.0 if item.get(key) else 0.0 for item in family_metrics]
            )
        summary[family] = metric_summary
    return summary


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
    rank_metrics = build_matrix_rank_metric_summary(suite_results)
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
        "rankMetrics": rank_metrics,
    }


def build_matrix_rank_metric_summary(suite_results: list[dict[str, Any]]) -> dict[str, Any]:
    synthetic_cases: list[dict[str, Any]] = []
    for suite in suite_results:
        synthetic_cases.extend(suite.get("cases", []))
    return build_rank_metric_summary(synthetic_cases)


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def append_rank_trend(output_dir: Path, matrix_result: dict[str, Any]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    record = {
        "generatedAt": matrix_result.get("generatedAt"),
        "suiteMatrix": matrix_result.get("suiteMatrix", []),
        "libraryId": (matrix_result.get("library") or {}).get("id"),
        "summary": matrix_result.get("summary", {}),
        "suites": [
            {
                "suiteId": suite.get("suite", {}).get("suiteId"),
                "rankMetrics": suite.get("summary", {}).get("rankMetrics", {}),
            }
            for suite in matrix_result.get("suites", [])
        ],
    }
    with (output_dir / RANK_TREND_FILE_NAME).open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")


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
        append_rank_trend(output_dir, matrix_result)

    print(json.dumps(matrix_result, ensure_ascii=False, indent=2))

    if args.strict and matrix_result["summary"]["strictBlockingSuites"] != matrix_result["summary"]["strictBlockingSuitesPassed"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
