#!/usr/bin/env python3
"""
Canonical MCP + assistant turn probe.

Measures two agent-facing surfaces that `profile-ui-endpoints.py` does not cover:

1. MCP graph/search tools (latency, payload size, and semantic coherence)
2. Assistant SSE turn execution (stream timing and grounding/reference presence)

Writes a markdown report to `tmp/agent-surface-profile-<timestamp>.md`.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import pathlib
import re
import statistics
import subprocess
import sys
import tempfile
import time
from collections.abc import Mapping
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any
from urllib import error as urllib_error
from uuid import uuid4


DEFAULT_BASE_URL = "http://127.0.0.1:19000"
DEFAULT_LOGIN = "admin"
PROBE_PASSWORD_ENV = "IRONRAG_PROBE_PASSWORD"  # pragma: allowlist secret
DEFAULT_ENTITY_QUERY: str | None = None
DEFAULT_ASSISTANT_QUESTION: str | None = None
DEFAULT_DOCUMENT_QUERY: str | None = None
DEFAULT_DOCUMENT_LIMIT = 5
DEFAULT_READ_LENGTH = 4000
DEFAULT_ASSISTANT_TOP_K = 8
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
DEFAULT_ASSISTANT_MAX_FIRST_FRAME_MS = 5000
DEFAULT_MIN_ANSWER_OVERLAP_RATIO: float | None = None
MCP_ANSWER_ROUTE = "/v1/mcp"
MCP_DIAGNOSTICS_ROUTE = "/v1/mcp/diagnostics"
MCP_ANSWER_CAPABILITIES_ROUTE = "/v1/mcp/capabilities"
MCP_PROTOCOL_HEADER = "mcp-protocol-version"
MCP_PROTOCOL_VERSION = "2025-11-25"
MCP_SESSION_HEADER = "mcp-session-id"
MCP_TOKEN_ENV = "IRONRAG_MCP_TOKEN"  # pragma: allowlist secret
PROBE_QUESTION_ENV = "IRONRAG_PROBE_QUESTION"
MCP_COMPACT_PROBE_MAX_REFERENCES = 8
JSON_MEDIA_TYPE = "application/json"
MCP_TOOLS_LIST_METHOD = "tools/list"
MCP_CONTRACT_HASH_PATTERN = re.compile(r"sha256:[0-9a-f]{64}\Z")
SENSITIVE_PROBE_ENV_NAMES = frozenset(
    {MCP_TOKEN_ENV, PROBE_PASSWORD_ENV, PROBE_QUESTION_ENV}
)


def sanitized_subprocess_environment(
    source: Mapping[str, str] | None = None,
) -> dict[str, str]:
    environment = dict(os.environ if source is None else source)
    for name in SENSITIVE_PROBE_ENV_NAMES:
        environment.pop(name, None)
    return environment


def ensure_private_directory(path: pathlib.Path) -> None:
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.chmod(0o700)


def write_private_text(path: pathlib.Path, content: str) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    file_descriptor, temporary_path = tempfile.mkstemp(
        prefix=f".{path.name}.",
        suffix=".tmp",
        dir=path.parent,
    )
    try:
        with os.fdopen(file_descriptor, "w", encoding="utf-8") as output:
            os.fchmod(output.fileno(), 0o600)
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary_path, path)
        path.chmod(0o600)
    finally:
        try:
            os.unlink(temporary_path)
        except FileNotFoundError:
            pass


@dataclass(frozen=True)
class CurlSample:
    status_code: int
    time_total_s: float
    size_download_bytes: int
    payload: Any
    response_headers: dict[str, str] = field(default_factory=dict)


@dataclass(frozen=True)
class LibraryCatalogContext:
    workspace_id: str
    catalog_ref: str


@dataclass(frozen=True)
class ProbeInputs:
    password: str
    mcp_token: str | None
    question: str | None


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
    probe_entity_label: str | None
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
class AssistantTurnSummary:
    time_to_completed_s: float
    answer_length: int
    answer_text: str
    total_reference_count: int
    verification_state: str | None
    completion_state: str | None
    query_execution_id: str | None
    runtime_execution_id: str | None
    references: tuple[str, ...] = ()
    time_to_first_frame_s: float | None = None
    time_to_first_activity_s: float | None = None
    time_to_first_model_request_s: float | None = None
    time_to_first_tool_call_s: float | None = None
    stream_event_count: int = 0
    tool_call_started_count: int = 0
    tool_call_finished_count: int = 0


@dataclass(frozen=True)
class GroundedAnswerSummary:
    answer_text: str
    verifier_level: str | None
    runtime_execution_id: str | None
    references: tuple[str, ...]


@dataclass(frozen=True)
class GateCheck:
    label: str
    status: str
    detail: str


class CurlSession:
    def __init__(self, base_url: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.cookie_jar = tempfile.NamedTemporaryFile(
            prefix="ironrag-agent-probe-cookies-", delete=False
        ).name
        self._mcp_sessions: dict[str, tuple[str, str | None]] = {}

    def cleanup(self) -> None:
        for route, (session_id, bearer_token) in tuple(self._mcp_sessions.items()):
            try:
                self.request_json(
                    "DELETE",
                    route,
                    headers={
                        MCP_PROTOCOL_HEADER: MCP_PROTOCOL_VERSION,
                        MCP_SESSION_HEADER: session_id,
                    },
                    bearer_token=bearer_token,
                    timeout_seconds=5,
                )
            except RuntimeError:
                pass
        self._mcp_sessions.clear()
        try:
            os.unlink(self.cookie_jar)
        except FileNotFoundError:
            pass

    def initialize_mcp(
        self,
        route: str,
        *,
        bearer_token: str | None,
        timeout_seconds: int = 60,
    ) -> str:
        existing = self._mcp_sessions.get(route)
        if existing is not None:
            return existing[0]
        sample = self.request_json(
            "POST",
            route,
            body={
                "jsonrpc": "2.0",
                "id": f"agent-probe-initialize-{uuid4().hex}",
                "method": "initialize",
                "params": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "ironrag-agent-probe", "version": "1"},
                },
            },
            headers={"Content-Type": JSON_MEDIA_TYPE},
            bearer_token=bearer_token,
            timeout_seconds=timeout_seconds,
        )
        if sample.status_code != 200:
            raise RuntimeError(f"MCP initialize returned HTTP {sample.status_code}")
        session_id = sample.response_headers.get(MCP_SESSION_HEADER)
        if not session_id:
            raise RuntimeError("MCP initialize returned no session identifier")
        self._mcp_sessions[route] = (session_id, bearer_token)
        return session_id

    def mcp_request_json(
        self,
        route: str,
        *,
        body: Any,
        bearer_token: str | None,
        timeout_seconds: int = 60,
        accept: str = JSON_MEDIA_TYPE,
    ) -> CurlSample:
        session_id = self.initialize_mcp(
            route,
            bearer_token=bearer_token,
            timeout_seconds=timeout_seconds,
        )
        return self.request_json(
            "POST",
            route,
            body=body,
            headers={
                "Content-Type": JSON_MEDIA_TYPE,
                MCP_PROTOCOL_HEADER: MCP_PROTOCOL_VERSION,
                MCP_SESSION_HEADER: session_id,
            },
            bearer_token=bearer_token,
            accept=accept,
            timeout_seconds=timeout_seconds,
        )

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
                f"Content-Type: {JSON_MEDIA_TYPE}",
                "--data-binary",
                "@-",
                f"{self.base_url}/v1/iam/session/login",
            ],
            input=payload,
            capture_output=True,
            check=False,
            env=sanitized_subprocess_environment(),
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
        accept: str = JSON_MEDIA_TYPE,
        timeout_seconds: int = 60,
    ) -> CurlSample:
        payload = json.dumps(body) if body is not None else None
        marker = "__CURL_METRICS__"
        header_lines = [safe_curl_header_line("Accept", accept)]
        if bearer_token:
            header_lines.append(
                safe_curl_header_line("Authorization", f"Bearer {bearer_token}")
            )
        if headers:
            header_lines.extend(
                safe_curl_header_line(key, value) for key, value in headers.items()
            )
        header_fd, header_path = tempfile.mkstemp(prefix="ironrag-agent-probe-headers-")
        response_header_fd, response_header_path = tempfile.mkstemp(
            prefix="ironrag-agent-probe-response-headers-"
        )
        try:
            os.fchmod(header_fd, 0o600)
            os.fchmod(response_header_fd, 0o600)
            os.close(response_header_fd)
            with os.fdopen(header_fd, "w", encoding="utf-8") as header_file:
                header_file.write("\n".join(header_lines))
                header_file.write("\n")
            args = [
                "curl",
                "-s",
                "-b",
                self.cookie_jar,
                "-X",
                method,
                "--header",
                f"@{header_path}",
                "--dump-header",
                response_header_path,
                "-w",
                f"\n{marker} %{{http_code}} %{{time_total}} %{{size_download}}",
                "--max-time",
                str(timeout_seconds),
            ]
            if payload is not None:
                args.extend(["--data-binary", "@-"])
            args.append(f"{self.base_url}{uri}")
            proc = subprocess.run(
                args,
                input=payload,
                capture_output=True,
                check=False,
                env=sanitized_subprocess_environment(),
                text=True,
            )
            response_headers = parse_curl_response_headers(
                pathlib.Path(response_header_path).read_text(encoding="utf-8")
            )
        finally:
            for temporary_path in (header_path, response_header_path):
                try:
                    os.unlink(temporary_path)
                except FileNotFoundError:
                    pass
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
            response_headers=response_headers,
        )


def safe_curl_header_line(name: str, value: str) -> str:
    if not name or any(char in name for char in "\r\n:"):
        raise ValueError("curl header name contains an invalid character")
    if any(char in value for char in "\r\n"):
        raise ValueError("curl header value contains an invalid character")
    return f"{name}: {value}"


def parse_curl_response_headers(raw_headers: str) -> dict[str, str]:
    response_headers: dict[str, str] = {}
    current_headers: dict[str, str] = {}
    for raw_line in raw_headers.replace("\r\n", "\n").split("\n"):
        line = raw_line.strip()
        if line.startswith("HTTP/"):
            current_headers = {}
            continue
        if not line:
            if current_headers:
                response_headers = current_headers
            continue
        name, separator, value = line.partition(":")
        if separator:
            current_headers[name.strip().lower()] = value.strip()
    if current_headers:
        response_headers = current_headers
    return response_headers


def discover_library_catalog_context(session: CurlSession, library_id: str) -> LibraryCatalogContext:
    library = session.request_json("GET", f"/v1/catalog/libraries/{library_id}")
    if library.status_code != 200:
        raise RuntimeError(f"get catalog library returned HTTP {library.status_code}")
    library_payload = library.payload
    if not isinstance(library_payload, dict):
        raise RuntimeError("get catalog library returned non-dict payload")
    workspace_id = library_payload.get("workspaceId")
    library_slug = library_payload.get("slug")
    if not isinstance(workspace_id, str) or not isinstance(library_slug, str):
        raise RuntimeError("catalog library payload missing workspaceId or slug")

    workspace = session.request_json("GET", f"/v1/catalog/workspaces/{workspace_id}")
    if workspace.status_code != 200:
        raise RuntimeError(f"get catalog workspace returned HTTP {workspace.status_code}")
    workspace_payload = workspace.payload
    if not isinstance(workspace_payload, dict):
        raise RuntimeError("get catalog workspace returned non-dict payload")
    workspace_slug = workspace_payload.get("slug")
    if not isinstance(workspace_slug, str):
        raise RuntimeError("catalog workspace payload missing slug")

    return LibraryCatalogContext(
        workspace_id=workspace_id,
        catalog_ref=f"{workspace_slug}/{library_slug}",
    )


def create_query_session(session: CurlSession, workspace_id: str, library_id: str) -> str:
    sample = session.request_json(
        "POST",
        "/v1/query/sessions",
        body={"workspaceId": workspace_id, "libraryId": library_id},
        headers={"Content-Type": JSON_MEDIA_TYPE},
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


def _validated_tool_names(descriptors: Any) -> list[str]:
    if not isinstance(descriptors, list):
        raise RuntimeError("answer MCP tools/list returned no tool descriptors")
    tool_names: list[str] = []
    for descriptor in descriptors:
        if not isinstance(descriptor, dict):
            raise RuntimeError("answer MCP tools/list returned an invalid tool descriptor")
        name = descriptor.get("name")
        if not isinstance(name, str) or not name:
            raise RuntimeError("answer MCP tools/list returned an invalid tool name")
        tool_names.append(name)
    if len(tool_names) != len(set(tool_names)):
        raise RuntimeError("answer MCP tools/list returned duplicate tool names")
    return tool_names


def _validate_capability_tools(capability_tools: Any, tool_names: list[str]) -> None:
    if not isinstance(capability_tools, list) or not all(
        isinstance(name, str) and bool(name) for name in capability_tools
    ):
        raise RuntimeError("answer MCP capabilities returned no visible tool names")
    if len(capability_tools) != len(set(capability_tools)):
        raise RuntimeError("answer MCP capabilities returned duplicate tool names")
    if set(capability_tools) != set(tool_names):
        raise RuntimeError(
            "answer MCP tool set drift: capabilities and tools/list do not match"
        )
    if "grounded_answer" not in tool_names:
        raise RuntimeError(
            "answer MCP discovery is incomplete: grounded_answer is not visible"
        )


def _validate_contract_metadata(capabilities: dict[str, Any], listed: dict[str, Any]) -> str:
    metadata = listed.get("_meta")
    if not isinstance(metadata, dict):
        raise RuntimeError("answer MCP tools/list returned no contract metadata")
    versions = (
        capabilities.get("toolContractVersion"),
        metadata.get("ironrag/toolContractVersion"),
    )
    if not all(
        isinstance(version, int) and not isinstance(version, bool) and version > 0
        for version in versions
    ):
        raise RuntimeError("answer MCP tool contract version must be a positive integer")
    if versions[0] != versions[1]:
        raise RuntimeError(
            "answer MCP tool contract version drift: capabilities and tools/list do not match"
        )
    hashes = (
        capabilities.get("toolContractHash"),
        metadata.get("ironrag/toolContractHash"),
    )
    if not all(
        isinstance(contract_hash, str)
        and MCP_CONTRACT_HASH_PATTERN.fullmatch(contract_hash) is not None
        for contract_hash in hashes
    ):
        raise RuntimeError("answer MCP tool contract hash is not a canonical SHA-256 digest")
    if hashes[0] != hashes[1]:
        raise RuntimeError(
            "answer MCP tool contract hash drift: capabilities and tools/list do not match"
        )
    return hashes[0]


def validate_answer_mcp_discovery(
    capabilities: CurlSample,
    tools_list: CurlSample,
) -> str:
    """Fail before an LLM turn when the answer MCP contract is incomplete or drifting."""
    if capabilities.status_code != 200:
        raise RuntimeError(
            f"answer MCP capabilities returned HTTP {capabilities.status_code}"
        )
    if not isinstance(capabilities.payload, dict):
        raise RuntimeError("answer MCP capabilities returned a non-object payload")
    listed = ensure_jsonrpc_result(tools_list, "answer MCP tools/list")
    if not isinstance(listed, dict):
        raise RuntimeError("answer MCP tools/list returned a non-object result")
    tool_names = _validated_tool_names(listed.get("tools"))
    _validate_capability_tools(capabilities.payload.get("tools"), tool_names)
    return _validate_contract_metadata(capabilities.payload, listed)


def build_grounded_answer_probe_arguments(
    library_ref: str,
    question: str,
    top_k: int,
) -> dict[str, Any]:
    return {
        "library": library_ref,
        "query": question,
        "topK": top_k,
        "responseProfile": "compact",
        "maxReferences": MCP_COMPACT_PROBE_MAX_REFERENCES,
    }


def probe_mcp_tool(
    session: CurlSession,
    *,
    bearer_token: str | None,
    tool_name: str,
    arguments: dict[str, Any],
    route: str = MCP_DIAGNOSTICS_ROUTE,
) -> CurlSample:
    return session.mcp_request_json(
        route,
        body={
            "jsonrpc": "2.0",
            "id": f"agent-probe-{tool_name}-{uuid4().hex}",
            "method": "tools/call",
            "params": {"name": tool_name, "arguments": arguments},
        },
        bearer_token=bearer_token,
        timeout_seconds=120,
    )


def build_document_search_arguments(library_ref: str, query: str, limit: int) -> dict[str, Any]:
    return {
        "query": query,
        "libraries": [library_ref],
        "limit": limit,
        "includeReferences": True,
    }


def normalize_quality_text(value: Any) -> str:
    if not isinstance(value, str):
        return ""
    return " ".join(tokenize_quality_text(value))


def tokenize_quality_text(value: str) -> tuple[str, ...]:
    tokens: list[str] = []
    current: list[str] = []
    for char in value.casefold():
        if char.isalnum():
            current.append(char)
        elif current:
            tokens.append("".join(current))
            current.clear()
    if current:
        tokens.append("".join(current))
    return tuple(tokens)


def significant_answer_tokens(value: str) -> set[str]:
    return {
        token
        for token in tokenize_quality_text(value)
        if len(token) >= 2 or any(char.isdigit() for char in token)
    }


def answer_token_overlap_ratio(left: str, right: str) -> float | None:
    left_tokens = significant_answer_tokens(left)
    right_tokens = significant_answer_tokens(right)
    if not left_tokens or not right_tokens:
        return None
    return (2.0 * len(left_tokens & right_tokens)) / (len(left_tokens) + len(right_tokens))


def count_duplicate_keys(keys: list[tuple[Any, ...] | str]) -> int:
    counter = collections.Counter(key for key in keys if key)
    return sum(1 for occurrences in counter.values() if occurrences > 1)


def is_probe_entity_label(value: Any) -> bool:
    if not isinstance(value, str):
        return False
    label = value.strip()
    if not label:
        return False
    if label.casefold() in {"true", "false", "null"}:
        return False
    return any(char.isalpha() for char in label)


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
    probe_entity_label = next(
        (entity.get("label") for entity in entities if is_probe_entity_label(entity.get("label"))),
        None,
    )
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
        probe_entity_label=probe_entity_label if isinstance(probe_entity_label, str) else None,
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


def build_assistant_turn_curl_request(
    *,
    base_url: str,
    cookie_jar: str,
    session_id: str,
    question: str,
    top_k: int,
    timeout_seconds: int,
) -> tuple[list[str], str]:
    payload = json.dumps({"contentText": question, "topK": top_k})
    marker = "__CURL_METRICS__"
    args = [
        "curl",
        "-N",
        "-s",
        "-b",
        cookie_jar,
        "-X",
        "POST",
        "-H",
        "Accept: text/event-stream",
        "-H",
        f"Content-Type: {JSON_MEDIA_TYPE}",
        "-w",
        f"\n{marker} %{{http_code}} %{{time_total}} %{{size_download}}",
        "--max-time",
        str(timeout_seconds),
        "--data-binary",
        "@-",
        f"{base_url}/v1/query/sessions/{session_id}/turns",
    ]
    return args, payload


@dataclass
class AssistantStreamState:
    started_at: float
    footer: tuple[int, float, int] | None = None
    completed_payload: dict[str, Any] | None = None
    failed_message: str | None = None
    time_to_first_frame_s: float | None = None
    time_to_first_activity_s: float | None = None
    time_to_first_model_request_s: float | None = None
    time_to_first_tool_call_s: float | None = None
    stream_event_count: int = 0
    tool_call_started_count: int = 0
    tool_call_finished_count: int = 0

    def elapsed(self) -> float:
        return time.monotonic() - self.started_at


def _record_assistant_activity(
    state: AssistantStreamState,
    activity: Any,
) -> None:
    if state.time_to_first_activity_s is None:
        state.time_to_first_activity_s = state.elapsed()
    if not isinstance(activity, dict):
        return
    activity_type = activity.get("type")
    if activity_type == "model_request" and state.time_to_first_model_request_s is None:
        state.time_to_first_model_request_s = state.elapsed()
    if activity_type == "tool_call_started":
        state.tool_call_started_count += 1
        if state.time_to_first_tool_call_s is None:
            state.time_to_first_tool_call_s = state.elapsed()
    elif activity_type == "tool_call_finished":
        state.tool_call_finished_count += 1


def _dispatch_assistant_sse_data(state: AssistantStreamState, raw_data: str) -> None:
    if not raw_data.strip():
        return
    try:
        event_payload = json.loads(raw_data)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"assistant SSE emitted invalid JSON: {raw_data[:200]!r}") from exc
    if not isinstance(event_payload, dict):
        raise RuntimeError("assistant SSE emitted non-object data")
    state.stream_event_count += 1
    payload_type = event_payload.get("type")
    if payload_type == "activity":
        _record_assistant_activity(state, event_payload.get("event") or {})
        return
    if payload_type == "completed":
        detail = event_payload.get("detail")
        if not isinstance(detail, dict):
            raise RuntimeError("assistant SSE completed event missing detail object")
        state.completed_payload = detail
        return
    if payload_type == "failed":
        message = event_payload.get("message")
        state.failed_message = message if isinstance(message, str) else "assistant turn failed"


def _parse_assistant_footer(marker: str, line: str) -> tuple[int, float, int]:
    parts = line[len(marker):].strip().split(" ", 2)
    if len(parts) != 3:
        raise RuntimeError(f"assistant turn emitted malformed curl footer: {line!r}")
    return int(parts[0]), float(parts[1]), int(float(parts[2]))


def _flush_assistant_data_lines(
    state: AssistantStreamState,
    data_lines: list[str],
) -> None:
    if not data_lines:
        return
    _dispatch_assistant_sse_data(state, "\n".join(data_lines))
    data_lines.clear()


def _consume_assistant_stream_line(
    line: str,
    marker: str,
    state: AssistantStreamState,
    data_lines: list[str],
) -> None:
    if line.startswith(marker):
        state.footer = _parse_assistant_footer(marker, line)
        return
    if state.time_to_first_frame_s is None and line.startswith(("event:", "data:", ":")):
        state.time_to_first_frame_s = state.elapsed()
    if not line:
        _flush_assistant_data_lines(state, data_lines)
        return
    if line.startswith(":"):
        return
    if line.startswith("data:"):
        data_lines.append(line[5:].lstrip())


def _consume_assistant_stream(
    proc: subprocess.Popen[str],
    marker: str,
    state: AssistantStreamState,
) -> tuple[str, int]:
    if proc.stdout is None:
        raise RuntimeError("failed to capture assistant SSE stdout")
    data_lines: list[str] = []
    try:
        for raw_line in proc.stdout:
            _consume_assistant_stream_line(
                raw_line.rstrip("\n"), marker, state, data_lines
            )
        _flush_assistant_data_lines(state, data_lines)
    finally:
        stderr = proc.stderr.read() if proc.stderr is not None else ""
        return_code = proc.wait()
    return stderr, return_code


def _validate_assistant_stream_result(
    state: AssistantStreamState,
    stderr: str,
    return_code: int,
) -> tuple[dict[str, Any], float]:
    if return_code != 0:
        raise RuntimeError(
            f"assistant SSE turn failed: curl exit {return_code}: {stderr[:200]}"
        )
    if state.footer is None:
        raise RuntimeError("assistant SSE turn did not emit curl metrics footer")
    status_code, time_total_s, _size_download = state.footer
    if status_code != 200:
        raise RuntimeError(f"assistant SSE turn returned HTTP {status_code}")
    if state.failed_message is not None:
        raise RuntimeError(f"assistant SSE turn failed: {state.failed_message}")
    if state.completed_payload is None:
        raise RuntimeError("assistant SSE turn ended without a completed event")
    return state.completed_payload, time_total_s


def _assistant_turn_summary(
    completed_payload: dict[str, Any],
    time_total_s: float,
    state: AssistantStreamState,
) -> AssistantTurnSummary:
    execution = completed_payload.get("execution") or {}
    completion_state = execution.get("lifecycleState") if isinstance(execution, dict) else None
    query_execution_id = execution.get("id") if isinstance(execution, dict) else None
    artifacts = summarize_assistant_turn_artifacts(completed_payload)
    return AssistantTurnSummary(
        time_to_completed_s=time_total_s,
        answer_length=len(artifacts.answer_text),
        answer_text=artifacts.answer_text,
        total_reference_count=total_reference_count(completed_payload),
        verification_state=artifacts.verifier_level,
        completion_state=completion_state if isinstance(completion_state, str) else None,
        query_execution_id=query_execution_id if isinstance(query_execution_id, str) else None,
        runtime_execution_id=artifacts.runtime_execution_id,
        references=artifacts.references,
        time_to_first_frame_s=state.time_to_first_frame_s,
        time_to_first_activity_s=state.time_to_first_activity_s,
        time_to_first_model_request_s=state.time_to_first_model_request_s,
        time_to_first_tool_call_s=state.time_to_first_tool_call_s,
        stream_event_count=state.stream_event_count,
        tool_call_started_count=state.tool_call_started_count,
        tool_call_finished_count=state.tool_call_finished_count,
    )


def execute_assistant_turn(
    session: CurlSession,
    session_id: str,
    question: str,
    *,
    top_k: int,
    timeout_seconds: int,
) -> AssistantTurnSummary:
    args, payload = build_assistant_turn_curl_request(
        base_url=session.base_url,
        cookie_jar=session.cookie_jar,
        session_id=session_id,
        question=question,
        top_k=top_k,
        timeout_seconds=timeout_seconds,
    )
    state = AssistantStreamState(started_at=time.monotonic())
    proc = subprocess.Popen(
        args,
        env=sanitized_subprocess_environment(),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    if proc.stdin is None:
        proc.kill()
        raise RuntimeError("failed to open assistant SSE request stdin")
    proc.stdin.write(payload)
    proc.stdin.close()
    stderr, return_code = _consume_assistant_stream(proc, "__CURL_METRICS__", state)
    completed_payload, time_total_s = _validate_assistant_stream_result(
        state, stderr, return_code
    )
    return _assistant_turn_summary(completed_payload, time_total_s, state)


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


def normalize_text_for_comparison(value: str) -> str:
    return " ".join(value.replace("\r\n", "\n").split())


def _coerce_reference_token(*parts: Any) -> str | None:
    values = [str(part).strip() for part in parts if str(part).strip()]
    if not values:
        return None
    return "|".join(values)


def summarize_assistant_turn_artifacts(payload: dict[str, Any]) -> GroundedAnswerSummary:
    response_turn = payload.get("responseTurn") or {}
    execution = payload.get("execution") or {}
    answer_text = response_turn.get("contentText") if isinstance(response_turn.get("contentText"), str) else ""
    verifier_level = payload.get("verificationState")
    if not isinstance(verifier_level, str):
        verifier_level = None
    runtime_execution_id = execution.get("runtimeExecutionId")
    if not isinstance(runtime_execution_id, str):
        runtime_execution_id = None
    references = tuple(
        sorted(
            {
                value
                for row in (payload.get("chunkReferences") or [])
                if isinstance(row, dict)
                for value in [_coerce_reference_token("chunk", row.get("chunkId"))]
                if value is not None
            }
            | {
                value
                for row in (payload.get("entityReferences") or [])
                if isinstance(row, dict)
                for value in [_coerce_reference_token("entity", row.get("nodeId"))]
                if value is not None
            }
            | {
                value
                for row in (payload.get("relationReferences") or [])
                if isinstance(row, dict)
                for value in [
                    _coerce_reference_token(
                        "relation",
                        row.get("edgeId"),
                    )
                ]
                if value is not None
            }
            | {
                value
                for row in (payload.get("preparedSegmentReferences") or [])
                if isinstance(row, dict)
                for value in [_coerce_reference_token("segment", row.get("segmentId"))]
                if value is not None
            }
            | {
                value
                for row in (payload.get("technicalFactReferences") or [])
                if isinstance(row, dict)
                for value in [_coerce_reference_token("fact", row.get("factId"))]
                if value is not None
            }
        )
    )

    return GroundedAnswerSummary(
        answer_text=answer_text,
        verifier_level=verifier_level,
        runtime_execution_id=runtime_execution_id,
        references=references,
    )


def _compact_reference_tokens(compact_references: Any) -> tuple[str, ...]:
    reference_fields = {
        "chunk": ("chunk", "chunkId"),
        "prepared_segment": ("segment", "segmentId"),
        "technical_fact": ("fact", "factId"),
        "entity": ("entity", "nodeId"),
        "relation": ("relation", "edgeId"),
    }
    if not isinstance(compact_references, list):
        return ()
    references: set[str] = set()
    for reference in compact_references:
        if not isinstance(reference, dict):
            continue
        reference_field = reference_fields.get(reference.get("kind"))
        if reference_field is None:
            continue
        canonical_kind, identifier_field = reference_field
        token = _coerce_reference_token(
            canonical_kind,
            reference.get(identifier_field),
        )
        if token is not None:
            references.add(token)
    return tuple(sorted(references))


def _summarize_compact_grounded_answer(payload: dict[str, Any]) -> GroundedAnswerSummary:
    answer_text = payload.get("answerBody")
    verifier = payload.get("verifier")
    verifier_level = verifier.get("state") if isinstance(verifier, dict) else None
    runtime_execution_id = payload.get("runtimeExecutionId")
    reference_summary = payload.get("referenceSummary")
    compact_references = (
        reference_summary.get("references")
        if isinstance(reference_summary, dict)
        else []
    )
    return GroundedAnswerSummary(
        answer_text=answer_text if isinstance(answer_text, str) else "",
        verifier_level=verifier_level if isinstance(verifier_level, str) else None,
        runtime_execution_id=(
            runtime_execution_id if isinstance(runtime_execution_id, str) else None
        ),
        references=_compact_reference_tokens(compact_references),
    )


def summarize_grounded_answer(payload: dict[str, Any]) -> GroundedAnswerSummary:
    if payload.get("responseProfile") == "compact":
        return _summarize_compact_grounded_answer(payload)
    execution_detail = payload.get("executionDetail")
    if isinstance(execution_detail, dict):
        return summarize_assistant_turn_artifacts(execution_detail)
    return GroundedAnswerSummary(
        answer_text="",
        verifier_level=None,
        runtime_execution_id=None,
        references=(),
    )


def parse_csv_terms(value: str) -> list[str]:
    return [term.strip() for term in value.split(",") if term.strip()]


def resolve_probe_queries(
    *,
    entity_query: str | None,
    document_query: str | None,
    question: str | None,
    graph_quality: McpQualitySummary,
) -> tuple[str, str, str]:
    resolved_entity_query = (
        entity_query or graph_quality.probe_entity_label or graph_quality.top_entity_label or "library"
    )
    resolved_question = question or f"What does this library say about {resolved_entity_query}?"
    resolved_document_query = document_query or resolved_question
    return resolved_entity_query, resolved_document_query, resolved_question


def contains_all_terms(text: str, required_terms: list[str]) -> bool:
    text_folded = text.casefold()
    return all(term.casefold() in text_folded for term in required_terms)


def contains_any_term(text: str, candidate_terms: list[str]) -> bool:
    text_folded = text.casefold()
    return any(term.casefold() in text_folded for term in candidate_terms)


@dataclass(frozen=True)
class GateThresholds:
    graph_min_entities: int
    graph_min_relations: int
    graph_min_documents: int
    community_min_count: int
    entity_search_min_hits: int
    search_min_hits: int
    search_min_readable_hits: int
    read_min_content_chars: int
    read_min_references: int
    assistant_min_references: int
    assistant_expected_verification: str
    max_tool_latency_ms: int | None
    max_completed_ms: int | None
    max_first_frame_ms: int | None = None
    min_answer_overlap_ratio: float | None = DEFAULT_MIN_ANSWER_OVERLAP_RATIO


@dataclass(frozen=True)
class GateInputs:
    entity_search_summary: EntitySearchSummary
    document_search_summary: DocumentSearchSummary
    document_read_summary: DocumentReadSummary | None
    graph_quality: McpQualitySummary
    relation_list_summary: RelationListSummary
    community_summary: CommunitySummary
    assistant_summaries: list[AssistantTurnSummary]
    runtime_execution_summary: RuntimeExecutionProbeSummary | None
    runtime_trace_summary: RuntimeTraceProbeSummary | None
    legacy_runtime_execution_error: ToolErrorSummary | None
    grounded_answer_summary: GroundedAnswerSummary | None
    assistant_require_all: list[str]
    assistant_forbid_any: list[str]
    expected_search_top_label: str | None
    tool_samples: list[tuple[str, CurlSample]]


def _gate_check(
    label: str,
    passes: bool,
    detail: str,
    *,
    failure_status: str = "fail",
) -> GateCheck:
    return GateCheck(
        label=label,
        status="pass" if passes else failure_status,
        detail=detail,
    )


def _search_alignment_gate(inputs: GateInputs) -> list[GateCheck]:
    search_top_label = inputs.entity_search_summary.top_label
    expected_label = inputs.expected_search_top_label
    if expected_label is not None:
        return [
            _gate_check(
                "graph.search_top_label",
                search_top_label == expected_label,
                f"top={search_top_label or 'n/a'} expected={expected_label}",
            )
        ]
    graph = inputs.graph_quality
    if search_top_label is None or graph.top_entity_label is None:
        return []
    normalized_label = normalize_quality_text(search_top_label)
    is_visible = normalized_label in graph.visible_entity_labels_normalized
    return [
        _gate_check(
            "graph.search_alignment",
            bool(normalized_label) and is_visible,
            (
                f"search_top={search_top_label} visible_in_topology={is_visible} "
                f"topology_top={graph.top_entity_label}"
            ),
            failure_status="warn",
        )
    ]


def _build_graph_gate_checks(inputs: GateInputs, thresholds: GateThresholds) -> list[GateCheck]:
    entity_search = inputs.entity_search_summary
    graph = inputs.graph_quality
    relations = inputs.relation_list_summary
    communities = inputs.community_summary
    checks = [
        _gate_check(
            "graph.entities",
            graph.entity_count >= thresholds.graph_min_entities,
            f"entities={graph.entity_count} min={thresholds.graph_min_entities}",
        ),
        _gate_check(
            "graph.search_entities_hits",
            entity_search.hit_count >= thresholds.entity_search_min_hits,
            f"hits={entity_search.hit_count} min={thresholds.entity_search_min_hits}",
        ),
        _gate_check(
            "graph.relations",
            graph.relation_count >= thresholds.graph_min_relations,
            f"relations={graph.relation_count} min={thresholds.graph_min_relations}",
        ),
        _gate_check(
            "graph.documents",
            graph.document_count >= thresholds.graph_min_documents,
            f"documents={graph.document_count} min={thresholds.graph_min_documents}",
        ),
        _gate_check(
            "graph.coherence",
            graph.quality_status == "pass",
            (
                f"quality={graph.quality_status} "
                f"orphan_relations={graph.orphan_relation_count} "
                f"orphan_links={graph.orphan_link_count} "
                f"orphan_documents={graph.orphan_document_count}"
            ),
        ),
        _gate_check(
            "graph.document_links_visible_documents",
            graph.orphan_document_count == 0,
            f"orphan_documents={graph.orphan_document_count}",
        ),
        _gate_check(
            "graph.documents_ranked_by_support",
            graph.document_rank_monotonic,
            f"document_rank_monotonic={graph.document_rank_monotonic}",
        ),
        _gate_check(
            "graph.duplicate_entity_labels",
            graph.duplicate_entity_label_count == 0,
            f"duplicate_entity_labels={graph.duplicate_entity_label_count}",
        ),
        _gate_check(
            "graph.duplicate_relation_signatures",
            graph.duplicate_relation_signature_count == 0,
            f"duplicate_relation_signatures={graph.duplicate_relation_signature_count}",
        ),
        _gate_check(
            "graph.list_relations_labels",
            relations.unknown_label_count == 0,
            f"unknown_labels={relations.unknown_label_count}",
        ),
        _gate_check(
            "graph.list_relations_duplicates",
            relations.duplicate_signature_count == 0,
            f"duplicate_signatures={relations.duplicate_signature_count}",
        ),
        _gate_check(
            "graph.communities",
            communities.count >= thresholds.community_min_count,
            f"communities={communities.count} min={thresholds.community_min_count}",
        ),
        _gate_check(
            "graph.community_summaries",
            communities.count == 0
            or communities.communities_with_summary == communities.count,
            (
                f"with_summary={communities.communities_with_summary} "
                f"count={communities.count}"
            ),
        ),
    ]
    checks.extend(_search_alignment_gate(inputs))
    return checks


def _document_read_gate_checks(
    search: DocumentSearchSummary,
    read: DocumentReadSummary | None,
    thresholds: GateThresholds,
) -> list[GateCheck]:
    if read is None:
        return [
            GateCheck(
                label="mcp.read_document",
                status="fail",
                detail="no readable top hit available for read_document probe",
            )
        ]
    checks = [
        _gate_check(
            "mcp.read_document_readability",
            read.readability_state == "readable",
            f"readability={read.readability_state or 'n/a'}",
        ),
        _gate_check(
            "mcp.read_document_content",
            read.content_length >= thresholds.read_min_content_chars,
            f"content_chars={read.content_length} min={thresholds.read_min_content_chars}",
        ),
        _gate_check(
            "mcp.read_document_references",
            read.total_reference_count >= thresholds.read_min_references,
            f"references={read.total_reference_count} min={thresholds.read_min_references}",
        ),
        _gate_check(
            "mcp.read_document_alignment",
            read.document_id == search.top_document_id,
            (
                f"read_document_id={read.document_id} "
                f"search_top_document_id={search.top_document_id}"
            ),
        ),
    ]
    if search.top_suggested_start_offset is not None:
        checks.append(
            _gate_check(
                "mcp.read_document_offset_alignment",
                read.slice_start_offset == search.top_suggested_start_offset,
                (
                    f"slice_start={read.slice_start_offset} "
                    f"suggested_start={search.top_suggested_start_offset}"
                ),
            )
        )
    return checks


def _build_document_gate_checks(inputs: GateInputs, thresholds: GateThresholds) -> list[GateCheck]:
    search = inputs.document_search_summary
    has_guidance = search.top_suggested_start_offset is not None
    checks = [
        _gate_check(
            "mcp.search_documents_hits",
            search.hit_count >= thresholds.search_min_hits,
            f"hits={search.hit_count} min={thresholds.search_min_hits}",
        ),
        _gate_check(
            "mcp.search_documents_readable_hits",
            search.readable_hit_count >= thresholds.search_min_readable_hits,
            (
                f"readable_hits={search.readable_hit_count} "
                f"min={thresholds.search_min_readable_hits}"
            ),
        ),
        _gate_check(
            "mcp.search_documents_guidance",
            has_guidance,
            (
                "top_hit suggestedStartOffset present"
                if has_guidance
                else "top_hit suggestedStartOffset missing"
            ),
        ),
    ]
    checks.extend(
        _document_read_gate_checks(search, inputs.document_read_summary, thresholds)
    )
    return checks


def _optional_assistant_gate_checks(
    run_number: int,
    summary: AssistantTurnSummary,
    inputs: GateInputs,
    thresholds: GateThresholds,
) -> list[GateCheck]:
    prefix = f"assistant.run_{run_number}"
    checks: list[GateCheck] = []
    if thresholds.max_first_frame_ms is not None:
        first_frame_ms = (
            int(summary.time_to_first_frame_s * 1000)
            if summary.time_to_first_frame_s is not None
            else None
        )
        checks.append(
            _gate_check(
                f"{prefix}.first_frame_budget",
                first_frame_ms is not None
                and first_frame_ms <= thresholds.max_first_frame_ms,
                f"first_frame_ms={first_frame_ms or 'n/a'} max={thresholds.max_first_frame_ms}",
            )
        )
    if inputs.assistant_require_all:
        checks.append(
            _gate_check(
                f"{prefix}.required_terms",
                contains_all_terms(summary.answer_text, inputs.assistant_require_all),
                f"required={inputs.assistant_require_all}",
            )
        )
    if inputs.assistant_forbid_any:
        checks.append(
            _gate_check(
                f"{prefix}.forbidden_terms",
                not contains_any_term(summary.answer_text, inputs.assistant_forbid_any),
                f"forbidden={inputs.assistant_forbid_any}",
            )
        )
    if thresholds.max_completed_ms is not None:
        completed_ms = int(summary.time_to_completed_s * 1000)
        checks.append(
            _gate_check(
                f"{prefix}.completed_budget",
                completed_ms <= thresholds.max_completed_ms,
                f"completed_ms={completed_ms} max={thresholds.max_completed_ms}",
            )
        )
    return checks


def _assistant_run_gate_checks(
    run_number: int,
    summary: AssistantTurnSummary,
    inputs: GateInputs,
    thresholds: GateThresholds,
) -> list[GateCheck]:
    prefix = f"assistant.run_{run_number}"
    tool_events_match = (
        summary.tool_call_started_count > 0
        and summary.tool_call_started_count == summary.tool_call_finished_count
    )
    checks = [
        _gate_check(
            f"{prefix}.stream_events",
            summary.stream_event_count > 0,
            f"events={summary.stream_event_count}",
        ),
        _gate_check(
            f"{prefix}.stream_tool_events",
            tool_events_match,
            (
                f"started={summary.tool_call_started_count} "
                f"finished={summary.tool_call_finished_count}"
            ),
        ),
        _gate_check(
            f"{prefix}.verification",
            summary.verification_state == thresholds.assistant_expected_verification,
            (
                f"verification={summary.verification_state or 'n/a'} "
                f"expected={thresholds.assistant_expected_verification}"
            ),
        ),
        _gate_check(
            f"{prefix}.references",
            summary.total_reference_count >= thresholds.assistant_min_references,
            (
                f"references={summary.total_reference_count} "
                f"min={thresholds.assistant_min_references}"
            ),
        ),
        GateCheck(
            label=f"{prefix}.completed",
            status="pass",
            detail=f"completed={format_seconds(summary.time_to_completed_s)}",
        ),
    ]
    checks.extend(
        _optional_assistant_gate_checks(run_number, summary, inputs, thresholds)
    )
    return checks


def _build_assistant_gate_checks(inputs: GateInputs, thresholds: GateThresholds) -> list[GateCheck]:
    checks: list[GateCheck] = []
    for run_number, summary in enumerate(inputs.assistant_summaries, start=1):
        checks.extend(
            _assistant_run_gate_checks(run_number, summary, inputs, thresholds)
        )
    return checks


def _runtime_execution_gate_checks(
    summary: RuntimeExecutionProbeSummary | None,
    expected_runtime_id: str | None,
) -> list[GateCheck]:
    if summary is None:
        return [
            GateCheck(
                label="mcp.get_runtime_execution",
                status="fail",
                detail="runtime execution probe missing",
            )
        ]
    return [
        _gate_check(
            "mcp.get_runtime_execution_alignment",
            summary.runtime_execution_id == expected_runtime_id,
            (
                f"probe={summary.runtime_execution_id or 'missing'} "
                f"assistant={expected_runtime_id or 'missing'}"
            ),
        ),
        _gate_check(
            "mcp.get_runtime_execution_lifecycle",
            summary.lifecycle_state == "completed",
            f"lifecycle={summary.lifecycle_state or 'missing'}",
        ),
    ]


def _runtime_trace_gate_checks(
    summary: RuntimeTraceProbeSummary | None,
    expected_runtime_id: str | None,
) -> list[GateCheck]:
    if summary is None:
        return [
            GateCheck(
                label="mcp.get_runtime_execution_trace",
                status="fail",
                detail="runtime trace probe missing",
            )
        ]
    return [
        _gate_check(
            "mcp.get_runtime_execution_trace_alignment",
            summary.runtime_execution_id == expected_runtime_id,
            (
                f"probe={summary.runtime_execution_id or 'missing'} "
                f"assistant={expected_runtime_id or 'missing'}"
            ),
        ),
        _gate_check(
            "mcp.get_runtime_execution_trace_stages",
            summary.stage_count >= 1,
            f"stage_count={summary.stage_count}",
        ),
    ]


def _legacy_runtime_gate_check(summary: ToolErrorSummary | None) -> GateCheck:
    label = "mcp.get_runtime_execution_legacy_field_rejected"
    if summary is None:
        return GateCheck(
            label=label,
            status="fail",
            detail="legacy executionId rejection probe missing",
        )
    is_rejected = (
        summary.error_kind == "invalid_mcp_tool_call"
        and summary.message is not None
        and "runtimeExecutionId" in summary.message
    )
    return _gate_check(
        label,
        is_rejected,
        (
            f"error_kind={summary.error_kind or 'missing'} "
            f"message={summary.message or 'missing'}"
        ),
    )


def _build_runtime_gate_checks(inputs: GateInputs) -> list[GateCheck]:
    runtime_id = next(
        (
            summary.runtime_execution_id
            for summary in inputs.assistant_summaries
            if summary.runtime_execution_id
        ),
        None,
    )
    checks = [
        _gate_check(
            "assistant.runtime_execution_id",
            runtime_id is not None,
            f"runtimeExecutionId={runtime_id or 'missing'}",
        )
    ]
    checks.extend(
        _runtime_execution_gate_checks(inputs.runtime_execution_summary, runtime_id)
    )
    checks.extend(_runtime_trace_gate_checks(inputs.runtime_trace_summary, runtime_id))
    checks.append(_legacy_runtime_gate_check(inputs.legacy_runtime_execution_error))
    return checks


def _answer_quality_parity_gate(
    grounded: GroundedAnswerSummary,
    assistant_summaries: list[AssistantTurnSummary],
    thresholds: GateThresholds,
) -> GateCheck:
    label = "assistant.run_1.mcp_answer_quality_parity"
    if not assistant_summaries:
        return GateCheck(
            label=label,
            status="fail",
            detail="assistant run missing for MCP answer quality comparison",
        )
    assistant = assistant_summaries[0]
    answer_overlap = answer_token_overlap_ratio(
        assistant.answer_text, grounded.answer_text
    )
    overlap_passes = thresholds.min_answer_overlap_ratio is None or (
        answer_overlap is not None
        and answer_overlap >= thresholds.min_answer_overlap_ratio
    )
    references_pass = (
        assistant.total_reference_count >= thresholds.assistant_min_references
        and len(grounded.references) >= thresholds.assistant_min_references
    )
    return _gate_check(
        label,
        assistant.verification_state == grounded.verifier_level
        and references_pass
        and overlap_passes,
        (
            f"ui_verifier={assistant.verification_state or 'n/a'} "
            f"mcp_verifier={grounded.verifier_level or 'n/a'} "
            f"ui_references={assistant.total_reference_count} "
            f"mcp_references={len(grounded.references)} "
            f"answer_overlap={answer_overlap if answer_overlap is not None else 'n/a'} "
            "min_overlap="
            f"{thresholds.min_answer_overlap_ratio if thresholds.min_answer_overlap_ratio is not None else 'disabled'}"
        ),
    )


def _build_grounded_answer_gate_checks(inputs: GateInputs, thresholds: GateThresholds) -> list[GateCheck]:
    grounded = inputs.grounded_answer_summary
    if grounded is None:
        return [
            GateCheck(
                label="mcp.grounded_answer",
                status="fail",
                detail="grounded_answer probe missing",
            )
        ]
    checks = [
        _gate_check(
            "mcp.grounded_answer.verifier",
            grounded.verifier_level == thresholds.assistant_expected_verification,
            (
                f"verifier={grounded.verifier_level or 'n/a'} "
                f"expected={thresholds.assistant_expected_verification}"
            ),
        ),
        _gate_check(
            "mcp.grounded_answer.references",
            len(grounded.references) >= thresholds.assistant_min_references,
            (
                f"references={len(grounded.references)} "
                f"min={thresholds.assistant_min_references}"
            ),
        ),
        _gate_check(
            "mcp.grounded_answer.runtime_execution_id",
            bool(grounded.runtime_execution_id),
            f"runtimeExecutionId={grounded.runtime_execution_id or 'missing'}",
        ),
        _answer_quality_parity_gate(
            grounded, inputs.assistant_summaries, thresholds
        ),
    ]
    return checks


def _tool_latency_gate_check(
    name: str,
    sample: CurlSample,
    thresholds: GateThresholds,
) -> GateCheck:
    tool_ms = int(sample.time_total_s * 1000)
    if name == "grounded_answer" and thresholds.max_completed_ms is not None:
        return _gate_check(
            "tool.grounded_answer.completed_budget",
            tool_ms <= thresholds.max_completed_ms,
            f"completed_ms={tool_ms} max={thresholds.max_completed_ms}",
        )
    return _gate_check(
        f"tool.{name}.latency_budget",
        tool_ms <= thresholds.max_tool_latency_ms,
        f"latency_ms={tool_ms} max={thresholds.max_tool_latency_ms}",
    )


def _build_tool_latency_gate_checks(inputs: GateInputs, thresholds: GateThresholds) -> list[GateCheck]:
    if thresholds.max_tool_latency_ms is None:
        return []
    return [
        _tool_latency_gate_check(name, sample, thresholds)
        for name, sample in inputs.tool_samples
    ]


def build_gate_checks(inputs: GateInputs, thresholds: GateThresholds) -> list[GateCheck]:
    checks = _build_graph_gate_checks(inputs, thresholds)
    checks.extend(_build_document_gate_checks(inputs, thresholds))
    checks.extend(_build_assistant_gate_checks(inputs, thresholds))
    checks.extend(_build_runtime_gate_checks(inputs))
    checks.extend(_build_grounded_answer_gate_checks(inputs, thresholds))
    checks.extend(_build_tool_latency_gate_checks(inputs, thresholds))
    return checks


@dataclass(frozen=True)
class ReportIdentity:
    base_url: str
    library_id: str
    workspace_id: str
    entity_query: str
    document_query: str
    question: str


@dataclass(frozen=True)
class ReportSamples:
    tools_list: CurlSample
    entity_search: CurlSample
    document_search: CurlSample
    document_read: CurlSample | None
    graph_topology: CurlSample
    list_relations: CurlSample
    communities: CurlSample
    runtime_execution: CurlSample | None
    runtime_trace: CurlSample | None
    legacy_runtime_execution_probe: CurlSample | None
    grounded_answer: CurlSample | None


@dataclass(frozen=True)
class ReportSummaries:
    entity_search_summary: EntitySearchSummary
    document_search_summary: DocumentSearchSummary
    document_read_summary: DocumentReadSummary | None
    community_summary: CommunitySummary
    runtime_execution_summary: RuntimeExecutionProbeSummary | None
    runtime_trace_summary: RuntimeTraceProbeSummary | None
    legacy_runtime_execution_error: ToolErrorSummary | None
    grounded_answer_summary: GroundedAnswerSummary | None
    graph_quality: McpQualitySummary
    relation_list_summary: RelationListSummary
    assistant_summaries: list[AssistantTurnSummary]


def _sample_report_fields(sample: CurlSample | None) -> tuple[str | int, str, str]:
    if sample is None:
        return "n/a", "n/a", "n/a"
    return (
        sample.status_code,
        format_seconds(sample.time_total_s),
        format_bytes(sample.size_download_bytes),
    )


def _summary_value(summary: Any | None, field_name: str, default: Any) -> Any:
    if summary is None:
        return default
    return getattr(summary, field_name)


def _mcp_probe_report(samples: ReportSamples, summaries: ReportSummaries) -> str:
    tools_status, tools_time, tools_size = _sample_report_fields(samples.tools_list)
    entity_status, entity_time, entity_size = _sample_report_fields(samples.entity_search)
    search_status, search_time, search_size = _sample_report_fields(samples.document_search)
    read_status, read_time, read_size = _sample_report_fields(samples.document_read)
    graph_status, graph_time, graph_size = _sample_report_fields(samples.graph_topology)
    relations_status, relations_time, relations_size = _sample_report_fields(samples.list_relations)
    communities_status, communities_time, communities_size = _sample_report_fields(samples.communities)
    runtime_status, runtime_time, runtime_size = _sample_report_fields(samples.runtime_execution)
    trace_status, trace_time, trace_size = _sample_report_fields(samples.runtime_trace)
    legacy_status, legacy_time, legacy_size = _sample_report_fields(
        samples.legacy_runtime_execution_probe
    )
    grounded_status, grounded_time, grounded_size = _sample_report_fields(
        samples.grounded_answer
    )
    entity = summaries.entity_search_summary
    search = summaries.document_search_summary
    read = summaries.document_read_summary
    graph = summaries.graph_quality
    relations = summaries.relation_list_summary
    communities = summaries.community_summary
    runtime = summaries.runtime_execution_summary
    trace = summaries.runtime_trace_summary
    legacy = summaries.legacy_runtime_execution_error
    grounded = summaries.grounded_answer_summary
    tools_count = len((samples.tools_list.payload.get("result") or {}).get("tools") or [])
    return f"""## MCP probes

