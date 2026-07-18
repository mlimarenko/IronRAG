#!/usr/bin/env python3

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from copy import deepcopy
from dataclasses import replace
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parent))

import compare_benchmarks as comparison  # noqa: E402
import run_live_benchmark as benchmark  # noqa: E402


def benchmark_case(
    case_id: str,
    answer_latency_ms: float,
    *,
    strict_case_pass: bool = True,
) -> dict:
    question = f"Synthetic question for {case_id}"
    return {
        "caseId": case_id,
        "caseDefinitionSha256": benchmark.canonical_json_sha256(
            {"caseId": case_id, "question": question}
        ),
        "question": question,
        "topSearchDocumentOk": strict_case_pass,
        "retrievalContainsRequired": strict_case_pass,
        "answerPass": strict_case_pass,
        "graphUsagePass": strict_case_pass,
        "entityReferenceLabelsPass": strict_case_pass,
        "relationReferenceTextPass": strict_case_pass,
        "structuredEvidencePass": strict_case_pass,
        "verificationPass": strict_case_pass,
        "strictCasePass": strict_case_pass,
        "answerHasForbidden": False,
        "failedChecks": [] if strict_case_pass else ["answer_required"],
        "rankMetrics": {},
        "answerLatencyMs": answer_latency_ms,
    }


def healthy_host_snapshot() -> dict:
    return {
        "schemaVersion": benchmark.HOST_SNAPSHOT_SCHEMA_VERSION,
        "capturedAt": "2026-07-10T00:00:00+00:00",
        "capturedMonotonicNs": 1,
        "cpuCount": 8,
        "load1m": 2.0,
        "load5m": 2.0,
        "load15m": 2.0,
        "load1mPerCpu": 999.0,
        "MemTotalKiB": 16 * 1024 * 1024,
        "MemAvailableKiB": 12 * 1024 * 1024,
        "SwapTotalKiB": 4 * 1024 * 1024,
        "SwapFreeKiB": 4 * 1024 * 1024,
        "pressure": {
            "cpu": {"some": {"avg10": 0.5, "avg60": 0.5, "avg300": 0.5, "total": 1}},
            "memory": {
                "some": {"avg10": 0.0, "avg60": 0.0, "avg300": 0.0, "total": 0},
                "full": {"avg10": 0.0, "avg60": 0.0, "avg300": 0.0, "total": 0},
            },
            "io": {
                "some": {"avg10": 0.0, "avg60": 0.0, "avg300": 0.0, "total": 0},
                "full": {"avg10": 0.0, "avg60": 0.0, "avg300": 0.0, "total": 0},
            },
        },
    }


def healthy_measurement_protocol(*, cache_policy: str, round_id: str) -> dict:
    preflight = healthy_host_snapshot()
    midflight = deepcopy(preflight)
    postflight = deepcopy(preflight)
    midflight["capturedAt"] = "2026-07-10T00:00:01+00:00"
    postflight["capturedAt"] = "2026-07-10T00:00:02+00:00"
    midflight["capturedMonotonicNs"] = 2
    postflight["capturedMonotonicNs"] = 3
    policy = benchmark.build_host_eligibility_policy()
    environment = benchmark.build_environment_identity(
        preflight,
        system="Linux",
        kernel_release="synthetic-kernel",
        machine="x86_64",
        host_id_sha256="1" * 64,
        boot_id_sha256="2" * 64,
        cpu_affinity_count=8,
        cgroup_cpu_max="max 100000",
        cgroup_memory_max="max",
    )
    return benchmark.build_measurement_protocol(
        cache_policy=cache_policy,
        round_id=round_id,
        environment_identity=environment,
        host_policy=policy,
        host_preflight=preflight,
        host_midflight=midflight,
        host_postflight=postflight,
        busy_host_override=False,
    )


