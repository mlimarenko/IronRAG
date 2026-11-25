#!/usr/bin/env python3
"""Offline scoring of preserved full grounded_answer JSON-RPC exchanges."""

from __future__ import annotations

import argparse
import copy
import json
import math
from dataclasses import asdict, replace
from pathlib import Path
from typing import Any
from uuid import UUID

import run_live_benchmark as benchmark

MAX_CAPTURE_BYTES = 16 * 1024 * 1024
MAX_JSON_DEPTH = 64
SURFACE = "mcp_grounded_answer"
INVOCATION_POLICY = "explicit_full_history_transient_conversation_no_repair"
TRANSPORT_CREDENTIAL_FIELDS = frozenset({
    "headers", "authorization", "credentials", "accesstoken", "apikey",
    "cookie", "cookies", "setcookie", "password", "secret", "clientsecret",
    "refreshtoken", "bearertoken", "token",
})


def unique_json_object(pairs: list[tuple[str, Any]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON field in native evidence")
        result[key] = value
    return result


def object_fields(value: Any, allowed: set[str], required: set[str]) -> dict:
    if not isinstance(value, dict) or not required <= value.keys() or value.keys() - allowed:
        raise ValueError("invalid native evidence fields")
    return value


def normalized_field_name(key: str) -> str:
    """Case- and separator-insensitive field name, so header variants cannot slip through."""
    return key.casefold().replace("_", "").replace("-", "")


def bounded_visit(node: Any, depth: int) -> None:
    if depth > MAX_JSON_DEPTH:
        raise ValueError("native evidence nesting exceeds bound")
    if isinstance(node, dict):
        for key, child in node.items():
            if not isinstance(key, str) or normalized_field_name(key) in TRANSPORT_CREDENTIAL_FIELDS:
                raise ValueError("transport credentials are not native evidence")
            bounded_visit(child, depth + 1)
    elif isinstance(node, list):
        for child in node:
            bounded_visit(child, depth + 1)


def bounded_capture(value: Any) -> None:
    """Bound in-memory callers too, and refuse transport secrets, not just CLI input."""
    bounded_visit(value, 0)
    if len(json.dumps(value, ensure_ascii=False, allow_nan=False).encode()) > MAX_CAPTURE_BYTES:
        raise ValueError("native evidence exceeds byte bound")


def captured_timing(value: Any) -> dict:
    if value is None:
        return {"status": "unavailable", "answerLatencyMs": None, "searchLatencyMs": None}
    timing = object_fields(
        value, {"source", "answerLatencyMs", "searchLatencyMs"}, {"source", "answerLatencyMs"}
    )
    if timing["source"] != "captured":
        raise ValueError("timing must be a captured observation")
    for key in ("answerLatencyMs", "searchLatencyMs"):
        number = timing.get(key)
        if number is None and key == "searchLatencyMs":
            continue
        if (
            isinstance(number, bool)
            or not isinstance(number, (int, float))
            or not math.isfinite(number)
            or number < 0
        ):
            raise ValueError("invalid captured timing")
    return {
        "status": "captured",
        "answerLatencyMs": timing["answerLatencyMs"],
        "searchLatencyMs": timing.get("searchLatencyMs"),
    }


def native_call(request: Any, case: benchmark.BenchmarkCase, capture: dict) -> tuple[Any, dict]:
    """Validate the JSON-RPC tools/call envelope and return its ID and arguments."""
    request = object_fields(
        request, {"jsonrpc", "id", "method", "params"}, {"jsonrpc", "id", "method", "params"}
    )
    identifier = request["id"]
    if isinstance(identifier, bool) or not isinstance(identifier, (str, int)) or identifier == "":
        raise ValueError("invalid JSON-RPC request ID")
    if request["jsonrpc"] != "2.0" or request["method"] != "tools/call":
        raise ValueError("expected JSON-RPC tools/call")
    params = object_fields(request["params"], {"name", "arguments"}, {"name", "arguments"})
    if params["name"] != "grounded_answer":
        raise ValueError("expected native grounded_answer tool name")
    args = object_fields(params["arguments"], {
        "library", "query", "conversationTurns", "topK", "includeDebug", "responseProfile",
    }, {"library", "query", "responseProfile"})
    validate_native_arguments(args, case, capture)
    return identifier, args


def validate_native_arguments(
    args: dict, case: benchmark.BenchmarkCase, capture: dict
) -> None:
    if args["responseProfile"] != "full":
        raise ValueError("native replay requires explicit full profile without maxReferences")
    if args["query"] != case.question or not 0 < len(case.question) <= 4000 or case.question != case.question.strip():
        raise ValueError("native question mismatch or bound")
    if args["library"] != capture["library"] or not isinstance(args["library"], str) or not args["library"].strip():
        raise ValueError("native library mismatch")
    if "includeDebug" in args and not isinstance(args["includeDebug"], bool):
        raise ValueError("invalid includeDebug")
    if "topK" in args and (type(args["topK"]) is not int or not 1 <= args["topK"] <= 32):
        raise ValueError("invalid topK")


def validate_native_history(prior: Any, history: list[dict]) -> None:
    if not isinstance(prior, list) or len(prior) > 20 or prior != history:
        raise ValueError("native history must exactly preserve observed chronological turns")
    for turn in prior:
        object_fields(turn, {"role", "content"}, {"role", "content"})
        content = turn["content"]
        if not isinstance(content, str) or not 0 < len(content) <= 1200 or content != content.strip():
            raise ValueError("native history content exceeds bounds or would be rewritten")


def native_result(response: Any, identifier: Any) -> dict:
    """Correlate the JSON-RPC response and return its successful result object."""
    response = object_fields(response, {"jsonrpc", "id", "result", "error"}, {"jsonrpc", "id"})
    if response["jsonrpc"] != "2.0" or type(response["id"]) is not type(identifier) or response["id"] != identifier:
        raise ValueError("JSON-RPC response correlation mismatch")
    if "error" in response or "result" not in response:
        raise ValueError("JSON-RPC terminal error")
    result = object_fields(response["result"], {"content", "structuredContent", "isError", "_meta"}, {"content", "structuredContent"})
    if result.get("isError", False) is not False:
        raise ValueError("MCP terminal tool error")
    return result


def native_structured_content(
    result: dict, case: benchmark.BenchmarkCase, capture: dict
) -> tuple[dict, dict, dict]:
    """Return structured content, its execution detail and the canonical turn payload."""
    structured = result["structuredContent"]
    if not isinstance(structured, dict) or structured.get("responseProfile") != "full":
        raise ValueError("compact evidence cannot supply a complete execution")
    detail = structured.get("executionDetail")
    if not isinstance(detail, dict):
        raise ValueError("missing native execution detail")
    canonical = benchmark.canonical_turn_payload(
        detail, expected_question=case.question, expected_library_id=capture["libraryId"],
        expected_workspace_id=capture["workspaceId"],
    )
    if structured.get("answerBody") != canonical["answerText"]:
        raise ValueError("native answer disagrees with execution detail")
    return structured, detail, canonical


def native_tool_text(result: dict, structured: dict, canonical: dict) -> None:
    content = result["content"]
    if (not isinstance(content, list) or len(content) != 1 or not isinstance(content[0], dict)
        or set(content[0]) != {"type", "text"} or content[0]["type"] != "text"
        or not isinstance(content[0]["text"], str)):
        raise ValueError("invalid native tool text block")
    # ok_tool_result appends the structured JSON to the answer for text-only
    # callers. grounded.rs attaches queryLanguage to structuredContent afterwards.
    prefix = canonical["answerText"] + "\n"
    if not content[0]["text"].startswith(prefix):
        raise ValueError("native tool text disagrees with execution detail")
    text_payload = json.loads(content[0]["text"][len(prefix):], object_pairs_hook=unique_json_object)
    if text_payload != {key: value for key, value in structured.items() if key != "queryLanguage"}:
        raise ValueError("native text payload disagrees with structured content")


def native_completion_evidence(structured: dict, detail: dict, canonical: dict) -> None:
    """Terminal completion, readiness and identity shortcuts of one native execution."""
    if structured.get("clarification") != {
        "required": detail["clarification"]["required"],
        "question": detail["clarification"].get("question"),
    } or detail["clarification"]["required"]:
        raise ValueError("native clarification is terminal")
    if (canonical["lifecycleState"] != "completed" or canonical["answerDisposition"] != "factual_ready"
        or structured.get("finalAnswerReady") is not True or structured.get("finalizable") is not True
        or not isinstance(structured.get("completion"), dict)
        or set(structured["completion"]) != {"complete"}
        or structured["completion"]["complete"] is not True):
        raise ValueError("native execution is not final answer ready")
    repair = structured.get("repairPolicy")
    if (not isinstance(repair, dict) or repair.get("required") is not False
        or repair.get("reason") is not None or type(repair.get("maxAdditionalGroundedAnswerCalls")) is not int
        or repair["maxAdditionalGroundedAnswerCalls"] != 0):
        raise ValueError("native evidence requires repair; replay never repairs")
    expected_readiness = {
        "lifecycleState": "completed", "answerDisposition": "factual_ready",
        "finalAnswerReady": True, "finalizable": True, "clarificationRequired": False,
        "completionRequired": False, "failureCode": detail["execution"]["failureCode"],
    }
    readiness = structured.get("readiness")
    if (not isinstance(readiness, dict) or readiness != expected_readiness
        or any(type(readiness[key]) is not bool for key in (
            "finalAnswerReady", "finalizable", "clarificationRequired", "completionRequired"
        ))):
        raise ValueError("native readiness disagrees with execution detail")
    for shortcut, field in {
        "executionId": "id", "runtimeExecutionId": "runtimeExecutionId",
        "conversationId": "conversationId", "workspaceId": "workspaceId",
        "libraryId": "libraryId", "lifecycleState": "lifecycleState",
    }.items():
        if shortcut in structured and structured[shortcut] != detail["execution"][field]:
            raise ValueError("native identity shortcut disagrees with detail")


def native_detail(
    request: Any,
    response: Any,
    case: benchmark.BenchmarkCase,
    capture: dict,
    history: list[dict],
) -> dict:
    identifier, args = native_call(request, case, capture)
    validate_native_history(args.get("conversationTurns", []), history)
    result = native_result(response, identifier)
    structured, detail, canonical = native_structured_content(result, case, capture)
    native_tool_text(result, structured, canonical)
    native_completion_evidence(structured, detail, canonical)
    return detail


def validate_native_capture(
    capture: Any, *, suite_id: str, case: benchmark.BenchmarkCase
) -> dict:
    """Check the captured envelope against the trusted suite and case identity."""
    bounded_capture(capture)
    capture = object_fields(capture, {
        "schemaVersion", "suiteId", "caseId", "caseDefinitionSha256", "workspaceId",
        "libraryId", "library", "turns",
    }, {"schemaVersion", "suiteId", "caseId", "caseDefinitionSha256", "workspaceId", "libraryId", "library", "turns"})
    if (type(capture["schemaVersion"]) is not int or capture["schemaVersion"] != 1
        or capture["suiteId"] != suite_id or capture["caseId"] != case.case_id
        or capture["caseDefinitionSha256"] != benchmark.case_definition_sha256(case)):
        raise ValueError("native suite/case identity mismatch")
    for key in ("workspaceId", "libraryId"):
        value = capture[key]
        if not isinstance(value, str) or str(UUID(value)) != value:
            raise ValueError("invalid native scope identity")
    if (not isinstance(capture["library"], str) or not 0 < len(capture["library"]) <= 256
        or capture["library"] != capture["library"].strip()):
        raise ValueError("native library reference is invalid")
    return capture


def validate_auxiliary_search(search: Any, capture: dict, policy: benchmark.BenchmarkCase) -> None:
    object_fields(
        search,
        {"workspaceId", "libraryId", "queryText", "responsePayload"},
        {"workspaceId", "libraryId", "queryText", "responsePayload"},
    )
    if (search["workspaceId"] != capture["workspaceId"] or search["libraryId"] != capture["libraryId"]
        or search["queryText"] != policy.search_query or not isinstance(search["responsePayload"], dict)
        or not isinstance(search["responsePayload"].get("documentHits"), list)):
        raise ValueError("auxiliary search scope mismatch or missing response")


def validate_captured_cost(cost: Any, detail: dict, execution_id: str) -> None:
    try:
        components = benchmark.validate_cost_observation(cost, execution_id)
    except (KeyError, TypeError, AttributeError, ValueError) as error:
        raise ValueError("invalid captured native cost evidence") from error
    # MCP strips sourceUri/sourceAccess from prepared-segment references.
    # A preserved HTTP cost component can therefore differ in reference
    # metadata, but must identify exactly this execution and both turns.
    cost_detail = components[execution_id]["executionDetail"]
    if any(cost_detail[key] != detail[key] for key in (
        "execution", "requestTurn", "responseTurn"
    )):
        raise ValueError("cost root turn disagrees with native execution")


def native_turn_result(
    turn: Any,
    policy: benchmark.BenchmarkCase,
    capture: dict,
    history: list[dict],
    execution_ids: set[str],
) -> dict:
    """Score one preserved exchange together with its observation."""
    object_fields(
        turn, {"request", "response", "observation"}, {"request", "response", "observation"}
    )
    detail = native_detail(turn["request"], turn["response"], policy, capture, history)
    execution_id = detail["execution"]["id"]
    if execution_id in execution_ids:
        raise ValueError("duplicate native execution ID")
    execution_ids.add(execution_id)
    observation = object_fields(
        turn["observation"], {"timing", "cost", "search"}, {"timing", "cost", "search"}
    )
    timing = captured_timing(observation["timing"])
    search = observation["search"]
    if search is not None:
        validate_auxiliary_search(search, capture, policy)
    elif timing["searchLatencyMs"] is not None:
        raise ValueError("search timing without auxiliary search evidence")
    cost = observation["cost"]
    if cost is not None:
        validate_captured_cost(cost, detail, execution_id)
    result = benchmark.evaluate_execution_detail(
        policy, detail, search_payload=None if search is None else search["responsePayload"],
    )
    result.update({
        "surface": SURFACE, "invocationPolicy": INVOCATION_POLICY,
        "queryConversationId": detail["execution"]["conversationId"],
        "timing": timing, "answerLatencyMs": timing["answerLatencyMs"],
        "searchLatencyMs": timing["searchLatencyMs"],
        "auxiliarySearchStatus": "unavailable" if search is None else "observed",
        "executionCost": {"status": "unavailable" if cost is None else "measured", "evidence": cost,
                          "error": "not_captured" if cost is None else None},
    })
    return result


def final_native_case(
    evaluated: list[dict], case: benchmark.BenchmarkCase, suite_id: str, capture: dict
) -> dict:
    final = dict(evaluated[-1])
    final.update(benchmark.case_metadata(case))
    final["casePolicy"] = asdict(case)
    final["suiteId"] = suite_id
    final["turnExecutions"] = evaluated
    final["rawNativeEvidence"] = copy.deepcopy(capture)
    preceding_failures = [f"turn:{turn['caseId']}:{failure}" for turn in evaluated[:-1] for failure in turn["failedChecks"]]
    final["failedChecks"] = list(final["failedChecks"]) + preceding_failures
    final["strictCasePass"] = all(turn["strictCasePass"] for turn in evaluated)
    final["answerPass"] = evaluated[-1]["answerPass"] and not preceding_failures
    final["conversationHistoryComplete"] = True
    # Reaching this point requires all native exchanges and exact observed
    # history to have passed validation. Do not apply HTTP session invariants
    # or read a saved capture's completeness flag for this transient surface.
    final["conversationCost"] = benchmark.build_conversation_cost_summary(
        [final], validated_history_complete=True
    )
    return final


def replay_native_case(capture: Any, *, suite_id: str, case: benchmark.BenchmarkCase) -> dict:
    """Use trusted suite policy; never infer a complete execution from compact text."""
    capture = validate_native_capture(capture, suite_id=suite_id, case=case)
    benchmark.validate_preceding_turns(
        case.preceding_turns, root=Path(__file__).resolve().parent, final_id=case.case_id
    )
    policies = [benchmark.preceding_turn_case(case, turn) for turn in case.preceding_turns]
    policies.append(replace(case, preceding_turns=[]))
    turns = capture["turns"]
    if not isinstance(turns, list) or not 0 < len(turns) <= 11 or len(turns) != len(policies):
        raise ValueError("native evidence requires every actual turn within history bounds")
    history: list[dict] = []
    evaluated = []
    execution_ids: set[str] = set()
    for turn, policy in zip(turns, policies, strict=True):
        result = native_turn_result(turn, policy, capture, history, execution_ids)
        evaluated.append(result)
        history.extend([{"role": "user", "content": policy.question},
                        {"role": "assistant", "content": result["answer"]}])
    return final_native_case(evaluated, case, suite_id, capture)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", type=Path, required=True)
    parser.add_argument("--case", required=True)
    parser.add_argument("--capture", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    _, cases, suite = benchmark.load_suite(args.suite)
    matching = [case for case in cases if case.case_id == args.case]
    if len(matching) != 1:
        parser.error("case must identify exactly one suite case")
    with args.capture.open("rb") as source:
        raw = source.read(MAX_CAPTURE_BYTES + 1)
    if len(raw) > MAX_CAPTURE_BYTES:
        parser.error("native evidence exceeds byte bound")
    result = replay_native_case(
        json.loads(raw, object_pairs_hook=unique_json_object),
        suite_id=suite["suiteId"], case=matching[0],
    )
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2, allow_nan=False) + "\n")
    return 0 if result["strictCasePass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