| Probe | HTTP | Time | Size | Notes |
|---|---:|---:|---:|---|
| `tools/list` | {tools_status} | {tools_time} | {tools_size} | tools={tools_count} |
| `search_entities` | {entity_status} | {entity_time} | {entity_size} | hits={entity.hit_count} top={entity.top_label or "n/a"} |
| `search_documents` | {search_status} | {search_time} | {search_size} | hits={search.hit_count} top={search.top_document_title or "n/a"} |
| `read_document` | {read_status} | {read_time} | {read_size} | chars={_summary_value(read, "content_length", 0)} refs={_summary_value(read, "total_reference_count", 0)} |
| `get_graph_topology` | {graph_status} | {graph_time} | {graph_size} | quality={graph.quality_status} entities={graph.entity_count} relations={graph.relation_count} docs={graph.document_count} |
| `list_relations` | {relations_status} | {relations_time} | {relations_size} | rows={relations.row_count} |
| `get_communities` | {communities_status} | {communities_time} | {communities_size} | communities={communities.count} summarized={communities.communities_with_summary} |
| `get_runtime_execution` | {runtime_status} | {runtime_time} | {runtime_size} | lifecycle={_summary_value(runtime, "lifecycle_state", "n/a")} |
| `get_runtime_execution_trace` | {trace_status} | {trace_time} | {trace_size} | stages={_summary_value(trace, "stage_count", 0)} actions={_summary_value(trace, "action_count", 0)} |
| `get_runtime_execution (legacy executionId)` | {legacy_status} | {legacy_time} | {legacy_size} | error={_summary_value(legacy, "error_kind", "n/a")} |
| `grounded_answer` | {grounded_status} | {grounded_time} | {grounded_size} | verifier={_summary_value(grounded, "verifier_level", "n/a")} runtime={_summary_value(grounded, "runtime_execution_id", "n/a")} references={len(grounded.references) if grounded else 0} |
"""


def _quality_report(summaries: ReportSummaries) -> str:
    graph = summaries.graph_quality
    relations = summaries.relation_list_summary
    communities = summaries.community_summary
    search = summaries.document_search_summary
    read = summaries.document_read_summary
    suggested_offset = (
        search.top_suggested_start_offset
        if search.top_suggested_start_offset is not None
        else "n/a"
    )
    return f"""
