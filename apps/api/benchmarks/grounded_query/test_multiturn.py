from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import compare_benchmarks as comparison
import run_live_benchmark as benchmark
from test_answer_completeness import hardening_case
from test_execution_cost import evidence, matrix, snapshot
from test_rank_metrics import neutral_benchmark_case

from apps.api.benchmarks.benchmark_test_support import canonical_execution_detail

SESSION = "00000000-0000-4000-8000-000000000002"
WORKSPACE = "00000000-0000-4000-8000-000000000007"
LIBRARY = "00000000-0000-4000-8000-000000000008"


def dialog_case():
    return replace(
        neutral_benchmark_case("final"),
        preceding_turns=[
            {
                "id": "context",
                "question": "Initial condition?",
                "answerRequiredAll": ["initial"],
                "answerRequiredRegexAny": [r"condition\b"],
            }
        ],
    )


class Client:
    def __init__(
        self, *, bad_answer=False, fail_at=None, wrong_identity=False, currencies=None
    ):
        self.bad_answer = bad_answer
        self.fail_at = fail_at
        self.wrong_identity = wrong_identity
        self.currencies = currencies or ["USD", "USD"]
        self.posts = []
        self.details = {}

    def post_json(self, path, payload):
        self.posts.append((path, payload))
        if path.endswith("/search"):
            return {"documentHits": []}
        count = len(self.details) + 1
        if count == self.fail_at:
            raise benchmark.requests.ConnectionError("private URL must not be recorded")
        detail = canonical_execution_detail(
            "wrong" if self.bad_answer and count == 1 else "initial condition answer",
            question=payload["contentText"],
            execution_id=f"00000000-0000-4000-8000-{count:012d}",
        )
        if self.wrong_identity and count == 1:
            detail["execution"]["queryText"] = "another question"
        self.details[detail["execution"]["id"]] = detail
        return copy.deepcopy(detail)

    def get_json(self, path):
        if path.endswith("/llm-context"):
            return snapshot(path.split("/")[-2])
        execution_id = path.rsplit("/", 1)[-1]
        if path.startswith("/billing/"):
            result = evidence(currency=self.currencies[int(execution_id[-1]) - 1])
            result["owningExecutionId"] = execution_id
            return result
        return copy.deepcopy(self.details[execution_id])


def run(client=None, case=None, session=SESSION):
    return benchmark.run_case(
        client or Client(), WORKSPACE, LIBRARY, session, case or dialog_case(), 8
    )


