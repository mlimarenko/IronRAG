#!/usr/bin/env python3
"""
Canonical MCP + assistant streaming probe.

Measures two agent-facing surfaces that `profile-ui-endpoints.py` does not cover:

1. MCP graph/search tools (latency, payload size, and semantic coherence)
2. Assistant SSE turn streaming (TTFB, first delta, tool-call phase timing,
   completion timing, and grounding/reference presence)

Writes a markdown report to `tmp/agent-surface-profile-<timestamp>.md`.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import pathlib
import re
import select
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any
from urllib import error as urllib_error


DEFAULT_BASE_URL = "http://127.0.0.1:19000"
DEFAULT_LOGIN = "admin"
DEFAULT_PASSWORD = "rustrag123"
DEFAULT_ENTITY_QUERY = "Orion"
DEFAULT_ASSISTANT_QUESTION = "What does this library say about Orion?"
DEFAULT_DOCUMENT_QUERY = DEFAULT_ASSISTANT_QUESTION
DEFAULT_DOCUMENT_LIMIT = 5
DEFAULT_READ_LENGTH = 4000
DEFAULT_GRAPH_MIN_ENTITIES = 1
DEFAULT_GRAPH_MIN_RELATIONS = 1
DEFAULT_GRAPH_MIN_DOCUMENTS = 1
DEFAULT_COMMUNITY_MIN_COUNT = 0
DEFAULT_ENTITY_SEARCH_MIN_HITS = 1
DEFAULT_SEARCH_MIN_HITS = 1
DEFAULT_SEARCH_MIN_READABLE_HITS = 1
DEFAULT_READ_MIN_CONTENT_CHARS = 200
DEFAULT_READ_MIN_REFERENCES = 1
DEFAULT_ASSISTANT_MIN_REFERENCES = 1
DEFAULT_ASSISTANT_EXPECTED_VERIFICATION = "verified"


@dataclass(frozen=True)
class CurlSample:
    status_code: int
    time_total_s: float
    size_download_bytes: int
    payload: Any


@dataclass(frozen=True)
class McpQualitySummary:
    entity_count: int
    relation_count: int
    document_count: int
    document_link_count: int
    orphan_relation_count: int
    orphan_link_count: int
    orphan_document_count: int
    entity_rank_monotonic: bool
    relation_rank_monotonic: bool
    document_rank_monotonic: bool
    duplicate_entity_label_count: int
    duplicate_relation_signature_count: int
    top_entity_label: str | None
    visible_entity_labels_normalized: tuple[str, ...]

    @property
    def quality_status(self) -> str:
        if (
            self.orphan_relation_count
            or self.orphan_link_count
            or self.orphan_document_count
            or self.duplicate_entity_label_count
            or self.duplicate_relation_signature_count
        ):
            return "broken"
        if (
            not self.entity_rank_monotonic
            or not self.relation_rank_monotonic
            or not self.document_rank_monotonic
        ):
            return "warn"
        return "pass"


@dataclass(frozen=True)
class EntitySearchSummary:
    hit_count: int
    top_label: str | None
    top_score: float | None


@dataclass(frozen=True)
class DocumentSearchSummary:
    hit_count: int
    readable_hit_count: int
    top_document_id: str | None
    top_document_title: str | None
    top_suggested_start_offset: int | None
    top_excerpt_length: int
    top_chunk_reference_count: int
    top_score: float | None


@dataclass(frozen=True)
class DocumentReadSummary:
    document_id: str | None
    document_title: str | None
    readability_state: str | None
    content_length: int
    total_reference_count: int
    has_more: bool
    slice_start_offset: int | None
    slice_end_offset: int | None


@dataclass(frozen=True)
class RelationListSummary:
    row_count: int
    unknown_label_count: int
    duplicate_signature_count: int


@dataclass(frozen=True)
class CommunitySummary:
    count: int
    communities_with_summary: int
    top_entity_count: int


@dataclass(frozen=True)
class RuntimeExecutionProbeSummary:
    runtime_execution_id: str | None
    lifecycle_state: str | None
    active_stage: str | None


@dataclass(frozen=True)
class RuntimeTraceProbeSummary:
    runtime_execution_id: str | None
    stage_count: int
    action_count: int
    policy_decision_count: int


@dataclass(frozen=True)
class ToolErrorSummary:
    error_kind: str | None
    message: str | None


@dataclass(frozen=True)
class SseSummary:
    time_to_first_frame_s: float | None
    time_to_first_delta_s: float | None
    time_to_first_tool_call_s: float | None
    time_to_completed_s: float | None
    delta_event_count: int
    tool_call_started_count: int
    tool_call_completed_count: int
    answer_length: int
    answer_text: str
    total_reference_count: int
    verification_state: str | None
    completion_state: str | None
    query_execution_id: str | None
    runtime_execution_id: str | None


@dataclass(frozen=True)
class GateCheck:
    label: str
    status: str
    detail: str


NON_ALNUM_RE = re.compile(r"[^0-9a-zа-яё]+", re.IGNORECASE)


class CurlSession:
    def __init__(self, base_url: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.cookie_jar = tempfile.NamedTemporaryFile(
            prefix="ironrag-agent-probe-cookies-", delete=False
        ).name

    def cleanup(self) -> None:
        try:
            os.unlink(self.cookie_jar)
        except FileNotFoundError:
            pass

    def login(self, login: str, password: str) -> None:
        payload = json.dumps({"login": login, "password": password})
        proc = subprocess.run(
            [
                "curl",
                "-s",
                "-c",
                self.cookie_jar,
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "--data",
                payload,
                f"{self.base_url}/v1/iam/session/login",
            ],
            capture_output=True,
            check=False,
            text=True,
        )
        if proc.returncode != 0:
            raise RuntimeError(f"login failed: curl exit {proc.returncode}: {proc.stderr[:200]}")
        try:
            body = json.loads(proc.stdout or "{}")
        except json.JSONDecodeError as exc:
            raise RuntimeError(f"login returned invalid JSON: {proc.stdout[:200]!r}") from exc
        if not body.get("sessionId"):
            raise RuntimeError(f"login failed: {proc.stdout[:200]!r}")

    def request_json(
        self,
        method: str,
        uri: str,
        *,
        body: Any | None = None,
        headers: dict[str, str] | None = None,
        bearer_token: str | None = None,
        accept: str = "application/json",
        timeout_seconds: int = 60,
    ) -> CurlSample:
        payload = json.dumps(body) if body is not None else None
        marker = "__CURL_METRICS__"
        args = [
            "curl",
            "-s",
            "-b",
            self.cookie_jar,
            "-X",
            method,
            "-H",
            f"Accept: {accept}",
            "-w",
            f"\n{marker} %{{http_code}} %{{time_total}} %{{size_download}}",
            "--max-time",
            str(timeout_seconds),
        ]
        if bearer_token:
            args.extend(["-H", f"Authorization: Bearer {bearer_token}"])
        if headers:
            for key, value in headers.items():
                args.extend(["-H", f"{key}: {value}"])
        if payload is not None:
            args.extend(["--data", payload])
        args.append(f"{self.base_url}{uri}")
        proc = subprocess.run(args, capture_output=True, check=False, text=True)
        if proc.returncode != 0:
            raise RuntimeError(
                f"{method} {uri} failed: curl exit {proc.returncode}: {proc.stderr[:200]}"
            )
        stdout = proc.stdout.rstrip()
        marker_index = stdout.rfind(marker)
        if marker_index == -1:
            raise RuntimeError(f"{method} {uri} did not emit curl metrics footer")
        raw_body = stdout[:marker_index].strip()
        footer = stdout[marker_index + len(marker) :].strip()
        status_text, time_text, size_text = footer.split(" ", 2)
        try:
            parsed = json.loads(raw_body) if raw_body else {}
        except json.JSONDecodeError as exc:
            raise RuntimeError(
                f"{method} {uri} returned invalid JSON body: {raw_body[:200]!r}"
            ) from exc
        return CurlSample(
            status_code=int(status_text),
            time_total_s=float(time_text),
            size_download_bytes=int(float(size_text)),
            payload=parsed,
        )


def discover_workspace_id(session: CurlSession, library_id: str) -> str:
    workspaces = session.request_json("GET", "/v1/catalog/workspaces").payload
    if not isinstance(workspaces, list):
        raise RuntimeError("catalog workspaces probe returned a non-list payload")
    for workspace in workspaces:
        workspace_id = workspace.get("id")
        if not workspace_id:
            continue
        libraries = session.request_json(
            "GET", f"/v1/catalog/workspaces/{workspace_id}/libraries"
        ).payload
        if not isinstance(libraries, list):
            continue
        if any(library.get("id") == library_id for library in libraries):
            return str(workspace_id)
    raise RuntimeError(f"failed to discover workspaceId for library {library_id}")


def create_query_session(session: CurlSession, workspace_id: str, library_id: str) -> str:
    sample = session.request_json(
        "POST",
        "/v1/query/sessions",
        body={"workspaceId": workspace_id, "libraryId": library_id},
        headers={"Content-Type": "application/json"},
    )
    if sample.status_code != 200:
        raise RuntimeError(f"create session returned HTTP {sample.status_code}")
    session_id = sample.payload.get("id")
    if not session_id:
        raise RuntimeError(f"create session returned no id: {sample.payload!r}")
    return str(session_id)


def ensure_jsonrpc_result(sample: CurlSample, method_name: str) -> Any:
    if sample.status_code != 200:
        raise RuntimeError(f"{method_name} returned HTTP {sample.status_code}")
    if sample.payload.get("error"):
        raise RuntimeError(f"{method_name} returned JSON-RPC error: {sample.payload['error']!r}")
    if "result" not in sample.payload:
        raise RuntimeError(f"{method_name} returned no result payload")
    return sample.payload["result"]


def probe_mcp_tool(
    session: CurlSession,
    *,
    bearer_token: str | None,
    tool_name: str,
    arguments: dict[str, Any],
) -> CurlSample:
    return session.request_json(
        "POST",
        "/v1/mcp",
        body={
            "jsonrpc": "2.0",
            "id": f"agent-probe-{tool_name}",
            "method": "tools/call",
            "params": {"name": tool_name, "arguments": arguments},
        },
        headers={"Content-Type": "application/json"},
        bearer_token=bearer_token,
        timeout_seconds=120,
    )


def normalize_quality_text(value: Any) -> str:
    if not isinstance(value, str):
        return ""
    lowered = value.casefold()
    collapsed = NON_ALNUM_RE.sub(" ", lowered)
    return " ".join(collapsed.split())


def count_duplicate_keys(keys: list[tuple[Any, ...] | str]) -> int:
    counter = collections.Counter(key for key in keys if key)
    return sum(1 for occurrences in counter.values() if occurrences > 1)


def summarize_graph_quality(payload: dict[str, Any]) -> McpQualitySummary:
    entities = payload.get("entities") or []
    relations = payload.get("relations") or []
    documents = payload.get("documents") or []
    document_links = payload.get("documentLinks") or []

    entity_ids = {
        str(entity.get("entityId")) for entity in entities if entity.get("entityId") is not None
    }
    relation_ids = {
        str(relation.get("relationId"))
        for relation in relations
        if relation.get("relationId") is not None
    }

    orphan_relations = [
        relation
        for relation in relations
        if str(relation.get("sourceEntityId")) not in entity_ids
        or str(relation.get("targetEntityId")) not in entity_ids
    ]
    orphan_links = [
        link
        for link in document_links
        if str(link.get("targetNodeId")) not in entity_ids
        and str(link.get("targetNodeId")) not in relation_ids
    ]
    document_ids = {
        str(document.get("documentId"))
        for document in documents
        if document.get("documentId") is not None
    }
    orphan_documents = [
        link for link in document_links if str(link.get("documentId")) not in document_ids
    ]

    entity_supports = [
        int(entity.get("supportCount") or 0) for entity in entities if entity.get("supportCount") is not None
    ]
    relation_supports = [
        int(relation.get("supportCount") or 0)
        for relation in relations
        if relation.get("supportCount") is not None
    ]
    entity_rank_monotonic = all(
        left >= right for left, right in zip(entity_supports, entity_supports[1:])
    )
    relation_rank_monotonic = all(
        left >= right for left, right in zip(relation_supports, relation_supports[1:])
    )
    document_supports = collections.Counter()
    for link in document_links:
        if link.get("documentId") is None:
            continue
        document_supports[str(link["documentId"])] += int(link.get("supportCount") or 0)
    document_rank_sequence = [
        document_supports[str(document["documentId"])]
        for document in documents
        if document.get("documentId") is not None
    ]
    document_rank_monotonic = all(
        left >= right for left, right in zip(document_rank_sequence, document_rank_sequence[1:])
    )
    duplicate_entity_label_count = count_duplicate_keys(
        [
            normalize_quality_text(entity.get("label"))
            for entity in entities
            if entity.get("label") is not None
        ]
    )
    duplicate_relation_signature_count = count_duplicate_keys(
        [
            (
                str(relation.get("sourceEntityId")),
                normalize_quality_text(relation.get("relationType")),
                str(relation.get("targetEntityId")),
            )
            for relation in relations
        ]
    )
    top_entity_label = entities[0].get("label") if entities else None
    visible_entity_labels_normalized = tuple(
        normalized_label
        for normalized_label in (
            normalize_quality_text(entity.get("label")) for entity in entities
        )
        if normalized_label
    )

    return McpQualitySummary(
        entity_count=len(entities),
        relation_count=len(relations),
        document_count=len(documents),
        document_link_count=len(document_links),
        orphan_relation_count=len(orphan_relations),
        orphan_link_count=len(orphan_links),
        orphan_document_count=len(orphan_documents),
        entity_rank_monotonic=entity_rank_monotonic,
        relation_rank_monotonic=relation_rank_monotonic,
        document_rank_monotonic=document_rank_monotonic,
        duplicate_entity_label_count=duplicate_entity_label_count,
        duplicate_relation_signature_count=duplicate_relation_signature_count,
        top_entity_label=top_entity_label if isinstance(top_entity_label, str) else None,
        visible_entity_labels_normalized=visible_entity_labels_normalized,
    )


def summarize_relation_list(payload: Any) -> RelationListSummary:
    if isinstance(payload, list):
        rows = payload
    elif isinstance(payload, dict):
        candidate_rows = payload.get("relations")
        rows = candidate_rows if isinstance(candidate_rows, list) else []
    else:
        rows = []
    unknown_label_count = sum(
        1
        for row in rows
        if normalize_quality_text(row.get("sourceLabel")) in ("", "unknown")
        or normalize_quality_text(row.get("targetLabel")) in ("", "unknown")
    )
    duplicate_signature_count = count_duplicate_keys(
        [
            (
                normalize_quality_text(row.get("sourceLabel")),
                normalize_quality_text(row.get("relationType")),
                normalize_quality_text(row.get("targetLabel")),
            )
            for row in rows
        ]
    )
    return RelationListSummary(
        row_count=len(rows),
        unknown_label_count=unknown_label_count,
        duplicate_signature_count=duplicate_signature_count,
    )


def summarize_communities(payload: dict[str, Any]) -> CommunitySummary:
    communities = payload.get("communities") or []
    return CommunitySummary(
        count=len(communities),
        communities_with_summary=sum(
            1
            for community in communities
            if isinstance(community, dict) and isinstance(community.get("summary"), str)
        ),
        top_entity_count=sum(
            len(community.get("topEntities") or [])
            for community in communities
            if isinstance(community, dict)
        ),
    )


def summarize_runtime_execution(payload: dict[str, Any]) -> RuntimeExecutionProbeSummary:
    return RuntimeExecutionProbeSummary(
        runtime_execution_id=(
            str(payload.get("runtimeExecutionId"))
            if payload.get("runtimeExecutionId") is not None
            else None
        ),
        lifecycle_state=(
            payload.get("lifecycleState")
            if isinstance(payload.get("lifecycleState"), str)
            else None
        ),
        active_stage=(
            payload.get("activeStage") if isinstance(payload.get("activeStage"), str) else None
        ),
    )


def summarize_runtime_trace(payload: dict[str, Any]) -> RuntimeTraceProbeSummary:
    execution = payload.get("execution") or {}
    return RuntimeTraceProbeSummary(
        runtime_execution_id=(
            str(execution.get("runtimeExecutionId"))
            if execution.get("runtimeExecutionId") is not None
            else None
        ),
        stage_count=len(payload.get("stages") or []),
        action_count=len(payload.get("actions") or []),
        policy_decision_count=len(payload.get("policyDecisions") or []),
    )


def summarize_tool_error(result: dict[str, Any]) -> ToolErrorSummary:
    payload = result.get("structuredContent") or {}
    return ToolErrorSummary(
        error_kind=(
            payload.get("errorKind") if isinstance(payload.get("errorKind"), str) else None
        ),
        message=payload.get("message") if isinstance(payload.get("message"), str) else None,
    )


def total_reference_count(payload: dict[str, Any]) -> int:
    return sum(
        len(payload.get(key) or [])
        for key in (
            "chunkReferences",
            "technicalFactReferences",
            "entityReferences",
            "relationReferences",
            "evidenceReferences",
            "preparedSegmentReferences",
        )
    )


def summarize_entity_search(payload: dict[str, Any]) -> EntitySearchSummary:
    entities = payload.get("entities") or []
    top_hit = entities[0] if entities else {}
    return EntitySearchSummary(
        hit_count=len(entities),
        top_label=(
            top_hit.get("label")
            if isinstance(top_hit, dict) and isinstance(top_hit.get("label"), str)
            else None
        ),
        top_score=(
            float(top_hit["score"])
            if isinstance(top_hit, dict) and top_hit.get("score") is not None
            else None
        ),
    )


def summarize_document_search(payload: dict[str, Any]) -> DocumentSearchSummary:
    hits = payload.get("hits") or []
    top_hit = hits[0] if hits else {}
    top_chunk_refs = len(top_hit.get("chunkReferences") or []) if isinstance(top_hit, dict) else 0
    return DocumentSearchSummary(
        hit_count=len(hits),
        readable_hit_count=sum(
            1 for hit in hits if hit.get("readabilityState") == "readable"
        ),
        top_document_id=(
            str(top_hit.get("documentId"))
            if isinstance(top_hit, dict) and top_hit.get("documentId") is not None
            else None
        ),
        top_document_title=(
            top_hit.get("documentTitle")
            if isinstance(top_hit, dict) and isinstance(top_hit.get("documentTitle"), str)
            else None
        ),
        top_suggested_start_offset=(
            int(top_hit["suggestedStartOffset"])
            if isinstance(top_hit, dict) and top_hit.get("suggestedStartOffset") is not None
            else None
        ),
        top_excerpt_length=(
            len(top_hit.get("excerpt") or "") if isinstance(top_hit, dict) else 0
        ),
        top_chunk_reference_count=top_chunk_refs,
        top_score=(
            float(top_hit["score"])
            if isinstance(top_hit, dict)
            and top_hit.get("score") is not None
            else None
        ),
    )


def summarize_document_read(payload: dict[str, Any]) -> DocumentReadSummary:
    content = payload.get("content") or ""
    return DocumentReadSummary(
        document_id=str(payload.get("documentId")) if payload.get("documentId") is not None else None,
        document_title=payload.get("documentTitle")
        if isinstance(payload.get("documentTitle"), str)
        else None,
        readability_state=payload.get("readabilityState")
        if isinstance(payload.get("readabilityState"), str)
        else None,
        content_length=len(content),
        total_reference_count=total_reference_count(payload),
        has_more=bool(payload.get("hasMore")),
        slice_start_offset=(
            int(payload["sliceStartOffset"])
            if payload.get("sliceStartOffset") is not None
            else None
        ),
        slice_end_offset=(
            int(payload["sliceEndOffset"]) if payload.get("sliceEndOffset") is not None else None
        ),
    )


def stream_assistant_turn(
    session: CurlSession,
    session_id: str,
    question: str,
    *,
    timeout_seconds: int,
) -> SseSummary:
    args = [
        "curl",
        "-sN",
        "-b",
        session.cookie_jar,
        "-X",
        "POST",
        "-H",
        "Content-Type: application/json",
        "-H",
        "Accept: text/event-stream",
        "--data",
        json.dumps({"contentText": question}),
        f"{session.base_url}/v1/query/sessions/{session_id}/turns",
    ]
    started_at = time.monotonic()
    proc = subprocess.Popen(
        args,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    assert proc.stdout is not None

    first_frame_at: float | None = None
    first_delta_at: float | None = None
    first_tool_call_at: float | None = None
    completed_at: float | None = None
    delta_count = 0
    tool_call_started_count = 0
    tool_call_completed_count = 0
    completed_payload: dict[str, Any] | None = None
    stream_error: str | None = None
    current_event = "message"
    current_data_lines: list[str] = []

    while True:
        if time.monotonic() - started_at > timeout_seconds:
            proc.kill()
            raise TimeoutError(f"assistant SSE probe timed out after {timeout_seconds}s")
        if proc.poll() is not None and not select.select([proc.stdout], [], [], 0.0)[0]:
            break
        ready, _, _ = select.select([proc.stdout], [], [], 0.25)
        if not ready:
            continue
        line = proc.stdout.readline()
        if line == "":
            if proc.poll() is not None:
                break
            continue
        stripped = line.rstrip("\n")
        if stripped == "":
            if not current_data_lines:
                current_event = "message"
                continue
            if first_frame_at is None:
                first_frame_at = time.monotonic() - started_at
            event_data = "".join(current_data_lines)
            try:
                payload = json.loads(event_data)
            except json.JSONDecodeError:
                payload = {}
            if current_event == "delta":
                delta_count += 1
                if first_delta_at is None:
                    first_delta_at = time.monotonic() - started_at
            elif current_event == "tool_call_started":
                tool_call_started_count += 1
                if first_tool_call_at is None:
                    first_tool_call_at = time.monotonic() - started_at
            elif current_event == "tool_call_completed":
                tool_call_completed_count += 1
            elif current_event == "completed":
                completed_at = time.monotonic() - started_at
                completed_payload = payload if isinstance(payload, dict) else {}
                break
            elif current_event == "error":
                stream_error = payload.get("error") if isinstance(payload, dict) else event_data
                break
            current_event = "message"
            current_data_lines = []
            continue
        if stripped.startswith("event:"):
            current_event = stripped[6:].strip()
        elif stripped.startswith("data:"):
            current_data_lines.append(stripped[5:].strip())

    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
    stderr = ""
    if proc.stderr is not None:
        stderr = proc.stderr.read().strip()
    if stream_error:
        raise RuntimeError(f"assistant SSE returned error frame: {stream_error}")
    if proc.returncode not in (0, None):
        raise RuntimeError(f"assistant SSE curl exited with {proc.returncode}: {stderr[:200]}")
    if completed_payload is None:
        raise RuntimeError("assistant SSE ended without a completed frame")

    response_turn = completed_payload.get("responseTurn") or {}
    answer_text = response_turn.get("contentText") or ""
    total_refs = total_reference_count(completed_payload)
    verification_state = completed_payload.get("verificationState")
    execution = completed_payload.get("execution") or {}
    completion_state = execution.get("lifecycleState")
    query_execution_id = execution.get("id")
    runtime_execution_id = execution.get("runtimeExecutionId")

    return SseSummary(
        time_to_first_frame_s=first_frame_at,
        time_to_first_delta_s=first_delta_at,
        time_to_first_tool_call_s=first_tool_call_at,
        time_to_completed_s=completed_at,
        delta_event_count=delta_count,
        tool_call_started_count=tool_call_started_count,
        tool_call_completed_count=tool_call_completed_count,
        answer_length=len(answer_text),
        answer_text=answer_text,
        total_reference_count=total_refs,
        verification_state=verification_state if isinstance(verification_state, str) else None,
        completion_state=completion_state if isinstance(completion_state, str) else None,
        query_execution_id=query_execution_id if isinstance(query_execution_id, str) else None,
        runtime_execution_id=(
            runtime_execution_id if isinstance(runtime_execution_id, str) else None
        ),
    )


def format_seconds(value: float | None) -> str:
    if value is None:
        return "n/a"
    return f"{value * 1000:.0f} ms"


def format_bytes(value: int) -> str:
    if value < 1024:
        return f"{value} B"
    if value < 1024 * 1024:
        return f"{value / 1024:.1f} KiB"
    return f"{value / (1024 * 1024):.2f} MiB"


def format_preview(text: str, limit: int = 280) -> str:
    collapsed = " ".join(text.split())
    if len(collapsed) <= limit:
        return collapsed or "n/a"
    return f"{collapsed[: limit - 3].rstrip()}..."


def parse_csv_terms(value: str) -> list[str]:
    return [term.strip() for term in value.split(",") if term.strip()]


def contains_all_terms(text: str, required_terms: list[str]) -> bool:
    text_folded = text.casefold()
    return all(term.casefold() in text_folded for term in required_terms)


def contains_any_term(text: str, candidate_terms: list[str]) -> bool:
    text_folded = text.casefold()
    return any(term.casefold() in text_folded for term in candidate_terms)


def build_gate_checks(
    *,
    entity_search_summary: EntitySearchSummary,
    document_search_summary: DocumentSearchSummary,
    document_read_summary: DocumentReadSummary | None,
    graph_quality: McpQualitySummary,
    relation_list_summary: RelationListSummary,
    community_summary: CommunitySummary,
    assistant_summaries: list[SseSummary],
    runtime_execution_summary: RuntimeExecutionProbeSummary | None,
    runtime_trace_summary: RuntimeTraceProbeSummary | None,
    legacy_runtime_execution_error: ToolErrorSummary | None,
    graph_min_entities: int,
    graph_min_relations: int,
    graph_min_documents: int,
    community_min_count: int,
    entity_search_min_hits: int,
    search_min_hits: int,
    search_min_readable_hits: int,
    read_min_content_chars: int,
    read_min_references: int,
    assistant_min_references: int,
    assistant_expected_verification: str,
    assistant_require_all: list[str],
    assistant_forbid_any: list[str],
    assistant_max_tool_starts: int | None,
    expected_search_top_label: str | None,
    max_tool_latency_ms: int | None,
    max_first_delta_ms: int | None,
    max_completed_ms: int | None,
    tool_samples: list[tuple[str, CurlSample]],
) -> list[GateCheck]:
    checks: list[GateCheck] = []

    checks.append(
        GateCheck(
            label="graph.entities",
            status="pass" if graph_quality.entity_count >= graph_min_entities else "fail",
            detail=f"entities={graph_quality.entity_count} min={graph_min_entities}",
        )
    )
    checks.append(
        GateCheck(
            label="graph.search_entities_hits",
            status=(
                "pass" if entity_search_summary.hit_count >= entity_search_min_hits else "fail"
            ),
            detail=f"hits={entity_search_summary.hit_count} min={entity_search_min_hits}",
        )
    )
    checks.append(
        GateCheck(
            label="graph.relations",
            status="pass" if graph_quality.relation_count >= graph_min_relations else "fail",
            detail=f"relations={graph_quality.relation_count} min={graph_min_relations}",
        )
    )
    checks.append(
        GateCheck(
            label="graph.documents",
            status="pass" if graph_quality.document_count >= graph_min_documents else "fail",
            detail=f"documents={graph_quality.document_count} min={graph_min_documents}",
        )
    )
    checks.append(
        GateCheck(
            label="graph.coherence",
            status="pass" if graph_quality.quality_status == "pass" else "fail",
            detail=(
                f"quality={graph_quality.quality_status} "
                f"orphan_relations={graph_quality.orphan_relation_count} "
                f"orphan_links={graph_quality.orphan_link_count} "
                f"orphan_documents={graph_quality.orphan_document_count}"
            ),
        )
    )
    checks.append(
        GateCheck(
            label="graph.document_links_visible_documents",
            status="pass" if graph_quality.orphan_document_count == 0 else "fail",
            detail=f"orphan_documents={graph_quality.orphan_document_count}",
        )
    )
    checks.append(
        GateCheck(
            label="graph.documents_ranked_by_support",
            status="pass" if graph_quality.document_rank_monotonic else "fail",
            detail=f"document_rank_monotonic={graph_quality.document_rank_monotonic}",
        )
    )
    checks.append(
        GateCheck(
            label="graph.duplicate_entity_labels",
            status="pass" if graph_quality.duplicate_entity_label_count == 0 else "fail",
            detail=f"duplicate_entity_labels={graph_quality.duplicate_entity_label_count}",
        )
    )
    checks.append(
        GateCheck(
            label="graph.duplicate_relation_signatures",
            status="pass" if graph_quality.duplicate_relation_signature_count == 0 else "fail",
            detail=(
                "duplicate_relation_signatures="
                f"{graph_quality.duplicate_relation_signature_count}"
            ),
        )
    )
    checks.append(
        GateCheck(
            label="graph.list_relations_labels",
            status="pass" if relation_list_summary.unknown_label_count == 0 else "fail",
            detail=f"unknown_labels={relation_list_summary.unknown_label_count}",
        )
    )
    checks.append(
        GateCheck(
            label="graph.list_relations_duplicates",
            status="pass" if relation_list_summary.duplicate_signature_count == 0 else "fail",
            detail=f"duplicate_signatures={relation_list_summary.duplicate_signature_count}",
        )
    )
    checks.append(
        GateCheck(
            label="graph.communities",
            status="pass" if community_summary.count >= community_min_count else "fail",
            detail=f"communities={community_summary.count} min={community_min_count}",
        )
    )
    checks.append(
        GateCheck(
            label="graph.community_summaries",
            status=(
                "pass"
                if community_summary.count == 0
                or community_summary.communities_with_summary == community_summary.count
                else "fail"
            ),
            detail=(
                f"with_summary={community_summary.communities_with_summary} "
                f"count={community_summary.count}"
            ),
        )
    )

    search_top_label = entity_search_summary.top_label
    if expected_search_top_label is not None:
        checks.append(
            GateCheck(
                label="graph.search_top_label",
                status="pass" if search_top_label == expected_search_top_label else "fail",
                detail=f"top={search_top_label or 'n/a'} expected={expected_search_top_label}",
            )
        )
    elif search_top_label is not None and graph_quality.top_entity_label is not None:
        normalized_top_label = normalize_quality_text(search_top_label)
        search_visible_in_topology = (
            normalized_top_label in graph_quality.visible_entity_labels_normalized
            if normalized_top_label
            else False
        )
        checks.append(
            GateCheck(
                label="graph.search_alignment",
                status="pass" if search_visible_in_topology else "warn",
                detail=(
                    f"search_top={search_top_label} "
                    f"visible_in_topology={search_visible_in_topology} "
                    f"topology_top={graph_quality.top_entity_label}"
                ),
            )
        )

    checks.append(
        GateCheck(
            label="mcp.search_documents_hits",
            status="pass" if document_search_summary.hit_count >= search_min_hits else "fail",
            detail=f"hits={document_search_summary.hit_count} min={search_min_hits}",
        )
    )
    checks.append(
        GateCheck(
            label="mcp.search_documents_readable_hits",
            status=(
                "pass"
                if document_search_summary.readable_hit_count >= search_min_readable_hits
                else "fail"
            ),
            detail=(
                f"readable_hits={document_search_summary.readable_hit_count} "
                f"min={search_min_readable_hits}"
            ),
        )
    )
    checks.append(
        GateCheck(
            label="mcp.search_documents_guidance",
            status=(
                "pass"
                if document_search_summary.top_suggested_start_offset is not None
                else "fail"
            ),
            detail=(
                "top_hit suggestedStartOffset present"
                if document_search_summary.top_suggested_start_offset is not None
                else "top_hit suggestedStartOffset missing"
            ),
        )
    )

    if document_read_summary is None:
        checks.append(
            GateCheck(
                label="mcp.read_document",
                status="fail",
                detail="no readable top hit available for read_document probe",
            )
        )
    else:
        checks.append(
            GateCheck(
                label="mcp.read_document_readability",
                status=(
                    "pass" if document_read_summary.readability_state == "readable" else "fail"
                ),
                detail=f"readability={document_read_summary.readability_state or 'n/a'}",
            )
        )
        checks.append(
            GateCheck(
                label="mcp.read_document_content",
                status=(
                    "pass"
                    if document_read_summary.content_length >= read_min_content_chars
                    else "fail"
                ),
                detail=(
                    f"content_chars={document_read_summary.content_length} "
                    f"min={read_min_content_chars}"
                ),
            )
        )
        checks.append(
            GateCheck(
                label="mcp.read_document_references",
                status=(
                    "pass"
                    if document_read_summary.total_reference_count >= read_min_references
                    else "fail"
                ),
                detail=(
                    f"references={document_read_summary.total_reference_count} "
                    f"min={read_min_references}"
                ),
            )
        )
        checks.append(
            GateCheck(
                label="mcp.read_document_alignment",
                status=(
                    "pass"
                    if document_read_summary.document_id == document_search_summary.top_document_id
                    else "fail"
                ),
                detail=(
                    f"read_document_id={document_read_summary.document_id} "
                    f"search_top_document_id={document_search_summary.top_document_id}"
                ),
            )
        )
        if document_search_summary.top_suggested_start_offset is not None:
            checks.append(
                GateCheck(
                    label="mcp.read_document_offset_alignment",
                    status=(
                        "pass"
                        if document_read_summary.slice_start_offset
                        == document_search_summary.top_suggested_start_offset
                        else "fail"
                    ),
                    detail=(
                        f"slice_start={document_read_summary.slice_start_offset} "
                        f"suggested_start={document_search_summary.top_suggested_start_offset}"
                    ),
                )
            )

    for idx, summary in enumerate(assistant_summaries, start=1):
        checks.append(
            GateCheck(
                label=f"assistant.run_{idx}.verification",
                status=(
                    "pass"
                    if summary.verification_state == assistant_expected_verification
                    else "fail"
                ),
                detail=(
                    f"verification={summary.verification_state or 'n/a'} "
                    f"expected={assistant_expected_verification}"
                ),
            )
        )
        checks.append(
            GateCheck(
                label=f"assistant.run_{idx}.references",
                status=(
                    "pass"
                    if summary.total_reference_count >= assistant_min_references
                    else "fail"
                ),
                detail=(
                    f"references={summary.total_reference_count} min={assistant_min_references}"
                ),
            )
        )
        checks.append(
            GateCheck(
                label=f"assistant.run_{idx}.delta",
                status="pass" if summary.time_to_first_delta_s is not None else "fail",
                detail=(
                    f"first_delta={format_seconds(summary.time_to_first_delta_s)}"
                    if summary.time_to_first_delta_s is not None
                    else "missing first delta"
                ),
            )
        )
        checks.append(
            GateCheck(
                label=f"assistant.run_{idx}.completed",
                status="pass" if summary.time_to_completed_s is not None else "fail",
                detail=(
                    f"completed={format_seconds(summary.time_to_completed_s)}"
                    if summary.time_to_completed_s is not None
                    else "missing completion"
                ),
            )
        )
        if assistant_require_all:
            checks.append(
                GateCheck(
                    label=f"assistant.run_{idx}.required_terms",
                    status=(
                        "pass"
                        if contains_all_terms(summary.answer_text, assistant_require_all)
                        else "fail"
                    ),
                    detail=f"required={assistant_require_all}",
                )
            )
        if assistant_forbid_any:
            checks.append(
                GateCheck(
                    label=f"assistant.run_{idx}.forbidden_terms",
                    status=(
                        "fail"
                        if contains_any_term(summary.answer_text, assistant_forbid_any)
                        else "pass"
                    ),
                    detail=f"forbidden={assistant_forbid_any}",
                )
            )
        if max_first_delta_ms is not None:
            first_delta_ms = (
                int(summary.time_to_first_delta_s * 1000)
                if summary.time_to_first_delta_s is not None
                else None
            )
            checks.append(
                GateCheck(
                    label=f"assistant.run_{idx}.first_delta_budget",
                    status=(
                        "pass"
                        if first_delta_ms is not None and first_delta_ms <= max_first_delta_ms
                        else "fail"
                    ),
                    detail=f"first_delta_ms={first_delta_ms} max={max_first_delta_ms}",
                )
            )
        if max_completed_ms is not None:
            completed_ms = (
                int(summary.time_to_completed_s * 1000)
                if summary.time_to_completed_s is not None
                else None
            )
            checks.append(
                GateCheck(
                    label=f"assistant.run_{idx}.completed_budget",
                    status=(
                        "pass"
                        if completed_ms is not None and completed_ms <= max_completed_ms
                        else "fail"
                    ),
                    detail=f"completed_ms={completed_ms} max={max_completed_ms}",
                )
            )
        if assistant_max_tool_starts is not None:
            checks.append(
                GateCheck(
                    label=f"assistant.run_{idx}.tool_start_budget",
                    status=(
                        "pass"
                        if summary.tool_call_started_count <= assistant_max_tool_starts
                        else "fail"
                    ),
                    detail=(
                        f"tool_starts={summary.tool_call_started_count} "
                        f"max={assistant_max_tool_starts}"
                    ),
                )
            )

    runtime_ids = [
        summary.runtime_execution_id for summary in assistant_summaries if summary.runtime_execution_id
    ]
    first_runtime_id = runtime_ids[0] if runtime_ids else None
    checks.append(
        GateCheck(
            label="assistant.runtime_execution_id",
            status="pass" if first_runtime_id is not None else "fail",
            detail=f"runtimeExecutionId={first_runtime_id or 'missing'}",
        )
    )
    if runtime_execution_summary is None:
        checks.append(
            GateCheck(
                label="mcp.get_runtime_execution",
                status="fail",
                detail="runtime execution probe missing",
            )
        )
    else:
        checks.append(
            GateCheck(
                label="mcp.get_runtime_execution_alignment",
                status=(
                    "pass"
                    if runtime_execution_summary.runtime_execution_id == first_runtime_id
                    else "fail"
                ),
                detail=(
                    "probe="
                    f"{runtime_execution_summary.runtime_execution_id or 'missing'} "
                    f"assistant={first_runtime_id or 'missing'}"
                ),
            )
        )
        checks.append(
            GateCheck(
                label="mcp.get_runtime_execution_lifecycle",
                status=(
                    "pass"
                    if runtime_execution_summary.lifecycle_state == "completed"
                    else "fail"
                ),
                detail=f"lifecycle={runtime_execution_summary.lifecycle_state or 'missing'}",
            )
        )
    if runtime_trace_summary is None:
        checks.append(
            GateCheck(
                label="mcp.get_runtime_execution_trace",
                status="fail",
                detail="runtime trace probe missing",
            )
        )
    else:
        checks.append(
            GateCheck(
                label="mcp.get_runtime_execution_trace_alignment",
                status=(
                    "pass"
                    if runtime_trace_summary.runtime_execution_id == first_runtime_id
                    else "fail"
                ),
                detail=(
                    "probe="
                    f"{runtime_trace_summary.runtime_execution_id or 'missing'} "
                    f"assistant={first_runtime_id or 'missing'}"
                ),
            )
        )
        checks.append(
            GateCheck(
                label="mcp.get_runtime_execution_trace_stages",
                status="pass" if runtime_trace_summary.stage_count >= 1 else "fail",
                detail=f"stage_count={runtime_trace_summary.stage_count}",
            )
        )
    if legacy_runtime_execution_error is None:
        checks.append(
            GateCheck(
                label="mcp.get_runtime_execution_legacy_field_rejected",
                status="fail",
                detail="legacy executionId rejection probe missing",
            )
        )
    else:
        checks.append(
            GateCheck(
                label="mcp.get_runtime_execution_legacy_field_rejected",
                status=(
                    "pass"
                    if legacy_runtime_execution_error.error_kind == "invalid_mcp_tool_call"
                    and legacy_runtime_execution_error.message is not None
                    and "runtimeExecutionId" in legacy_runtime_execution_error.message
                    else "fail"
                ),
                detail=(
                    f"error_kind={legacy_runtime_execution_error.error_kind or 'missing'} "
                    f"message={legacy_runtime_execution_error.message or 'missing'}"
                ),
            )
        )

    if max_tool_latency_ms is not None:
        for name, sample in tool_samples:
            tool_ms = int(sample.time_total_s * 1000)
            checks.append(
                GateCheck(
                    label=f"tool.{name}.latency_budget",
                    status="pass" if tool_ms <= max_tool_latency_ms else "fail",
                    detail=f"latency_ms={tool_ms} max={max_tool_latency_ms}",
                )
            )

    return checks


def render_report(
    *,
    output_path: pathlib.Path,
    base_url: str,
    library_id: str,
    workspace_id: str,
    tools_list: CurlSample,
    entity_search: CurlSample,
    entity_search_summary: EntitySearchSummary,
    document_search: CurlSample,
    document_search_summary: DocumentSearchSummary,
    document_read: CurlSample | None,
    document_read_summary: DocumentReadSummary | None,
    graph_topology: CurlSample,
    list_relations: CurlSample,
    communities: CurlSample,
    community_summary: CommunitySummary,
    runtime_execution: CurlSample | None,
    runtime_execution_summary: RuntimeExecutionProbeSummary | None,
    runtime_trace: CurlSample | None,
    runtime_trace_summary: RuntimeTraceProbeSummary | None,
    legacy_runtime_execution_probe: CurlSample | None,
    legacy_runtime_execution_error: ToolErrorSummary | None,
    graph_quality: McpQualitySummary,
    relation_list_summary: RelationListSummary,
    assistant_summaries: list[SseSummary],
    gate_checks: list[GateCheck],
) -> None:
    avg_completed = statistics.mean(
        summary.time_to_completed_s or 0.0 for summary in assistant_summaries
    )
    avg_first_delta = statistics.mean(
        summary.time_to_first_delta_s or 0.0 for summary in assistant_summaries
    )
    report = f"""# Agent surface profile

