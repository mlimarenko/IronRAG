from __future__ import annotations

import copy
import json
import subprocess
import sys
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import run_live_benchmark as benchmark
import native_surface_evidence as native
from test_answer_completeness import hardening_case
from test_execution_cost import evidence, snapshot
from test_rank_metrics import neutral_benchmark_case
from apps.api.benchmarks.benchmark_test_support import canonical_execution_detail


def capture_for(case, answers=None, *, search=True):
    policies = [benchmark.preceding_turn_case(case, turn) for turn in case.preceding_turns]
    policies.append(replace(case, preceding_turns=[]))
    turns = []
    history = []
    for index, policy in enumerate(policies):
        answer = answers[index] if answers else "answer"
        detail = canonical_execution_detail(
            answer, question=policy.question,
            execution_id=f"00000000-0000-4000-8000-{index + 1:012d}",
        )
        # Each native tools/call creates its own transient query conversation.
        conversation = f"00000000-0000-4000-8001-{index + 1:012d}"
        for row in (detail["execution"], detail["requestTurn"], detail["responseTurn"]):
            row["conversationId"] = conversation
        execution = detail["execution"]
        structured = {
            "answerBody": answer, "responseProfile": "full", "executionDetail": detail,
            "finalAnswerReady": True, "finalizable": True, "completion": {"complete": True},
            "repairPolicy": {"required": False, "reason": None, "maxAdditionalGroundedAnswerCalls": 0},
            "readiness": {"lifecycleState": "completed", "answerDisposition": "factual_ready",
                          "finalAnswerReady": True, "finalizable": True, "clarificationRequired": False,
                          "completionRequired": False, "failureCode": None},
            "clarification": {"required": False, "question": None},
            "answerCandidates": [], "mustPreserveSpans": [], "queryLanguage": "en",
            **{key: execution[key] for key in ("runtimeExecutionId", "conversationId", "libraryId", "workspaceId", "lifecycleState")},
            "executionId": execution["id"],
        }
        turns.append({
            "request": {"jsonrpc": "2.0", "id": index + 1, "method": "tools/call", "params": {
                "name": "grounded_answer", "arguments": {"library": "ide/benchmark", "query": policy.question,
                    "responseProfile": "full", "conversationTurns": copy.deepcopy(history)},
            }},
            "response": {"jsonrpc": "2.0", "id": index + 1, "result": {
                "isError": False, "content": [{"type": "text", "text": answer + "\n" + json.dumps({
                    key: value for key, value in structured.items() if key != "queryLanguage"
                }, indent=2)}], "structuredContent": structured,
            }},
            "observation": {"timing": None, "cost": None, "search": {
                "workspaceId": execution["workspaceId"], "libraryId": execution["libraryId"],
                "queryText": policy.search_query, "responsePayload": {"documentHits": []},
            } if search else None},
        })
        history.extend([{"role": "user", "content": policy.question}, {"role": "assistant", "content": answer}])
    return {"schemaVersion": 1, "suiteId": "native-test", "caseId": case.case_id,
            "caseDefinitionSha256": benchmark.case_definition_sha256(case),
            "workspaceId": execution["workspaceId"], "libraryId": execution["libraryId"],
            "library": "ide/benchmark", "turns": turns}


def replay(capture, case):
    return native.replay_native_case(capture, suite_id="native-test", case=case)


def structured(capture, index=0):
    return capture["turns"][index]["response"]["result"]["structuredContent"]


def refresh_text(capture, index=0):
    payload = structured(capture, index)
    capture["turns"][index]["response"]["result"]["content"][0]["text"] = payload["answerBody"] + "\n" + json.dumps({
        key: value for key, value in payload.items() if key != "queryLanguage"
    })


