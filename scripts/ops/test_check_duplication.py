from __future__ import annotations

from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from scripts.ops import check_duplication


def file_info(name: str, start: int, end: int) -> dict[str, object]:
    return {
        "name": name,
        "start": start,
        "end": end,
        "startLoc": {"line": start, "column": 0, "position": 0},
        "endLoc": {"line": end, "column": 0, "position": 0},
    }


class DuplicationGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = TemporaryDirectory()
        self.root = Path(self.directory.name)
        (self.root / "first.rs").write_text("prefix\nlet value = 1;\nreturn value;\n", encoding="utf-8")
        (self.root / "second.rs").write_text("let value = 1;\nreturn value;\n", encoding="utf-8")

    def tearDown(self) -> None:
        self.directory.cleanup()

    def report(self) -> dict[str, object]:
        return {
            "duplicates": [
                {
                    "format": "rust",
                    "firstFile": file_info("first.rs", 2, 3),
                    "secondFile": file_info("second.rs", 1, 2),
                }
            ]
        }

    def result(self) -> check_duplication.DuplicationResult:
        records = check_duplication.clone_records(self.report(), self.root)
        return check_duplication.DuplicationResult(
            records=records,
            statistics={
                "clones": 1,
                "duplicatedLines": 2,
                "duplicatedTokens": 8,
                "lines": 5,
                "percentage": 40.0,
                "percentageTokens": 40.0,
                "sources": 2,
                "tokens": 20,
            },
            config_sha256="config-hash",
        )

    def test_fingerprint_is_stable_when_unrelated_lines_move_a_clone(self) -> None:
        initial = self.result().records[0].fingerprint
        (self.root / "first.rs").write_text(
            "new prefix\nprefix\nlet value = 1;\nreturn value;\n", encoding="utf-8"
        )
        report = self.report()
        report["duplicates"][0]["firstFile"] = file_info("first.rs", 3, 4)
        moved = check_duplication.clone_records(report, self.root)[0].fingerprint
        self.assertEqual(initial, moved)

    def test_exact_baseline_accepts_no_change(self) -> None:
        result = self.result()
        baseline = check_duplication.baseline_document(result)
        self.assertEqual(check_duplication.compare_with_baseline(result, baseline), [])

    def test_new_clone_and_removed_clone_both_require_review(self) -> None:
        result = self.result()
        baseline = check_duplication.baseline_document(result)
        baseline["cloneFingerprints"] = ["removed-fingerprint"]
        errors = check_duplication.compare_with_baseline(result, baseline)
        self.assertTrue(any("new or changed" in error for error in errors))
        self.assertTrue(any("ratchet" in error for error in errors))

    def test_policy_change_invalidates_baseline(self) -> None:
        result = self.result()
        baseline = check_duplication.baseline_document(result)
        baseline["configSha256"] = "other-policy"
        errors = check_duplication.compare_with_baseline(result, baseline)
        self.assertIn("jscpd policy changed; review it and regenerate the exact baseline", errors)

    def test_incomplete_jscpd_statistics_fail_closed(self) -> None:
        result = check_duplication.DuplicationResult(
            records=(), statistics={}, config_sha256="config-hash"
        )
        with self.assertRaisesRegex(ValueError, "missing statistics"):
            check_duplication.baseline_document(result)


if __name__ == "__main__":
    unittest.main()