### Graph quality checks

| Check | Value |
|---|---|
| entity rank monotonic | {graph.entity_rank_monotonic} |
| relation rank monotonic | {graph.relation_rank_monotonic} |
| document rank monotonic | {graph.document_rank_monotonic} |
| orphan relations | {graph.orphan_relation_count} |
| orphan links | {graph.orphan_link_count} |
| orphan documents | {graph.orphan_document_count} |
| duplicate entity labels | {graph.duplicate_entity_label_count} |
| duplicate relation signatures | {graph.duplicate_relation_signature_count} |
| top entity label | {graph.top_entity_label or "n/a"} |
| probe entity label | {graph.probe_entity_label or "n/a"} |

### `list_relations` quality checks

| Check | Value |
|---|---|
| relation rows | {relations.row_count} |
| unknown endpoint labels | {relations.unknown_label_count} |
| duplicate relation signatures | {relations.duplicate_signature_count} |

### Community checks

| Check | Value |
|---|---|
| community rows | {communities.count} |
| summaries present | {communities.communities_with_summary} |
| total top entities surfaced | {communities.top_entity_count} |

## MCP document retrieval checks

| Check | Value |
|---|---|
| search hits | {search.hit_count} |
| readable search hits | {search.readable_hit_count} |
| top document title | {search.top_document_title or "n/a"} |
| top suggestedStartOffset | {suggested_offset} |
| top excerpt chars | {search.top_excerpt_length} |
| top hit chunk refs | {search.top_chunk_reference_count} |
| read content chars | {_summary_value(read, "content_length", 0)} |
| read references | {_summary_value(read, "total_reference_count", 0)} |
| read readability | {_summary_value(read, "readability_state", "n/a")} |
"""


def _assistant_run_row(run_number: int, summary: AssistantTurnSummary) -> str:
    return (
        f"| {run_number} | {format_seconds(summary.time_to_first_frame_s)}"
        f" | {format_seconds(summary.time_to_first_tool_call_s)}"
        f" | {format_seconds(summary.time_to_completed_s)}"
        f" | {summary.tool_call_started_count}/{summary.tool_call_finished_count}"
        f" | {summary.answer_length}"
        f" | {summary.total_reference_count}"
        f" | {summary.verification_state or 'n/a'}"
        f" | {summary.query_execution_id or 'n/a'}"
        f" | {summary.runtime_execution_id or 'n/a'}"
        f" | {summary.completion_state or 'n/a'} |\n"
    )


def _assistant_report(summaries: ReportSummaries) -> str:
    assistant_summaries = summaries.assistant_summaries
    avg_completed = statistics.mean(
        summary.time_to_completed_s or 0.0 for summary in assistant_summaries
    )
    avg_references = statistics.mean(
        summary.total_reference_count for summary in assistant_summaries
    )
    rows = "".join(
        _assistant_run_row(run_number, summary)
        for run_number, summary in enumerate(assistant_summaries, start=1)
    )
    return f"""
