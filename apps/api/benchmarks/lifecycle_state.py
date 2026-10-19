"""Canonical machine-verifiable identity for an IronRAG vector lifecycle state."""

from __future__ import annotations

import hashlib
import json
import math
import uuid
from datetime import datetime
from typing import Any, Mapping


SCHEMA_VERSION = 2
VECTOR_ROW_KEYS = (
    "currentReadable",
    "protectedActiveIngest",
    "supersededReadableHistory",
    "inconsistentOrOrphan",
)
PHYSICAL_BYTE_KEYS = ("table", "indexes", "hnsw")
DATABASE_IDENTITY_KEYS = (
    "deploymentId",
    "systemIdentifierSha256",
    "databaseName",
    "databaseOid",
)
SOURCE_STATE_FIELDS = (
    "libraryId",
    "logicalDataSha256",
    "providerConfigurationSha256",
    "providerConfigurationRows",
    "queryVisibleSchemaVersion",
    "queryVisibleRelationRows",
    "vectorRows",
    "vectorIdentitySha256",
    "vectorCategoryIdentitySha256",
    "headBoundVectorIdentitySha256",
    "entityVectorIdentitySha256",
    "entityVectorRows",
    "physicalBytes",
    "physicalConfigurationSha256",
    "physicalConfigurationRows",
    "physicalVectorStateSha256",
)
TREATMENT_INVARIANT_FIELDS = (
    "libraryId",
    "logicalDataSha256",
    "providerConfigurationSha256",
    "providerConfigurationRows",
    "queryVisibleSchemaVersion",
    "queryVisibleRelationRows",
    "entityVectorIdentitySha256",
    "entityVectorRows",
    "physicalConfigurationSha256",
    "physicalConfigurationRows",
)

TURN_REFERENCE_REQUIRED_FIELDS = {
    "chunkReferences": ("executionId", "chunkId", "rank", "score"),
    "preparedSegmentReferences": (
        "executionId", "segmentId", "revisionId", "blockKind", "rank", "score",
        "headingTrail", "sectionPath",
    ),
    "technicalFactReferences": (
        "executionId", "factId", "revisionId", "factKind", "canonicalValue",
        "displayValue", "rank", "score",
    ),
    "entityReferences": ("executionId", "nodeId", "rank", "score", "label"),
    "relationReferences": ("executionId", "edgeId", "rank", "score", "predicate"),
}
VERIFICATION_STATES = {
    "not_run", "verified", "partially_supported", "conflicting",
    "insufficient_evidence", "failed",
}
ANSWER_DISPOSITIONS = {"non_terminal", "factual_ready", "safe_fallback", "clarification"}
RUNTIME_LIFECYCLE_STATES = {
    "accepted", "running", "completed", "recovered", "failed", "canceled"
}
NONCANONICAL_TURN_FIELDS = {
    "content_text", "execution_id", "execution_state", "executionState",
    "document_id", "chunk_id", "normalized_assertion",
}


UTC_SUFFIX = "+00:00"


def parse_utc_timestamp(value: str) -> datetime:
    """Parse an ISO-8601 timestamp, accepting the ``Z`` spelling of UTC.

    Producers in this tree emit both spellings and no reader distinguishes
    them, so the normalization lives here rather than at each parse site.
    """
    return datetime.fromisoformat(value.replace("Z", UTC_SUFFIX))