def benchmark_matrix(
    cases: list[dict],
    *,
    artifact_digest: str,
    cache_policy: str = "cold",
    round_id: str = "round-1",
) -> dict:
    case_order = benchmark.permute_case_ids_for_round(
        [case["caseId"] for case in cases],
        round_id,
        "synthetic_latency_suite",
    )
    cases_by_id = {case["caseId"]: case for case in cases}
    cases = [cases_by_id[case_id] for case_id in case_order]
    suite = {
        "suite": {"suiteId": "synthetic_latency_suite"},
        "strictBlocking": True,
        "integrity": {
            "schemaVersion": benchmark.BENCHMARK_INTEGRITY_SCHEMA_VERSION,
            "suiteDefinitionSha256": benchmark.canonical_json_sha256(
                [case["caseDefinitionSha256"] for case in cases]
            ),
            "corpusSha256": benchmark.canonical_json_sha256("synthetic-corpus"),
            "serverCorpusSha256": benchmark.canonical_json_sha256("synthetic-corpus"),
            "corpusVerification": "server_source_bytes",
        },
        "caseOrderPolicy": benchmark.CASE_ORDER_POLICY,
        "caseOrder": case_order,
        "summary": benchmark.build_summary(cases),
        "cases": cases,
    }
    matrix = {
        "topologyCounts": {},
        "runtimeIdentity": {
            "label": "synthetic-runtime",
            "artifactDigest": artifact_digest,
            "versionEndpoint": {
                "service": "ironrag-api",
                "version": "synthetic",
                "environment": "benchmark",
                "role": "api",
            },
        },
        "measurementProtocol": healthy_measurement_protocol(
            cache_policy=cache_policy,
            round_id=round_id,
        ),
        "summary": benchmark.build_matrix_summary([suite]),
        "suites": [suite],
    }
    matrix["benchmarkIntegrity"] = benchmark.build_matrix_integrity(
        matrix["suites"],
        query_top_k=8,
        skip_upload=True,
        canonicalize_reused_library=False,
        cache_policy=cache_policy,
        round_id=round_id,
    )
    return matrix


def baseline_matrix(cases: list[dict], **kwargs: object) -> dict:
    return benchmark_matrix(cases, artifact_digest="a" * 64, **kwargs)


def candidate_matrix(cases: list[dict], **kwargs: object) -> dict:
    return benchmark_matrix(cases, artifact_digest="b" * 64, **kwargs)


class LatencySummaryTests(unittest.TestCase):
    def test_suite_summary_emits_machine_readable_answer_latency_percentiles(self) -> None:
        cases = [
            benchmark_case(f"case-{index}", latency)
            for index, latency in enumerate((100.0, 200.0, 300.0, 400.0, 500.0), start=1)
        ]

        summary = benchmark.build_summary(cases)

        self.assertEqual(
            summary["answerLatencyMs"],
            {
                "sampleCount": 5,
                "p50": 300.0,
                "p95": 500.0,
                "p99": 500.0,
            },
        )


