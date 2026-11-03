"""Coverage regressions for intervals, conjunctions and table parent anchors."""

import unittest

from score_evidence import (
    covers,
    direct_regions,
    fact_available,
    require_query_population,
)


class CoreEvidenceTests(unittest.TestCase):
    def test_missing_extra_and_empty_oracle_populations_are_refused(self):
        events = [
            {"event": "run"},
            {"event": "call", "queryId": "a"},
            {"event": "call", "queryId": "b"},
            {"event": "complete", "queries": 2},
        ]
        require_query_population({"a": {}, "b": {}}, events)
        for population in ({}, {"a": {}}, {"a": {}, "b": {}, "extra": {}}):
            with self.assertRaises(ValueError):
                require_query_population(population, events)

    def test_contiguous_union_covers_but_a_gap_does_not(self):
        self.assertTrue(covers([(0, 4), (4, 10)], 2, 8))
        self.assertFalse(covers([(0, 4), (5, 10)], 2, 8))
        self.assertFalse(covers([], 0, 1))

    def test_fact_requires_all_conditions_of_one_complete_alternative(self):
        fact = {
            "alternatives": [
                [{"start": 0, "end": 4}, {"start": 8, "end": 12}],
                [{"start": 20, "end": 30}],
            ]
        }
        self.assertFalse(fact_available(fact, [(0, 4), (8, 11)]))
        self.assertTrue(fact_available(fact, [(20, 30)]))

    def test_table_parent_id_does_not_expose_all_other_table_rows(self):
        treatment = {
            "blocks": [
                {
                    "blockId": "parent",
                    "blockKind": "table",
                    "normalizedText": "all rows",
                    "sourceSpan": {"startOffset": 0, "endOffset": 100},
                },
                {
                    "blockId": "row",
                    "blockKind": "table_row",
                    "normalizedText": "currency: USD",
                    "sourceSpan": {"startOffset": 10, "endOffset": 20},
                },
            ]
        }
        chunk = {
            "chunkKind": "table_row",
            "normalizedText": "currency: USD",
            "supportBlockIds": ["parent", "row"],
        }
        self.assertEqual(direct_regions(treatment, chunk), [(10, 20)])

    def test_a_support_reference_without_its_rendered_text_is_not_evidence(self):
        treatment = {
            "blocks": [
                {
                    "blockId": "one",
                    "blockKind": "paragraph",
                    "normalizedText": "The condition is required.",
                    "sourceSpan": {"startOffset": 0, "endOffset": 26},
                }
            ]
        }
        chunk = {
            "chunkKind": "paragraph",
            "normalizedText": "A different statement.",
            "supportBlockIds": ["one"],
        }
        self.assertEqual(direct_regions(treatment, chunk), [])


if __name__ == "__main__":
    unittest.main()
