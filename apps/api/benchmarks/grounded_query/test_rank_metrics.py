#!/usr/bin/env python3

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parent))

import run_live_benchmark as benchmark  # noqa: E402
from apps.api.benchmarks.benchmark_test_support import canonical_execution_detail  # noqa: E402


GOLDEN_SUITE_NAMES = (
    "golden_api_baseline_suite.json",
    "golden_code_suite.json",
    "golden_graph_multihop_suite.json",
    "golden_hardening_suite.json",
    "golden_infrastructure_suite.json",
    "golden_layout_noise_suite.json",
    "golden_multiformat_suite.json",
    "golden_programming_suite.json",
    "golden_protocols_suite.json",
    "golden_technical_contract_suite.json",
    "golden_upload_surface_suite.json",
    "golden_workflow_strict_suite.json",
)

RANK_RELEVANCE_GOLDEN_SUITE_NAMES = (
    "golden_api_baseline_suite.json",
    "golden_graph_multihop_suite.json",
)


def neutral_benchmark_case(case_id: str) -> benchmark.BenchmarkCase:
    """Minimal self-contained case with no expectations beyond verification."""
    return benchmark.BenchmarkCase(
        case_id=case_id,
        question="Neutral question?",
        search_query="neutral",
        expected_documents_contains=[],
        search_required_all=[],
        answer_required_all=[],
        answer_required_any=[],
        answer_opening_required_any=[],
        answer_forbidden_any=[],
        min_chunk_reference_count=0,
        min_prepared_segment_reference_count=0,
        min_technical_fact_reference_count=0,
        min_entity_reference_count=0,
        min_relation_reference_count=0,
        expected_entity_reference_labels_contains=[],
        expected_relation_reference_text_contains=[],
        allowed_verification_states=["verified"],
        relevant_documents=[],
        relevant_chunks=[],
    )


