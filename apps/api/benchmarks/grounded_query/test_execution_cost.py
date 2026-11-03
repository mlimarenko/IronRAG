from __future__ import annotations

import contextlib
import copy
import io
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import compare_benchmarks as comparison
import run_live_benchmark as benchmark
from test_latency_regression import baseline_matrix, benchmark_case, candidate_matrix
from test_rank_metrics import neutral_benchmark_case

from apps.api.benchmarks.benchmark_test_support import canonical_execution_detail

EXECUTION_ID = "00000000-0000-4000-8000-000000000001"


def snapshot(execution_id=EXECUTION_ID, children=None):
    return {
        "executionId": execution_id,
        "libraryId": "00000000-0000-4000-8000-000000000008",
        "capturedAt": "2026-08-26T00:00:01+00:00",
        "totalIterations": 1,
        "iterations": [{"iteration": 1, "childQueryExecutionIds": children or []}],
    }


def evidence(amount="0.1", currency="USD", calls=2, unpriced=0):
    return {
        "id": "cost-id",
        "owningExecutionKind": "query_execution",
        "owningExecutionId": EXECUTION_ID,
        "totalCost": amount,
        "currencyCode": currency,
        "providerCallCount": calls,
        "unpricedProviderCallCount": unpriced,
        "costCoverage": "complete"
        if unpriced == 0
        else "unknown"
        if unpriced >= calls
        else "partial",
        "updatedAt": "2026-10-22T00:00:00Z",
    }


def case(payload=None, case_id="one"):
    return {
        "caseId": case_id,
        "executionId": EXECUTION_ID,
        "executionCost": {
            "status": "measured",
            "evidence": {
                "rootExecutionId": EXECUTION_ID,
                "components": [
                    {
                        "billing": payload or evidence(),
                        "executionDetail": canonical_execution_detail(),
                        "llmContext": snapshot(),
                    }
                ],
            },
            "error": None,
        },
    }


def rebind_cost(record, execution_id):
    observation = record["executionCost"]["evidence"]
    observation["rootExecutionId"] = execution_id
    component = observation["components"][0]
    component["billing"]["owningExecutionId"] = execution_id
    component["executionDetail"] = copy.deepcopy(
        record.get("rawEvaluationEvidence", {}).get("executionDetail")
    ) or canonical_execution_detail(execution_id=execution_id)
    component["llmContext"] = snapshot(execution_id)
    component["llmContext"]["libraryId"] = component["executionDetail"]["execution"][
        "libraryId"
    ]
    record["executionId"] = execution_id
    return record


def matrix(cases):
    return {"suites": [{"suite": {"suiteId": "suite"}, "cases": cases}]}


class CostClient:
    def __init__(self, components):
        self.components = components
        self.paths = []

    def get_json(self, path):
        self.paths.append(path)
        execution_id = (
            path.split("/")[-2]
            if path.endswith("/llm-context")
            else path.rsplit("/", 1)[-1]
        )
        component = self.components[execution_id]
        key = (
            "llmContext"
            if path.endswith("/llm-context")
            else "billing"
            if path.startswith("/billing/")
            else "executionDetail"
        )
        value = component[key]
        if isinstance(value, Exception):
            raise value
        return copy.deepcopy(value)


def component(execution_id=EXECUTION_ID, children=None, payload=None):
    record = rebind_cost(case(payload), execution_id)
    result = record["executionCost"]["evidence"]["components"][0]
    result["llmContext"] = snapshot(execution_id, children)
    return result


