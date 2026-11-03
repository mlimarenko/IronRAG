from __future__ import annotations

import copy
import hashlib
import json
import sys
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import compare_benchmarks as comparison
import run_live_benchmark as benchmark
from test_latency_regression import baseline_matrix, benchmark_case, candidate_matrix
from test_rank_metrics import neutral_benchmark_case

from apps.api.benchmarks.benchmark_test_support import canonical_execution_detail

ROOT = Path(__file__).resolve().parent


def hardening_case(case_id):
    return next(
        case
        for case in benchmark.load_suite(ROOT / "golden_hardening_suite.json")[1]
        if case.case_id == case_id
    )


def run_answer(case, answer, *, clarification_required=False):
    def detail():
        payload = canonical_execution_detail(
            answer, question=case.question,
            answer_disposition="clarification" if clarification_required else "factual_ready",
        )
        if clarification_required:
            payload["clarification"].update(required=True, question=answer)
        return payload

    class Client:
        def post_json(self, path, payload):
            if path.endswith("/search"):
                return {"documentHits": []}
            return detail()

        def get_json(self, path, **kwargs):
            if path.startswith("/query/executions/"):
                return detail()
            raise OSError("offline billing observation unavailable")

    return benchmark.run_case(
        client=Client(),
        workspace_id="00000000-0000-4000-8000-000000000007",
        library_id="00000000-0000-4000-8000-000000000008",
        session_id="session-1",
        case=case,
        query_top_k=8,
    )


class ClarificationExpectationTests(unittest.TestCase):
    def load_case(self, **policy):
        with tempfile.TemporaryDirectory() as directory:
            suite = Path(directory) / "suite.json"
            suite.write_text(json.dumps({
                "documents": [],
                "cases": [{"id": "clarification", "question": "Which route should I use?", **policy}],
            }), encoding="utf-8")
            return benchmark.load_suite(suite)[1][0]

    def test_route_literal_cannot_replace_required_clarification(self):
        case = self.load_case(
            answerRequiredAny=["which", "specify", "clarify", "route", "context"],
            expectedClarification=True,
        )
        detail = canonical_execution_detail(
            "Use the Alpha Relay route.", question=case.question
        )
        result = benchmark.evaluate_execution_detail(
            case, detail, search_payload={"documentHits": []}
        )
        self.assertTrue(result["answerHasRequired"])
        self.assertFalse(result["answerPass"])
        self.assertFalse(result["strictCasePass"])
        self.assertEqual(result["failedChecks"], ["answer_clarification"])

    def test_true_false_and_unspecified_expectations(self):
        for expected in (True, False, None):
            case = self.load_case(expectedClarification=expected)
            self.assertIs(case.expected_clarification, expected)
            for required in (True, False, None):
                with self.subTest(expected=expected, required=required):
                    detail = canonical_execution_detail(
                        "Which route do you mean?" if required else "Use this route.",
                        question=case.question,
                        answer_disposition="clarification" if required else "factual_ready",
                    )
                    if required is None:
                        detail.pop("clarification")
                    else:
                        detail["clarification"]["required"] = required
                        if required:
                            detail["clarification"]["question"] = "Which route do you mean?"
                    result = benchmark.evaluate_execution_detail(
                        case, detail, search_payload={"documentHits": []}
                    )
                    passed = expected is None or required is expected
                    self.assertEqual(result["answerPass"], passed)
                    self.assertEqual(result["strictCasePass"], passed)
                    self.assertEqual(
                        result["failedChecks"], [] if passed else ["answer_clarification"]
                    )

    def test_loader_rejects_non_boolean_non_null_expectations(self):
        for invalid in (0, 1, "true", "false", [], {}):
            with self.subTest(invalid=invalid), self.assertRaisesRegex(
                ValueError, "expectedClarification must be boolean or null"
            ):
                self.load_case(expectedClarification=invalid)

    def test_policy_hash_tracks_declared_expectation_only(self):
        unspecified = self.load_case()
        null = self.load_case(expectedClarification=None)
        self.assertIsNone(unspecified.expected_clarification)
        self.assertEqual(
            benchmark.case_definition_sha256(unspecified),
            benchmark.case_definition_sha256(null),
        )
        hashes = {benchmark.case_definition_sha256(case) for case in (
            unspecified,
            self.load_case(expectedClarification=True),
            self.load_case(expectedClarification=False),
        )}
        self.assertEqual(len(hashes), 3)

    def test_final_expectation_does_not_leak_into_preceding_turn(self):
        final = self.load_case(expectedClarification=True)
        preceding = benchmark.preceding_turn_case(final, {
            "id": "preceding", "question": "Explain the options.", "answerRequiredAny": ["route"],
        })
        self.assertIsNone(preceding.expected_clarification)

    def test_compound_case_declares_clarification(self):
        case = next(
            case
            for case in benchmark.load_suite(ROOT / "golden_compound_search_suite.json")[1]
            if case.case_id == "ambiguous_route_clarification"
        )
        detail = canonical_execution_detail("Use the Alpha Relay route.", question=case.question)
        result = benchmark.evaluate_execution_detail(
            case, detail, search_payload={"documentHits": []}
        )
        self.assertTrue(result["answerHasRequired"])
        self.assertFalse(result["answerPass"])
        self.assertIn("answer_clarification", result["failedChecks"])

    def test_comparison_recomputes_clarification_from_raw_detail(self):
        for expected in (True, False, None):
            case = self.load_case(
                expectedClarification=expected, answerRequiredAny=["route"]
            )
            for required in (True, False):
                with self.subTest(expected=expected, required=required):
                    result = run_answer(
                        case,
                        "Which route do you mean?" if required else "Use the Alpha Relay route.",
                        clarification_required=required,
                    )
                    passed = expected is None or required is expected
                    result["answerPass"] = not passed
                    result["strictCasePass"] = not passed
                    result["failedChecks"] = [] if not passed else ["answer_clarification"]
                    recomputed = comparison.recompute_case_evaluation(result)
                    self.assertEqual(recomputed["passed"], passed)
                    self.assertEqual(
                        recomputed["failedChecks"], [] if passed else ["answer_clarification"]
                    )

    def test_comparison_rejects_invalid_persisted_clarification_policy(self):
        original = run_answer(self.load_case(), "Use the Alpha Relay route.")
        for invalid in (0, 1, "true", "false", [], {}):
            with self.subTest(invalid=invalid):
                result = copy.deepcopy(original)
                result["casePolicy"]["expected_clarification"] = invalid
                with self.assertRaisesRegex(ValueError, "invalid persisted case definition"):
                    comparison.recompute_case_evaluation(result)


