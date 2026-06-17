#!/usr/bin/env python3

from __future__ import annotations

import sys
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parent))

import run_live_benchmark as benchmark  # noqa: E402


class RankMetricTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