class ExecutionCostTests(unittest.TestCase):
    def test_native_zero_iteration_snapshot_is_valid_for_leaf_and_child(self):
        child = "00000000-0000-4000-8000-000000000014"
        for payload in (evidence("0", calls=0), evidence("0.00157591")):
            for nested in (False, True):
                with self.subTest(payload=payload, nested=nested):
                    leaf_id = child if nested else EXECUTION_ID
                    leaf = component(leaf_id, payload=payload)
                    leaf["llmContext"].update(iterations=[], totalIterations=0)
                    components = {leaf_id: leaf}
                    if nested:
                        components[EXECUTION_ID] = component(
                            children=[child], payload=evidence("0", calls=0)
                        )
                    observation = benchmark.fetch_execution_cost(
                        CostClient(components), EXECUTION_ID
                    )
                    record = dict(case(), executionCost=observation)
                    summary = benchmark.build_cost_summary([record])
                    self.assertEqual(observation["status"], "measured")
                    self.assertTrue(summary["complete"])
                    self.assertEqual(
                        summary["observedTotalsByCurrency"],
                        {"USD": payload["totalCost"]},
                    )
                    report = comparison.build_cost_comparison(
                        matrix([record]), matrix([copy.deepcopy(record)]), "10"
                    )
                    self.assertTrue(report["comparable"])
                    self.assertTrue(report["passed"])

    def test_distinct_descendants_and_repeated_lineage_references(self):
        left, right, leaf = [
            f"00000000-0000-4000-8000-{number:012d}" for number in (11, 12, 13)
        ]
        components = {
            EXECUTION_ID: component(
                children=[left, left, right], payload=evidence("0", calls=0)
            ),
            left: component(left, [leaf], evidence("0.1")),
            right: component(right, [leaf], evidence("1.25", "EUR")),
            leaf: component(leaf, payload=evidence("0.20000000000000000000000000001")),
        }
        # Neither messages nor arbitrary tool-result JSON are lineage.
        components[leaf]["llmContext"]["iterations"][0]["responseText"] = EXECUTION_ID
        client = CostClient(components)
        observation = benchmark.fetch_execution_cost(client, EXECUTION_ID)
        summary = benchmark.build_cost_summary(
            [dict(case(), executionCost=observation)]
        )
        self.assertEqual(
            summary["observedTotalsByCurrency"],
            {"USD": "0.30000000000000000000000000001", "EUR": "1.25"},
        )
        self.assertTrue(summary["complete"])
        self.assertEqual(
            len([path for path in client.paths if path.startswith("/billing/")]), 4
        )
        self.assertEqual(
            observation["evidence"]["components"][0], components[EXECUTION_ID]
        )

    def test_shared_descendant_is_unique_across_cases_and_turns(self):
        other, child = [f"00000000-0000-4000-8000-{number:012d}" for number in (11, 12)]
        client = CostClient(
            {
                EXECUTION_ID: component(children=[child]),
                other: component(other, [child]),
                child: component(child),
            }
        )
        records = [
            dict(
                case(),
                executionCost=benchmark.fetch_execution_cost(client, EXECUTION_ID),
            ),
            dict(
                case(case_id="two"),
                executionId=other,
                executionCost=benchmark.fetch_execution_cost(client, other),
            ),
        ]
        for summary in (
            benchmark.build_cost_summary(records),
            benchmark.build_conversation_cost_summary([{"turnExecutions": records}]),
        ):
            self.assertEqual(summary["observedTotalsByCurrency"], {"USD": "0.3"})
            self.assertEqual(summary["coverageCounts"]["complete"], 2)
        duplicate = benchmark.build_cost_summary(
            [records[0], copy.deepcopy(records[0])]
        )
        self.assertFalse(duplicate["complete"])
        self.assertEqual(duplicate["observedTotalsByCurrency"], {})
        self.assertEqual(duplicate["coverageCounts"]["unavailable"], 2)
        records[1]["executionCost"]["evidence"]["components"][1]["billing"][
            "totalCost"
        ] = "9"
        conflicted = benchmark.build_cost_summary(records)
        self.assertEqual(conflicted["coverageCounts"]["unavailable"], 2)
        self.assertEqual(conflicted["observedTotalsByCurrency"], {})

    def test_snapshot_and_scope_failures_are_unavailable_not_zero(self):
        child = "00000000-0000-4000-8000-000000000011"
        mutations = (
            lambda rows: rows[EXECUTION_ID].update(llmContext=None),
            lambda rows: rows[EXECUTION_ID]["llmContext"].pop("iterations"),
            lambda rows: rows[EXECUTION_ID]["llmContext"].update(totalIterations=2),
            lambda rows: rows[EXECUTION_ID]["llmContext"]["iterations"][0].pop(
                "childQueryExecutionIds"
            ),
            lambda rows: rows[EXECUTION_ID]["llmContext"]["iterations"][0].update(
                childQueryExecutionIds=["not-a-uuid"]
            ),
            lambda rows: rows[EXECUTION_ID]["llmContext"]["iterations"][0].update(
                childQueryExecutionIds=[child] * (benchmark.MAX_COST_REFERENCES + 1)
            ),
            lambda rows: rows[child]["llmContext"].update(executionId=EXECUTION_ID),
            lambda rows: rows[child]["llmContext"].update(libraryId=child),
            lambda rows: rows[child]["executionDetail"]["execution"].update(
                workspaceId=child
            ),
            lambda rows: rows[child]["llmContext"]["iterations"][0].update(
                childQueryExecutionIds=[EXECUTION_ID]
            ),
            lambda rows: rows[child].update(
                llmContext=RuntimeError("credential-bearing URL must not escape")
            ),
            lambda rows: rows.pop(child),
        )
        for index, mutate in enumerate(mutations):
            rows = {
                EXECUTION_ID: component(
                    children=[child], payload=evidence("0", calls=0)
                ),
                child: component(child),
            }
            mutate(rows)
            with self.subTest(index=index):
                observation = benchmark.fetch_execution_cost(
                    CostClient(rows), EXECUTION_ID
                )
                self.assertEqual(observation["status"], "unavailable")
                self.assertNotIn("credential", observation["error"])
                self.assertEqual(
                    observation["evidence"]["components"][0]["billing"],
                    rows[EXECUTION_ID]["billing"],
                )
                summary = benchmark.build_cost_summary(
                    [dict(case(), executionCost=observation)]
                )
                self.assertFalse(summary["complete"])
                self.assertEqual(summary["observedTotalsByCurrency"], {})

    def test_depth_and_cardinality_are_bounded(self):
        for limit_name, count in (("MAX_COST_DEPTH", 11), ("MAX_COST_EXECUTIONS", 66)):
            ids = [
                f"00000000-0000-4000-8000-{number:012d}"
                for number in range(1, count + 1)
            ]
            if limit_name == "MAX_COST_DEPTH":
                rows = {
                    identity: component(identity, ids[index + 1 : index + 2])
                    for index, identity in enumerate(ids)
                }
            else:
                rows = {identity: component(identity) for identity in ids}
                rows[EXECUTION_ID] = component(children=ids[1:])
            client = CostClient(rows)
            observation = benchmark.fetch_execution_cost(client, EXECUTION_ID)
            with self.subTest(limit=limit_name):
                self.assertEqual(observation["status"], "unavailable")
                self.assertLessEqual(
                    len(client.paths), benchmark.MAX_COST_EXECUTIONS * 3
                )

    def test_cached_or_historical_attribution_is_refused(self):
        child = "00000000-0000-4000-8000-000000000011"
        for replay in (False, True):
            rows = {EXECUTION_ID: component(children=[child]), child: component(child)}
            if replay:
                rows[EXECUTION_ID]["executionDetail"]["responseTurn"]["id"] = child
            else:
                rows[child]["executionDetail"]["execution"]["startedAt"] = (
                    "2026-08-25T00:00:00Z"
                )
            observation = benchmark.fetch_execution_cost(CostClient(rows), EXECUTION_ID)
            self.assertEqual(observation["status"], "unavailable")

    def test_comparator_recomputes_native_components_and_lineage(self):
        child = "00000000-0000-4000-8000-000000000011"
        rows = {
            EXECUTION_ID: component(children=[child], payload=evidence("0", calls=0)),
            child: component(child),
        }
        record = dict(
            case(),
            executionCost=benchmark.fetch_execution_cost(
                CostClient(rows), EXECUTION_ID
            ),
        )
        record["executionCost"]["evidence"]["totalCost"] = "999"
        record["executionCost"]["evidence"]["costCoverage"] = "unknown"
        report = comparison.build_cost_comparison(
            matrix([record]), matrix([record]), "0"
        )
        self.assertTrue(report["passed"])
        self.assertEqual(report["baseline"]["observedTotalsByCurrency"], {"USD": "0.1"})
        for mutation in (
            "missing",
            "orphan",
            "duplicate",
            "historical",
            "old_contract",
        ):
            tampered = copy.deepcopy(record)
            observation = tampered["executionCost"]["evidence"]
            if mutation == "missing":
                observation["components"].pop()
            elif mutation == "orphan":
                observation["components"][0]["llmContext"]["iterations"][0][
                    "childQueryExecutionIds"
                ] = []
            elif mutation == "duplicate":
                observation["components"].append(
                    copy.deepcopy(observation["components"][0])
                )
            elif mutation == "historical":
                observation["components"][1]["executionDetail"]["execution"][
                    "startedAt"
                ] = "2026-08-25T00:00:00Z"
            else:
                tampered["executionCost"]["evidence"] = evidence("0", calls=0)
            with self.subTest(mutation=mutation):
                report = comparison.build_cost_comparison(
                    matrix([record]), matrix([tampered]), "0"
                )
                self.assertFalse(report["passed"])
                self.assertFalse(report["comparable"])

    def test_nested_partial_and_unknown_coverage_remain_explicit(self):
        child = "00000000-0000-4000-8000-000000000011"
        for unpriced in (1, 2):
            rows = {
                EXECUTION_ID: component(
                    children=[child], payload=evidence("0", calls=0)
                ),
                child: component(child, payload=evidence("0", unpriced=unpriced)),
            }
            record = dict(
                case(),
                executionCost=benchmark.fetch_execution_cost(
                    CostClient(rows), EXECUTION_ID
                ),
            )
            summary = benchmark.build_cost_summary([record])
            self.assertFalse(summary["complete"])
            self.assertEqual(
                summary["coverageCounts"]["partial" if unpriced == 1 else "unknown"], 1
            )
            self.assertFalse(
                comparison.build_cost_comparison(
                    matrix([record]), matrix([record]), "0"
                )["passed"]
            )

    def test_zero_outer_paid_typed_child_is_not_complete_zero(self):
        child = "00000000-0000-4000-8000-000000000011"

        class Client:
            def get_json(self, path):
                execution_id = (
                    path.split("/")[-2]
                    if path.endswith("/llm-context")
                    else path.rsplit("/", 1)[-1]
                )
                if path.startswith("/billing/"):
                    result = evidence(
                        "0" if execution_id == EXECUTION_ID else "0.00157591",
                        calls=0 if execution_id == EXECUTION_ID else 6,
                    )
                    result["owningExecutionId"] = execution_id
                    return result
                if path.endswith("/llm-context"):
                    return snapshot(
                        execution_id, [child] if execution_id == EXECUTION_ID else []
                    )
                return canonical_execution_detail(execution_id=execution_id)

        observation = benchmark.fetch_execution_cost(Client(), EXECUTION_ID)
        summary = benchmark.build_cost_summary(
            [dict(case(), executionCost=observation)]
        )
        self.assertEqual(summary["observedTotalsByCurrency"], {"USD": "0.00157591"})
        self.assertTrue(summary["complete"])

    def test_native_coverage_including_zero_call(self):
        for payload in (
            evidence(),
            evidence(unpriced=1),
            evidence("0", unpriced=2),
            evidence("0", calls=0),
        ):
            with self.subTest(payload=payload):
                summary = benchmark.build_cost_summary([case(payload)])
                self.assertEqual(summary["coverageCounts"][payload["costCoverage"]], 1)
                self.assertEqual(
                    summary["complete"], payload["costCoverage"] == "complete"
                )

    def test_fetch_rejects_invalid_evidence_without_losing_failure_status(self):
        class Client:
            def get_json(self, path):
                return evidence("1_000")

        observation = benchmark.fetch_execution_cost(Client(), EXECUTION_ID)
        self.assertEqual(
            observation,
            {
                "status": "unavailable",
                "evidence": None,
                "error": "ValueError",
            },
        )

    def test_excess_unpriced_calls_are_unavailable_and_fail_opt_in_gate(self):
        class Client:
            def get_json(self, path):
                return evidence("0", calls=2, unpriced=3)

        observation = benchmark.fetch_execution_cost(Client(), EXECUTION_ID)
        self.assertEqual(observation["status"], "unavailable")
        self.assertIsNone(observation["evidence"])
        self.assertEqual(observation["error"], "ValueError")

        historical_case = case(evidence("0", calls=2, unpriced=3))
        summary = benchmark.build_cost_summary([historical_case])
        self.assertEqual(summary["coverageCounts"]["unavailable"], 1)
        self.assertEqual(summary["coverageCounts"]["unknown"], 0)
        self.assertEqual(summary["observedTotalsByCurrency"], {})
        self.assertFalse(summary["complete"])
        self.assertEqual(summary["invalidCaseIds"], ["one"])
        for baseline, candidate in (
            (matrix([case()]), matrix([historical_case])),
            (matrix([historical_case]), matrix([case()])),
        ):
            with self.subTest(baseline=baseline):
                report = comparison.build_cost_comparison(baseline, candidate, "10")
                self.assertFalse(report["comparable"])
                self.assertFalse(report["passed"])

    def test_currency_separation_and_exact_decimal_sum(self):
        summary = benchmark.build_cost_summary(
            [
                case(evidence("0.10000000000000000000000000001")),
                rebind_cost(
                    case(evidence("0.2"), "two"), "00000000-0000-4000-8000-000000000002"
                ),
                rebind_cost(
                    case(evidence("1.25", "EUR"), "three"),
                    "00000000-0000-4000-8000-000000000003",
                ),
            ]
        )
        self.assertEqual(
            summary["observedTotalsByCurrency"],
            {
                "USD": "0.30000000000000000000000000001",
                "EUR": "1.25",
            },
        )

    def test_untrusted_evidence_is_unavailable_not_zero(self):
        for field, value in (
            ("totalCost", "NaN"),
            ("totalCost", "-1"),
            ("totalCost", 0.1),
            ("totalCost", "1e999999"),
            ("currencyCode", "bad currency"),
            ("providerCallCount", True),
            ("unpricedProviderCallCount", -1),
            ("costCoverage", "unknown"),
            ("owningExecutionId", "another"),
            ("owningExecutionKind", "document_ingestion"),
        ):
            payload = evidence()
            payload[field] = value
            with self.subTest(field=field, value=value):
                summary = benchmark.build_cost_summary([case(payload)])
                self.assertEqual(summary["coverageCounts"]["unavailable"], 1)
                self.assertEqual(summary["observedTotalsByCurrency"], {})
                self.assertFalse(summary["complete"])

    def test_runner_fetch_failure_preserves_answer_and_evaluation(self):
        for failure in (False, True):
            paths = []

            class Client:
                def __init__(self, paths, failure):
                    self.paths = paths
                    self.failure = failure

                def post_json(self, path, payload):
                    if path.endswith("/search"):
                        return {"documentHits": []}
                    return canonical_execution_detail(
                        "A grounded answer.", question="Neutral question?"
                    )

                def get_json(self, path):
                    self.paths.append(path)
                    if path.startswith("/billing/"):
                        if self.failure:
                            raise RuntimeError("do not persist authenticated URL")
                        return evidence()
                    if path.endswith("/llm-context"):
                        return snapshot()
                    return canonical_execution_detail(
                        "A grounded answer.", question="Neutral question?"
                    )

            result = benchmark.run_case(
                Client(paths, failure),
                "00000000-0000-4000-8000-000000000007",
                "00000000-0000-4000-8000-000000000008",
                "session",
                neutral_benchmark_case("one"),
                8,
            )
            self.assertEqual(result["answer"], "A grounded answer.")
            self.assertTrue(result["strictCasePass"])
            observation = result["executionCost"]
            self.assertEqual(
                observation["status"], "unavailable" if failure else "measured"
            )
            self.assertEqual(observation["error"], "RuntimeError" if failure else None)
            self.assertIn(f"/billing/executions/query_execution/{EXECUTION_ID}", paths)
            suite = {
                "suite": {"suiteId": "suite"},
                "strictBlocking": True,
                "cases": [result],
                "summary": benchmark.build_summary([result]),
            }
            self.assertEqual(
                benchmark.build_matrix_summary([suite])["executionCost"],
                suite["summary"]["executionCost"],
            )

    def test_old_results_and_invalid_observations_do_not_pass_opt_in_gate(self):
        for cases in (
            [],
            [{"caseId": "one"}],
            [case(evidence(unpriced=1))],
            [dict(case(), executionCost=None)],
            [dict(case(), executionCost=42)],
        ):
            with self.subTest(cases=cases):
                report = comparison.build_cost_comparison(
                    matrix(cases), matrix(cases), "10"
                )
                self.assertFalse(report["passed"])
                self.assertFalse(report["comparable"])
                self.assertTrue(
                    comparison.build_cost_comparison(matrix(cases), matrix(cases))[
                        "passed"
                    ]
                )

    def test_downgrade_is_visible_but_advisory_by_default(self):
        old = matrix([case()])
        for new_case in (
            case(evidence(unpriced=1)),
            case(evidence("0", unpriced=2)),
            {"caseId": "one"},
        ):
            report = comparison.build_cost_comparison(old, matrix([new_case]))
            self.assertEqual(report["coverageDowngrades"], ["suite/one"])
            self.assertTrue(report["passed"])
            self.assertFalse(
                comparison.build_cost_comparison(old, matrix([new_case]), "10")[
                    "passed"
                ]
            )

    def test_opt_in_budget_including_zero_baseline_and_currency_change(self):
        for old, new, passed in (
            ("0.1", "0.11", True),
            ("0.1", "0.11000000000000000000001", False),
            ("0", "0", True),
            ("0", "0.01", False),
            ("1", "0.9", True),
        ):
            report = comparison.build_cost_comparison(
                matrix([case(evidence(old))]), matrix([case(evidence(new))]), "10"
            )
            self.assertEqual(report["passed"], passed)
        self.assertFalse(
            comparison.build_cost_comparison(
                matrix([case()]), matrix([case(evidence(currency="EUR"))]), "10"
            )["passed"]
        )
        changed = copy.deepcopy(matrix([case()]))
        changed["suites"][0]["cases"][0]["caseId"] = "different"
        self.assertFalse(
            comparison.build_cost_comparison(matrix([case()]), changed, "10")["passed"]
        )

    def test_invalid_threshold_rejected(self):
        for value in ("NaN", "Infinity", "-1", "bad", "1e9999"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                comparison.build_cost_comparison(
                    matrix([case()]), matrix([case()]), value
                )
        args = comparison.parse_args(
            ["base", "candidate", "--max-cost-regression-percent", "0.5"]
        )
        self.assertEqual(args.max_cost_regression_percent, "0.5")
        for value in ("NaN", "-1", "bad"):
            with (
                self.subTest(cli=value),
                contextlib.redirect_stderr(io.StringIO()),
                self.assertRaises(SystemExit) as error,
            ):
                comparison.parse_args(
                    ["base", "candidate", "--max-cost-regression-percent", value]
                )
            self.assertEqual(error.exception.code, 2)

    def test_report_cost_gate_controls_overall_result_only_when_enabled(self):
        before = benchmark_case("one", 100.0)
        after = copy.deepcopy(before)
        before["executionCost"] = case(evidence("1"))["executionCost"]
        after["executionCost"] = case(evidence("2"))["executionCost"]
        # Bind billing evidence to the fixture's real execution identity.
        for item in (before, after):
            item["executionId"] = item["rawEvaluationEvidence"]["executionDetail"][
                "execution"
            ]["id"]
            rebind_cost(item, item["executionId"])
        baseline, candidate = baseline_matrix([before]), candidate_matrix([after])
        advisory = comparison.build_comparison_report(baseline, candidate)
        self.assertTrue(advisory["passed"])
        gated = comparison.build_comparison_report(
            baseline, candidate, max_cost_regression_percent="10"
        )
        self.assertFalse(gated["passed"])
        self.assertEqual(gated["executionCost"]["regressedCurrencies"], ["USD"])


if __name__ == "__main__":
    unittest.main()
