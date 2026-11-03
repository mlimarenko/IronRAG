"""First-output parsing never repairs text or silently accepts incomplete claims."""

import json
import unittest

from propositions import parse_claims


class PropositionParsingTests(unittest.TestCase):
    def test_explicit_empty_output_is_distinct_from_malformed_output(self):
        self.assertEqual(parse_claims('{"claims":[]}', False)["state"], "empty")
        self.assertEqual(
            parse_claims('```json\n{"claims":[]}\n```', False)["state"], "invalid_json"
        )

    def test_token_capped_json_is_not_promoted_to_complete_candidates(self):
        self.assertEqual(parse_claims('{"claims":[]}', True)["state"], "output_cap")

    def test_conditions_and_units_are_not_normalized_or_inferred(self):
        claim = {
            "subject": "session",
            "assertion": "Session TTL is 24h unless explicitly changed.",
            "negation": "",
            "conditions": "unless explicitly changed",
            "time": "24h",
            "units": "hours",
            "scope": "Redis sessions",
        }
        result = parse_claims(json.dumps({"claims": [claim]}), False)
        self.assertEqual(result, {"state": "parsed", "claims": [claim]})

    def test_missing_identity_or_nonstring_field_is_refused(self):
        claim = {
            "subject": "",
            "assertion": "TTL is 24h.",
            "negation": "",
            "conditions": "",
            "time": "24h",
            "units": "hours",
            "scope": "sessions",
        }
        self.assertEqual(
            parse_claims(json.dumps({"claims": [claim]}), False)["state"],
            "missing_subject_or_assertion",
        )
        claim["subject"] = "session"
        claim["time"] = 24
        self.assertEqual(
            parse_claims(json.dumps({"claims": [claim]}), False)["state"],
            "invalid_claim_value",
        )


if __name__ == "__main__":
    unittest.main()
