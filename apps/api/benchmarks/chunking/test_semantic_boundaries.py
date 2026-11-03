"""Model-free boundary and input-identity checks; no inference or downloads."""

import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from semantic_boundaries import choose_breaks, load_input


class SemanticBoundaryTests(unittest.TestCase):
    def test_cosine_outlier_creates_a_canonical_boundary(self):
        self.assertEqual(choose_breaks([0, 2, 5, 8], [0.1, 0.8, 0.2], 0.9), [5])

    def test_bad_envelope_and_nonfinite_distances_are_refused(self):
        for distances in ([], [float("nan")], [float("inf")], [0.1, 0.2]):
            with self.assertRaises(ValueError):
                choose_breaks([0, 1], distances, 0.9)

    def test_literal_source_identity_is_required_before_encoding(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "canonical.json"
            text = "Conditions: quantity > 0; currency defaults to USD."
            data = {
                "protocol": "irg256-native-treatment-input/1",
                "cases": [
                    {
                        "caseId": "synthetic",
                        "normalizedText": text,
                        "normalizedSha256": hashlib.sha256(text.encode()).hexdigest(),
                        "blocks": [{"ordinal": 0}, {"ordinal": 1}],
                    }
                ],
            }
            path.write_text(json.dumps(data))
            pin = hashlib.sha256(path.read_bytes()).hexdigest()
            self.assertEqual(load_input(path, pin), data)
            path.write_text(json.dumps(data).replace("USD", "EUR"))
            with self.assertRaises(ValueError):
                load_input(path, pin)


if __name__ == "__main__":
    unittest.main()
