"""Boundary tests that require neither model weights nor hosted providers."""

import json
from pathlib import Path
import tempfile
import unittest

from run_local_comparison import (
    ModelForwardFailure,
    digest,
    finite_scores,
    load_inputs,
    preserve_forward_failure,
    write_new,
)


class LocalComparisonTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "input.json"
        self.data = {
            "protocol": "semantic-rerank-comparison-input/1",
            "inputSha256": "1" * 64,
            "queries": [
                {
                    "id": "q",
                    "query": "Что такое граф?",
                    "candidates": [
                        {"id": "b", "index": 0, "text": "graph\n"},
                        {"id": "a", "index": 1, "text": "a graph"},
                    ],
                }
            ],
        }

    def save(self):
        self.path.write_text(json.dumps(self.data))
        return digest(self.path)

    def test_exact_native_text_and_candidate_order_are_preserved(self):
        result = load_inputs(self.path, self.save())
        self.assertEqual(result["queries"][0]["candidates"][0]["text"], "graph\n")
        self.assertEqual(
            [
                row["id"]
                for row in finite_scores(result["queries"][0]["candidates"], [0.5, 0.5])
            ],
            ["b", "a"],
        )

    def test_mutated_input_is_not_the_admitted_export(self):
        expected = self.save()
        self.path.write_text(self.path.read_text() + " ")
        with self.assertRaisesRegex(ValueError, "identity mismatch"):
            load_inputs(self.path, expected)

    def test_duplicate_or_noncontiguous_candidates_are_rejected(self):
        rows = self.data["queries"][0]["candidates"]
        rows[1]["id"] = "b"
        with self.assertRaisesRegex(ValueError, "duplicate"):
            load_inputs(self.path, self.save())
        rows[1]["id"] = "a"
        rows[1]["index"] = 4
        with self.assertRaisesRegex(ValueError, "contiguous"):
            load_inputs(self.path, self.save())

    def test_nonfinite_or_partial_scores_fail_instead_of_becoming_a_ranking(self):
        candidates = self.data["queries"][0]["candidates"]
        for values in ([0.2], [0.2, float("nan")], [float("inf"), 0.3]):
            with self.assertRaises(ValueError):
                finite_scores(candidates, values)

    def test_first_result_cannot_be_overwritten(self):
        write_new(self.path, {"first": "failure"})
        with self.assertRaises(FileExistsError):
            write_new(self.path, {"second": "success"})
        self.assertEqual(json.loads(self.path.read_text()), {"first": "failure"})

    def test_first_forward_error_escapes_runtimeerror_batch_backoff_unchanged(self):
        original_error = RuntimeError("first CPU allocation failure")

        class FailingModel:
            def forward(self, **kwargs):
                raise original_error

        model = FailingModel()
        preserve_forward_failure(model)
        try:
            model.forward(input_ids=[1])
        except RuntimeError:
            self.fail("dependency RuntimeError backoff could swallow the first cause")
        except ModelForwardFailure as error:
            self.assertIs(error.__cause__, original_error)
        else:
            self.fail("failed forward was not propagated")

    def test_successful_forward_keeps_its_original_result_and_arguments(self):
        class Model:
            def forward(self, value, *, scale):
                return value * scale

        model = Model()
        preserve_forward_failure(model)
        self.assertEqual(model.forward(3, scale=4), 12)


if __name__ == "__main__":
    unittest.main()