def stable_json_sha256(payload: Any) -> str:
    encoded = json.dumps(
        payload,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


REQUIRED_TURN_TOP_LEVEL = {
    "contextBundleId",
    "execution",
    "runtimeSummary",
    "runtimeStageSummaries",
    *TURN_REFERENCE_REQUIRED_FIELDS,
    "verificationState",
    "verificationWarnings",
}

REQUIRED_EXECUTION_FIELDS = {
    "id",
    "workspaceId",
    "libraryId",
    "conversationId",
    "contextBundleId",
    "runtimeExecutionId",
    "lifecycleState",
    "queryText",
    "startedAt",
}

EXECUTION_STATE_ALIASES = ("execution_state", "executionState")

EXECUTION_UUID_FIELDS = (
    "workspaceId",
    "libraryId",
    "conversationId",
    "contextBundleId",
    "runtimeExecutionId",
)

EXECUTION_OPTIONAL_UUID_FIELDS = ("requestTurnId", "responseTurnId", "bindingId")

REQUIRED_TURN_FIELDS = {
    "id",
    "conversationId",
    "turnIndex",
    "turnKind",
    "contentText",
    "createdAt",
}

REQUIRED_RUNTIME_FIELDS = {
    "runtimeExecutionId",
    "lifecycleState",
    "turnBudget",
    "turnCount",
    "parallelActionLimit",
    "policySummary",
    "acceptedAt",
}

RUNTIME_COUNTER_FIELDS = ("turnBudget", "turnCount", "parallelActionLimit")

RUNTIME_OPTIONAL_STRING_FIELDS = ("activeStage", "failureCode", "failureSummaryRedacted")

POLICY_SUMMARY_FIELDS = {"allowCount", "rejectCount", "terminateCount", "recentDecisions"}

POLICY_COUNTER_FIELDS = ("allowCount", "rejectCount", "terminateCount")

POLICY_DECISION_FIELDS = ("targetKind", "decisionKind", "reasonCode", "targetId")

REFERENCE_STRING_FIELDS = (
    "blockKind",
    "factKind",
    "canonicalValue",
    "displayValue",
    "label",
    "predicate",
)

REFERENCE_STRING_ARRAY_FIELDS = ("headingTrail", "sectionPath")

REFERENCE_OPTIONAL_UUID_FIELDS = ("documentId",)

REFERENCE_OPTIONAL_STRING_FIELDS = (
    "documentTitle",
    "excerpt",
    "entityType",
    "summary",
    "documentHint",
)

CLARIFICATION_PROVENANCE_FIELDS = ("entityId", "documentId", "chunkId")


def require_object(value: Any, label: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise ValueError(f"turn response requires canonical {label}")
    return value


def require_uuid(value: Any, label: str) -> str:
    if not isinstance(value, str):
        raise ValueError(f"turn response {label} must be a UUID")
    try:
        uuid.UUID(value)
    except ValueError as error:
        raise ValueError(f"turn response {label} must be a UUID") from error
    return value


def require_string(value: Any, label: str, *, nonempty: bool = True) -> str:
    if not isinstance(value, str) or (nonempty and not value):
        raise ValueError(f"turn response {label} must be a string")
    return value


def require_integer(value: Any, label: str, *, minimum: int | None = None) -> int:
    if not isinstance(value, int) or isinstance(value, bool):
        raise ValueError(f"turn response {label} must be an integer")
    if minimum is not None and value < minimum:
        raise ValueError(f"turn response {label} is below its minimum")
    return value


def require_number(value: Any, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"turn response {label} must be a number")
    number = float(value)
    if not math.isfinite(number):
        raise ValueError(f"turn response {label} must be finite")
    return number


def optional_uuid(value: Any, label: str) -> None:
    if value is not None:
        require_uuid(value, label)


def optional_string(value: Any, label: str) -> None:
    if value is not None and not isinstance(value, str):
        raise ValueError(f"turn response {label} must be a string or null")


def require_datetime(value: Any, label: str) -> None:
    require_string(value, label)
    try:
        parsed = parse_utc_timestamp(value)
    except ValueError as error:
        raise ValueError(f"turn response {label} must be an ISO timestamp") from error
    if parsed.tzinfo is None or "T" not in value:
        raise ValueError(f"turn response {label} must be a timezone-aware timestamp")


def is_string_list(value: Any) -> bool:
    """A list holding nothing but strings."""
    return isinstance(value, list) and all(isinstance(item, str) for item in value)


def verify_canonical_turn_names(
    response_turn: Mapping[str, Any], execution: Mapping[str, Any]
) -> None:
    """Reject the snake_case and legacy aliases of the wire contract.

    An alias in the response means the API changed shape underneath the
    benchmark, and the measurement would otherwise compare two contracts as if
    they were one.
    """
    if "content_text" in response_turn:
        raise ValueError("turn response requires canonical responseTurn.contentText")
    if "executionId" in execution:
        raise ValueError("turn response requires canonical execution.id")
    if any(alias in execution for alias in EXECUTION_STATE_ALIASES):
        raise ValueError("turn response requires canonical execution.lifecycleState")


def verify_expected_binding(
    execution: Mapping[str, Any],
    query_text: str,
    *,
    expected_question: str | None,
    expected_library_id: str | None,
    expected_workspace_id: str | None,
) -> None:
    """The turn must answer the question we submitted, where we submitted it."""
    if expected_question is not None and query_text != expected_question:
        raise ValueError(
            "turn response execution.queryText differs from submitted question"
        )
    if expected_library_id is not None and execution["libraryId"] != expected_library_id:
        raise ValueError(
            "turn response execution.libraryId differs from selected library"
        )
    if (
        expected_workspace_id is not None
        and execution["workspaceId"] != expected_workspace_id
    ):
        raise ValueError(
            "turn response execution.workspaceId differs from selected workspace"
        )


def verify_execution_identity(
    payload: Mapping[str, Any],
    execution: Mapping[str, Any],
    *,
    expected_question: str | None,
    expected_library_id: str | None,
    expected_workspace_id: str | None,
) -> tuple[str, str, str]:
    """Return the execution id, lifecycle state and query text it declares."""
    if not REQUIRED_EXECUTION_FIELDS.issubset(execution):
        raise ValueError("turn response execution is incomplete")
    execution_id = require_uuid(execution.get("id"), "execution.id")
    for field in EXECUTION_UUID_FIELDS:
        require_uuid(execution.get(field), f"execution.{field}")
    for field in EXECUTION_OPTIONAL_UUID_FIELDS:
        optional_uuid(execution.get(field), f"execution.{field}")
    lifecycle_state = require_string(
        execution.get("lifecycleState"), "execution.lifecycleState"
    )
    if lifecycle_state not in RUNTIME_LIFECYCLE_STATES:
        raise ValueError("turn response execution.lifecycleState is invalid")
    query_text = require_string(
        execution.get("queryText"), "execution.queryText", nonempty=False
    )
    verify_expected_binding(
        execution,
        query_text,
        expected_question=expected_question,
        expected_library_id=expected_library_id,
        expected_workspace_id=expected_workspace_id,
    )
    require_datetime(execution.get("startedAt"), "execution.startedAt")
    optional_string(execution.get("activeStage"), "execution.activeStage")
    optional_string(execution.get("failureCode"), "execution.failureCode")
    context_bundle_id = require_uuid(payload.get("contextBundleId"), "contextBundleId")
    if context_bundle_id != execution["contextBundleId"]:
        raise ValueError("turn response contextBundleId differs from execution")
    return execution_id, lifecycle_state, query_text


def verify_response_turn(
    response_turn: Mapping[str, Any], execution: Mapping[str, Any], execution_id: str
) -> str:
    """Return the answer text of the assistant turn this execution produced."""
    if not REQUIRED_TURN_FIELDS.issubset(response_turn):
        raise ValueError("turn response responseTurn is incomplete")
    response_turn_id = require_uuid(response_turn.get("id"), "responseTurn.id")
    if response_turn.get("conversationId") != execution["conversationId"]:
        raise ValueError(
            "turn response responseTurn conversation differs from execution"
        )
    require_integer(response_turn.get("turnIndex"), "responseTurn.turnIndex", minimum=0)
    if response_turn.get("turnKind") != "assistant":
        raise ValueError("turn response responseTurn.turnKind must be assistant")
    answer_text = require_string(
        response_turn.get("contentText"), "responseTurn.contentText", nonempty=False
    )
    optional_uuid(
        response_turn.get("authorPrincipalId"), "responseTurn.authorPrincipalId"
    )
    if response_turn.get("executionId") != execution_id:
        raise ValueError("turn response responseTurn executionId differs from execution")
    require_datetime(response_turn.get("createdAt"), "responseTurn.createdAt")
    if execution.get("responseTurnId") not in (None, response_turn_id):
        raise ValueError(
            "turn response execution.responseTurnId differs from responseTurn"
        )
    return answer_text


def verify_request_turn(
    payload: Mapping[str, Any],
    execution: Mapping[str, Any],
    execution_id: str,
    query_text: str,
    expected_question: str | None,
) -> None:
    """The user turn is optional in general and mandatory for a benchmark."""
    request_turn = payload.get("requestTurn")
    if expected_question is not None and request_turn is None:
        raise ValueError("turn response requestTurn is required for benchmark binding")
    if request_turn is None:
        return
    request_turn = require_object(request_turn, "requestTurn")
    if not REQUIRED_TURN_FIELDS.issubset(request_turn):
        raise ValueError("turn response requestTurn is incomplete")
    request_turn_id = require_uuid(request_turn.get("id"), "requestTurn.id")
    if request_turn.get("conversationId") != execution["conversationId"]:
        raise ValueError("turn response requestTurn conversation differs from execution")
    require_integer(request_turn.get("turnIndex"), "requestTurn.turnIndex", minimum=0)
    if request_turn.get("turnKind") != "user":
        raise ValueError("turn response requestTurn.turnKind must be user")
    request_text = require_string(
        request_turn.get("contentText"), "requestTurn.contentText", nonempty=False
    )
    if request_text != query_text:
        raise ValueError(
            "turn response requestTurn.contentText differs from execution.queryText"
        )
    if expected_question is not None and request_text != expected_question:
        raise ValueError(
            "turn response requestTurn.contentText differs from submitted question"
        )
    optional_uuid(request_turn.get("authorPrincipalId"), "requestTurn.authorPrincipalId")
    if request_turn.get("executionId") not in (None, execution_id):
        raise ValueError("turn response requestTurn executionId differs from execution")
    require_datetime(request_turn.get("createdAt"), "requestTurn.createdAt")
    if execution.get("requestTurnId") not in (None, request_turn_id):
        raise ValueError(
            "turn response execution.requestTurnId differs from requestTurn"
        )


def verify_policy_summary(runtime: Mapping[str, Any]) -> None:
    """Policy counters and the decisions the runtime kept a record of."""
    policy = require_object(runtime.get("policySummary"), "runtimeSummary.policySummary")
    if not POLICY_SUMMARY_FIELDS.issubset(policy):
        raise ValueError("turn response policySummary is incomplete")
    for field in POLICY_COUNTER_FIELDS:
        require_integer(policy.get(field), f"policySummary.{field}", minimum=0)
    decisions = policy.get("recentDecisions")
    if not isinstance(decisions, list):
        raise ValueError("turn response policySummary.recentDecisions must be an array")
    for decision in decisions:
        decision = require_object(decision, "policy decision")
        for field in POLICY_DECISION_FIELDS:
            require_string(decision.get(field), f"policy decision.{field}")
        require_datetime(decision.get("decidedAt"), "policy decision.decidedAt")


def verify_runtime_summary(
    payload: Mapping[str, Any], execution: Mapping[str, Any]
) -> None:
    """The runtime the execution ran on, and the capacity counters it reports."""
    runtime = require_object(payload.get("runtimeSummary"), "runtimeSummary")
    if not REQUIRED_RUNTIME_FIELDS.issubset(runtime):
        raise ValueError("turn response runtimeSummary is incomplete")
    if runtime.get("runtimeExecutionId") != execution["runtimeExecutionId"]:
        raise ValueError("turn response runtime execution identity differs")
    runtime_lifecycle_state = require_string(
        runtime.get("lifecycleState"), "runtimeSummary.lifecycleState"
    )
    if runtime_lifecycle_state not in RUNTIME_LIFECYCLE_STATES:
        raise ValueError("turn response runtimeSummary.lifecycleState is invalid")
    for field in RUNTIME_COUNTER_FIELDS:
        require_integer(runtime.get(field), f"runtimeSummary.{field}", minimum=0)
    if runtime["turnCount"] > runtime["turnBudget"] or runtime["parallelActionLimit"] == 0:
        raise ValueError("turn response runtime capacity counters are inconsistent")
    for field in RUNTIME_OPTIONAL_STRING_FIELDS:
        optional_string(runtime.get(field), f"runtimeSummary.{field}")
    require_datetime(runtime.get("acceptedAt"), "runtimeSummary.acceptedAt")
    if runtime.get("completedAt") is not None:
        require_datetime(runtime["completedAt"], "runtimeSummary.completedAt")
    verify_policy_summary(runtime)


def verify_runtime_stages(payload: Mapping[str, Any]) -> None:
    """Per-stage rows of the runtime timeline."""
    stages = payload.get("runtimeStageSummaries")
    if not isinstance(stages, list):
        raise ValueError("turn response runtimeStageSummaries must be an array")
    for stage in stages:
        stage = require_object(stage, "runtime stage")
        require_string(stage.get("stageKind"), "runtime stage.stageKind")
        require_string(stage.get("stageLabel"), "runtime stage.stageLabel")
        if stage.get("durationMs") is not None:
            require_integer(stage["durationMs"], "runtime stage.durationMs", minimum=0)


def verify_reference_identity(
    field: str,
    reference: Any,
    required_fields: tuple[str, ...],
    execution_id: str,
) -> None:
    """Shape, canonical field names and execution binding of one reference row."""
    if not isinstance(reference, Mapping):
        raise ValueError(f"turn response {field} entries must be objects")
    aliases = NONCANONICAL_TURN_FIELDS.intersection(reference)
    if aliases:
        raise ValueError(f"turn response {field} contains noncanonical {min(aliases)}")
    if any(required not in reference for required in required_fields):
        raise ValueError(f"turn response {field} entry is incomplete")
    if (
        require_uuid(reference.get("executionId"), f"{field}.executionId")
        != execution_id
    ):
        raise ValueError(f"turn response {field} belongs to another execution")


def verify_reference_fields(
    field: str, reference: Mapping[str, Any], required_fields: tuple[str, ...]
) -> None:
    """Types of the fields this reference kind declares required."""
    for key in required_fields:
        if key.endswith("Id") and key != "executionId":
            require_uuid(reference.get(key), f"{field}.{key}")
    require_integer(reference.get("rank"), f"{field}.rank", minimum=0)
    require_number(reference.get("score"), f"{field}.score")
    for key in REFERENCE_STRING_FIELDS:
        if key in required_fields:
            require_string(reference.get(key), f"{field}.{key}")
    for key in REFERENCE_STRING_ARRAY_FIELDS:
        if key in required_fields and not is_string_list(reference.get(key)):
            raise ValueError(f"turn response {field}.{key} must be a string array")


def verify_reference_optional_fields(field: str, reference: Mapping[str, Any]) -> None:
    """Fields that stay optional, but are still typed once present."""
    for key in REFERENCE_OPTIONAL_UUID_FIELDS:
        if key in reference:
            optional_uuid(reference.get(key), f"{field}.{key}")
    for key in REFERENCE_OPTIONAL_STRING_FIELDS:
        if key in reference:
            optional_string(reference.get(key), f"{field}.{key}")


def verify_reference_kind_extras(field: str, reference: Mapping[str, Any]) -> None:
    """The per-kind rules the shared reference contract does not cover."""
    if (
        field == "chunkReferences"
        and "sectionPath" in reference
        and not is_string_list(reference["sectionPath"])
    ):
        raise ValueError("turn response chunkReferences.sectionPath must be a string array")
    if field == "preparedSegmentReferences" and reference.get("sourceAccess") is not None:
        source_access = require_object(
            reference["sourceAccess"], "preparedSegmentReferences.sourceAccess"
        )
        require_string(source_access.get("kind"), "sourceAccess.kind")
        require_string(source_access.get("href"), "sourceAccess.href")
    if (
        field == "relationReferences"
        and "normalizedAssertion" in reference
        and not (
            reference["normalizedAssertion"] is None
            or isinstance(reference["normalizedAssertion"], str)
        )
    ):
        raise ValueError("turn response relation normalizedAssertion is invalid")


def verify_reference_rows(payload: Mapping[str, Any], execution_id: str) -> int:
    """Check every reference list on the turn and return how many rows it held."""
    reference_count = 0
    for field, required_fields in TURN_REFERENCE_REQUIRED_FIELDS.items():
        references = payload.get(field)
        if not isinstance(references, list):
            raise ValueError(f"turn response requires canonical {field}")
        for reference in references:
            verify_reference_identity(field, reference, required_fields, execution_id)
            verify_reference_fields(field, reference, required_fields)
            verify_reference_optional_fields(field, reference)
            verify_reference_kind_extras(field, reference)
        reference_count += len(references)
    return reference_count


def verify_verification_warnings(payload: Mapping[str, Any]) -> None:
    """Warnings the verifier attached to the answer."""
    warnings = payload.get("verificationWarnings")
    if not isinstance(warnings, list):
        raise ValueError("turn response verificationWarnings must be an array")
    for warning in warnings:
        warning = require_object(warning, "verification warning")
        require_string(warning.get("code"), "verification warning.code")
        require_string(warning.get("message"), "verification warning.message")
        optional_uuid(
            warning.get("relatedSegmentId"), "verification warning.relatedSegmentId"
        )
        optional_uuid(warning.get("relatedFactId"), "verification warning.relatedFactId")


def verify_reference_summary(payload: Mapping[str, Any], reference_count: int) -> None:
    """The declared counts must match the reference rows actually returned."""
    reference_summary = payload.get("referenceSummary")
    if reference_summary is None:
        return
    reference_summary = require_object(reference_summary, "referenceSummary")
    total_count = require_integer(
        reference_summary.get("totalCount"), "referenceSummary.totalCount", minimum=0
    )
    returned_count = require_integer(
        reference_summary.get("returnedCount"),
        "referenceSummary.returnedCount",
        minimum=0,
    )
    if not isinstance(reference_summary.get("truncated"), bool):
        raise ValueError("turn response referenceSummary.truncated must be boolean")
    if returned_count != reference_count or total_count < returned_count:
        raise ValueError("turn response referenceSummary counts are inconsistent")


def verify_clarification(payload: Mapping[str, Any]) -> None:
    """The clarification block, when the runtime asked instead of answering."""
    clarification = payload.get("clarification")
    if clarification is None:
        return
    clarification = require_object(clarification, "clarification")
    if not isinstance(clarification.get("required"), bool):
        raise ValueError("turn response clarification.required must be boolean")
    optional_string(clarification.get("question"), "clarification.question")
    candidates = clarification.get("answerCandidates", [])
    if not isinstance(candidates, list):
        raise ValueError("turn response clarification.answerCandidates must be an array")
    for candidate in candidates:
        candidate = require_object(candidate, "clarification candidate")
        require_string(candidate.get("label"), "clarification candidate.label")
        require_string(candidate.get("kind"), "clarification candidate.kind")
        if candidate.get("confidence") is not None:
            require_number(candidate["confidence"], "clarification candidate.confidence")
        provenance = require_object(
            candidate.get("provenance"), "clarification candidate.provenance"
        )
        for field in CLARIFICATION_PROVENANCE_FIELDS:
            optional_uuid(provenance.get(field), f"clarification candidate.{field}")


def canonical_turn_payload(
    payload: Mapping[str, Any],
    *,
    expected_question: str | None = None,
    expected_library_id: str | None = None,
    expected_workspace_id: str | None = None,
) -> dict[str, Any]:
    """Validate the complete public AssistantExecutionDetail wire contract."""
    if not REQUIRED_TURN_TOP_LEVEL.issubset(payload):
        raise ValueError("turn response is not a complete AssistantExecutionDetail")
    response_turn = require_object(payload.get("responseTurn"), "responseTurn")
    execution = require_object(payload.get("execution"), "execution")
    verify_canonical_turn_names(response_turn, execution)
    execution_id, lifecycle_state, query_text = verify_execution_identity(
        payload,
        execution,
        expected_question=expected_question,
        expected_library_id=expected_library_id,
        expected_workspace_id=expected_workspace_id,
    )
    answer_text = verify_response_turn(response_turn, execution, execution_id)
    verify_request_turn(payload, execution, execution_id, query_text, expected_question)
    verify_runtime_summary(payload, execution)
    verify_runtime_stages(payload)
    verification_state = payload.get("verificationState")
    if verification_state not in VERIFICATION_STATES:
        raise ValueError("turn response requires canonical verificationState")
    reference_count = verify_reference_rows(payload, execution_id)
    verify_verification_warnings(payload)
    disposition = payload.get("answerDisposition", "non_terminal")
    if disposition not in ANSWER_DISPOSITIONS:
        raise ValueError("turn response answerDisposition is invalid")
    verify_reference_summary(payload, reference_count)
    verify_clarification(payload)
    return {
        "answerText": answer_text,
        "executionId": execution_id,
        "lifecycleState": lifecycle_state,
        "verificationState": verification_state,
        "answerDisposition": disposition,
        "referenceCount": reference_count,
        "queryText": query_text,
        "libraryId": execution["libraryId"],
        "workspaceId": execution["workspaceId"],
    }


def load_workload_sha256(source_sha256: str, top_k: int, repetitions: int) -> str:
    if (
        not valid_sha256(source_sha256)
        or not isinstance(top_k, int)
        or isinstance(top_k, bool)
        or top_k <= 0
        or not isinstance(repetitions, int)
        or isinstance(repetitions, bool)
        or repetitions <= 0
    ):
        raise ValueError("load workload identity is invalid")
    return stable_json_sha256(
        {
            "sourceSha256": source_sha256,
            "topK": top_k,
            "repetitions": repetitions,
        }
    )


def golden_source_sha256(suites: list[Mapping[str, Any]]) -> str:
    """Fingerprint trusted suite policy and corpus identities independent of results."""
    definition = []
    for suite in suites:
        suite_meta = suite.get("suite")
        integrity = suite.get("integrity")
        cases = suite.get("cases")
        if not isinstance(suite_meta, Mapping) or not isinstance(integrity, Mapping):
            raise ValueError("golden suite source identity is incomplete")
        if not isinstance(cases, list) or not cases:
            raise ValueError("golden suite source requires cases")
        case_policies = []
        for case in cases:
            if not isinstance(case, Mapping) or not isinstance(case.get("casePolicy"), Mapping):
                raise ValueError("golden case policy is missing")
            case_policies.append(dict(case["casePolicy"]))
        definition.append(
            {
                "suiteId": suite_meta.get("suiteId"),
                "strictBlocking": suite.get("strictBlocking"),
                "integrity": dict(integrity),
                "casePolicies": sorted(case_policies, key=lambda item: str(item.get("case_id"))),
            }
        )
    return stable_json_sha256(
        {
            "schemaVersion": 1,
            "suites": sorted(definition, key=lambda item: str(item.get("suiteId"))),
        }
    )


def valid_sha256(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def nonnegative_integer(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def build_data_state_identity(document: Mapping[str, Any]) -> dict[str, Any]:
    """Derive the only accepted identity from a collector inventory document."""
    inventory = document.get("inventory")
    if not isinstance(inventory, Mapping):
        raise ValueError("collector document is missing inventory")
    identity = {
        "schemaVersion": document.get("schemaVersion"),
        "role": document.get("role"),
        "observedAt": document.get("observedAt"),
        "databaseIdentity": inventory.get("databaseIdentity"),
        "libraryId": inventory.get("libraryId"),
        "logicalDataSha256": inventory.get("logicalDataSha256"),
        "providerConfigurationSha256": inventory.get("providerConfigurationSha256"),
        "providerConfigurationRows": inventory.get("providerConfigurationRows"),
        "queryVisibleSchemaVersion": inventory.get("queryVisibleSchemaVersion"),
        "queryVisibleRelationRows": inventory.get("queryVisibleRelationRows"),
        "evidenceArtifactSha256": stable_json_sha256(inventory),
        "vectorRows": inventory.get("vectorRows"),
        "vectorIdentitySha256": inventory.get("vectorIdentitySha256"),
        "vectorCategoryIdentitySha256": inventory.get(
            "vectorCategoryIdentitySha256"
        ),
        "headBoundVectorIdentitySha256": inventory.get(
            "headBoundVectorIdentitySha256"
        ),
        "entityVectorIdentitySha256": inventory.get("entityVectorIdentitySha256"),
        "entityVectorRows": inventory.get("entityVectorRows"),
        "physicalBytes": inventory.get("physicalBytes"),
        "physicalConfigurationSha256": inventory.get("physicalConfigurationSha256"),
        "physicalConfigurationRows": inventory.get("physicalConfigurationRows"),
    }
    identity["physicalVectorStateSha256"] = stable_json_sha256(
        {
            "libraryId": identity["libraryId"],
            "vectorRows": identity["vectorRows"],
            "vectorIdentitySha256": identity["vectorIdentitySha256"],
            "vectorCategoryIdentitySha256": identity[
                "vectorCategoryIdentitySha256"
            ],
            "headBoundVectorIdentitySha256": identity[
                "headBoundVectorIdentitySha256"
            ],
            "entityVectorIdentitySha256": identity["entityVectorIdentitySha256"],
            "entityVectorRows": identity["entityVectorRows"],
            "physicalBytes": identity["physicalBytes"],
            "physicalConfigurationSha256": identity["physicalConfigurationSha256"],
            "physicalConfigurationRows": identity["physicalConfigurationRows"],
        }
    )
    identity["fingerprintSha256"] = stable_json_sha256(identity)
    return identity


IDENTITY_DIGEST_KEYS = (
    "logicalDataSha256",
    "providerConfigurationSha256",
    "physicalConfigurationSha256",
    "evidenceArtifactSha256",
    "vectorIdentitySha256",
    "headBoundVectorIdentitySha256",
    "entityVectorIdentitySha256",
    "physicalVectorStateSha256",
    "fingerprintSha256",
)

MINIMUM_ROW_COUNTS = (
    ("providerConfigurationRows", 4),
    ("physicalConfigurationRows", 1),
    ("entityVectorRows", 0),
)


def invalid_envelope_fields(identity: Mapping[str, Any]) -> list[str]:
    """Schema version, role and the timezone-aware observation time."""
    invalid: list[str] = []
    if identity.get("schemaVersion") != SCHEMA_VERSION:
        invalid.append("schemaVersion")
    if identity.get("role") not in {"baseline", "candidate"}:
        invalid.append("role")
    try:
        observed_at = identity.get("observedAt")
        if not isinstance(observed_at, str):
            raise ValueError
        if parse_utc_timestamp(observed_at).tzinfo is None:
            raise ValueError
    except ValueError:
        invalid.append("observedAt")
    return invalid


def invalid_database_identity_fields(database_identity: Any) -> list[str]:
    """The database this state was read from must be fully identified."""
    if not isinstance(database_identity, Mapping):
        return ["databaseIdentity"]
    invalid = [
        f"databaseIdentity.{key}"
        for key in DATABASE_IDENTITY_KEYS
        if not isinstance(database_identity.get(key), str)
        or not database_identity.get(key)
    ]
    if not valid_sha256(database_identity.get("systemIdentifierSha256")):
        invalid.append("databaseIdentity.systemIdentifierSha256")
    return invalid


def invalid_query_visible_fields(identity: Mapping[str, Any]) -> list[str]:
    """The library and the query-visible relation counts behind it."""
    invalid: list[str] = []
    library_id = identity.get("libraryId")
    if not isinstance(library_id, str) or not library_id:
        invalid.append("libraryId")
    if identity.get("queryVisibleSchemaVersion") != 1:
        invalid.append("queryVisibleSchemaVersion")
    relation_rows = identity.get("queryVisibleRelationRows")
    if (
        not isinstance(relation_rows, Mapping)
        or not relation_rows
        or any(
            not nonnegative_integer(value) or value < 1
            for value in relation_rows.values()
        )
    ):
        invalid.append("queryVisibleRelationRows")
    return invalid


def invalid_digest_fields(identity: Mapping[str, Any]) -> list[str]:
    """Every field that must carry one concrete digest."""
    return [key for key in IDENTITY_DIGEST_KEYS if not valid_sha256(identity.get(key))]


def invalid_row_counts(identity: Mapping[str, Any]) -> list[str]:
    """Row counts that must exist and, where the table must be populated, reach its floor."""
    return [
        key
        for key, minimum in MINIMUM_ROW_COUNTS
        if not nonnegative_integer(identity.get(key)) or identity.get(key, 0) < minimum
    ]


def invalid_vector_category_fields(identity: Mapping[str, Any]) -> list[str]:
    """One digest per vector category."""
    category_identities = identity.get("vectorCategoryIdentitySha256")
    if not isinstance(category_identities, Mapping):
        return ["vectorCategoryIdentitySha256"]
    return [
        f"vectorCategoryIdentitySha256.{key}"
        for key in VECTOR_ROW_KEYS
        if not valid_sha256(category_identities.get(key))
    ]


def invalid_vector_row_fields(vector_rows: Any) -> list[str]:
    """Per-category counts, and a total that is exactly their sum."""
    if not isinstance(vector_rows, Mapping):
        return ["vectorRows"]
    keys = (*VECTOR_ROW_KEYS, "total")
    invalid = [
        f"vectorRows.{key}" for key in keys if not nonnegative_integer(vector_rows.get(key))
    ]
    if invalid:
        return invalid
    if vector_rows["total"] != sum(vector_rows[key] for key in VECTOR_ROW_KEYS):
        invalid.append("vectorRows.total")
    return invalid


def invalid_physical_byte_fields(physical_bytes: Any) -> list[str]:
    """Table and index sizes that add up, with HNSW inside the index total."""
    if not isinstance(physical_bytes, Mapping):
        return ["physicalBytes"]
    keys = (*PHYSICAL_BYTE_KEYS, "total")
    invalid = [
        f"physicalBytes.{key}"
        for key in keys
        if not nonnegative_integer(physical_bytes.get(key))
    ]
    if invalid:
        return invalid
    if physical_bytes["total"] != physical_bytes["table"] + physical_bytes["indexes"]:
        invalid.append("physicalBytes.total")
    if physical_bytes["hnsw"] > physical_bytes["indexes"]:
        invalid.append("physicalBytes.hnsw")
    return invalid


def invalid_derived_digests(identity: Mapping[str, Any]) -> list[str]:
    """The two digests the collector derives from everything else it recorded.

    A stale one means the document was edited after collection, which is the
    single failure this whole identity exists to make detectable.
    """
    invalid: list[str] = []
    expected_physical = stable_json_sha256(
        {
            "libraryId": identity.get("libraryId"),
            "vectorRows": identity.get("vectorRows"),
            "vectorIdentitySha256": identity.get("vectorIdentitySha256"),
            "vectorCategoryIdentitySha256": identity.get("vectorCategoryIdentitySha256"),
            "headBoundVectorIdentitySha256": identity.get(
                "headBoundVectorIdentitySha256"
            ),
            "entityVectorIdentitySha256": identity.get("entityVectorIdentitySha256"),
            "entityVectorRows": identity.get("entityVectorRows"),
            "physicalBytes": identity.get("physicalBytes"),
            "physicalConfigurationSha256": identity.get("physicalConfigurationSha256"),
            "physicalConfigurationRows": identity.get("physicalConfigurationRows"),
        }
    )
    if identity.get("physicalVectorStateSha256") != expected_physical:
        invalid.append("physicalVectorStateSha256:stale")
    definition = {
        key: value for key, value in identity.items() if key != "fingerprintSha256"
    }
    if identity.get("fingerprintSha256") != stable_json_sha256(definition):
        invalid.append("fingerprintSha256:stale")
    return invalid


def validate_data_state_identity(identity: Mapping[str, Any]) -> list[str]:
    return sorted(
        {
            *invalid_envelope_fields(identity),
            *invalid_database_identity_fields(identity.get("databaseIdentity")),
            *invalid_query_visible_fields(identity),
            *invalid_digest_fields(identity),
            *invalid_row_counts(identity),
            *invalid_vector_category_fields(identity),
            *invalid_vector_row_fields(identity.get("vectorRows")),
            *invalid_physical_byte_fields(identity.get("physicalBytes")),
            *invalid_derived_digests(identity),
        }
    )


def load_collector_document(path: Any) -> tuple[dict[str, Any], dict[str, Any]]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError("collector file must contain one JSON object")
    identity = build_data_state_identity(payload)
    invalid = validate_data_state_identity(identity)
    if invalid:
        raise ValueError("invalid collector identity: " + ", ".join(invalid))
    if payload.get("dataStateIdentity") != identity:
        raise ValueError("collector dataStateIdentity is stale")
    return payload, identity