- Generated at: {datetime.now(timezone.utc).isoformat()}
- Base URL: `{base_url}`
- Library ID: `{library_id}`
- Workspace ID: `{workspace_id}`

## MCP probes

| Probe | HTTP | Time | Size | Notes |
|---|---:|---:|---:|---|
| `tools/list` | {tools_list.status_code} | {format_seconds(tools_list.time_total_s)} | {format_bytes(tools_list.size_download_bytes)} | tools={len((tools_list.payload.get("result") or {}).get("tools") or [])} |
| `search_entities` | {entity_search.status_code} | {format_seconds(entity_search.time_total_s)} | {format_bytes(entity_search.size_download_bytes)} | hits={entity_search_summary.hit_count} top={entity_search_summary.top_label or "n/a"} |
| `search_documents` | {document_search.status_code} | {format_seconds(document_search.time_total_s)} | {format_bytes(document_search.size_download_bytes)} | hits={document_search_summary.hit_count} top={document_search_summary.top_document_title or "n/a"} |
| `read_document` | {(document_read.status_code if document_read else "n/a")} | {(format_seconds(document_read.time_total_s) if document_read else "n/a")} | {(format_bytes(document_read.size_download_bytes) if document_read else "n/a")} | chars={(document_read_summary.content_length if document_read_summary else 0)} refs={(document_read_summary.total_reference_count if document_read_summary else 0)} |
| `get_graph_topology` | {graph_topology.status_code} | {format_seconds(graph_topology.time_total_s)} | {format_bytes(graph_topology.size_download_bytes)} | quality={graph_quality.quality_status} entities={graph_quality.entity_count} relations={graph_quality.relation_count} docs={graph_quality.document_count} |
| `list_relations` | {list_relations.status_code} | {format_seconds(list_relations.time_total_s)} | {format_bytes(list_relations.size_download_bytes)} | rows={relation_list_summary.row_count} |
| `get_communities` | {communities.status_code} | {format_seconds(communities.time_total_s)} | {format_bytes(communities.size_download_bytes)} | communities={community_summary.count} summarized={community_summary.communities_with_summary} |
| `get_runtime_execution` | {(runtime_execution.status_code if runtime_execution else "n/a")} | {(format_seconds(runtime_execution.time_total_s) if runtime_execution else "n/a")} | {(format_bytes(runtime_execution.size_download_bytes) if runtime_execution else "n/a")} | lifecycle={(runtime_execution_summary.lifecycle_state if runtime_execution_summary else "n/a")} |
| `get_runtime_execution_trace` | {(runtime_trace.status_code if runtime_trace else "n/a")} | {(format_seconds(runtime_trace.time_total_s) if runtime_trace else "n/a")} | {(format_bytes(runtime_trace.size_download_bytes) if runtime_trace else "n/a")} | stages={(runtime_trace_summary.stage_count if runtime_trace_summary else 0)} actions={(runtime_trace_summary.action_count if runtime_trace_summary else 0)} |
| `get_runtime_execution (legacy executionId)` | {(legacy_runtime_execution_probe.status_code if legacy_runtime_execution_probe else "n/a")} | {(format_seconds(legacy_runtime_execution_probe.time_total_s) if legacy_runtime_execution_probe else "n/a")} | {(format_bytes(legacy_runtime_execution_probe.size_download_bytes) if legacy_runtime_execution_probe else "n/a")} | error={(legacy_runtime_execution_error.error_kind if legacy_runtime_execution_error else "n/a")} |