class MultiturnTests(unittest.TestCase):
    def test_omitted_optional_preceding_policy_matches_canonical_default(self):
        result = run()
        preceding = result["turnExecutions"][0]
        original_hash = preceding["caseDefinitionSha256"]
        del preceding["casePolicy"]["expected_clarification"]
        self.assertEqual(
            benchmark.case_definition_sha256(
                benchmark.BenchmarkCase(**preceding["casePolicy"])
            ),
            original_hash,
        )
        self.assertTrue(comparison.recompute_case_evaluation(result)["passed"])

    def test_changed_preceding_policy_still_rejects_with_valid_hash(self):
        for field, value in (
            ("expected_clarification", False),
            ("question", "Another initial condition?"),
            ("answer_required_all", ["another expectation"]),
        ):
            with self.subTest(field=field):
                result = run()
                preceding = result["turnExecutions"][0]
                preceding["casePolicy"][field] = value
                preceding["caseDefinitionSha256"] = benchmark.case_definition_sha256(
                    benchmark.BenchmarkCase(**preceding["casePolicy"])
                )
                with self.assertRaisesRegex(ValueError, "definition|identity"):
                    comparison.recompute_case_evaluation(result)

    def test_invalid_preceding_optional_policy_is_not_normalized_to_default(self):
        result = run()
        result["turnExecutions"][0]["casePolicy"]["expected_clarification"] = "false"
        with self.assertRaisesRegex(ValueError, "invalid persisted case definition"):
            comparison.recompute_case_evaluation(result)

    def test_nested_final_cost_flows_through_runner_and_conversation_comparator(self):
        child = "00000000-0000-4000-8000-000000000011"
        final = "00000000-0000-4000-8000-000000000002"

        class NestedClient(Client):
            def get_json(self, path):
                execution_id = (
                    path.split("/")[-2]
                    if path.endswith("/llm-context")
                    else path.rsplit("/", 1)[-1]
                )
                if path.endswith("/llm-context"):
                    return snapshot(
                        execution_id, [child, child] if execution_id == final else []
                    )
                if path.startswith("/billing/") and execution_id in (final, child):
                    result = evidence(
                        "0" if execution_id == final else "0.00157591",
                        calls=0 if execution_id == final else 6,
                    )
                    result["owningExecutionId"] = execution_id
                    return result
                if execution_id == child:
                    return canonical_execution_detail(execution_id=child)
                return super().get_json(path)

        result = run(NestedClient())
        self.assertTrue(result["strictCasePass"])
        summary = benchmark.build_summary([result])
        self.assertEqual(
            summary["executionCost"]["observedTotalsByCurrency"], {"USD": "0.00157591"}
        )
        self.assertEqual(
            summary["conversationCost"]["observedTotalsByCurrency"],
            {"USD": "0.10157591"},
        )
        self.assertTrue(summary["conversationCost"]["complete"])
        for scope, expected in (
            ("final_turn_query_execution_tree", "0.00157591"),
            ("conversation_query_execution_union", "0.10157591"),
        ):
            report = comparison.build_cost_comparison(
                matrix([result]), matrix([result]), "0", scope=scope
            )
            self.assertTrue(report["passed"])
            self.assertEqual(
                report["candidate"]["observedTotalsByCurrency"], {"USD": expected}
            )

    def test_removed_preceding_policy_cannot_hide_failed_history(self):
        original = run(Client(bad_answer=True))
        self.assertFalse(comparison.recompute_case_evaluation(original)["passed"])
        for remove in (False, True):
            tampered = copy.deepcopy(original)
            if remove:
                del tampered["casePolicy"]["preceding_turns"]
            else:
                tampered["casePolicy"]["preceding_turns"] = []
            invalid = []
            comparison.validate_case_integrities(
                "candidate", {"dialog": tampered}, invalid
            )
            self.assertEqual(invalid, ["candidate.case:dialog.caseDefinitionSha256"])
            with (
                self.subTest(remove=remove),
                self.assertRaisesRegex(ValueError, "definition"),
            ):
                comparison.recompute_case_evaluation(tampered)
            # Even a replacement definition digest cannot make retained multi-turn
            # evidence a single-turn history.
            tampered["caseDefinitionSha256"] = benchmark.case_definition_sha256(
                benchmark.BenchmarkCase(**tampered["casePolicy"])
            )
            with (
                self.subTest(remove=remove),
                self.assertRaisesRegex(ValueError, "history"),
            ):
                comparison.recompute_case_evaluation(tampered)

    def test_single_turn_policy_tamper_and_historical_contract(self):
        result = run(case=neutral_benchmark_case("final"))
        result["casePolicy"]["answer_required_all"] = ["missing"]
        with self.assertRaisesRegex(ValueError, "definition"):
            comparison.recompute_case_evaluation(result)
        historical = run(case=neutral_benchmark_case("final"))
        del historical["turnExecutions"]
        for field in ("preceding_turns", "expected_facts", "mandatory_caveats"):
            del historical["casePolicy"][field]
        self.assertTrue(comparison.recompute_case_evaluation(historical)["passed"])
        historical["turnExecutions"] = []
        with self.assertRaisesRegex(ValueError, "history"):
            comparison.recompute_case_evaluation(historical)
        partial = run(Client(fail_at=2))
        self.assertFalse(partial["strictCasePass"])
        with self.assertRaises(ValueError):
            comparison.recompute_case_evaluation(partial)

    def test_order_native_session_and_separate_final_cost(self):
        client = Client()
        result = run(client)
        turns = [row for row in client.posts if row[0].endswith("/turns")]
        self.assertEqual(
            [row[1]["contentText"] for row in turns],
            ["Initial condition?", dialog_case().question],
        )
        self.assertEqual(
            {row[0] for row in turns}, {f"/query/sessions/{SESSION}/turns"}
        )
        self.assertEqual(len(client.posts), 3)  # Two turns, then final-only rank probe.
        self.assertTrue(result["strictCasePass"])
        self.assertEqual(len(result["turnExecutions"]), 2)
        self.assertEqual(
            result["executionCost"]["evidence"]["components"][0]["billing"][
                "totalCost"
            ],
            "0.1",
        )
        self.assertEqual(
            result["conversationCost"]["observedTotalsByCurrency"], {"USD": "0.2"}
        )
        self.assertEqual(result["conversationCost"]["coverageCounts"]["complete"], 2)
        self.assertGreaterEqual(
            result["conversationLatencyMs"], result["answerLatencyMs"]
        )
        self.assertTrue(comparison.recompute_case_evaluation(result)["passed"])

    def test_preceding_failure_survives_final_pass_and_recomputation(self):
        result = run(Client(bad_answer=True))
        self.assertFalse(result["strictCasePass"])
        self.assertTrue(result["turnExecutions"][-1]["strictCasePass"])
        self.assertIn("turn:context:answer_required", result["failedChecks"])
        result["strictCasePass"] = True
        result["failedChecks"] = []
        self.assertFalse(comparison.recompute_case_evaluation(result)["passed"])

    def test_later_error_preserves_history_and_never_retries(self):
        client = Client(fail_at=2)
        result = run(client)
        self.assertEqual(len(client.posts), 2)
        self.assertEqual(
            result["turnExecutions"][0]["answer"], "initial condition answer"
        )
        self.assertIsNotNone(result["turnExecutions"][0]["executionId"])
        self.assertEqual(
            result["turnExecutions"][1]["caseExecutionError"], "ConnectionError"
        )
        self.assertFalse(result["strictCasePass"])
        self.assertFalse(result["conversationCost"]["complete"])
        self.assertEqual(
            result["conversationCost"]["observedTotalsByCurrency"], {"USD": "0.1"}
        )
        self.assertNotIn("private URL", json.dumps(result))

    def test_detail_error_preserves_current_native_response(self):
        class BrokenDetail(Client):
            def get_json(self, path):
                if path.startswith("/query/executions/") and len(self.details) == 2:
                    raise ValueError("private URL")
                return super().get_json(path)

        result = run(BrokenDetail())
        failed = result["turnExecutions"][-1]
        self.assertIsNotNone(failed["executionId"])
        self.assertEqual(failed["answer"], "initial condition answer")
        self.assertIsNotNone(failed["rawEvaluationEvidence"]["turnPayload"])
        self.assertEqual(
            result["executionCost"]["evidence"]["components"][0]["billing"][
                "totalCost"
            ],
            "0.1",
        )
        self.assertFalse(result["conversationCost"]["complete"])

    def test_single_turn_transport_error_retains_completed_evidence(self):
        class BrokenDetail(Client):
            def get_json(self, path):
                if path.startswith("/query/executions/"):
                    raise benchmark.requests.ConnectionError("private URL")
                return super().get_json(path)

        result = run(BrokenDetail(), neutral_benchmark_case("final"))
        self.assertFalse(result["strictCasePass"])
        self.assertIsNotNone(result["turnExecutions"][0]["executionId"])
        self.assertEqual(
            result["executionCost"]["evidence"]["components"][0]["billing"][
                "totalCost"
            ],
            "0.1",
        )
        self.assertFalse(result["conversationHistoryComplete"])

    def test_fact_caveat_evaluator_and_source_validation_are_reused(self):
        annotated = hardening_case("aurora_latency_budgets")
        case = dialog_case()
        case.preceding_turns[0].update(
            expectedFacts=annotated.expected_facts,
            mandatoryCaveats=annotated.mandatory_caveats,
        )
        result = run(case=case)
        self.assertIn("turn:context:answer_facts", result["failedChecks"])
        self.assertIn("turn:context:answer_caveats", result["failedChecks"])
        self.assertFalse(comparison.recompute_case_evaluation(result)["passed"])
        case.preceding_turns[0]["expectedFacts"][0]["sourceSha256"] = "0" * 64
        client = Client()
        with self.assertRaisesRegex(ValueError, "source digest mismatch"):
            run(client, case)
        self.assertEqual(client.posts, [])

    def test_duplicate_execution_and_detail_identity_are_rejected(self):
        class Duplicate(Client):
            def post_json(self, path, payload):
                result = super().post_json(path, payload)
                if path.endswith("/turns") and len(self.details) == 2:
                    result = canonical_execution_detail(
                        "answer", question=payload["contentText"]
                    )
                    self.details[result["execution"]["id"]] = result
                return result

        result = run(Duplicate())
        self.assertFalse(result["strictCasePass"])
        self.assertFalse(result["conversationCost"]["complete"])
        self.assertEqual(result["conversationCost"]["coverageCounts"]["unavailable"], 2)

        class MismatchedDetail(Client):
            def get_json(self, path):
                result = super().get_json(path)
                if path.startswith("/query/executions/"):
                    result["responseTurn"]["contentText"] = "different answer"
                return result

        result = run(MismatchedDetail())
        self.assertFalse(result["strictCasePass"])
        self.assertEqual(len(result["turnExecutions"]), 1)

    def test_identity_fails_closed_and_retains_raw_response(self):
        client = Client(wrong_identity=True)
        result = run(client)
        self.assertEqual(len(client.posts), 1)
        self.assertFalse(result["strictCasePass"])
        self.assertIsNotNone(
            result["turnExecutions"][0]["rawEvaluationEvidence"]["turnPayload"]
        )
        result = run(session="00000000-0000-4000-8000-000000000099")
        self.assertFalse(result["strictCasePass"])

    def test_single_turn_and_case_isolation(self):
        client = Client()
        case = neutral_benchmark_case("final")
        first = run(client, case)
        second = run(client, case, "other-session")
        self.assertTrue(first["strictCasePass"])
        self.assertEqual(len(first["turnExecutions"]), 1)
        self.assertEqual(
            first["conversationCost"]["observedTotalsByCurrency"], {"USD": "0.1"}
        )
        self.assertEqual(second["querySessionId"], "other-session")
        self.assertNotEqual(first["querySessionId"], second["querySessionId"])

    def test_cost_currencies_unknown_and_unique_execution_sum(self):
        result = run(Client(currencies=["EUR", "USD"]))
        self.assertEqual(
            result["conversationCost"]["observedTotalsByCurrency"],
            {"EUR": "0.1", "USD": "0.1"},
        )
        result["turnExecutions"].append(copy.deepcopy(result["turnExecutions"][-1]))
        summary = benchmark.build_conversation_cost_summary([result])
        self.assertEqual(summary["observedTotalsByCurrency"], {"EUR": "0.1"})
        self.assertFalse(summary["historyComplete"])
        result["turnExecutions"][0]["executionCost"]["evidence"]["components"][0][
            "billing"
        ].update(unpricedProviderCallCount=2, costCoverage="unknown")
        summary = benchmark.build_conversation_cost_summary([result])
        self.assertFalse(summary["complete"])
        self.assertEqual(summary["coverageCounts"]["unknown"], 1)
        result = run()
        result["turnExecutions"][0]["executionCost"]["evidence"]["components"][0][
            "billing"
        ].update(unpricedProviderCallCount=1, costCoverage="partial")
        self.assertEqual(
            benchmark.build_conversation_cost_summary([result])["coverageCounts"][
                "partial"
            ],
            1,
        )
        result["turnExecutions"][0]["executionCost"] = {
            "status": "unavailable",
            "evidence": None,
            "error": "offline",
        }
        self.assertFalse(
            benchmark.build_conversation_cost_summary([result])["complete"]
        )

    def test_comparator_history_definition_and_cost_scopes(self):
        result = run()
        report = comparison.build_cost_comparison(
            matrix([result]),
            matrix([result]),
            "0",
            scope="conversation_query_execution_union",
        )
        self.assertTrue(report["passed"])
        self.assertEqual(report["baseline"]["observedTotalsByCurrency"], {"USD": "0.2"})
        summary = benchmark.build_summary([result])
        suite = {
            "suite": {"suiteId": "suite"},
            "strictBlocking": True,
            "cases": [result],
            "summary": summary,
        }
        matrix_summary = benchmark.build_matrix_summary([suite])
        self.assertEqual(
            matrix_summary["executionCost"]["observedTotalsByCurrency"], {"USD": "0.1"}
        )
        self.assertEqual(
            matrix_summary["conversationCost"], summary["conversationCost"]
        )
        expensive_context = copy.deepcopy(result)
        expensive_context["turnExecutions"][0]["executionCost"]["evidence"][
            "components"
        ][0]["billing"]["totalCost"] = "1.0"
        self.assertTrue(
            comparison.build_cost_comparison(
                matrix([result]), matrix([expensive_context]), "0"
            )["passed"]
        )
        self.assertFalse(
            comparison.build_cost_comparison(
                matrix([result]),
                matrix([expensive_context]),
                "0",
                scope="conversation_query_execution_union",
            )["passed"]
        )
        changed = copy.deepcopy(result)
        changed["casePolicy"]["preceding_turns"][0]["answerRequiredAll"] = ["changed"]
        with self.assertRaisesRegex(ValueError, "definition differs"):
            comparison.recompute_case_evaluation(changed)
        self.assertFalse(
            comparison.build_cost_comparison(matrix([result]), matrix([changed]), "0")[
                "passed"
            ]
        )
        changed = copy.deepcopy(result)
        del changed["turnExecutions"]
        with self.assertRaisesRegex(ValueError, "history"):
            comparison.recompute_case_evaluation(changed)
        self.assertFalse(
            comparison.build_cost_comparison(
                matrix([result]),
                matrix([changed]),
                "0",
                scope="conversation_query_execution_union",
            )["passed"]
        )

    def test_input_validation_and_digest(self):
        case = dialog_case()
        base = replace(case, preceding_turns=[])
        self.assertNotEqual(
            benchmark.case_definition_sha256(case),
            benchmark.case_definition_sha256(base),
        )
        malformed = [
            None,
            [{}],
            [case.preceding_turns[0]] * 2,
            [{"id": "a", "question": " "}],
            [{"id": "a", "question": "q", "answerRequiredRegexAny": ["["]}],
            [{"id": "a", "question": "q", "answerRequiredRegexAny": [".*"]}],
            [{"id": "a", "question": "q", "answerRequiredAll": "bad"}],
            [{"id": "a", "question": "q", "expectedFacts": [{}]}],
        ]
        for turns in malformed:
            with self.subTest(turns=turns), self.assertRaises((ValueError, TypeError)):
                benchmark.validate_preceding_turns(turns, root=Path(__file__).parent)
        root = Path(__file__).parent
        source = root / "golden_technical_contract_suite.json"
        documents, cases, payload = benchmark.load_suite(source)
        raw = {document: document.read_bytes() for document in documents}
        before = benchmark.build_suite_integrity(payload, cases, documents, raw)
        changed = [replace(cases[0], preceding_turns=case.preceding_turns), *cases[1:]]
        after = benchmark.build_suite_integrity(payload, changed, documents, raw)
        self.assertNotEqual(
            before["suiteDefinitionSha256"], after["suiteDefinitionSha256"]
        )
        self.assertEqual(before["corpusSha256"], after["corpusSha256"])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "suite.json"
            payload["cases"][0]["question"] = " "
            path.write_text(json.dumps(payload))
            with self.assertRaisesRegex(ValueError, "empty benchmark question"):
                benchmark.load_suite(path)


if __name__ == "__main__":
    unittest.main()
