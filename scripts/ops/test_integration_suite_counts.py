"""The integration gate must tell 'everything passed' from 'nothing ran'."""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "apps" / "api" / "scripts" / "check_integration_suite_counts.py"

_spec = importlib.util.spec_from_file_location("check_integration_suite_counts", MODULE_PATH)
assert _spec is not None
assert _spec.loader is not None
checker = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(checker)

LOG = """
running 3 tests
test webhook_delivery::sends_once ... ok
test webhook_delivery::retries_once ... ok
test audit_events::records_denials ... ok

test result: ok. 3 passed; 0 failed; 0 ignored
"""

NESTED_LOG = """
running 2 tests
test services::query::execution::semantic_rerank::binding_tests::resolves_rerank ... ok
test services::query::execution::semantic_rerank::binding_tests::no_compile_fallback ... ok

test result: ok. 2 passed; 0 failed; 0 ignored
"""

WORKSPACE_LOG = """
     Running unittests src/lib.rs (/build-cache/debug/deps/ironrag_backend-123abc)
running 218 tests
test result: ok. 200 passed; 0 failed; 18 ignored; 0 measured; 0 filtered out
     Running tests/it/main.rs (/build-cache/debug/deps/it-456def)
running 519 tests
test result: ok. 42 passed; 0 failed; 477 ignored; 0 measured; 0 filtered out
     Running unittests src/lib.rs (/build-cache/debug/deps/contracts-789abc)
running 5 tests
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
"""