### Graph quality checks

| Check | Value |
|---|---|
| entity rank monotonic | {graph_quality.entity_rank_monotonic} |
| relation rank monotonic | {graph_quality.relation_rank_monotonic} |
| document rank monotonic | {graph_quality.document_rank_monotonic} |
| orphan relations | {graph_quality.orphan_relation_count} |
| orphan links | {graph_quality.orphan_link_count} |
| orphan documents | {graph_quality.orphan_document_count} |
| duplicate entity labels | {graph_quality.duplicate_entity_label_count} |
| duplicate relation signatures | {graph_quality.duplicate_relation_signature_count} |
| top entity label | {graph_quality.top_entity_label or "n/a"} |

### `list_relations` quality checks

| Check | Value |
|---|---|
| relation rows | {relation_list_summary.row_count} |
| unknown endpoint labels | {relation_list_summary.unknown_label_count} |
| duplicate relation signatures | {relation_list_summary.duplicate_signature_count} |

### Community checks

| Check | Value |
|---|---|
| community rows | {community_summary.count} |
| summaries present | {community_summary.communities_with_summary} |
| total top entities surfaced | {community_summary.top_entity_count} |

## MCP document retrieval checks

| Check | Value |
|---|---|
| search hits | {document_search_summary.hit_count} |
| readable search hits | {document_search_summary.readable_hit_count} |
| top document title | {document_search_summary.top_document_title or "n/a"} |
| top suggestedStartOffset | {document_search_summary.top_suggested_start_offset if document_search_summary.top_suggested_start_offset is not None else "n/a"} |
| top excerpt chars | {document_search_summary.top_excerpt_length} |
| top hit chunk refs | {document_search_summary.top_chunk_reference_count} |
| read content chars | {document_read_summary.content_length if document_read_summary else 0} |
| read references | {document_read_summary.total_reference_count if document_read_summary else 0} |
| read readability | {document_read_summary.readability_state if document_read_summary else "n/a"} |