## Assistant and runtime probes

| Runs | Avg completed | Avg references |
|---:|---:|---:|
| {len(assistant_summaries)} | {format_seconds(avg_completed)} | {avg_references:.1f} |

| Run | First frame | First tool | Completed | Tool events | Answer chars | References | Verification | Query execution | Runtime execution | Lifecycle |
|---|---:|---:|---:|---:|---:|---:|---|---|---|---|
{rows}"""


def _runtime_and_preview_report(summaries: ReportSummaries) -> str:
    runtime = summaries.runtime_execution_summary
    trace = summaries.runtime_trace_summary
    legacy = summaries.legacy_runtime_execution_error
    preview_rows = "".join(
        f"| {run_number} | {format_preview(summary.answer_text).replace('|', r'\|')} |\n"
        for run_number, summary in enumerate(summaries.assistant_summaries, start=1)
    )
    return f"""

### Runtime lookup checks

| Check | Value |
|---|---|
| runtime execution id | {_summary_value(runtime, "runtime_execution_id", "n/a")} |
| runtime lifecycle | {_summary_value(runtime, "lifecycle_state", "n/a")} |
| runtime active stage | {_summary_value(runtime, "active_stage", "n/a")} |
| runtime trace stages | {_summary_value(trace, "stage_count", 0)} |
| runtime trace actions | {_summary_value(trace, "action_count", 0)} |
| runtime trace policy decisions | {_summary_value(trace, "policy_decision_count", 0)} |
| legacy runtime field rejection | {_summary_value(legacy, "error_kind", "n/a")} |