class AnswerCompletenessTests(unittest.TestCase):
    def test_full_partial_and_empty_answers_count_each_fact(self):
        case = hardening_case("aurora_latency_budgets")
        for answer, matched, missing, caveats in (
            (
                "Synchronous latency is 250 **ms** at the edge; quorum internal budget is 40 ms.",
                ["synchronous-budget-250ms", "quorum-budget-40ms"],
                [],
                1,
            ),
            (
                "Synchronous latency is 250 ms at the edge.",
                ["synchronous-budget-250ms"],
                ["quorum-budget-40ms"],
                1,
            ),
            ("", [], ["synchronous-budget-250ms", "quorum-budget-40ms"], 0),
        ):
            with self.subTest(answer=answer):
                result = benchmark.evaluate_answer_items(
                    answer, case.expected_facts, case.mandatory_caveats
                )
                self.assertEqual(result["facts"]["matchedIds"], matched)
                self.assertEqual(result["facts"]["missingIds"], missing)
                self.assertEqual(result["facts"]["fraction"], len(matched) / 2)
                self.assertEqual(result["caveats"]["matchedCount"], caveats)

    def test_negation_fact_does_not_match_affirmative_resize(self):
        case = hardening_case("aurora_quorum_fixed_size")
        negative_fact = case.mandatory_caveats[0]
        for answer, expected in (
            ("The autoscaler must never resize the quorum tier.", 1),
            ("The quorum must never be resized by the autoscaler.", 1),
            ("The autoscaler may resize the quorum tier.", 0),
            ("The quorum uses 5 nodes with the autoscaler.", 0),
        ):
            with self.subTest(answer=answer):
                result = benchmark.evaluate_answer_items(answer, [negative_fact], [])
                self.assertEqual(result["facts"]["matchedCount"], expected)

    def test_mandatory_caveat_and_existing_failure_are_never_waived(self):
        annotated = hardening_case("aurora_quorum_fixed_size")
        case = replace(
            neutral_benchmark_case("quorum"),
            expected_facts=annotated.expected_facts,
            mandatory_caveats=annotated.mandatory_caveats,
        )
        full = "The quorum is fixed at 5 nodes and must never be resized by the autoscaler."
        for answer, passed, failures in (
            (full, True, []),
            ("The quorum is fixed at 5 nodes.", False, ["answer_caveats"]),
            ("", False, ["answer_facts", "answer_caveats"]),
        ):
            with self.subTest(answer=answer):
                result = run_answer(case, answer)
                recomputed = comparison.recompute_case_evaluation(result)
                self.assertEqual(result["strictCasePass"], passed)
                self.assertEqual(result["answerPass"], passed)
                self.assertEqual(result["failedChecks"], failures)
                self.assertEqual(recomputed["passed"], passed)
                self.assertEqual(recomputed["failedChecks"], failures)
                self.assertEqual(
                    recomputed["answerCompleteness"], result["answerCompleteness"]
                )
                summary = benchmark.build_summary([result])["answerCompleteness"]
                self.assertEqual(summary["facts"]["fraction"], 1 if answer else 0)
        result = run_answer(
            replace(case, answer_required_all=["absent-old-requirement"]), full
        )
        self.assertFalse(result["strictCasePass"])
        self.assertEqual(result["failedChecks"], ["answer_required"])
        self.assertFalse(comparison.case_passed(result))

    def test_replication_allowlist_condition_is_not_just_a_cidr_mention(self):
        case = hardening_case("gateway_replication_cidr")
        for answer, expected in (
            ("relay-2.internal:7601 accepts connections only from 10.32.0.0/19.", 1),
            (
                "relay-2.internal:7601 accepts connections from 10.32.0.0/19 and elsewhere.",
                0,
            ),
        ):
            result = benchmark.evaluate_answer_items(
                answer, case.expected_facts, case.mandatory_caveats
            )
            self.assertEqual(result["facts"]["matchedCount"], 1)
            self.assertEqual(result["caveats"]["matchedCount"], expected)

    def test_unannotated_historical_cases_are_not_complete_or_newly_gated(self):
        old = benchmark_case("historical", 100)
        result = comparison.recompute_case_evaluation(old)
        self.assertTrue(result["passed"])
        self.assertEqual(result["answerCompleteness"]["status"], "not_annotated")
        self.assertIsNone(result["answerCompleteness"]["facts"]["fraction"])
        report = comparison.build_comparison_report(
            baseline_matrix([old]), candidate_matrix([old])
        )
        self.assertTrue(report["passed"])
        completeness = report["answerCompleteness"]
        self.assertFalse(completeness["comparable"])
        self.assertEqual(completeness["baseline"]["notAnnotatedCases"], 1)
        self.assertEqual(completeness["baseline"]["annotationCoverage"], 0)
        self.assertIsNone(completeness["baseline"]["facts"]["fraction"])

    def test_comparator_recomputes_item_regressions_and_ignores_stamped_success(self):
        annotated = hardening_case("aurora_quorum_fixed_size")
        case = replace(
            neutral_benchmark_case("quorum"),
            expected_facts=annotated.expected_facts,
            mandatory_caveats=annotated.mandatory_caveats,
        )
        before = run_answer(
            case,
            "The quorum is fixed at 5 nodes and must never be resized by the autoscaler.",
        )
        after = run_answer(case, "The quorum is fixed at 5 nodes.")
        after["answerCompleteness"] = before["answerCompleteness"]
        after["strictCasePass"] = True
        baseline, candidate = baseline_matrix([before]), candidate_matrix([after])
        result = comparison.build_completeness_comparison(baseline, candidate)
        pair = next(iter(result["cases"].values()))
        self.assertTrue(pair["comparable"])
        self.assertEqual(pair["regressedIds"]["caveats"], ["quorum-no-autoscale"])
        self.assertEqual(
            pair["candidate"]["caveats"]["missingIds"], ["quorum-no-autoscale"]
        )
        self.assertFalse(
            comparison.build_correctness_gate(baseline, candidate)["passed"]
        )
        candidate["suites"][0]["cases"][0]["casePolicy"].pop("mandatory_caveats")
        changed = candidate["suites"][0]["cases"][0]
        changed["caseDefinitionSha256"] = benchmark.case_definition_sha256(
            benchmark.BenchmarkCase(**changed["casePolicy"])
        )
        changed["turnExecutions"][0]["casePolicy"] = copy.deepcopy(changed["casePolicy"])
        changed["turnExecutions"][0]["caseDefinitionSha256"] = changed["caseDefinitionSha256"]
        self.assertFalse(
            comparison.build_completeness_comparison(baseline, candidate)["comparable"]
        )

    def test_unavailable_execution_is_not_complete(self):
        result = benchmark.failed_case_result(
            neutral_benchmark_case("broken"), OSError("offline")
        )
        summary = benchmark.build_summary([result])["answerCompleteness"]
        self.assertEqual(summary["unavailableCases"], 1)
        self.assertIsNone(summary["facts"]["fraction"])

    def test_mixed_coverage_and_item_weighted_matrix_summary(self):
        annotated = hardening_case("aurora_latency_budgets")
        case = replace(
            neutral_benchmark_case("budgets"),
            expected_facts=annotated.expected_facts,
            mandatory_caveats=annotated.mandatory_caveats,
        )
        partial = run_answer(case, "Synchronous latency is 250 ms at the edge.")
        old = run_answer(neutral_benchmark_case("old"), "A grounded answer.")
        failed = benchmark.failed_case_result(case, OSError("offline"))
        cases = [partial, old, failed]
        summary = benchmark.build_summary(cases)
        matrix = benchmark.build_matrix_summary(
            [
                {
                    "suite": {"suiteId": "test"},
                    "strictBlocking": True,
                    "summary": summary,
                    "cases": cases,
                }
            ]
        )
        completeness = matrix["answerCompleteness"]
        self.assertEqual(completeness, summary["answerCompleteness"])
        self.assertEqual(completeness["annotatedCases"], 1)
        self.assertEqual(completeness["notAnnotatedCases"], 1)
        self.assertEqual(completeness["unavailableCases"], 1)
        self.assertEqual(completeness["annotationCoverage"], 1 / 3)
        self.assertEqual(completeness["facts"]["fraction"], 0.5)
        self.assertFalse(matrix["strictCasePassCount"] == matrix["totalCases"])

    def test_comparator_rejects_tampered_provenance(self):
        annotated = hardening_case("aurora_quorum_fixed_size")
        case = replace(
            neutral_benchmark_case("quorum"), expected_facts=annotated.expected_facts
        )
        result = run_answer(case, "The quorum is fixed at 5 nodes.")
        result["casePolicy"]["expected_facts"][0]["sourceSha256"] = "0" * 64
        result["caseDefinitionSha256"] = benchmark.case_definition_sha256(
            benchmark.BenchmarkCase(**result["casePolicy"])
        )
        with self.assertRaisesRegex(ValueError, "source digest mismatch"):
            comparison.recompute_case_evaluation(result)


