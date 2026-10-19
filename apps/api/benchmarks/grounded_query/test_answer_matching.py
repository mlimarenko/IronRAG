"""Contract tests for how benchmark assertions match an answer.

A matcher defect is indistinguishable from a product defect in the score, so
these pin the two behaviours that decide whether a case passes: forbidden
terms must not fire inside a longer word, and they must still fire when the
term really is present.
"""

import unittest

from run_live_benchmark import contains_any, contains_any_whole_word


class ForbiddenTermMatching(unittest.TestCase):
    def test_forbidden_term_does_not_fire_inside_a_longer_word(self) -> None:
        answer = "This mutation is restricted to administrators."
        self.assertFalse(contains_any_whole_word(answer, ["REST"]))
        # Plain containment is what used to fail this answer.
        self.assertTrue(contains_any(answer, ["REST"]))

    def test_forbidden_term_still_fires_when_present(self) -> None:
        for answer in (
            "The service exposes a REST API.",
            "Compared to REST, GraphQL sends one request.",
            "Built with Django ORM.",
        ):
            with self.subTest(answer=answer):
                self.assertTrue(contains_any_whole_word(answer, ["REST", "Django"]))

    def test_needles_carrying_punctuation_keep_matching(self) -> None:
        self.assertTrue(contains_any_whole_word("exposes /health and /ready", ["/health"]))
        self.assertTrue(
            contains_any_whole_word("sends X-RateLimit-Limit: 1000", ["X-RateLimit"])
        )

    def test_prefix_collision_does_not_fire(self) -> None:
        self.assertFalse(contains_any_whole_word("An expressive schema", ["Express"]))

    def test_empty_needle_list_never_fires(self) -> None:
        self.assertFalse(contains_any_whole_word("any answer", []))
        self.assertFalse(contains_any_whole_word("any answer", [""]))


if __name__ == "__main__":
    unittest.main()