### Assistant answer previews

| Run | Answer preview |
|---|---|
{preview_rows}

### MCP grounded_answer answer preview

| Answer preview | Verifier | Runtime execution id | References |
|---|---|---|---|
"""


def _grounded_answer_report(summary: GroundedAnswerSummary | None) -> str:
    if summary is None:
        return "| n/a | n/a | n/a | 0 |\n"
    preview = format_preview(summary.answer_text).replace("|", "\\|")
    return (
        f"| {preview} | {summary.verifier_level or 'n/a'} | "
        f"{summary.runtime_execution_id or 'n/a'} | {len(summary.references)} |\n"
    )


def _release_gate_report(gate_checks: list[GateCheck]) -> str:
    rows = "".join(
        f"| `{check.label}` | {check.status} | {check.detail} |\n"
        for check in gate_checks
    )
    return f"""

## Release gate

| Check | Status | Detail |
|---|---|---|
{rows}"""


def render_report(
    output_path: pathlib.Path,
    identity: ReportIdentity,
    samples: ReportSamples,
    summaries: ReportSummaries,
    gate_checks: list[GateCheck],
) -> None:
    header = f"""# Agent surface profile

- Generated at: {datetime.now(timezone.utc).isoformat()}
- Base URL: `{identity.base_url}`
- Library ID: `{identity.library_id}`
- Workspace ID: `{identity.workspace_id}`
- Entity query: `{identity.entity_query}`
- Document query: `{identity.document_query}`
- Assistant question: `{identity.question}`

