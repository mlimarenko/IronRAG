from __future__ import annotations

from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from scripts.ops import lint_blanket_suppressions as scanner


class BlanketSuppressionScannerTests(unittest.TestCase):
    def scan(self, suffix: str, content: str) -> list[scanner.Violation]:
        with TemporaryDirectory() as directory:
            path = Path(directory) / f"fixture{suffix}"
            path.write_text(content, encoding="utf-8")
            return scanner.scan_file(path)

    def test_rejects_rust_crate_level_allow(self) -> None:
        violations = self.scan(".rs", "#! [ allow(dead_code) ]\nfn main() {}\n")
        self.assertEqual([(item.line, item.rule) for item in violations], [(1, "rust-crate-allow")])

    def test_allows_targeted_rust_item_allow_with_reason(self) -> None:
        violations = self.scan(
            ".rs",
            '#[allow(dead_code, reason = "exercised by an external protocol harness")]\nfn helper() {}\n',
        )
        self.assertEqual(violations, [])

    def test_rejects_eslint_file_disable_but_allows_documented_next_line(self) -> None:
        violations = self.scan(
            ".ts",
            "/* eslint-disable no-console */\n"
            "// eslint-disable-next-line no-console -- protocol trace is intentional\n"
            "console.log('trace')\n",
        )
        self.assertEqual([(item.line, item.rule) for item in violations], [(1, "eslint-file-disable")])

    def test_rejects_python_module_directives_without_matching_strings(self) -> None:
        violations = self.scan(
            ".py",
            'EXAMPLE = "# type: ignore"\n'
            "# mypy: ignore-errors\n"
            "import optional_dependency  # type: ignore[import-not-found]\n",
        )
        self.assertEqual([(item.line, item.rule) for item in violations], [(2, "python-file-suppression")])

    def test_rejects_python_file_level_type_ignore(self) -> None:
        violations = self.scan(".py", "# type: ignore\nVALUE = 1\n")
        self.assertEqual([(item.line, item.rule) for item in violations], [(1, "python-file-suppression")])

    def test_rejects_targeted_rule_when_it_is_disabled_for_the_whole_python_file(self) -> None:
        violations = self.scan(".py", "# pylint: disable=too-many-lines\nVALUE = 1\n")
        self.assertEqual([(item.line, item.rule) for item in violations], [(1, "python-file-suppression")])


if __name__ == "__main__":
    unittest.main()