class NativeEvidenceTests(unittest.TestCase):
    def test_omitted_optional_clarification_question_replays_without_mutating_capture(self):
        case = neutral_benchmark_case("optional-question")
        capture = capture_for(case)
        structured(capture)["executionDetail"]["clarification"].pop("question")
        refresh_text(capture)
        original = copy.deepcopy(capture)
        result = replay(capture, case)
        self.assertTrue(result["strictCasePass"])
        self.assertEqual(result["rawNativeEvidence"], original)
        self.assertEqual(capture, original)

    def test_omitted_detail_question_does_not_allow_conflicting_envelope_question(self):
        case = neutral_benchmark_case("conflicting-question")
        capture = capture_for(case)
        structured(capture)["executionDetail"]["clarification"].pop("question")
        structured(capture)["clarification"]["question"] = "Unexpected clarification"
        refresh_text(capture)
        with self.assertRaises(ValueError):
            replay(capture, case)

    def test_full_detail_has_transport_free_quality_owner(self):
        case = neutral_benchmark_case("native")
        detail = canonical_execution_detail(question=case.question)
        result = benchmark.evaluate_execution_detail(case, detail, search_payload=None)
        self.assertTrue(result["answerPass"])
        self.assertFalse(result["strictCasePass"])
        self.assertIn("auxiliary_search_unavailable", result["failedChecks"])

    def test_full_native_and_http_share_quality_without_fake_http_evidence(self):
        from unittest.mock import patch
        case = replace(neutral_benchmark_case("native"), answer_required_all=["answer"])
        capture = capture_for(case)
        original = copy.deepcopy(capture)
        with patch.object(benchmark, "evaluate_execution_detail", wraps=benchmark.evaluate_execution_detail) as owner:
            result = replay(capture, case)
        owner.assert_called_once()
        self.assertTrue(result["strictCasePass"])
        self.assertEqual(result["surface"], "mcp_grounded_answer")
        self.assertIn("no_repair", result["invocationPolicy"])
        self.assertNotIn("rawEvaluationEvidence", result)
        self.assertEqual(result["rawNativeEvidence"], original)
        self.assertEqual(capture, original)
        http = benchmark.evaluate_execution_detail(case, structured(capture)["executionDetail"], search_payload={"documentHits": []})
        self.assertEqual(result["failedChecks"], http["failedChecks"])

    def test_annotation_negation_and_absence_rubric_is_not_waived(self):
        annotated = hardening_case("aurora_quorum_fixed_size")
        case = replace(neutral_benchmark_case("native"), expected_facts=annotated.expected_facts,
                       mandatory_caveats=annotated.mandatory_caveats)
        for answer, passes in (("The quorum uses 5 nodes. The autoscaler must never resize the quorum tier.", True),
                               ("The quorum uses 5 nodes.", False),
                               ("The quorum uses 5 nodes. The autoscaler may resize the quorum tier.", False)):
            with self.subTest(answer=answer):
                result = replay(capture_for(case, [answer]), case)
                self.assertEqual(result["answerPass"], passes)
                if not passes:
                    self.assertIn("answer_caveats", result["failedChecks"])
        absent = replace(neutral_benchmark_case("absent"), answer_required_all=["not defined"],
                         answer_forbidden_any=["42"])
        self.assertTrue(replay(capture_for(absent, ["The value is not defined."]), absent)["answerPass"])
        self.assertFalse(replay(capture_for(absent, ["The value is 42."]), absent)["answerPass"])

    def test_required_opening_forbidden_regex_references_and_verification(self):
        case = replace(neutral_benchmark_case("native"), answer_required_any=["answer"],
                       answer_required_regex_any=[r"answer\b"], answer_opening_required_any=["first"],
                       answer_forbidden_regex_any=[r"wrong\b"], min_chunk_reference_count=1,
                       min_prepared_segment_reference_count=1, min_technical_fact_reference_count=1,
                       min_entity_reference_count=1, min_relation_reference_count=1,
                       allowed_verification_states=["partially_supported"])
        result = replay(capture_for(case, ["Wrong response."]), case)
        self.assertTrue({"answer_required", "answer_opening_required", "answer_forbidden", "chunk_references",
                         "prepared_segment_references", "technical_fact_references", "entity_references",
                         "relation_references", "verification_state"} <= set(result["failedChecks"]))

    def test_missing_search_and_timing_are_unavailable_not_zero_or_pass(self):
        case = neutral_benchmark_case("native")
        result = replay(capture_for(case, search=False), case)
        self.assertTrue(result["answerPass"])
        self.assertFalse(result["strictCasePass"])
        self.assertFalse(result["retrievalContainsRequired"])
        self.assertFalse(result["topSearchDocumentOk"])
        self.assertEqual(result["rankMetrics"], {})
        self.assertIsNone(result["answerLatencyMs"])
        self.assertEqual(result["timing"]["status"], "unavailable")
        self.assertFalse(result["conversationCost"]["complete"])
        self.assertEqual(result["conversationCost"]["observedTotalsByCurrency"], {})

    def test_compact_incomplete_error_and_identity_refusals(self):
        case = neutral_benchmark_case("native")
        mutations = {
            "compact_request": lambda c: c["turns"][0]["request"]["params"]["arguments"].update(responseProfile="compact"),
            "max_references": lambda c: c["turns"][0]["request"]["params"]["arguments"].update(maxReferences=8),
            "wrong_method": lambda c: c["turns"][0]["request"].update(method="tools/list"),
            "wrong_name": lambda c: c["turns"][0]["request"]["params"].update(name="ironrag_grounded_answer"),
            "correlation": lambda c: c["turns"][0]["response"].update(id="1"),
            "rpc_error": lambda c: c["turns"][0]["response"].update(error={"code": -32603, "message": "error"}),
            "tool_error": lambda c: c["turns"][0]["response"]["result"].update(isError=True),
            "compact_response": lambda c: structured(c).update(responseProfile="compact"),
            "missing_detail": lambda c: structured(c).pop("executionDetail"),
            "incomplete_detail": lambda c: structured(c)["executionDetail"].pop("requestTurn"),
            "wrong_question": lambda c: structured(c)["executionDetail"]["execution"].update(queryText="other"),
            "wrong_workspace": lambda c: c.update(workspaceId="00000000-0000-4000-8000-000000000099"),
            "null_workspace": lambda c: c.update(workspaceId=None),
            "wrong_library": lambda c: c.update(libraryId="00000000-0000-4000-8000-000000000099"),
            "wrong_shortcut": lambda c: structured(c).update(executionId="00000000-0000-4000-8000-000000000099"),
            "wrong_answer": lambda c: structured(c).update(answerBody="rewritten"),
            "clarification": lambda c: structured(c).update(clarification={"required": True, "question": "Which?"}),
            "incomplete": lambda c: structured(c).update(finalAnswerReady=False),
            "repair": lambda c: structured(c)["repairPolicy"].update(required=True),
            "completion": lambda c: structured(c).update(completion={"complete": False}),
            "numeric_completion": lambda c: structured(c).update(completion={"complete": 1}),
            "headers": lambda c: c.update(headers={"Authorization": "not a credential"}),
            "nested_credentials": lambda c: c["turns"][0]["response"]["result"].update(_meta={"access_token": "not a credential"}),
            "wrong_case_digest": lambda c: c.update(caseDefinitionSha256="0" * 64),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                capture = capture_for(case)
                mutate(capture)
                refresh_text(capture)
                with self.assertRaises(ValueError):
                    replay(capture, case)

    def test_multiturn_exact_history_distinct_conversations_and_preceding_failure(self):
        case = replace(neutral_benchmark_case("final"), preceding_turns=[{
            "id": "context", "question": "Initial condition?", "answerRequiredAll": ["initial"],
        }])
        capture = capture_for(case, ["initial condition", "answer"])
        result = replay(capture, case)
        self.assertTrue(result["strictCasePass"])
        self.assertNotEqual(result["turnExecutions"][0]["queryConversationId"], result["queryConversationId"])
        bad = replay(capture_for(case, ["wrong", "answer"]), case)
        self.assertFalse(bad["answerPass"])
        self.assertIn("turn:context:answer_required", bad["failedChecks"])
        for history in ([], list(reversed(capture["turns"][1]["request"]["params"]["arguments"]["conversationTurns"])),
                        [{"role": "user", "content": "Initial condition?"}],
                        [{"role": "user", "content": "Initial condition?"}, {"role": "assistant", "content": "initial"}]):
            altered = copy.deepcopy(capture)
            altered["turns"][1]["request"]["params"]["arguments"]["conversationTurns"] = history
            with self.subTest(history=history), self.assertRaisesRegex(ValueError, "history"):
                replay(altered, case)
        too_long = capture_for(case, ["x" * 1201, "answer"])
        with self.assertRaisesRegex(ValueError, "history"):
            replay(too_long, case)
        missing = copy.deepcopy(capture)
        missing["turns"].pop()
        with self.assertRaises(ValueError):
            replay(missing, case)

    def test_duplicate_executions_and_history_count_bound(self):
        case = replace(neutral_benchmark_case("final"), preceding_turns=[{"id": "context", "question": "Initial?", "answerRequiredAll": ["answer"]}])
        capture = capture_for(case)
        detail = structured(capture, 1)["executionDetail"]
        identity = structured(capture)["executionId"]
        detail["execution"]["id"] = identity
        for row in (detail["requestTurn"], detail["responseTurn"]):
            row["executionId"] = identity
        structured(capture, 1)["executionId"] = identity
        refresh_text(capture, 1)
        with self.assertRaisesRegex(ValueError, "duplicate"):
            replay(capture, case)
        capture = capture_for(case)
        capture["turns"][1]["request"]["params"]["arguments"]["conversationTurns"] *= 11
        with self.assertRaisesRegex(ValueError, "history"):
            replay(capture, case)

    def test_captured_timing_and_invalid_timing(self):
        case = neutral_benchmark_case("native")
        for value in (-1, True, "12", None):
            capture = capture_for(case)
            capture["turns"][0]["observation"]["timing"] = {"source": "captured", "answerLatencyMs": value}
            with self.subTest(value=value), self.assertRaises(ValueError):
                replay(capture, case)
        capture = capture_for(case)
        capture["turns"][0]["observation"]["timing"] = {"source": "captured", "answerLatencyMs": 12.5, "searchLatencyMs": 3}
        self.assertEqual(replay(capture, case)["answerLatencyMs"], 12.5)

    def test_invalid_cost_and_timing_declarations_fail_closed(self):
        case = neutral_benchmark_case("native")
        for cost in ({}, {"rootExecutionId": structured(capture_for(case))["executionId"], "components": []},
                     {"totalCost": "0.01"}):
            capture = capture_for(case)
            capture["turns"][0]["observation"]["cost"] = cost
            with self.subTest(cost=cost), self.assertRaises(ValueError):
                replay(capture, case)
        capture = capture_for(case)
        capture["turns"][0]["observation"]["timing"] = {"source": "estimated", "answerLatencyMs": 0}
        with self.assertRaisesRegex(ValueError, "captured"):
            replay(capture, case)
        capture = capture_for(case, search=False)
        capture["turns"][0]["observation"]["timing"] = {"source": "captured", "answerLatencyMs": 1, "searchLatencyMs": 1}
        with self.assertRaisesRegex(ValueError, "search timing"):
            replay(capture, case)

    def test_capture_bounds_and_duplicate_json_fields(self):
        case = neutral_benchmark_case("native")
        capture = capture_for(case)
        capture["turns"][0]["response"]["result"]["_meta"] = {"padding": "x" * native.MAX_CAPTURE_BYTES}
        with self.assertRaisesRegex(ValueError, "byte bound"):
            replay(capture, case)
        with self.assertRaisesRegex(ValueError, "duplicate JSON"):
            json.loads('{"id":1,"id":2}', object_pairs_hook=native.unique_json_object)

    def test_offline_cli_replays_capture_without_claiming_live_baseline(self):
        case = hardening_case("aurora_quorum_fixed_size")
        capture = capture_for(case, ["The quorum uses 5 nodes. The autoscaler must never resize the quorum tier."])
        root = Path(__file__).resolve().parent
        capture["suiteId"] = benchmark.load_suite(root / "golden_hardening_suite.json")[2]["suiteId"]
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "capture.json"
            output = Path(temporary) / "result.json"
            source.write_text(json.dumps(capture))
            completed = subprocess.run([
                sys.executable, str(root / "native_surface_evidence.py"),
                "--suite", str(root / "golden_hardening_suite.json"), "--case", case.case_id,
                "--capture", str(source), "--output", str(output),
            ], capture_output=True, text=True, timeout=20, check=False)
            self.assertEqual(completed.returncode, 1, completed.stderr)
            result = json.loads(output.read_text())
        self.assertEqual(result["surface"], native.SURFACE)
        self.assertIn("chunk_references", result["failedChecks"])
        self.assertEqual(result["rawNativeEvidence"], capture)

    def test_complete_pricing_preserves_validated_native_history(self):
        for preceding in ([], [{
            "id": "context", "question": "Initial?", "answerRequiredAll": ["answer"],
        }]):
            with self.subTest(turn_count=len(preceding) + 1):
                case = replace(neutral_benchmark_case("final"), preceding_turns=preceding)
                capture = capture_for(case)
                for index, turn in enumerate(capture["turns"]):
                    detail = structured(capture, index)["executionDetail"]
                    execution_id = detail["execution"]["id"]
                    billing = evidence()
                    billing["owningExecutionId"] = execution_id
                    turn["observation"]["cost"] = {
                        "rootExecutionId": execution_id,
                        "components": [{
                            "billing": billing, "executionDetail": copy.deepcopy(detail),
                            "llmContext": snapshot(execution_id),
                        }],
                    }
                result = replay(capture, case)
                summary = result["conversationCost"]
                self.assertTrue(result["conversationHistoryComplete"])
                self.assertTrue(summary["historyComplete"])
                self.assertTrue(summary["complete"])
                self.assertEqual(summary["scope"], "conversation_query_execution_union")
                self.assertEqual(summary["coverageCounts"]["complete"], len(preceding) + 1)
                self.assertEqual(summary["observedTotalsByCurrency"], {
                    "USD": "0.2" if preceding else "0.1",
                })
                # History validation is independent of cost availability.
                capture["turns"][0]["observation"]["cost"] = None
                unavailable = replay(capture, case)["conversationCost"]
                self.assertTrue(unavailable["historyComplete"])
                self.assertFalse(unavailable["complete"])
                self.assertEqual(unavailable["coverageCounts"]["unavailable"], 1)

    def test_typed_cost_components_currency_and_unpriced_coverage(self):
        case = replace(neutral_benchmark_case("final"), preceding_turns=[{"id": "context", "question": "Initial?", "answerRequiredAll": ["answer"]}])
        capture = capture_for(case)
        for index, currency in enumerate(("USD", "EUR")):
            detail = structured(capture, index)["executionDetail"]
            execution_id = detail["execution"]["id"]
            billing = evidence(currency=currency, unpriced=1)
            billing["owningExecutionId"] = execution_id
            capture["turns"][index]["observation"]["cost"] = {"rootExecutionId": execution_id, "components": [{
                "billing": billing, "executionDetail": copy.deepcopy(detail), "llmContext": snapshot(execution_id),
            }]}
        result = replay(capture, case)
        self.assertEqual(result["conversationCost"]["observedTotalsByCurrency"], {"USD": "0.1", "EUR": "0.1"})
        self.assertEqual(result["conversationCost"]["coverageCounts"]["partial"], 2)
        self.assertTrue(result["conversationCost"]["historyComplete"])
        self.assertFalse(result["conversationCost"]["complete"])
        distorted = copy.deepcopy(capture)
        distorted["turns"][0]["observation"]["cost"]["components"][0]["executionDetail"]["responseTurn"]["contentText"] = "another response"
        with self.assertRaisesRegex(ValueError, "cost root turn"):
            replay(distorted, case)
        capture["turns"][0]["observation"]["cost"]["rootExecutionId"] = "wrong"
        with self.assertRaises(ValueError):
            replay(capture, case)


if __name__ == "__main__":
    unittest.main()