## Assistant and runtime probes

| Runs | Avg first delta | Avg completed | Avg references | Avg tool starts |
|---:|---:|---:|---:|---:|
| {len(assistant_summaries)} | {format_seconds(avg_first_delta)} | {format_seconds(avg_completed)} | {statistics.mean(summary.total_reference_count for summary in assistant_summaries):.1f} | {statistics.mean(summary.tool_call_started_count for summary in assistant_summaries):.1f} |

| Run | First frame | First delta | First tool call | Completed | Deltas | Tool starts | Tool completes | Answer chars | References | Verification | Query execution | Runtime execution | Lifecycle |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
"""
    for idx, summary in enumerate(assistant_summaries, start=1):
        report += (
            f"| {idx} | {format_seconds(summary.time_to_first_frame_s)}"
            f" | {format_seconds(summary.time_to_first_delta_s)}"
            f" | {format_seconds(summary.time_to_first_tool_call_s)}"
            f" | {format_seconds(summary.time_to_completed_s)}"
            f" | {summary.delta_event_count}"
            f" | {summary.tool_call_started_count}"
            f" | {summary.tool_call_completed_count}"
            f" | {summary.answer_length}"
            f" | {summary.total_reference_count}"
            f" | {summary.verification_state or 'n/a'}"
            f" | {summary.query_execution_id or 'n/a'}"
            f" | {summary.runtime_execution_id or 'n/a'}"
            f" | {summary.completion_state or 'n/a'} |\n"
        )
    report += f"""