class BenchmarkIntegrityFingerprintTests(unittest.TestCase):
    @staticmethod
    def case() -> benchmark.BenchmarkCase:
        return benchmark.BenchmarkCase(
            case_id="integrity-case",
            question="Which neutral marker is current?",
            search_query="neutral marker",
            expected_documents_contains=["fixture.md"],
            search_required_all=["marker"],
            answer_required_all=["current"],
            answer_required_any=["alpha", "beta"],
            answer_opening_required_any=["alpha"],
            answer_forbidden_any=["forbidden"],
            min_chunk_reference_count=1,
            min_prepared_segment_reference_count=0,
            min_technical_fact_reference_count=0,
            min_entity_reference_count=0,
            min_relation_reference_count=0,
            expected_entity_reference_labels_contains=["Neutral entity"],
            expected_relation_reference_text_contains=["relates_to"],
            allowed_verification_states=["verified"],
            relevant_documents=["fixture.md"],
            relevant_chunks=["marker"],
        )

    def test_case_definition_hash_tracks_question_expected_labels_and_thresholds(self) -> None:
        baseline = self.case()
        baseline_hash = benchmark.case_definition_sha256(baseline)

        variants = [
            replace(baseline, question="Which other neutral marker is current?"),
            replace(
                baseline,
                expected_entity_reference_labels_contains=["Different entity"],
            ),
            replace(baseline, min_chunk_reference_count=2),
        ]
        for variant in variants:
            with self.subTest(variant=variant):
                self.assertNotEqual(
                    baseline_hash,
                    benchmark.case_definition_sha256(variant),
                )

    def test_corpus_hash_tracks_fixture_bytes_under_the_same_document_reference(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            fixture = Path(temporary_directory) / "fixture.md"
            fixture.write_bytes(b"neutral corpus version one\n")
            baseline_hash = benchmark.corpus_sha256([fixture])

            fixture.write_bytes(b"neutral corpus version two\n")
            candidate_hash = benchmark.corpus_sha256([fixture])

        self.assertNotEqual(baseline_hash, candidate_hash)

    def test_matrix_percentiles_are_calculated_from_cases_not_suite_percentiles(self) -> None:
        first_cases = [benchmark_case("fast", 100.0)]
        second_cases = [
            benchmark_case("medium", 200.0),
            benchmark_case("slow", 900.0),
        ]
        suites = [
            {
                "suite": {"suiteId": "first"},
                "strictBlocking": True,
                "summary": benchmark.build_summary(first_cases),
                "cases": first_cases,
            },
            {
                "suite": {"suiteId": "second"},
                "strictBlocking": True,
                "summary": benchmark.build_summary(second_cases),
                "cases": second_cases,
            },
        ]

        summary = benchmark.build_matrix_summary(suites)

        self.assertEqual(
            summary["answerLatencyMs"],
            {
                "sampleCount": 3,
                "p50": 200.0,
                "p95": 900.0,
                "p99": 900.0,
            },
        )


class BenchmarkIdentityValidationTests(unittest.TestCase):
    def test_identity_string_validation_preserves_key_order_for_blank_and_non_string_values(self) -> None:
        identity = {
            "system": " ",
            "kernelRelease": 7,
            "machine": "valid",
            "hostIdSha256": "valid",
            "bootIdSha256": "valid",
            "cgroupCpuMax": "valid",
            "cgroupMemoryMax": "valid",
        }

        invalid = benchmark._validate_identity_strings(identity)

        self.assertEqual(invalid, ["system", "kernelRelease"])

    def test_identity_string_validation_rejects_unavailable_cgroup_limits_in_key_order(self) -> None:
        identity = {
            "system": "valid",
            "kernelRelease": "valid",
            "machine": "valid",
            "hostIdSha256": "valid",
            "bootIdSha256": "valid",
            "cgroupCpuMax": "unavailable",
            "cgroupMemoryMax": "unavailable",
        }

        invalid = benchmark._validate_identity_strings(identity)

        self.assertEqual(invalid, ["cgroupCpuMax", "cgroupMemoryMax"])


class BenchmarkHostEligibilityTests(unittest.TestCase):
    def test_eligibility_recomputes_raw_load_memory_swap_and_psi(self) -> None:
        snapshot = healthy_host_snapshot()
        snapshot["load1mPerCpu"] = 999.0

        evaluation = benchmark.evaluate_host_snapshot(
            snapshot,
            benchmark.build_host_eligibility_policy(),
        )

        self.assertTrue(evaluation["passed"])
        self.assertEqual(evaluation["computed"]["load1mPerCpu"], 0.25)

    def test_host_snapshot_reports_each_invalid_measurement(self) -> None:
        snapshot = healthy_host_snapshot()
        snapshot.update({"cpuCount": None, "MemAvailableKiB": None, "SwapFreeKiB": None})
        snapshot["pressure"] = {}

        evaluation = benchmark.evaluate_host_snapshot(
            snapshot,
            benchmark.build_host_eligibility_policy(),
        )

        self.assertEqual(
            evaluation["violations"],
            [
                "cpuPsiSomeAvg10.invalid",
                "ioPsiFullAvg10.invalid",
                "ioPsiSomeAvg10.invalid",
                "load.invalid",
                "memory.invalid",
                "memoryPsiFullAvg10.invalid",
                "memoryPsiSomeAvg10.invalid",
                "swap.invalid",
            ],
        )

    def test_environment_identity_reports_stale_fingerprint_once(self) -> None:
        snapshot = healthy_host_snapshot()
        identity = benchmark.build_environment_identity(
            snapshot,
            system="Synthetic",
            kernel_release="test-kernel",
            machine="test-machine",
            host_id_sha256="1" * 64,
            boot_id_sha256="2" * 64,
            cpu_affinity_count=8,
            cgroup_cpu_max="max 100000",
            cgroup_memory_max="1073741824",
        )
        identity["machine"] = "changed-machine"

        self.assertEqual(
            benchmark.validate_environment_identity(identity),
            ["fingerprintSha256:stale"],
        )

    def test_memory_swap_and_psi_pressure_fail_closed(self) -> None:
        policy = benchmark.build_host_eligibility_policy()
        mutations = {
            "memory": lambda snapshot: snapshot.update({"MemAvailableKiB": 1}),
            "swap": lambda snapshot: snapshot.update({"SwapFreeKiB": 0}),
            "cpuPsi": lambda snapshot: snapshot["pressure"]["cpu"]["some"].update(
                {"avg10": 99.0}
            ),
            "memoryPsi": lambda snapshot: snapshot["pressure"]["memory"]["full"].update(
                {"avg10": 99.0}
            ),
            "ioPsi": lambda snapshot: snapshot["pressure"]["io"]["some"].update(
                {"avg10": 99.0}
            ),
        }

        for label, mutate in mutations.items():
            with self.subTest(label=label):
                snapshot = healthy_host_snapshot()
                mutate(snapshot)
                self.assertFalse(
                    benchmark.evaluate_host_snapshot(snapshot, policy)["passed"]
                )

    def test_session_cookie_is_loaded_from_environment_or_file_not_argv(self) -> None:
        self.assertEqual(
            benchmark.resolve_session_cookie({"IRONRAG_SESSION_COOKIE": "secret"}),
            "secret",
        )
        with tempfile.TemporaryDirectory() as temporary_directory:
            secret_file = Path(temporary_directory) / "cookie"
            secret_file.write_text("file-secret\n", encoding="utf-8")
            self.assertEqual(
                benchmark.resolve_session_cookie(
                    {"IRONRAG_SESSION_COOKIE_FILE": str(secret_file)}
                ),
                "file-secret",
            )
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            benchmark.parse_args(["--session-cookie", "must-not-enter-argv"])


class BenchmarkComparisonTests(unittest.TestCase):
    def test_comparison_uses_actual_flat_case_schema(self) -> None:
        case = benchmark_case("passing-case", 100.0)

        self.assertTrue(comparison.case_passed(case))
        self.assertEqual(comparison.case_failure_details(case), [])

    def test_zero_latency_baselines_do_not_require_float_equality(self) -> None:
        self.assertEqual(comparison.latency_regression_percent(0.0, 0.0), 0.0)
        self.assertIsNone(comparison.latency_regression_percent(0.0, 1.0))

    def test_exact_latency_budget_is_allowed(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 110.0)])

        report = comparison.build_comparison_report(
            baseline,
            candidate,
            max_latency_regression_percent=10.0,
        )

        self.assertTrue(report["latencyGate"]["passed"])
        self.assertTrue(report["correctnessGate"]["passed"])
        self.assertTrue(report["passed"])

    def test_absolute_latency_slo_fails_even_when_candidate_is_faster(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 40_000.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 39_000.0)])

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertEqual(
            report["latencyGate"]["breachedAbsolutePercentiles"],
            ["p50", "p95"],
        )
        self.assertFalse(report["latencyGate"]["passed"])
        self.assertFalse(report["passed"])

    def test_latency_regression_above_budget_fails_all_percentile_gate(self) -> None:
        baseline = baseline_matrix(
            [
                benchmark_case("case-1", 100.0),
                benchmark_case("case-2", 200.0),
                benchmark_case("case-3", 300.0),
            ]
        )
        candidate = candidate_matrix(
            [
                benchmark_case("case-1", 111.0),
                benchmark_case("case-2", 222.0),
                benchmark_case("case-3", 333.0),
            ]
        )

        report = comparison.build_comparison_report(
            baseline,
            candidate,
            max_latency_regression_percent=10.0,
        )

        self.assertFalse(report["latencyGate"]["passed"])
        self.assertEqual(
            report["latencyGate"]["regressedPercentiles"],
            ["p50", "p95", "p99"],
        )
        self.assertFalse(report["passed"])

    def test_correctness_regression_fails_even_when_candidate_is_faster(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix(
            [benchmark_case("case-1", 80.0, strict_case_pass=False)]
        )

        report = comparison.build_comparison_report(
            baseline,
            candidate,
            max_latency_regression_percent=10.0,
        )

        self.assertTrue(report["latencyGate"]["passed"])
        self.assertFalse(report["correctnessGate"]["passed"])
        self.assertEqual(
            report["correctnessGate"]["regressedCases"],
            ["synthetic_latency_suite/case-1"],
        )
        self.assertFalse(report["passed"])

    def test_identically_failing_baseline_and_candidate_cannot_pass_absolute_gate(self) -> None:
        baseline = baseline_matrix(
            [benchmark_case("case-1", 100.0, strict_case_pass=False)]
        )
        candidate = candidate_matrix(
            [benchmark_case("case-1", 95.0, strict_case_pass=False)]
        )

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertTrue(report["correctnessGate"]["passRatePreserved"])
        self.assertFalse(report["correctnessGate"]["candidateAllStrictPassed"])
        self.assertFalse(report["correctnessGate"]["passed"])
        self.assertFalse(report["passed"])

    def test_case_identity_is_stable_when_suites_reuse_ids_and_reorder(self) -> None:
        first = baseline_matrix([benchmark_case("shared", 100.0)])
        first["suites"].append(
            {
                "suite": {"suiteId": "second-suite"},
                "strictBlocking": True,
                "summary": benchmark.build_summary([benchmark_case("shared", 100.0)]),
                "cases": [benchmark_case("shared", 100.0)],
            }
        )
        first["summary"] = benchmark.build_matrix_summary(first["suites"])

        reordered = {**first, "suites": list(reversed(first["suites"]))}
        reordered["summary"] = benchmark.build_matrix_summary(reordered["suites"])

        gate = comparison.build_correctness_gate(first, reordered)

        self.assertTrue(gate["passed"])
        self.assertEqual(gate["missingCases"], [])
        self.assertEqual(gate["addedCases"], [])

    def test_empty_correctness_results_fail_closed(self) -> None:
        gate = comparison.build_correctness_gate({"suites": []}, {"suites": []})

        self.assertFalse(gate["passed"])

    def test_zero_baseline_latency_keeps_comparison_report_strict_json(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 0.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 1.0)])

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertFalse(report["latencyGate"]["passed"])
        json.dumps(report, allow_nan=False)

    def test_missing_candidate_latency_sample_fails_closed(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate_case = benchmark_case("case-1", 80.0)
        candidate_case["answerLatencyMs"] = None
        candidate = candidate_matrix([candidate_case])

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertFalse(report["latencyGate"]["sampleCountPreserved"])
        self.assertFalse(report["latencyGate"]["passed"])
        self.assertFalse(report["passed"])

    def test_added_fast_case_cannot_replace_missing_baseline_latency_sample(self) -> None:
        baseline = baseline_matrix(
            [
                benchmark_case("fast", 100.0),
                benchmark_case("slow", 900.0),
            ]
        )
        candidate_slow = benchmark_case("slow", 1.0)
        candidate_slow["answerLatencyMs"] = None
        candidate = candidate_matrix(
            [
                benchmark_case("fast", 90.0),
                candidate_slow,
                benchmark_case("new-fast", 1.0),
            ]
        )

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertFalse(report["latencyGate"]["sampleCountPreserved"])
        self.assertEqual(
            report["latencyGate"]["missingPairedSamples"],
            ["synthetic_latency_suite/slow"],
        )
        self.assertFalse(report["passed"])

    def test_added_cases_do_not_change_paired_latency_percentiles(self) -> None:
        baseline = baseline_matrix([benchmark_case("paired", 100.0)])
        candidate = candidate_matrix(
            [
                benchmark_case("paired", 105.0),
                benchmark_case("new-slow", 100_000.0),
            ]
        )

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertEqual(report["latencyGate"]["baseline"]["p50"], 100.0)
        self.assertEqual(report["latencyGate"]["candidate"]["p50"], 105.0)
        self.assertTrue(report["latencyGate"]["passed"])

    def test_retrieval_rank_metric_regression_fails_even_when_cases_pass(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 90.0)])
        baseline["summary"]["rankMetrics"] = {
            "documents": {"caseCount": 1, "mrr": 1.0, "hit@1": 1.0}
        }
        candidate["summary"]["rankMetrics"] = {
            "documents": {"caseCount": 1, "mrr": 0.5, "hit@1": 0.0}
        }

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertFalse(report["retrievalQualityGate"]["passed"])
        self.assertEqual(
            report["retrievalQualityGate"]["regressedMetrics"],
            ["documents.hit@1", "documents.mrr"],
        )
        self.assertFalse(report["passed"])

    def test_retrieval_rank_metric_improvement_passes(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 100.0)])
        baseline["summary"]["rankMetrics"] = {
            "documents": {"caseCount": 1, "mrr": 0.5, "hit@1": 0.0}
        }
        candidate["summary"]["rankMetrics"] = {
            "documents": {"caseCount": 1, "mrr": 1.0, "hit@1": 1.0}
        }

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertTrue(report["retrievalQualityGate"]["passed"])
        self.assertTrue(report["passed"])

    def test_added_case_cannot_replace_missing_paired_rank_metrics(self) -> None:
        baseline_case = benchmark_case("baseline-labelled", 100.0)
        baseline_case["rankMetrics"] = {
            "documents": {"mrr": 1.0, "hit@1": True, "hit@3": True, "hit@5": True, "hit@10": True}
        }
        baseline = baseline_matrix([baseline_case])

        candidate_baseline_case = benchmark_case("baseline-labelled", 90.0)
        added_case = benchmark_case("added-replacement", 10.0)
        added_case["rankMetrics"] = {
            "documents": {"mrr": 1.0, "hit@1": True, "hit@3": True, "hit@5": True, "hit@10": True}
        }
        candidate = candidate_matrix([candidate_baseline_case, added_case])

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertFalse(report["retrievalQualityGate"]["passed"])
        self.assertEqual(
            report["retrievalQualityGate"]["missingPairedCases"],
            ["synthetic_latency_suite/baseline-labelled:documents"],
        )
        self.assertFalse(report["passed"])
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            comparison.print_comparison_report(baseline, candidate, report)
        self.assertIn(
            "synthetic_latency_suite/baseline-labelled:documents",
            output.getvalue(),
        )

    def test_missing_benchmark_integrity_fails_closed(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 100.0)])
        del candidate["benchmarkIntegrity"]

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertFalse(report["benchmarkIntegrityGate"]["passed"])
        self.assertIn(
            "candidate.benchmarkIntegrity",
            report["benchmarkIntegrityGate"]["invalidCandidate"],
        )
        self.assertFalse(report["passed"])

    def test_changed_case_definition_under_same_ids_fails_integrity_gate(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 100.0)])
        candidate_case = candidate["suites"][0]["cases"][0]
        candidate_case["question"] = "Edited question under the same identifier"
        candidate_case["caseDefinitionSha256"] = benchmark.canonical_json_sha256(
            {"caseId": "case-1", "question": candidate_case["question"]}
        )
        candidate["suites"][0]["integrity"]["suiteDefinitionSha256"] = (
            benchmark.canonical_json_sha256(
                [candidate_case["caseDefinitionSha256"]]
            )
        )
        candidate["benchmarkIntegrity"] = benchmark.build_matrix_integrity(
            candidate["suites"],
            query_top_k=8,
            skip_upload=True,
            canonicalize_reused_library=False,
        )

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertFalse(report["benchmarkIntegrityGate"]["passed"])
        self.assertIn(
            "case:synthetic_latency_suite/case-1.caseDefinitionSha256",
            report["benchmarkIntegrityGate"]["mismatches"],
        )
        self.assertFalse(report["passed"])

    def test_changed_corpus_bytes_under_same_ids_fails_integrity_gate(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 100.0)])
        candidate["suites"][0]["integrity"]["corpusSha256"] = (
            benchmark.canonical_json_sha256("changed-synthetic-corpus")
        )
        candidate["benchmarkIntegrity"] = benchmark.build_matrix_integrity(
            candidate["suites"],
            query_top_k=8,
            skip_upload=True,
            canonicalize_reused_library=False,
        )

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertFalse(report["benchmarkIntegrityGate"]["passed"])
        self.assertIn(
            "suite:synthetic_latency_suite.corpusSha256",
            report["benchmarkIntegrityGate"]["mismatches"],
        )
        self.assertFalse(report["passed"])

    def test_runtime_artifact_digests_are_required_and_must_be_distinct(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        missing_digest = candidate_matrix([benchmark_case("case-1", 100.0)])
        missing_digest["runtimeIdentity"]["artifactDigest"] = ""

        missing_report = comparison.build_comparison_report(baseline, missing_digest)

        self.assertIn(
            "candidate.runtimeIdentity.artifactDigest",
            missing_report["benchmarkIntegrityGate"]["invalidCandidate"],
        )
        same_digest = candidate_matrix([benchmark_case("case-1", 100.0)])
        same_digest["runtimeIdentity"]["artifactDigest"] = "sha256:" + "a" * 64

        same_report = comparison.build_comparison_report(baseline, same_digest)

        self.assertIn(
            "runtime.artifactDigest:notDistinct",
            same_report["benchmarkIntegrityGate"]["mismatches"],
        )
        self.assertFalse(same_report["passed"])

    def test_comparator_recomputes_pre_mid_post_host_eligibility(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 100.0)])
        candidate_protocol = candidate["measurementProtocol"]
        candidate_protocol["hostMidflight"]["SwapFreeKiB"] = 0
        # These forged claims must not override recomputation from raw samples.
        candidate_protocol["releaseEvidenceEligible"] = True

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertIn(
            "candidate.measurementProtocol.hostMidflight:ineligible",
            report["benchmarkIntegrityGate"]["invalidCandidate"],
        )
        self.assertIn(
            "candidate.measurementProtocol.hostEligibility:stale",
            report["benchmarkIntegrityGate"]["invalidCandidate"],
        )
        self.assertFalse(report["passed"])

    def test_pre_mid_post_snapshots_must_be_independently_ordered(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 100.0)])
        protocol = candidate["measurementProtocol"]
        protocol["hostMidflight"]["capturedAt"] = protocol["hostPreflight"][
            "capturedAt"
        ]

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertIn(
            "candidate.measurementProtocol.hostSnapshotSequence",
            report["benchmarkIntegrityGate"]["invalidCandidate"],
        )
        self.assertFalse(report["passed"])

    def test_comparator_requires_compatible_environment_identity(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 100.0)])
        identity = deepcopy(candidate["measurementProtocol"]["environmentIdentity"])
        identity["hostIdSha256"] = "3" * 64
        definition = {
            key: value for key, value in identity.items() if key != "fingerprintSha256"
        }
        identity["fingerprintSha256"] = benchmark.canonical_json_sha256(definition)
        candidate["measurementProtocol"]["environmentIdentity"] = identity

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertIn(
            "environmentIdentity.fingerprintSha256",
            report["benchmarkIntegrityGate"]["mismatches"],
        )
        self.assertFalse(report["passed"])

    def test_comparator_requires_compatible_runtime_role_and_environment(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 100.0)])
        candidate["runtimeIdentity"]["versionEndpoint"]["environment"] = "other"

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertIn(
            "runtime.versionEndpoint.environment",
            report["benchmarkIntegrityGate"]["mismatches"],
        )
        self.assertFalse(report["passed"])

    def test_unconstrained_cache_policy_is_rejected(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 100.0)])
        candidate["benchmarkIntegrity"]["cachePolicy"] = "unspecified"
        candidate["measurementProtocol"]["cachePolicy"] = "unspecified"

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertIn(
            "candidate.benchmarkIntegrity.cachePolicy",
            report["benchmarkIntegrityGate"]["invalidCandidate"],
        )
        self.assertFalse(report["passed"])

    def test_invalid_baseline_does_not_skip_candidate_validation(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 100.0)])
        baseline["benchmarkIntegrity"]["cachePolicy"] = "unspecified"
        candidate["runtimeIdentity"]["artifactDigest"] = ""

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertIn(
            "baseline.benchmarkIntegrity.cachePolicy",
            report["benchmarkIntegrityGate"]["invalidBaseline"],
        )
        self.assertIn(
            "candidate.runtimeIdentity.artifactDigest",
            report["benchmarkIntegrityGate"]["invalidCandidate"],
        )
        self.assertFalse(report["passed"])

    def test_claimed_case_order_must_match_round_permutation(self) -> None:
        cases = [benchmark_case(f"case-{index}", 100.0) for index in range(8)]
        baseline = baseline_matrix(cases)
        candidate = candidate_matrix(cases)
        candidate_suite = candidate["suites"][0]
        candidate_suite["cases"] = list(reversed(candidate_suite["cases"]))
        candidate_suite["caseOrder"] = [
            case["caseId"] for case in candidate_suite["cases"]
        ]
        candidate["benchmarkIntegrity"] = benchmark.build_matrix_integrity(
            candidate["suites"],
            query_top_k=8,
            skip_upload=True,
            canonicalize_reused_library=False,
            cache_policy="cold",
            round_id="round-1",
        )

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertIn(
            "candidate.suite:synthetic_latency_suite.caseOrder:policyMismatch",
            report["benchmarkIntegrityGate"]["invalidCandidate"],
        )
        self.assertFalse(report["passed"])

    def test_release_host_policy_cannot_be_weakened_in_both_artifacts(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 100.0)])
        for matrix in (baseline, candidate):
            protocol = matrix["measurementProtocol"]
            protocol["hostEligibilityPolicy"]["maxSwapUsedPercent"] = 100.0
            protocol["hostEligibility"] = {
                phase: benchmark.evaluate_host_snapshot(
                    protocol[field],
                    protocol["hostEligibilityPolicy"],
                )
                for phase, field in (
                    ("pre", "hostPreflight"),
                    ("mid", "hostMidflight"),
                    ("post", "hostPostflight"),
                )
            }

        report = comparison.build_comparison_report(baseline, candidate)

        self.assertIn(
            "baseline.measurementProtocol.hostEligibilityPolicy.maxSwapUsedPercent:weakened",
            report["benchmarkIntegrityGate"]["invalidBaseline"],
        )
        self.assertFalse(report["passed"])

    def test_cli_returns_nonzero_for_latency_regression(self) -> None:
        baseline = baseline_matrix([benchmark_case("case-1", 100.0)])
        candidate = candidate_matrix([benchmark_case("case-1", 111.0)])

        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            baseline_dir = root / "baseline"
            candidate_dir = root / "candidate"
            baseline_dir.mkdir()
            candidate_dir.mkdir()
            (baseline_dir / "matrix.result.json").write_text(json.dumps(baseline))
            (candidate_dir / "matrix.result.json").write_text(json.dumps(candidate))

            with contextlib.redirect_stdout(io.StringIO()):
                exit_code = comparison.main([str(baseline_dir), str(candidate_dir)])

        self.assertEqual(exit_code, 1)


if __name__ == "__main__":
    unittest.main()