class RankMetricTests(unittest.TestCase):
    def test_rank_relevance_registry_matches_and_hydrates_golden_suites(self) -> None:
        suite_directory = Path(__file__).resolve().parent
        registry = benchmark.load_rank_relevance(
            suite_directory / "rank_relevance.json"
        )
        expected_suites: dict[str, list[benchmark.BenchmarkCase]] = {}

        for suite_name in RANK_RELEVANCE_GOLDEN_SUITE_NAMES:
            _, cases, payload = benchmark.load_suite(suite_directory / suite_name)
            suite_id = payload.get("suiteId")
            self.assertIsInstance(suite_id, str)
            assert isinstance(suite_id, str)
            self.assertNotIn(suite_id, expected_suites)
            expected_suites[suite_id] = cases

        self.assertEqual(set(registry), set(expected_suites))

        for suite_id, loaded_cases in expected_suites.items():
            with self.subTest(suite=suite_id):
                suite_relevance = registry[suite_id]
                self.assertIsInstance(suite_relevance, dict)
                case_relevance = suite_relevance.get("cases")
                self.assertIsInstance(case_relevance, dict)
                assert isinstance(case_relevance, dict)
                self.assertEqual(
                    set(case_relevance),
                    {case.case_id for case in loaded_cases},
                )

                for case in loaded_cases:
                    with self.subTest(suite=suite_id, case=case.case_id):
                        expected_relevance = case_relevance[case.case_id]
                        self.assertIsInstance(expected_relevance, dict)
                        assert isinstance(expected_relevance, dict)
                        self.assertEqual(
                            benchmark.suite_case_rank_relevance(
                                registry,
                                suite_id,
                                case.case_id,
                            ),
                            expected_relevance,
                        )

                        expected_chunks = expected_relevance.get("relevantChunks")
                        self.assertIsInstance(expected_chunks, list)
                        self.assertTrue(expected_chunks)
                        self.assertEqual(case.relevant_chunks, expected_chunks)

                        if "relevantDocuments" in expected_relevance:
                            expected_documents = expected_relevance[
                                "relevantDocuments"
                            ]
                            self.assertIsInstance(expected_documents, list)
                            self.assertTrue(expected_documents)
                            self.assertEqual(
                                case.relevant_documents,
                                expected_documents,
                            )

    def test_golden_suites_allow_only_successful_verification_states(self) -> None:
        suite_directory = Path(__file__).resolve().parent

        for suite_name in GOLDEN_SUITE_NAMES:
            with self.subTest(suite=suite_name):
                _, cases, _ = benchmark.load_suite(suite_directory / suite_name)
                self.assertTrue(cases)
                for case in cases:
                    # Every registry suite may only accept SUCCESSFUL
                    # verification outcomes; individual suites may be
                    # stricter (verified-only) but never looser.
                    self.assertTrue(case.allowed_verification_states)
                    self.assertLessEqual(
                        set(case.allowed_verification_states),
                        {"verified", "partially_supported"},
                    )

    def test_golden_answer_required_any_has_no_weak_standalone_tokens(self) -> None:
        suite_directory = Path(__file__).resolve().parent

        for suite_name in GOLDEN_SUITE_NAMES:
            _, cases, _ = benchmark.load_suite(suite_directory / suite_name)
            for case in cases:
                for token in case.answer_required_any:
                    with self.subTest(suite=suite_name, case=case.case_id, token=token):
                        self.assertGreaterEqual(len(token), 3)
                        self.assertFalse(token.isdecimal())

    def test_pending_transition_forbidden_patterns_are_relation_scoped(self) -> None:
        suite_directory = Path(__file__).resolve().parent
        _, cases, _ = benchmark.load_suite(suite_directory / "golden_code_suite.json")
        case = next(case for case in cases if case.case_id == "rust_pending_transitions")
        correct_answer = (
            "Pending transitions to PaymentProcessing or Cancelled. "
            "Shipped, Delivered, and Refunded occur later and are not direct "
            "transitions from Pending."
        )

        self.assertFalse(benchmark.contains_any(correct_answer, case.answer_forbidden_any))
        self.assertFalse(
            benchmark.matches_any_regex(correct_answer, case.answer_forbidden_regex_any)
        )
        for wrong_answer in (
            "Pending transitions directly to Shipped.",
            "Pending -> Delivered.",
            "Valid transitions from Pending include PaymentProcessing and Refunded.",
            "Refunded is a valid transition from Pending.",
        ):
            with self.subTest(answer=wrong_answer):
                self.assertTrue(
                    benchmark.matches_any_regex(
                        wrong_answer,
                        case.answer_forbidden_regex_any,
                    )
                )

    def test_eks_scaling_assertion_rejects_instance_types_without_limits(self) -> None:
        suite_directory = Path(__file__).resolve().parent
        _, cases, _ = benchmark.load_suite(
            suite_directory / "golden_programming_suite.json"
        )
        case = next(case for case in cases if case.case_id == "terraform_eks_node_groups")
        incomplete_answer = (
            "The application node group uses m6i.xlarge and the system node group "
            "uses t3.large."
        )
        correct_answer = f"{incomplete_answer} The application group scales up to 10 nodes."

        self.assertTrue(benchmark.contains_all(incomplete_answer, case.answer_required_all))
        self.assertFalse(benchmark.contains_any(incomplete_answer, case.answer_required_any))
        self.assertTrue(benchmark.contains_any(correct_answer, case.answer_required_any))

    def test_enterprise_discount_requires_the_percentage_in_context(self) -> None:
        suite_directory = Path(__file__).resolve().parent
        _, cases, _ = benchmark.load_suite(
            suite_directory / "golden_multiformat_suite.json"
        )
        case = next(
            case for case in cases if case.case_id == "xlsx_enterprise_renewal_policy"
        )

        self.assertFalse(
            benchmark.matches_any_regex(
                "Enterprise renewals require Finance Lead approval at 10 percent.",
                case.answer_required_regex_any,
            )
        )
        self.assertTrue(
            benchmark.matches_any_regex(
                "Enterprise renewals receive a 12% discount with Finance Lead approval.",
                case.answer_required_regex_any,
            )
        )

    def test_cross_domain_comparison_accepts_any_answering_document_at_rank_one(self) -> None:
        suite_directory = Path(__file__).resolve().parent
        _, cases, _ = benchmark.load_suite(
            suite_directory / "golden_programming_suite.json"
        )
        case = next(case for case in cases if case.case_id == "cross_domain_auth_comparison")

        self.assertEqual(
            set(case.expected_documents_contains),
            {
                "sample_rust_http_server",
                "sample_docker_compose",
                "sample_api_specification",
            },
        )

    def test_case_order_is_deterministic_for_a_round_and_permuted_between_rounds(self) -> None:
        cases = [self._case(f"case-{index}") for index in range(12)]

        first = benchmark.permute_cases_for_round(cases, "round-a", "suite-a")
        repeated = benchmark.permute_cases_for_round(cases, "round-a", "suite-a")
        next_round = benchmark.permute_cases_for_round(cases, "round-b", "suite-a")

        self.assertEqual(
            [case.case_id for case in first],
            [case.case_id for case in repeated],
        )
        self.assertNotEqual(
            [case.case_id for case in first],
            [case.case_id for case in next_round],
        )
        self.assertEqual(
            {case.case_id for case in first},
            {case.case_id for case in cases},
        )

    @staticmethod
    def _case(case_id: str) -> benchmark.BenchmarkCase:
        return benchmark.BenchmarkCase(
            case_id=case_id,
            question=f"Neutral question {case_id}?",
            search_query="neutral",
            expected_documents_contains=[],
            search_required_all=[],
            answer_required_all=[],
            answer_required_any=[],
            answer_opening_required_any=[],
            answer_forbidden_any=[],
            min_chunk_reference_count=0,
            min_prepared_segment_reference_count=0,
            min_technical_fact_reference_count=0,
            min_entity_reference_count=0,
            min_relation_reference_count=0,
            expected_entity_reference_labels_contains=[],
            expected_relation_reference_text_contains=[],
            allowed_verification_states=["verified"],
            relevant_documents=[],
            relevant_chunks=[],
        )

    def test_compute_rank_metrics_reports_hits_and_mrr(self) -> None:
        metrics = benchmark.compute_rank_metrics(["alpha", "beta", "gamma"], {"gamma"})

        self.assertIsNotNone(metrics)
        assert metrics is not None
        self.assertFalse(metrics["hit@1"])
        self.assertTrue(metrics["hit@3"])
        self.assertEqual(metrics["firstRelevantRank"], 3)
        self.assertEqual(metrics["mrr"], 0.333333)

    def test_compute_rank_metrics_omits_absent_relevance(self) -> None:
        self.assertIsNone(benchmark.compute_rank_metrics(["alpha"], set()))

    def test_compute_marker_rank_metrics_matches_chunk_content(self) -> None:
        metrics = benchmark.compute_marker_rank_metrics(
            [
                "unrelated preface",
                "The selected path is Bridge Gamma Route.",
            ],
            ["Bridge Gamma Route"],
        )

        self.assertIsNotNone(metrics)
        assert metrics is not None
        self.assertFalse(metrics["hit@1"])
        self.assertTrue(metrics["hit@3"])
        self.assertEqual(metrics["firstRelevantRank"], 2)
        self.assertEqual(metrics["mrr"], 0.5)

    def test_document_keys_normalize_file_names(self) -> None:
        self.assertEqual(
            benchmark.normalize_relevant_keys(["corpus/docs/alpha_suite.md"]),
            {"alpha_suite"},
        )

    def test_search_summary_rejects_noncanonical_wire_aliases(self) -> None:
        payload = {
            "documentHits": [
                {
                    "document": {
                        "document_id": "document-1",
                        "fileName": "source.md",
                        "externalKey": "source",
                    },
                    "chunkHits": [],
                    "vectorChunkHits": [],
                    "score": 1.0,
                }
            ]
        }
        with self.assertRaisesRegex(ValueError, "canonical documentId"):
            benchmark.summarize_search_hits(payload)

    def test_turn_execution_rejects_noncanonical_execution_id_alias(self) -> None:
        class AliasClient:
            def post_json(self, path: str, _payload: dict) -> dict:
                if path.endswith("/search"):
                    return {"documentHits": []}
                payload = canonical_execution_detail(question="Neutral question?")
                payload["execution"]["executionId"] = payload["execution"].pop("id")
                return payload

        client = AliasClient()
        case = neutral_benchmark_case("alias")
        with self.assertRaisesRegex(ValueError, "canonical execution.id"):
            benchmark.run_case(
                client=client,
                workspace_id="00000000-0000-4000-8000-000000000007",
                library_id="00000000-0000-4000-8000-000000000008",
                session_id="session-1",
                case=case,
                query_top_k=8,
            )

    def test_turn_execution_rejects_noncanonical_execution_state_alias(self) -> None:
        class AliasClient:
            def post_json(self, path: str, _payload: dict) -> dict:
                payload = canonical_execution_detail(question="Neutral question?")
                payload["execution"]["execution_state"] = payload["execution"].pop(
                    "lifecycleState"
                )
                return payload

        client = AliasClient()
        case = neutral_benchmark_case("alias")
        with self.assertRaisesRegex(ValueError, "canonical execution.lifecycleState"):
            benchmark.run_case(
                client=client,
                workspace_id="00000000-0000-4000-8000-000000000007",
                library_id="00000000-0000-4000-8000-000000000008",
                session_id="session-1",
                case=case,
                query_top_k=8,
            )

    def test_relation_reference_rejects_noncanonical_normalized_assertion_alias(self) -> None:
        with self.assertRaisesRegex(ValueError, "canonical normalizedAssertion"):
            benchmark.relation_references_contain_all_text(
                [{"normalized_assertion": "A relates to B"}], ["relates"]
            )

    def test_timed_answer_runs_before_auxiliary_rank_search(self) -> None:
        events: list[str] = []

        class FakeClient:
            def post_json(self, path: str, payload: dict) -> dict:
                del payload
                events.append(f"post:{path}")
                if path.endswith("/search"):
                    return {"documentHits": []}
                return canonical_execution_detail(
                    "A grounded answer.", question="Neutral question?"
                )

            def get_json(self, path: str, **kwargs: object) -> dict:
                del kwargs
                events.append(f"get:{path}")
                if path.startswith("/query/executions/"):
                    return canonical_execution_detail(
                        "A grounded answer.", question="Neutral question?"
                    )
                return {"documentHits": []}

        case = neutral_benchmark_case("order")

        benchmark.run_case(
            client=FakeClient(),
            workspace_id="00000000-0000-4000-8000-000000000007",
            library_id="00000000-0000-4000-8000-000000000008",
            session_id="session-1",
            case=case,
            query_top_k=8,
        )

        self.assertEqual(
            events,
            [
                "post:/query/sessions/session-1/turns",
                "get:/query/executions/00000000-0000-4000-8000-000000000001",
                "post:/knowledge/libraries/00000000-0000-4000-8000-000000000008/search",
            ],
        )

    def test_server_corpus_attestation_requires_exact_primary_source_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            first = root / "first.md"
            second = root / "second.md"
            first.write_bytes(b"first fixture\n")
            second.write_bytes(b"second fixture\n")

            class FakeClient:
                def get_json(self, path: str) -> dict:
                    self.path = path
                    return {
                        "items": [
                            {
                                "id": "document-1",
                                "fileName": "first.md",
                                "documentState": "active",
                                "status": "ready",
                            },
                            {
                                "id": "document-2",
                                "fileName": "second.md",
                                "documentState": "active",
                                "status": "ready",
                            },
                        ]
                    }

                def get_bytes(self, path: str) -> bytes:
                    return {
                        "/content/documents/document-1/source": b"first fixture\n",
                        "/content/documents/document-2/source": b"second fixture\n",
                    }[path]

            verified = benchmark.verify_server_corpus_bytes(
                FakeClient(),
                "library-1",
                [first, second],
            )

            self.assertEqual(verified[first], first.read_bytes())
            self.assertEqual(verified[second], second.read_bytes())

    def test_case_metadata_preserves_case_identity_and_expectations(self) -> None:
        case = benchmark.BenchmarkCase(
            case_id="case-1",
            question="What is the configured value?",
            search_query="configured value",
            expected_documents_contains=["guide"],
            search_required_all=["configured"],
            answer_required_all=["value"],
            answer_required_any=["exact"],
            answer_opening_required_any=["configured"],
            answer_forbidden_any=["unsupported"],
            min_chunk_reference_count=1,
            min_prepared_segment_reference_count=2,
            min_technical_fact_reference_count=3,
            min_entity_reference_count=4,
            min_relation_reference_count=5,
            expected_entity_reference_labels_contains=["setting"],
            expected_relation_reference_text_contains=["configured"],
            allowed_verification_states=["verified"],
            relevant_documents=["guide"],
            relevant_chunks=["configured value"],
        )

        metadata = benchmark.case_metadata(case)

        self.assertEqual(
            metadata,
            {
                "caseId": "case-1",
                "caseDefinitionSha256": benchmark.case_definition_sha256(case),
                "question": "What is the configured value?",
                "searchQuery": "configured value",
                "relevantDocuments": ["guide"],
                "relevantChunks": ["configured value"],
                "minChunkReferenceCount": 1,
                "minPreparedSegmentReferenceCount": 2,
                "minTechnicalFactReferenceCount": 3,
                "minEntityReferenceCount": 4,
                "minRelationReferenceCount": 5,
                "expectedEntityReferenceLabelsContains": ["setting"],
                "expectedRelationReferenceTextContains": ["configured"],
                "allowedVerificationStates": ["verified"],
            },
        )

    def test_failed_case_result_is_summary_compatible_and_strict_failing(self) -> None:
        case = neutral_benchmark_case("broken")

        record = benchmark.failed_case_result(case, RuntimeError("boom"))
        summary = benchmark.build_summary([record])

        self.assertFalse(record["strictCasePass"])
        self.assertEqual(record["failedChecks"], ["case_execution_error"])
        self.assertEqual(record["caseExecutionError"], "RuntimeError: boom")
        self.assertEqual(summary["totalCases"], 1)
        self.assertEqual(summary["strictCasePassCount"], 0)
        self.assertEqual(summary["failureReasonCounts"], {"case_execution_error": 1})


if __name__ == "__main__":
    unittest.main()