"""
    report = "".join(
        (
            header,
            _mcp_probe_report(samples, summaries),
            _quality_report(summaries),
            _assistant_report(summaries),
            _runtime_and_preview_report(summaries),
            _grounded_answer_report(summaries.grounded_answer_summary),
            _release_gate_report(gate_checks),
        )
    )
    write_private_text(output_path, report)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Profile MCP graph and assistant turn surfaces.")
    parser.add_argument("--base-url", default=DEFAULT_BASE_URL)
    parser.add_argument("--login", default=DEFAULT_LOGIN)
    parser.add_argument("--library-id", required=True)
    parser.add_argument("--workspace-id")
    parser.add_argument(
        "--probe-input-stdin",
        action="store_true",
        help=(
            "Read suite-only password, MCP token, and question inputs from one JSON object on "
            "stdin. Direct invocations use the IRONRAG_* environment variables instead."
        ),
    )
    parser.add_argument(
        "--entity-query",
        default=DEFAULT_ENTITY_QUERY,
        help="Entity search probe query. Defaults to the top entity from graph topology.",
    )
    parser.add_argument(
        "--document-query",
        default=DEFAULT_DOCUMENT_QUERY,
        help="Document search probe query. Defaults to the assistant question.",
    )
    parser.add_argument("--document-limit", type=int, default=DEFAULT_DOCUMENT_LIMIT)
    parser.add_argument("--graph-limit", type=int, default=50)
    parser.add_argument("--read-length", type=int, default=DEFAULT_READ_LENGTH)
    parser.add_argument(
        "--question",
        default=os.environ.get(PROBE_QUESTION_ENV, DEFAULT_ASSISTANT_QUESTION),
        help="Assistant and grounded_answer probe question. Defaults to the top graph entity.",
    )
    parser.add_argument("--assistant-top-k", type=int, default=DEFAULT_ASSISTANT_TOP_K)
    parser.add_argument("--assistant-runs", type=int, default=2)
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
    parser.add_argument("--expected-search-top-label")
    parser.add_argument(
        "--min-answer-overlap-ratio",
        type=float,
        default=DEFAULT_MIN_ANSWER_OVERLAP_RATIO,
        help=(
            "Optional UI/MCP answer text overlap gate. Disabled by default because "
            "the UI may validly synthesize or summarize a matching tool answer."
        ),
    )
    parser.add_argument("--max-tool-latency-ms", type=int)
    parser.add_argument("--max-completed-ms", type=int)
    parser.add_argument("--max-first-frame-ms", type=int, default=DEFAULT_ASSISTANT_MAX_FIRST_FRAME_MS)
    parser.add_argument("--timeout-seconds", type=int, default=120)
    parser.add_argument("--output-path")
    return parser.parse_args(argv)


def resolve_probe_inputs(args: argparse.Namespace) -> ProbeInputs:
    if args.probe_input_stdin:
        try:
            payload = json.loads(sys.stdin.read())
        except json.JSONDecodeError as error:
            raise SystemExit("probe stdin input must be valid JSON") from error
        if not isinstance(payload, dict):
            raise SystemExit("probe stdin input must be a JSON object")
        password = payload.get("password")
        mcp_token = payload.get("mcpToken")
        question = payload.get("question", args.question)
    else:
        password = os.environ.get(PROBE_PASSWORD_ENV)
        mcp_token = os.environ.get(MCP_TOKEN_ENV)
        question = args.question

    if not isinstance(password, str) or not password:
        raise SystemExit(f"{PROBE_PASSWORD_ENV} is required")
    if mcp_token is not None and (not isinstance(mcp_token, str) or not mcp_token):
        raise SystemExit(f"{MCP_TOKEN_ENV} must be a non-empty string when provided")
    if question is not None and not isinstance(question, str):
        raise SystemExit(f"{PROBE_QUESTION_ENV} must be a string when provided")
    return ProbeInputs(password=password, mcp_token=mcp_token, question=question)


@dataclass(frozen=True)
class DocumentProbe:
    search_sample: CurlSample
    search_summary: DocumentSearchSummary
    read_sample: CurlSample | None
    read_summary: DocumentReadSummary | None


@dataclass(frozen=True)
class RuntimeProbe:
    execution_sample: CurlSample | None
    execution_summary: RuntimeExecutionProbeSummary | None
    trace_sample: CurlSample | None
    trace_summary: RuntimeTraceProbeSummary | None
    legacy_sample: CurlSample | None
    legacy_error: ToolErrorSummary | None


@dataclass(frozen=True)
class ProbeExecution:
    identity: ReportIdentity
    samples: ReportSamples
    summaries: ReportSummaries


def _tool_content(sample: CurlSample, tool_name: str) -> dict[str, Any]:
    result = ensure_jsonrpc_result(sample, tool_name)
    if result.get("isError"):
        raise RuntimeError(f"{tool_name} returned tool error: {result!r}")
    content = result.get("structuredContent") or {}
    return content if isinstance(content, dict) else {}


def _discover_mcp_tools(
    session: CurlSession,
    args: argparse.Namespace,
    token: str | None,
) -> CurlSample:
    capabilities = session.request_json(
        "GET",
        MCP_ANSWER_CAPABILITIES_ROUTE,
        bearer_token=token,
        timeout_seconds=args.timeout_seconds,
    )
    answer_tools = session.mcp_request_json(
        MCP_ANSWER_ROUTE,
        body={
            "jsonrpc": "2.0",
            "id": "agent-probe-answer-tools-list",
            "method": MCP_TOOLS_LIST_METHOD,
            "params": {},
        },
        bearer_token=token,
        timeout_seconds=args.timeout_seconds,
    )
    validate_answer_mcp_discovery(capabilities, answer_tools)
    diagnostic_tools = session.mcp_request_json(
        MCP_DIAGNOSTICS_ROUTE,
        body={
            "jsonrpc": "2.0",
            "id": "agent-probe-tools-list",
            "method": MCP_TOOLS_LIST_METHOD,
            "params": {},
        },
        bearer_token=token,
        timeout_seconds=args.timeout_seconds,
    )
    ensure_jsonrpc_result(diagnostic_tools, MCP_TOOLS_LIST_METHOD)
    return diagnostic_tools


def _probe_topology(
    session: CurlSession,
    token: str | None,
    library_ref: str,
    limit: int,
) -> tuple[CurlSample, McpQualitySummary]:
    sample = probe_mcp_tool(
        session,
        bearer_token=token,
        tool_name="get_graph_topology",
        arguments={"library": library_ref, "limit": limit},
    )
    return sample, summarize_graph_quality(_tool_content(sample, "get_graph_topology"))


def _probe_entity_search(
    session: CurlSession,
    token: str | None,
    library_ref: str,
    query: str,
) -> tuple[CurlSample, EntitySearchSummary]:
    sample = probe_mcp_tool(
        session,
        bearer_token=token,
        tool_name="search_entities",
        arguments={"library": library_ref, "query": query, "limit": 10},
    )
    return sample, summarize_entity_search(_tool_content(sample, "search_entities"))


def _probe_document_read(
    session: CurlSession,
    args: argparse.Namespace,
    token: str | None,
    search: DocumentSearchSummary,
) -> tuple[CurlSample | None, DocumentReadSummary | None]:
    if search.top_document_id is None:
        return None, None
    arguments: dict[str, Any] = {
        "documentId": search.top_document_id,
        "mode": "excerpt",
        "length": args.read_length,
        "includeReferences": True,
    }
    if search.top_suggested_start_offset is not None:
        arguments["startOffset"] = search.top_suggested_start_offset
    sample = probe_mcp_tool(
        session,
        bearer_token=token,
        tool_name="read_document",
        arguments=arguments,
    )
    return sample, summarize_document_read(_tool_content(sample, "read_document"))


def _probe_documents(
    session: CurlSession,
    args: argparse.Namespace,
    token: str | None,
    library_ref: str,
    query: str,
) -> DocumentProbe:
    search_sample = probe_mcp_tool(
        session,
        bearer_token=token,
        tool_name="search_documents",
        arguments=build_document_search_arguments(
            library_ref, query, args.document_limit
        ),
    )
    search_summary = summarize_document_search(
        _tool_content(search_sample, "search_documents")
    )
    read_sample, read_summary = _probe_document_read(
        session, args, token, search_summary
    )
    return DocumentProbe(search_sample, search_summary, read_sample, read_summary)


def _probe_relations(
    session: CurlSession,
    token: str | None,
    library_ref: str,
    limit: int,
) -> tuple[CurlSample, RelationListSummary]:
    sample = probe_mcp_tool(
        session,
        bearer_token=token,
        tool_name="list_relations",
        arguments={"library": library_ref, "limit": limit},
    )
    result = ensure_jsonrpc_result(sample, "list_relations")
    return sample, summarize_relation_list(result.get("structuredContent") or [])


def _probe_communities(
    session: CurlSession,
    token: str | None,
    library_ref: str,
    limit: int,
) -> tuple[CurlSample, CommunitySummary]:
    sample = probe_mcp_tool(
        session,
        bearer_token=token,
        tool_name="get_communities",
        arguments={"library": library_ref, "limit": limit},
    )
    return sample, summarize_communities(_tool_content(sample, "get_communities"))


def _probe_grounded_answer(
    session: CurlSession,
    args: argparse.Namespace,
    token: str | None,
    library_ref: str,
    question: str,
) -> tuple[CurlSample, GroundedAnswerSummary]:
    sample = probe_mcp_tool(
        session,
        bearer_token=token,
        tool_name="grounded_answer",
        arguments=build_grounded_answer_probe_arguments(
            library_ref, question, args.assistant_top_k
        ),
        route=MCP_ANSWER_ROUTE,
    )
    return sample, summarize_grounded_answer(_tool_content(sample, "grounded_answer"))


def _probe_assistant_runs(
    session: CurlSession,
    args: argparse.Namespace,
    workspace_id: str,
    question: str,
) -> list[AssistantTurnSummary]:
    summaries: list[AssistantTurnSummary] = []
    for _ in range(args.assistant_runs):
        query_session_id = create_query_session(session, workspace_id, args.library_id)
        summaries.append(
            execute_assistant_turn(
                session,
                query_session_id,
                question,
                top_k=args.assistant_top_k,
                timeout_seconds=args.timeout_seconds,
            )
        )
    return summaries


def _first_runtime_execution_id(
    summaries: list[AssistantTurnSummary],
) -> str | None:
    return next(
        (
            summary.runtime_execution_id
            for summary in summaries
            if summary.runtime_execution_id is not None
        ),
        None,
    )


def _probe_runtime(
    session: CurlSession,
    token: str | None,
    runtime_execution_id: str | None,
) -> RuntimeProbe:
    if runtime_execution_id is None:
        return RuntimeProbe(None, None, None, None, None, None)
    execution_sample = probe_mcp_tool(
        session,
        bearer_token=token,
        tool_name="get_runtime_execution",
        arguments={"runtimeExecutionId": runtime_execution_id},
    )
    execution_summary = summarize_runtime_execution(
        _tool_content(execution_sample, "get_runtime_execution")
    )
    trace_sample = probe_mcp_tool(
        session,
        bearer_token=token,
        tool_name="get_runtime_execution_trace",
        arguments={"runtimeExecutionId": runtime_execution_id},
    )
    trace_summary = summarize_runtime_trace(
        _tool_content(trace_sample, "get_runtime_execution_trace")
    )
    legacy_sample = probe_mcp_tool(
        session,
        bearer_token=token,
        tool_name="get_runtime_execution",
        arguments={"executionId": runtime_execution_id},
    )
    legacy_result = ensure_jsonrpc_result(
        legacy_sample, "get_runtime_execution legacy executionId"
    )
    return RuntimeProbe(
        execution_sample,
        execution_summary,
        trace_sample,
        trace_summary,
        legacy_sample,
        summarize_tool_error(legacy_result),
    )


def _run_probe_surfaces(
    session: CurlSession,
    args: argparse.Namespace,
    probe_inputs: ProbeInputs,
) -> ProbeExecution:
    session.login(args.login, probe_inputs.password)
    catalog = discover_library_catalog_context(session, args.library_id)
    workspace_id = args.workspace_id or catalog.workspace_id
    token = probe_inputs.mcp_token
    tools_list = _discover_mcp_tools(session, args, token)
    topology_sample, graph = _probe_topology(
        session, token, catalog.catalog_ref, args.graph_limit
    )
    entity_query, document_query, question = resolve_probe_queries(
        entity_query=args.entity_query,
        document_query=args.document_query,
        question=probe_inputs.question,
        graph_quality=graph,
    )
    entity_sample, entity_summary = _probe_entity_search(
        session, token, catalog.catalog_ref, entity_query
    )
    documents = _probe_documents(
        session, args, token, catalog.catalog_ref, document_query
    )
    relation_sample, relation_summary = _probe_relations(
        session, token, catalog.catalog_ref, args.graph_limit
    )
    community_sample, community_summary = _probe_communities(
        session, token, catalog.catalog_ref, args.graph_limit
    )
    grounded_sample, grounded_summary = _probe_grounded_answer(
        session, args, token, catalog.catalog_ref, question
    )
    assistant_summaries = _probe_assistant_runs(
        session, args, workspace_id, question
    )
    runtime = _probe_runtime(
        session, token, _first_runtime_execution_id(assistant_summaries)
    )
    return ProbeExecution(
        identity=ReportIdentity(
            base_url=args.base_url,
            library_id=args.library_id,
            workspace_id=workspace_id,
            entity_query=entity_query,
            document_query=document_query,
            question=question,
        ),
        samples=ReportSamples(
            tools_list=tools_list,
            entity_search=entity_sample,
            document_search=documents.search_sample,
            document_read=documents.read_sample,
            graph_topology=topology_sample,
            list_relations=relation_sample,
            communities=community_sample,
            runtime_execution=runtime.execution_sample,
            runtime_trace=runtime.trace_sample,
            legacy_runtime_execution_probe=runtime.legacy_sample,
            grounded_answer=grounded_sample,
        ),
        summaries=ReportSummaries(
            entity_search_summary=entity_summary,
            document_search_summary=documents.search_summary,
            document_read_summary=documents.read_summary,
            community_summary=community_summary,
            runtime_execution_summary=runtime.execution_summary,
            runtime_trace_summary=runtime.trace_summary,
            legacy_runtime_execution_error=runtime.legacy_error,
            grounded_answer_summary=grounded_summary,
            graph_quality=graph,
            relation_list_summary=relation_summary,
            assistant_summaries=assistant_summaries,
        ),
    )


def _tool_samples(samples: ReportSamples) -> list[tuple[str, CurlSample]]:
    required = [
        ("tools_list", samples.tools_list),
        ("search_entities", samples.entity_search),
        ("search_documents", samples.document_search),
        ("get_graph_topology", samples.graph_topology),
        ("list_relations", samples.list_relations),
        ("get_communities", samples.communities),
    ]
    optional = [
        ("read_document", samples.document_read),
        ("get_runtime_execution", samples.runtime_execution),
        ("get_runtime_execution_trace", samples.runtime_trace),
        ("get_runtime_execution_legacy_field", samples.legacy_runtime_execution_probe),
        ("grounded_answer", samples.grounded_answer),
    ]
    required.extend((name, sample) for name, sample in optional if sample is not None)
    return required


def _gate_thresholds(args: argparse.Namespace) -> GateThresholds:
    return GateThresholds(
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
        max_tool_latency_ms=args.max_tool_latency_ms,
        max_completed_ms=args.max_completed_ms,
        max_first_frame_ms=args.max_first_frame_ms,
        min_answer_overlap_ratio=args.min_answer_overlap_ratio,
    )


def _probe_gate_checks(
    execution: ProbeExecution,
    args: argparse.Namespace,
) -> list[GateCheck]:
    summaries = execution.summaries
    return build_gate_checks(
        GateInputs(
            entity_search_summary=summaries.entity_search_summary,
            document_search_summary=summaries.document_search_summary,
            document_read_summary=summaries.document_read_summary,
            graph_quality=summaries.graph_quality,
            relation_list_summary=summaries.relation_list_summary,
            community_summary=summaries.community_summary,
            assistant_summaries=summaries.assistant_summaries,
            runtime_execution_summary=summaries.runtime_execution_summary,
            runtime_trace_summary=summaries.runtime_trace_summary,
            legacy_runtime_execution_error=summaries.legacy_runtime_execution_error,
            grounded_answer_summary=summaries.grounded_answer_summary,
            assistant_require_all=parse_csv_terms(args.assistant_require_all),
            assistant_forbid_any=parse_csv_terms(args.assistant_forbid_any),
            expected_search_top_label=args.expected_search_top_label,
            tool_samples=_tool_samples(execution.samples),
        ),
        _gate_thresholds(args),
    )


def _report_output_path(args: argparse.Namespace) -> pathlib.Path:
    if args.output_path:
        output_path = pathlib.Path(args.output_path)
        output_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        return output_path
    output_directory = pathlib.Path("tmp")
    ensure_private_directory(output_directory)
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return output_directory / f"agent-surface-profile-{timestamp}.md"


def _finish_probe(execution: ProbeExecution, args: argparse.Namespace) -> int:
    gate_checks = _probe_gate_checks(execution, args)
    output_path = _report_output_path(args)
    render_report(
        output_path,
        execution.identity,
        execution.samples,
        execution.summaries,
        gate_checks,
    )
    print(output_path)
    failed_checks = [check for check in gate_checks if check.status == "fail"]
    if not failed_checks:
        return 0
    print(
        "agent surface probe failed release gate: "
        + ", ".join(check.label for check in failed_checks),
        file=sys.stderr,
    )
    return 2


def run_probe(argv: list[str]) -> int:
    args = parse_args(argv)
    probe_inputs = resolve_probe_inputs(args)
    session = CurlSession(args.base_url)
    try:
        execution = _run_probe_surfaces(session, args, probe_inputs)
        return _finish_probe(execution, args)
    except (RuntimeError, TimeoutError, urllib_error.URLError) as exc:
        print(f"agent surface probe failed: {exc}", file=sys.stderr)
        return 1
    finally:
        session.cleanup()


def main(argv: list[str]) -> int:
    return run_probe(argv)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
