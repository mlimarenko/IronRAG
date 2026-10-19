"""Shared dynamic-module loader for stdlib-only benchmark contract tests."""

from __future__ import annotations

import importlib.util
import pathlib
import sys
from types import ModuleType


REPOSITORY_ROOT = pathlib.Path(__file__).resolve().parents[3]
# One fixed instant for every fixture timestamp: two documents a contract
# test compares must not differ by when each of them was built.
FIXTURE_TIMESTAMP = "2026-08-26T00:00:00+00:00"


def load_benchmark_module(name: str, filename: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, REPOSITORY_ROOT / filename)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load benchmark module: {filename}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def canonical_execution_detail(
    answer: str = "answer",
    verification_state: str = "verified",
    *,
    with_chunk: bool = False,
    execution_id: str = "00000000-0000-4000-8000-000000000001",
    question: str = "question",
    workspace_id: str = "00000000-0000-4000-8000-000000000007",
    library_id: str = "00000000-0000-4000-8000-000000000008",
    answer_disposition: str = "factual_ready",
) -> dict:
    conversation_id = "00000000-0000-4000-8000-000000000002"
    context_bundle_id = "00000000-0000-4000-8000-000000000003"
    runtime_execution_id = "00000000-0000-4000-8000-000000000004"
    response_turn_id = "00000000-0000-4000-8000-000000000005"
    request_turn_id = "00000000-0000-4000-8000-000000000009"
    chunk_references = []
    if with_chunk:
        chunk_references.append(
            {
                "executionId": execution_id,
                "chunkId": "00000000-0000-4000-8000-000000000006",
                "rank": 1,
                "score": 1.0,
            }
        )
    return {
        "contextBundleId": context_bundle_id,
        "execution": {
            "id": execution_id,
            "workspaceId": workspace_id,
            "libraryId": library_id,
            "conversationId": conversation_id,
            "contextBundleId": context_bundle_id,
            "requestTurnId": request_turn_id,
            "responseTurnId": response_turn_id,
            "bindingId": None,
            "runtimeExecutionId": runtime_execution_id,
            "lifecycleState": "completed",
            "activeStage": None,
            "queryText": question,
            "failureCode": None,
            "startedAt": FIXTURE_TIMESTAMP,
        },
        "runtimeSummary": {
            "runtimeExecutionId": runtime_execution_id,
            "lifecycleState": "completed",
            "activeStage": None,
            "turnBudget": 8,
            "turnCount": 1,
            "parallelActionLimit": 4,
            "failureCode": None,
            "failureSummaryRedacted": None,
            "policySummary": {
                "allowCount": 1,
                "rejectCount": 0,
                "terminateCount": 0,
                "recentDecisions": [],
            },
            "acceptedAt": FIXTURE_TIMESTAMP,
            "completedAt": "2026-08-26T00:00:01+00:00",
        },
        "runtimeStageSummaries": [],
        "requestTurn": {
            "id": request_turn_id,
            "conversationId": conversation_id,
            "turnIndex": 0,
            "turnKind": "user",
            "authorPrincipalId": None,
            "contentText": question,
            "executionId": execution_id,
            "createdAt": FIXTURE_TIMESTAMP,
        },
        "responseTurn": {
            "id": response_turn_id,
            "conversationId": conversation_id,
            "turnIndex": 1,
            "turnKind": "assistant",
            "authorPrincipalId": None,
            "contentText": answer,
            "executionId": execution_id,
            "createdAt": "2026-08-26T00:00:01+00:00",
        },
        "chunkReferences": chunk_references,
        "preparedSegmentReferences": [],
        "technicalFactReferences": [],
        "entityReferences": [],
        "relationReferences": [],
        "verificationState": verification_state,
        "verificationWarnings": [],
        "answerDisposition": answer_disposition,
        "clarification": {"required": False, "question": None, "answerCandidates": []},
    }
