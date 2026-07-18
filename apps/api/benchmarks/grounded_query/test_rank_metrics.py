#!/usr/bin/env python3

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parent))

import run_live_benchmark as benchmark  # noqa: E402


class RankMetricTests(unittest.TestCase):
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

    def test_timed_answer_runs_before_auxiliary_rank_search(self) -> None:
        events: list[str] = []

        class FakeClient:
            def post_json(self, path: str, payload: dict) -> dict:
                del payload
                events.append(f"post:{path}")
                return {
                    "responseTurn": {"contentText": "A grounded answer."},
                    "execution": {"id": "execution-1", "executionState": "completed"},
                }

            def get_json(self, path: str, **kwargs: object) -> dict:
                del kwargs
                events.append(f"get:{path}")
                if path == "/query/executions/execution-1":
                    return {
                        "verificationState": "verified",
                        "verificationWarnings": [],
                        "chunkReferences": [],
                        "preparedSegmentReferences": [],
                        "technicalFactReferences": [],
                        "entityReferences": [],
                        "relationReferences": [],
                    }
                return {"documentHits": []}

        case = benchmark.BenchmarkCase(
            case_id="order",
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

        benchmark.run_case(FakeClient(), "library-1", "session-1", case, 8)

        self.assertEqual(
            events,
            [
                "post:/query/sessions/session-1/turns",
                "get:/query/executions/execution-1",
                "get:/knowledge/libraries/library-1/search/documents",
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
                def get_json(self, path: str) -> list[dict]:
                    self.path = path
                    return [
                        {
                            "document_id": "document-1",
                            "file_name": "first.md",
                            "document_role": "primary",
                            "readable_revision_id": "revision-1",
                            "deleted_at": None,
                        },
                        {
                            "document_id": "document-2",
                            "file_name": "second.md",
                            "document_role": "primary",
                            "readable_revision_id": "revision-2",
                            "deleted_at": None,
                        },
                    ]

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


if __name__ == "__main__":
    unittest.main()
