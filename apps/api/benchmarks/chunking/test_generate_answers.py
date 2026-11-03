"""Model-free checks of paired questions and literal context transport."""

import hashlib
import unittest

from generate_answers import messages, require_prompt_budget, validate_rows


class AnswerInputTests(unittest.TestCase):
    def test_missing_arm_identity_is_refused_before_generation(self):
        row = {
            "id": "one",
            "queryId": "q",
            "query": "Which limit applies?",
            "contexts": [],
        }
        with self.assertRaises(ValueError):
            validate_rows({"protocol": "irg256-answer-input/1", "cases": [row]})

    def test_absent_retrieval_context_is_preserved_for_abstention(self):
        row = {
            "id": "one",
            "queryId": "q",
            "armId": "structural",
            "query": "Which limit applies?",
            "contexts": [],
        }
        self.assertEqual(
            validate_rows({"protocol": "irg256-answer-input/1", "cases": [row]}), [row]
        )
        self.assertIn(row["query"], messages(row["query"], [])[1]["content"])

    def test_conditions_are_not_rewritten_in_prompt(self):
        text = (
            "quantity > 0; currency defaults to 'USD' except when explicitly changed."
        )
        context = {
            "text": text,
            "textSha256": hashlib.sha256(text.encode()).hexdigest(),
        }
        row = {
            "id": "one",
            "queryId": "q",
            "armId": "structural",
            "query": "Which conditions?",
            "contexts": [context],
        }
        validate_rows({"protocol": "irg256-answer-input/1", "cases": [row]})
        self.assertIn(text, messages(row["query"], row["contexts"])[1]["content"])
        context["text"] = text.replace("USD", "EUR")
        with self.assertRaises(ValueError):
            validate_rows({"protocol": "irg256-answer-input/1", "cases": [row]})

    def test_different_questions_cannot_claim_a_paired_identity(self):
        rows = [
            {
                "id": "a",
                "queryId": "q",
                "armId": "structural",
                "query": "First?",
                "contexts": [],
            },
            {
                "id": "b",
                "queryId": "q",
                "armId": "recursive",
                "query": "Second?",
                "contexts": [],
            },
        ]
        with self.assertRaises(ValueError):
            validate_rows({"protocol": "irg256-answer-input/1", "cases": rows})

    def test_final_prompt_budget_also_applies_without_excerpts(self):
        require_prompt_budget(4096)
        for count in (0, 4097):
            with self.assertRaises(ValueError):
                require_prompt_budget(count)


if __name__ == "__main__":
    unittest.main()