### Runtime lookup checks

| Check | Value |
|---|---|
| runtime execution id | {runtime_execution_summary.runtime_execution_id if runtime_execution_summary else "n/a"} |
| runtime lifecycle | {runtime_execution_summary.lifecycle_state if runtime_execution_summary else "n/a"} |
| runtime active stage | {runtime_execution_summary.active_stage if runtime_execution_summary else "n/a"} |
| runtime trace stages | {runtime_trace_summary.stage_count if runtime_trace_summary else 0} |
| runtime trace actions | {runtime_trace_summary.action_count if runtime_trace_summary else 0} |
| runtime trace policy decisions | {runtime_trace_summary.policy_decision_count if runtime_trace_summary else 0} |
| legacy runtime field rejection | {legacy_runtime_execution_error.error_kind if legacy_runtime_execution_error else "n/a"} |

### Assistant answer previews

| Run | Answer preview |
|---|---|
"""
    for idx, summary in enumerate(assistant_summaries, start=1):
        preview = format_preview(summary.answer_text).replace("|", "\\|")
        report += f"| {idx} | {preview} |\n"
    report += """

## Release gate

| Check | Status | Detail |
|---|---|---|
"""
    for check in gate_checks:
        report += f"| `{check.label}` | {check.status} | {check.detail} |\n"
    output_path.write_text(report, encoding="utf-8")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Profile MCP graph and assistant SSE surfaces.")
    parser.add_argument("--base-url", default=DEFAULT_BASE_URL)
    parser.add_argument("--login", default=DEFAULT_LOGIN)
    parser.add_argument("--password", default=DEFAULT_PASSWORD)
    parser.add_argument("--library-id", required=True)
    parser.add_argument("--workspace-id")
    parser.add_argument("--mcp-token", default=os.environ.get("IRONRAG_MCP_TOKEN"))
    parser.add_argument("--entity-query", default=DEFAULT_ENTITY_QUERY)
    parser.add_argument("--document-query", default=DEFAULT_DOCUMENT_QUERY)
    parser.add_argument("--document-limit", type=int, default=DEFAULT_DOCUMENT_LIMIT)
    parser.add_argument("--graph-limit", type=int, default=50)
    parser.add_argument("--read-length", type=int, default=DEFAULT_READ_LENGTH)
    parser.add_argument("--question", default=DEFAULT_ASSISTANT_QUESTION)
    parser.add_argument("--sse-runs", type=int, default=2)
    parser.add_argument("--graph-min-entities", type=int, default=DEFAULT_GRAPH_MIN_ENTITIES)
    parser.add_argument("--graph-min-relations", type=int, default=DEFAULT_GRAPH_MIN_RELATIONS)
    parser.add_argument("--graph-min-documents", type=int, default=DEFAULT_GRAPH_MIN_DOCUMENTS)
    parser.add_argument("--community-min-count", type=int, default=DEFAULT_COMMUNITY_MIN_COUNT)
    parser.add_argument(
        "--entity-search-min-hits", type=int, default=DEFAULT_ENTITY_SEARCH_MIN_HITS
    )
    parser.add_argument("--search-min-hits", type=int, default=DEFAULT_SEARCH_MIN_HITS)
    parser.add_argument(
        "--search-min-readable-hits", type=int, default=DEFAULT_SEARCH_MIN_READABLE_HITS
    )
    parser.add_argument("--read-min-content-chars", type=int, default=DEFAULT_READ_MIN_CONTENT_CHARS)
    parser.add_argument("--read-min-references", type=int, default=DEFAULT_READ_MIN_REFERENCES)
    parser.add_argument(
        "--assistant-min-references", type=int, default=DEFAULT_ASSISTANT_MIN_REFERENCES
    )
    parser.add_argument(
        "--assistant-expected-verification",
        default=DEFAULT_ASSISTANT_EXPECTED_VERIFICATION,
    )
    parser.add_argument("--assistant-require-all", default="")
    parser.add_argument("--assistant-forbid-any", default="")
    parser.add_argument("--assistant-max-tool-starts", type=int)
    parser.add_argument("--expected-search-top-label")
    parser.add_argument("--max-tool-latency-ms", type=int)
    parser.add_argument("--max-first-delta-ms", type=int)
    parser.add_argument("--max-completed-ms", type=int)
    parser.add_argument("--timeout-seconds", type=int, default=120)
    parser.add_argument("--output-path")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    session = CurlSession(args.base_url)
    try:
        session.login(args.login, args.password)
        workspace_id = args.workspace_id or discover_workspace_id(session, args.library_id)

        tools_list = session.request_json(
            "POST",
            "/v1/mcp",
            body={
                "jsonrpc": "2.0",
                "id": "agent-probe-tools-list",
                "method": "tools/list",
                "params": {},
            },
            headers={"Content-Type": "application/json"},
            bearer_token=args.mcp_token,
            timeout_seconds=args.timeout_seconds,
        )
        ensure_jsonrpc_result(tools_list, "tools/list")

        entity_search = probe_mcp_tool(
            session,
            bearer_token=args.mcp_token,
            tool_name="search_entities",
            arguments={"libraryId": args.library_id, "query": args.entity_query, "limit": 10},
        )
        entity_search_result = ensure_jsonrpc_result(entity_search, "search_entities")
        if entity_search_result.get("isError"):
            raise RuntimeError(f"search_entities returned tool error: {entity_search_result!r}")
        entity_search_summary = summarize_entity_search(
            entity_search_result.get("structuredContent") or {}
        )

        document_search = probe_mcp_tool(
            session,
            bearer_token=args.mcp_token,
            tool_name="search_documents",
            arguments={
                "query": args.document_query,
                "libraryIds": [args.library_id],
                "limit": args.document_limit,
                "includeReferences": True,
            },
        )
        document_search_result = ensure_jsonrpc_result(document_search, "search_documents")
        if document_search_result.get("isError"):
            raise RuntimeError(
                f"search_documents returned tool error: {document_search_result!r}"
            )
        document_search_summary = summarize_document_search(
            document_search_result.get("structuredContent") or {}
        )

        document_read: CurlSample | None = None
        document_read_summary: DocumentReadSummary | None = None
        if document_search_summary.top_document_id is not None:
            read_arguments: dict[str, Any] = {
                "documentId": document_search_summary.top_document_id,
                "mode": "excerpt",
                "length": args.read_length,
                "includeReferences": True,
            }
            if document_search_summary.top_suggested_start_offset is not None:
                read_arguments["startOffset"] = document_search_summary.top_suggested_start_offset
            document_read = probe_mcp_tool(
                session,
                bearer_token=args.mcp_token,
                tool_name="read_document",
                arguments=read_arguments,
            )
            document_read_result = ensure_jsonrpc_result(document_read, "read_document")
            if document_read_result.get("isError"):
                raise RuntimeError(f"read_document returned tool error: {document_read_result!r}")
            document_read_summary = summarize_document_read(
                document_read_result.get("structuredContent") or {}
            )

        graph_topology = probe_mcp_tool(
            session,
            bearer_token=args.mcp_token,
            tool_name="get_graph_topology",
            arguments={"libraryId": args.library_id, "limit": args.graph_limit},
        )
        topology_result = ensure_jsonrpc_result(graph_topology, "get_graph_topology")
        if topology_result.get("isError"):
            raise RuntimeError(f"get_graph_topology returned tool error: {topology_result!r}")
        graph_quality = summarize_graph_quality(topology_result.get("structuredContent") or {})

        list_relations = probe_mcp_tool(
            session,
            bearer_token=args.mcp_token,
            tool_name="list_relations",
            arguments={"libraryId": args.library_id, "limit": args.graph_limit},
        )
        list_relations_result = ensure_jsonrpc_result(list_relations, "list_relations")
        relation_list_summary = summarize_relation_list(
            list_relations_result.get("structuredContent") or []
        )

        communities = probe_mcp_tool(
            session,
            bearer_token=args.mcp_token,
            tool_name="get_communities",
            arguments={"libraryId": args.library_id, "limit": args.graph_limit},
        )
        communities_result = ensure_jsonrpc_result(communities, "get_communities")
        if communities_result.get("isError"):
            raise RuntimeError(f"get_communities returned tool error: {communities_result!r}")
        community_summary = summarize_communities(
            communities_result.get("structuredContent") or {}
        )

        assistant_summaries: list[SseSummary] = []
        for _ in range(args.sse_runs):
            query_session_id = create_query_session(session, workspace_id, args.library_id)
            assistant_summaries.append(
                stream_assistant_turn(
                    session,
                    query_session_id,
                    args.question,
                    timeout_seconds=args.timeout_seconds,
                )
            )

        first_runtime_execution_id = next(
            (
                summary.runtime_execution_id
                for summary in assistant_summaries
                if summary.runtime_execution_id is not None
            ),
            None,
        )
        runtime_execution: CurlSample | None = None
        runtime_execution_summary: RuntimeExecutionProbeSummary | None = None
        runtime_trace: CurlSample | None = None
        runtime_trace_summary: RuntimeTraceProbeSummary | None = None
        legacy_runtime_execution_probe: CurlSample | None = None
        legacy_runtime_execution_error: ToolErrorSummary | None = None
        if first_runtime_execution_id is not None:
            runtime_execution = probe_mcp_tool(
                session,
                bearer_token=args.mcp_token,
                tool_name="get_runtime_execution",
                arguments={"runtimeExecutionId": first_runtime_execution_id},
            )
            runtime_execution_result = ensure_jsonrpc_result(
                runtime_execution, "get_runtime_execution"
            )
            if runtime_execution_result.get("isError"):
                raise RuntimeError(
                    "get_runtime_execution returned tool error: "
                    f"{runtime_execution_result!r}"
                )
            runtime_execution_summary = summarize_runtime_execution(
                runtime_execution_result.get("structuredContent") or {}
            )

            runtime_trace = probe_mcp_tool(
                session,
                bearer_token=args.mcp_token,
                tool_name="get_runtime_execution_trace",
                arguments={"runtimeExecutionId": first_runtime_execution_id},
            )
            runtime_trace_result = ensure_jsonrpc_result(
                runtime_trace, "get_runtime_execution_trace"
            )
            if runtime_trace_result.get("isError"):
                raise RuntimeError(
                    "get_runtime_execution_trace returned tool error: "
                    f"{runtime_trace_result!r}"
                )
            runtime_trace_summary = summarize_runtime_trace(
                runtime_trace_result.get("structuredContent") or {}
            )

            legacy_runtime_execution_probe = probe_mcp_tool(
                session,
                bearer_token=args.mcp_token,
                tool_name="get_runtime_execution",
                arguments={"executionId": first_runtime_execution_id},
            )
            legacy_runtime_execution_result = ensure_jsonrpc_result(
                legacy_runtime_execution_probe, "get_runtime_execution legacy executionId"
            )
            legacy_runtime_execution_error = summarize_tool_error(
                legacy_runtime_execution_result
            )

        gate_checks = build_gate_checks(
            entity_search_summary=entity_search_summary,
            document_search_summary=document_search_summary,
            document_read_summary=document_read_summary,
            graph_quality=graph_quality,
            relation_list_summary=relation_list_summary,
            community_summary=community_summary,
            assistant_summaries=assistant_summaries,
            runtime_execution_summary=runtime_execution_summary,
            runtime_trace_summary=runtime_trace_summary,
            legacy_runtime_execution_error=legacy_runtime_execution_error,
            graph_min_entities=args.graph_min_entities,
            graph_min_relations=args.graph_min_relations,
            graph_min_documents=args.graph_min_documents,
            community_min_count=args.community_min_count,
            entity_search_min_hits=args.entity_search_min_hits,
            search_min_hits=args.search_min_hits,
            search_min_readable_hits=args.search_min_readable_hits,
            read_min_content_chars=args.read_min_content_chars,
            read_min_references=args.read_min_references,
            assistant_min_references=args.assistant_min_references,
            assistant_expected_verification=args.assistant_expected_verification,
            assistant_require_all=parse_csv_terms(args.assistant_require_all),
            assistant_forbid_any=parse_csv_terms(args.assistant_forbid_any),
            assistant_max_tool_starts=args.assistant_max_tool_starts,
            expected_search_top_label=args.expected_search_top_label,
            max_tool_latency_ms=args.max_tool_latency_ms,
            max_first_delta_ms=args.max_first_delta_ms,
            max_completed_ms=args.max_completed_ms,
            tool_samples=[
                ("tools_list", tools_list),
                ("search_entities", entity_search),
                ("search_documents", document_search),
                *((("read_document", document_read),) if document_read is not None else ()),
                ("get_graph_topology", graph_topology),
                ("list_relations", list_relations),
                ("get_communities", communities),
                *((("get_runtime_execution", runtime_execution),) if runtime_execution is not None else ()),
                *((("get_runtime_execution_trace", runtime_trace),) if runtime_trace is not None else ()),
                *((("get_runtime_execution_legacy_field", legacy_runtime_execution_probe),) if legacy_runtime_execution_probe is not None else ()),
            ],
        )

        timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        output_path = (
            pathlib.Path(args.output_path)
            if args.output_path
            else pathlib.Path("tmp") / f"agent-surface-profile-{timestamp}.md"
        )
        output_path.parent.mkdir(parents=True, exist_ok=True)
        render_report(
            output_path=output_path,
            base_url=args.base_url,
            library_id=args.library_id,
            workspace_id=workspace_id,
            tools_list=tools_list,
            entity_search=entity_search,
            entity_search_summary=entity_search_summary,
            document_search=document_search,
            document_search_summary=document_search_summary,
            document_read=document_read,
            document_read_summary=document_read_summary,
            graph_topology=graph_topology,
            list_relations=list_relations,
            communities=communities,
            community_summary=community_summary,
            runtime_execution=runtime_execution,
            runtime_execution_summary=runtime_execution_summary,
            runtime_trace=runtime_trace,
            runtime_trace_summary=runtime_trace_summary,
            legacy_runtime_execution_probe=legacy_runtime_execution_probe,
            legacy_runtime_execution_error=legacy_runtime_execution_error,
            graph_quality=graph_quality,
            relation_list_summary=relation_list_summary,
            assistant_summaries=assistant_summaries,
            gate_checks=gate_checks,
        )
        print(output_path)
        failed_checks = [check for check in gate_checks if check.status == "fail"]
        if failed_checks:
            print(
                "agent surface probe failed release gate: "
                + ", ".join(check.label for check in failed_checks),
                file=sys.stderr,
            )
            return 2
        return 0
    except (RuntimeError, TimeoutError, urllib_error.URLError) as exc:
        print(f"agent surface probe failed: {exc}", file=sys.stderr)
        return 1
    finally:
        session.cleanup()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