class BinaryTotalCountTests(unittest.TestCase):
    def test_large_malformed_header_is_rejected_within_a_bounded_process(self) -> None:
        completed = subprocess.run(
            [sys.executable, "-c", """
import importlib.util
import sys
spec = importlib.util.spec_from_file_location('checker', sys.argv[1])
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
lines, ok = checker.report_total('it', 1, 'Running ' + ' (a' * 20000 + '\\n')
assert not ok, lines
lines, ok = checker.report_total('it', 1, '\\n' * 50000)
assert not ok, lines
""", str(MODULE_PATH)],
            capture_output=True,
            text=True,
            timeout=2,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_cargo_binary_paths_and_invalid_headers(self) -> None:
        for path in ("it-abc123", "/cache/debug/deps/it-abc123", r"C:\cache\it-abc123.exe"):
            with self.subTest(path=path):
                self.assertEqual(checker.running_binary(f"Running tests/it/main.rs ({path})"), "it")
        for path in ("it-", "it-xyz", "it abc-123", "it-123.exe.trailing", "it-123"):
            with self.subTest(path=path):
                header = f"Running tests/it/main.rs ({path})" if path != "it-123" else "Running broken (it-123"
                self.assertIsNone(checker.running_binary(header))

    def test_selects_the_integration_binary_among_workspace_targets(self) -> None:
        lines, ok = checker.report_total("it", 42, WORKSPACE_LOG)
        self.assertTrue(ok, lines)
        self.assertEqual(lines, ["ok it: 42 of 42 tests passed"])

    def test_rejects_missing_or_duplicate_binary_sections(self) -> None:
        for log, count in (
            (WORKSPACE_LOG.replace("deps/it-", "deps/other-"), 0),
            (WORKSPACE_LOG + WORKSPACE_LOG, 2),
        ):
            with self.subTest(count=count):
                lines, ok = checker.report_total("it", 42, log)
                self.assertFalse(ok)
                self.assertIn(f"got {count}", lines[0])

    def test_rejects_wrong_count_even_when_another_binary_matches(self) -> None:
        lines, ok = checker.report_total("it", 41, WORKSPACE_LOG)
        self.assertFalse(ok, lines)
        self.assertIn("got 42 passed", lines[0])

    def test_rejects_a_failed_summary(self) -> None:
        log = WORKSPACE_LOG.replace("ok. 42 passed; 0 failed", "FAILED. 41 passed; 1 failed")
        lines, ok = checker.report_total("it", 42, log)
        self.assertFalse(ok, lines)

    def test_ignores_a_child_harness_summary_inside_the_integration_binary(self) -> None:
        log = WORKSPACE_LOG.replace(
            "running 519 tests\n",
            "running 519 tests\n"
            "test observability_smoke::init_tracing_child_disabled_by_flag ... ok\n"
            "running 1 test\n"
            "test observability_smoke::init_tracing_child_disabled_by_flag ... ok\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 518 filtered out; finished in 0.00s\n",
        )
        lines, ok = checker.report_total("it", 42, log)
        self.assertTrue(ok, lines)

    def test_requires_one_summary_in_the_selected_section(self) -> None:
        summary = "test result: ok. 42 passed; 0 failed; 477 ignored; 0 measured; 0 filtered out"
        for replacement in ("", summary + "\n" + summary):
            with self.subTest(replacement=replacement):
                lines, ok = checker.report_total("it", 42, WORKSPACE_LOG.replace(summary, replacement))
                self.assertFalse(ok)
                self.assertIn("expected one test summary", lines[0])

    def test_does_not_take_a_doctest_summary_for_the_binary(self) -> None:
        log = WORKSPACE_LOG.replace("test result: ok. 42 passed", "   Doc-tests it\ntest result: ok. 42 passed")
        lines, ok = checker.report_total("it", 42, log)
        self.assertFalse(ok, lines)


class IntegrationSuiteCountTests(unittest.TestCase):
    def test_accepts_a_run_that_matches_the_expectations(self) -> None:
        lines, ok = checker.report("webhook_delivery:2 audit_events:1", LOG)

        self.assertTrue(ok, lines)
        self.assertIn("ok webhook_delivery: 2 of 2 tests passed", lines)

    def test_rejects_a_suite_that_ran_nothing(self) -> None:
        # The whole point: a filter that matches nothing still exits 0, so the
        # run looks successful while the suite never executed.
        lines, ok = checker.report("webhook_delivery:2 audit_events:1 knowledge_search:20", LOG)

        self.assertFalse(ok)
        self.assertIn("FAIL knowledge_search: expected 20 passing tests, got 0", lines)

    def test_rejects_a_suite_that_grew_or_shrank(self) -> None:
        lines, ok = checker.report("webhook_delivery:5 audit_events:1", LOG)

        self.assertFalse(ok)
        self.assertIn("FAIL webhook_delivery: expected 5 passing tests, got 2", lines)

    def test_counts_only_tests_that_passed(self) -> None:
        failing = LOG.replace("test audit_events::records_denials ... ok", "test audit_events::records_denials ... FAILED")

        lines, ok = checker.report("webhook_delivery:2 audit_events:1", failing)

        self.assertFalse(ok)
        self.assertIn("FAIL audit_events: expected 1 passing tests, got 0", lines)

    def test_rejects_a_malformed_or_zero_expectation(self) -> None:
        for raw, expected in (
            ("webhook_delivery", "entries must read"),
            ("webhook_delivery:many", "is not a number"),
            ("webhook_delivery:0", "must be greater than zero"),
        ):
            with self.subTest(raw=raw):
                lines, ok = checker.report(raw, LOG)
                self.assertFalse(ok)
                self.assertTrue(any(expected in line for line in lines), lines)

    def test_rejects_a_module_that_executed_but_was_not_declared(self) -> None:
        # A renamed or re-pointed module still runs while the declared counts
        # describe something else; the per-entry check alone would not notice.
        lines, ok = checker.report("webhook_delivery:2 audit_events:1", LOG + "test stray_suite::left_behind ... ok\n")

        self.assertFalse(ok)
        self.assertIn("FAIL unexpected modules executed but not declared: stray_suite", lines)

    def test_accepts_a_nested_module_path(self) -> None:
        lines, ok = checker.report(
            "services::query::execution::semantic_rerank::binding_tests:2", NESTED_LOG
        )

        self.assertTrue(ok, lines)
        self.assertIn(
            "ok services::query::execution::semantic_rerank::binding_tests: 2 of 2 tests passed",
            lines,
        )


if __name__ == "__main__":
    unittest.main()