class CohortAnnotationTests(unittest.TestCase):
    def test_inline_normalization_and_source_backed_policy_repairs(self):
        examples = {
            "train_canary_owner_and_predecessor": (
                "The **Traffic Stewards** own the canary-slice stage. "
                "Immediately before it, **sealed-rehearsal** must succeed."
            ),
            "orbita_ru_confirmation_deadline": (
                "Оркестратор должен подтвердить остановку в течение **90 секунд**. "
                "Если подтверждение не пришло за это время, перезапуск запрещён."
            ),
            "ledger_export_raise_plans": (
                "Only **LP-070 (Biome)** and **LP-080 (Sphere)** may request "
                "a temporary export size raise, **once per billing month**."
            ),
            "orbita_crosslang_forbidden_stage": (
                "Restarting the Orbita service is forbidden during **canary-slice** "
                "unless the Traffic Stewards sign an exception."
            ),
        }
        mutations = {
            "train_canary_owner_and_predecessor": (
                ("Traffic Stewards", "Replay Guild"),
                ("sealed-rehearsal", "compile-and-gate"),
                ("must succeed", "must fail"),
            ),
            "orbita_ru_confirmation_deadline": (
                ("90", "120"), ("запрещён", "разрешён"),
                ("не пришло", "пришло"),
            ),
            "ledger_export_raise_plans": (
                ("LP-070", "LP-060"), ("Only", "Also"),
                ("once", "twice"), ("once", "more than once"),
                ("billing month", "week"),
            ),
            "orbita_crosslang_forbidden_stage": (
                ("Traffic Stewards", "Replay Guild"), ("forbidden", "allowed"),
            ),
        }
        for case_id, answer in examples.items():
            case = self.cases[case_id]
            with self.subTest(case_id=case_id):
                result = self.evaluate(case_id, answer)
                self.assertTrue(result["answerPass"], result["failedChecks"])
                self.assertFalse(result["strictCasePass"])
                self.assertIn("auxiliary_search_unavailable", result["failedChecks"])
                policy = result["answerCompleteness"]["policy"]
                self.assertEqual(policy["version"], benchmark.ANSWER_ITEM_POLICY_VERSION)
                self.assertEqual(len(policy["sha256"]), 64)
                for before, after in mutations[case_id]:
                    changed = answer.replace(before, after)
                    self.assertNotEqual(changed, answer)
                    completeness = benchmark.evaluate_answer_items(
                        changed, case.expected_facts, case.mandatory_caveats
                    )
                    self.assertTrue(benchmark.answer_item_failures(completeness), changed)
                without_caveat = answer.split(". ")[0] + "."
                if case.mandatory_caveats and ". " in answer:
                    self.assertFalse(self.evaluate(case_id, without_caveat)["answerPass"])

    def test_inline_normalization_preserves_identifiers_and_line_boundaries(self):
        self.assertEqual(
            benchmark.normalize_answer_item_text(
                "**Alpha** uses `system_info`.\n[Read policy](https://example.org) &amp; wait."
            ),
            "alpha uses system_info.\nread policy & wait.",
        )

    @classmethod
    def setUpClass(cls):
        cls.cases = {
            case.case_id: case
            for suite in ("hardening", "technical_contract")
            for case in benchmark.load_suite(ROOT / f"golden_{suite}_suite.json")[1]
        }

    def evaluate(self, case_id, answer):
        case = self.cases[case_id]
        return benchmark.evaluate_execution_detail(
            case,
            canonical_execution_detail(answer, question=case.question),
            search_payload=None,
        )

    def test_complete_source_backed_answers_pass_answer_policy(self):
        answers = {
            "train_canary_owner_and_predecessor": (
                "Traffic Stewards own canary-slice. "
                "sealed-rehearsal must succeed immediately before canary-slice."
            ),
            "beacon_config_listen_port": (
                "The listener uses port 8443."
            ),
            "orbita_ru_confirmation_deadline": (
                "Подтверждение остановки должно прийти в течение 90 секунд. "
                "Если подтверждение остановки не пришло за 90 секунд, перезапуск запрещён."
            ),
            "orbita_crosslang_forbidden_stage": (
                "During canary-slice, restarting Orbita is forbidden without "
                "an exception signed by Traffic Stewards."
            ),
            "relay_latency_sensitive_choice": (
                "Latency-sensitive rings should use RLY-EXT-517 with a 16 frames window."
            ),
            "ledger_export_raise_plans": (
                "Only LP-070 and LP-080 may request a temporary export size raise, "
                "and only once per billing month."
            ),
            "lattice_scheduler_no_split": (
                "LatticeScheduler assigns spool segments to replay workers and "
                "never splits a coalesced window across workers."
            ),
            "inventory_wsdl": (
                "WSDL: http://demo.local:8080/inventory-api/ws/inventory.wsdl"
            ),
            "graphql_absent": "Нет, GraphQL API не публикуется в этой библиотеке.",
        }
        for case_id, answer in answers.items():
            with self.subTest(case_id=case_id):
                case = self.cases[case_id]
                items = benchmark.evaluate_answer_items(
                    answer, case.expected_facts, case.mandatory_caveats
                )
                self.assertEqual(items["facts"]["missingIds"], [])
                self.assertEqual(items["caveats"]["missingIds"], [])
                result = self.evaluate(case_id, answer)
                self.assertTrue(result["answerPass"], result["failedChecks"])
                self.assertEqual(result["answerCompleteness"], items)
                # Offline answer proof does not assert retrieval or citation quality.
                self.assertFalse(result["strictCasePass"])
                self.assertIn("auxiliary_search_unavailable", result["failedChecks"])

    def test_concise_markdown_answers_do_not_repeat_question_context(self):
        for case_id, answer in (
            ("beacon_config_listen_port", "The listener listens on port 8443."),
            ("beacon_config_listen_port", "The listener listens on port **8443**."),
            (
                "relay_latency_sensitive_choice",
                "Use the **Extended Interconnect** profile (**RLY-EXT-517**); "
                "its frame window is **16 frames**.",
            ),
            (
                "relay_latency_sensitive_choice",
                "Use **RLY-EXT-517**; its frame window is **16** frames.",
            ),
            (
                "inventory_wsdl",
                "SOAP: `http://demo.local:8080/inventory-api/ws/inventory.wsdl`",
            ),
        ):
            with self.subTest(case_id=case_id, answer=answer):
                result = self.evaluate(case_id, answer)
                self.assertTrue(result["answerHasRequired"])
                self.assertTrue(result["answerPass"], result["answerCompleteness"])

    def test_checkout_method_path_relation_accepts_inline_markdown(self):
        final = self.cases["inventory_wsdl"]
        preceding = benchmark.preceding_turn_case(final, final.preceding_turns[0])
        for answer, passed in (
            ("Use the GET endpoint `/system/info`.", True),
            ("Use the **GET** endpoint **`/system/info`**.", True),
            ("GET /other retrieves data; POST /system/info returns system information.", False),
            ("Use the POST endpoint `/system/info`; GET is for `/other`.", False),
            ("Use the GET endpoint `/system/info/other`.", False),
            ("- Method: `POST`\n- Path: `/system/info`\nGET is for `/other`.", False),
            ("- Method: `GET`\n- Path: `/other`\nPOST is for `/system/info`.", False),
            ("- Method: `GET`\n- Path: `/system/info/other`", False),
        ):
            with self.subTest(answer=answer):
                result = benchmark.evaluate_execution_detail(
                    preceding,
                    canonical_execution_detail(answer, question=preceding.question),
                    search_payload=None,
                )
                self.assertTrue(result["answerHasRequired"])
                self.assertEqual(result["answerPass"], passed, result["answerCompleteness"])

    def test_reviewed_concise_port_and_fixture_method_path_answers_pass(self):
        final = self.cases["inventory_wsdl"]
        preceding = benchmark.preceding_turn_case(final, final.preceding_turns[0])
        for case, answer in (
            (self.cases["beacon_config_listen_port"], "Port **8443**."),
            (preceding, "- Method: `GET`\n- Path: `/system/info`"),
        ):
            with self.subTest(case_id=case.case_id, answer=answer):
                result = benchmark.evaluate_execution_detail(
                    case,
                    canonical_execution_detail(answer, question=case.question),
                    search_payload=None,
                )
                self.assertTrue(result["answerHasRequired"])
                self.assertTrue(result["answerPass"], result["answerCompleteness"])

    def test_omitted_qualifiers_and_opposite_conditions_fail_caveats(self):
        examples = {
            "orbita_ru_confirmation_deadline": (
                "Подтверждение остановки должно прийти в течение 90 секунд.",
                "Подтверждение остановки должно прийти в течение 90 секунд. "
                "Если подтверждение остановки не пришло за 90 секунд, перезапуск разрешён.",
                "Подтверждение остановки должно прийти в течение 90 секунд. "
                "Если подтверждение остановки пришло за 90 секунд, перезапуск запрещён.",
            ),
            "orbita_crosslang_forbidden_stage": (
                "The stage is canary-slice, owned by Traffic Stewards.",
                "During canary-slice, restarting Orbita is allowed without "
                "an exception signed by Traffic Stewards.",
                "During canary-slice, restarting Orbita is forbidden with "
                "an exception signed by Traffic Stewards.",
            ),
            "ledger_export_raise_plans": (
                "Only LP-070 and LP-080 may request a temporary export size raise "
                "more than once per billing month.",
            ),
            "lattice_scheduler_no_split": (
                "LatticeScheduler assigns coalesced windows to replay workers.",
                "LatticeScheduler splits a coalesced window across workers.",
                "LatticeScheduler never splits an individual frame across workers "
                "but splits a coalesced window across workers.",
            ),
        }
        for case_id, answers in examples.items():
            for answer in answers:
                with self.subTest(case_id=case_id, answer=answer):
                    result = self.evaluate(case_id, answer)
                    self.assertTrue(result["answerHasRequired"])
                    self.assertEqual(
                        result["answerCompleteness"]["facts"]["missingIds"], []
                    )
                    self.assertFalse(result["answerPass"])
                    self.assertIn("answer_caveats", result["failedChecks"])

    def test_omitted_frequency_also_keeps_existing_legacy_failure(self):
        result = self.evaluate(
            "ledger_export_raise_plans",
            "Only LP-070 and LP-080 may request a temporary export size raise.",
        )
        self.assertFalse(result["answerPass"])
        self.assertIn("answer_required", result["failedChecks"])
        self.assertIn("answer_caveats", result["failedChecks"])
        self.assertEqual(result["answerCompleteness"]["facts"]["missingIds"], [])

    def test_missing_or_wrong_source_backed_facts_fail(self):
        for case_id, answer, missing in (
            (
                "beacon_config_listen_port",
                "The listener listens on port 9443.",
                "beacon-active-listener-8443",
            ),
            (
                "train_canary_owner_and_predecessor",
                "Traffic Stewards own canary-slice; sealed-rehearsal is another stage.",
                "canary-predecessor-sealed-rehearsal",
            ),
            (
                "train_canary_owner_and_predecessor",
                "Replay Guild owns canary-slice; Traffic Stewards own broad-rollout. "
                "sealed-rehearsal must succeed immediately before canary-slice.",
                "canary-owner-traffic-stewards",
            ),
            (
                "relay_latency_sensitive_choice",
                "Latency-sensitive rings use RLY-EXT-517 with 64 frames.",
                "relay-extended-window-16-frames",
            ),
        ):
            with self.subTest(case_id=case_id, answer=answer):
                result = self.evaluate(case_id, answer)
                self.assertFalse(result["answerPass"])
                self.assertIn(
                    missing, result["answerCompleteness"]["facts"]["missingIds"]
                )

    def test_documented_absence_is_not_unknown_or_positive_capability(self):
        for answer in (
            "Нет данных: неизвестно, есть ли GraphQL API в библиотеке.",
            "GraphQL API не описан; наличие неизвестно.",
            "GraphQL API is published by the checkout server.",
        ):
            with self.subTest(answer=answer):
                result = self.evaluate("graphql_absent", answer)
                self.assertFalse(result["answerPass"])
                self.assertIn("answer_facts", result["failedChecks"])

    def test_preceding_checkout_turn_keeps_its_own_endpoint_and_method(self):
        final = self.cases["inventory_wsdl"]
        preceding = benchmark.preceding_turn_case(final, final.preceding_turns[0])
        for answer, passed in (
            ("GET /system/info возвращает текущую информацию checkout server.", True),
            ("/system/info returns system information.", False),
            ("POST /system/info returns system information.", False),
            ("WSDL: http://demo.local:8080/inventory-api/ws/inventory.wsdl", False),
        ):
            with self.subTest(answer=answer):
                result = benchmark.evaluate_execution_detail(
                    preceding,
                    canonical_execution_detail(answer, question=preceding.question),
                    search_payload=None,
                )
                self.assertEqual(result["answerPass"], passed)
                self.assertEqual(
                    result["answerCompleteness"]["facts"]["fraction"], int(passed)
                )
        result = self.evaluate("inventory_wsdl", "GET /system/info")
        self.assertFalse(result["answerPass"])

    def test_selected_turn_provenance_is_validated_by_native_loader(self):
        payload = json.loads((ROOT / "golden_technical_contract_suite.json").read_text())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for document in payload["documents"]:
                destination = root / document
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes((ROOT / document).read_bytes())
            suite = root / "suite.json"
            selected = next(c for c in payload["cases"] if c["id"] == "inventory_wsdl")
            item = selected["precedingTurns"][0]["expectedFacts"][0]
            for key, value, error in (
                ("sourceSha256", "0" * 64, "source digest mismatch"),
                ("sourceExcerpt", "POST /system/info", "not verbatim fixture text"),
                ("fixture", "corpus/docs/relay_twin_extended.md", "outside suite documents"),
            ):
                original = item[key]
                item[key] = value
                suite.write_text(json.dumps(payload), encoding="utf-8")
                with self.subTest(key=key), self.assertRaisesRegex(ValueError, error):
                    benchmark.load_suite(suite)
                item[key] = original

    def test_binary_extract_remains_outside_utf8_annotation_scope(self):
        case = self.cases["aurora_pdf_spool_retention"]
        result = benchmark.evaluate_answer_items(
            "", case.expected_facts, case.mandatory_caveats
        )
        self.assertEqual(result["status"], "not_annotated")
        self.assertIsNone(result["facts"]["fraction"])


class AnswerItemBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "corpus").mkdir()
        self.source = self.root / "corpus" / "limits.md"
        self.source.write_text(
            "The worker must not exceed 5 retries.\n", encoding="utf-8"
        )
        self.item = {
            "id": "retry-limit",
            "fixture": "corpus/limits.md",
            "sourceExcerpt": "must not exceed 5 retries",
            "sourceSha256": hashlib.sha256(self.source.read_bytes()).hexdigest(),
            "requiredAll": ["must not exceed 5 retries"],
            "requiredAny": [],
            "requiredRegexAny": [],
        }
        self.payload = {
            "suiteId": "limits",
            "documents": ["corpus/limits.md"],
            "cases": [
                {
                    "id": "limit",
                    "question": "What is the retry limit?",
                    "expectedFacts": [self.item],
                }
            ],
        }

    def load(self):
        suite = self.root / "suite.json"
        suite.write_text(json.dumps(self.payload), encoding="utf-8")
        return benchmark.load_suite(suite)

    def test_valid_load_retains_annotations_and_existing_case_properties(self):
        self.payload["cases"][0]["answerForbiddenAny"] = ["unlimited"]
        documents, cases, _ = self.load()
        self.assertEqual(documents, [self.source])
        self.assertEqual(cases[0].expected_facts, [self.item])
        self.assertEqual(cases[0].answer_forbidden_any, ["unlimited"])

    def test_invalid_provenance_and_matchers_fail_closed(self):
        for key, value in (
            ("id", ""),
            ("fixture", "corpus/missing.md"),
            ("fixture", "/etc/passwd"),
            ("fixture", "corpus/../limits.md"),
            ("fixture", "outside.md"),
            ("sourceExcerpt", "invented requirement"),
            ("sourceExcerpt", ""),
            ("sourceSha256", "0" * 64),
            ("requiredAll", []),
            ("requiredAll", [""]),
            ("requiredAll", ["   "]),
            ("requiredAll", ["—"]),
            ("requiredAll", "not an array"),
            ("requiredAll", [5]),
            ("requiredRegexAny", ["("]),
            ("requiredRegexAny", [".*"]),
        ):
            original = copy.deepcopy(self.item)
            self.item[key] = value
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                self.load()
            self.item.clear()
            self.item.update(original)
        del self.item["sourceExcerpt"]
        with self.assertRaises(ValueError):
            self.load()

    def test_duplicate_case_and_cross_family_item_ids_are_rejected(self):
        self.payload["cases"].append(copy.deepcopy(self.payload["cases"][0]))
        with self.assertRaisesRegex(ValueError, "duplicate benchmark case"):
            self.load()
        self.payload["cases"].pop()
        self.payload["cases"][0]["mandatoryCaveats"] = [copy.deepcopy(self.item)]
        with self.assertRaisesRegex(ValueError, "duplicate answer item"):
            self.load()
        del self.payload["cases"][0]["mandatoryCaveats"]
        self.payload["cases"][0]["expectedFacts"].append(copy.deepcopy(self.item))
        with self.assertRaisesRegex(ValueError, "duplicate answer item"):
            self.load()

    def test_non_array_annotations_are_rejected(self):
        for value in (None, {}, "retry-limit"):
            self.payload["cases"][0]["expectedFacts"] = value
            with (
                self.subTest(value=value),
                self.assertRaisesRegex(TypeError, "must be arrays"),
            ):
                self.load()

    def test_fixture_scope_symlink_escape_and_binary_are_rejected(self):
        self.payload["documents"] = []
        with self.assertRaisesRegex(ValueError, "outside suite documents"):
            self.load()
        outside = self.root / "outside.md"
        outside.write_bytes(self.source.read_bytes())
        self.source.unlink()
        self.source.symlink_to(outside)
        self.payload["documents"] = ["corpus/limits.md"]
        with self.assertRaisesRegex(ValueError, "escapes corpus"):
            self.load()
        self.source.unlink()
        self.source.write_bytes(b"\xff\x00")
        with self.assertRaisesRegex(ValueError, "UTF-8"):
            self.load()

    def test_source_changes_and_annotation_changes_bind_distinct_digests(self):
        def integrity():
            documents, cases, payload = self.load()
            return benchmark.build_suite_integrity(
                payload, cases, documents, {p: p.read_bytes() for p in documents}
            )

        before = integrity()
        self.item["requiredAny"] = ["worker"]
        changed_annotation = integrity()
        self.assertNotEqual(
            before["suiteDefinitionSha256"], changed_annotation["suiteDefinitionSha256"]
        )
        self.assertEqual(before["corpusSha256"], changed_annotation["corpusSha256"])
        self.source.write_text(
            "The worker must not exceed 5 retries.\nReviewed.\n", encoding="utf-8"
        )
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            self.load()
        self.item["sourceSha256"] = hashlib.sha256(self.source.read_bytes()).hexdigest()
        changed_source = integrity()
        self.assertNotEqual(changed_source["corpusSha256"], before["corpusSha256"])
        self.assertNotEqual(
            changed_source["suiteDefinitionSha256"],
            changed_annotation["suiteDefinitionSha256"],
        )

    def test_every_item_field_changes_definition_identity(self):
        _, cases, _ = self.load()
        original = cases[0]
        before = benchmark.case_definition_sha256(original)
        for key in self.item:
            item = copy.deepcopy(self.item)
            item[key] = ["different"] if isinstance(item[key], list) else "different"
            with self.subTest(key=key):
                self.assertNotEqual(
                    before,
                    benchmark.case_definition_sha256(
                        replace(original, expected_facts=[item])
                    ),
                )
        self.assertNotEqual(
            before,
            benchmark.case_definition_sha256(
                replace(original, expected_facts=[], mandatory_caveats=[self.item])
            ),
        )

    def test_empty_arrays_preserve_unannotated_definition_identity(self):
        del self.payload["cases"][0]["expectedFacts"]
        before = self.load()[1][0]
        self.payload["cases"][0].update(expectedFacts=[], mandatoryCaveats=[])
        after = self.load()[1][0]
        self.assertEqual(
            benchmark.case_definition_sha256(before),
            benchmark.case_definition_sha256(after),
        )
        self.assertEqual(
            benchmark.evaluate_answer_items(
                "", after.expected_facts, after.mandatory_caveats
            )["status"],
            "not_annotated",
        )


if __name__ == "__main__":
    unittest.main()
