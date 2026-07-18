#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[4]
SCRIPT_PATH = REPO_ROOT / "scripts/bench/compare_pg_vs_baseline.py"
SPEC = importlib.util.spec_from_file_location("compare_pg_vs_baseline", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
legacy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(legacy)


def write_agent_result(path: Path, *, p95_ms: float, gate_passed: bool = True) -> None:
    path.mkdir(parents=True, exist_ok=True)
    (path / "agent_turn_p95.result.json").write_text(
        json.dumps(
            {
                "runs": 5,
                "successes": 5,
                "failures": 0,
                "p50_ms": p95_ms / 2,
                "p95_ms": p95_ms,
                "p99_ms": p95_ms,
                "gate_passed": gate_passed,
            }
        ),
        encoding="utf-8",
    )


class LegacyComparatorTests(unittest.TestCase):
    def test_agent_turn_is_a_required_release_verdict(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            baseline = root / "baseline"
            candidate = root / "candidate"
            baseline.mkdir()
            candidate.mkdir()

            gate = legacy.build_agent_turn_gate(baseline, candidate)

        self.assertFalse(gate["passed"])
        self.assertEqual(gate["invalidArtifacts"], ["baseline", "candidate"])

    def test_agent_turn_enforces_absolute_and_relative_p95(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            baseline = root / "baseline"
            candidate = root / "candidate"
            write_agent_result(baseline, p95_ms=80_000.0)
            write_agent_result(candidate, p95_ms=89_000.0)

            gate = legacy.build_agent_turn_gate(baseline, candidate)

        self.assertFalse(gate["relativePassed"])
        self.assertTrue(gate["absolutePassed"])
        self.assertFalse(gate["passed"])

    def test_agent_turn_requires_candidate_quality_gate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            baseline = root / "baseline"
            candidate = root / "candidate"
            write_agent_result(baseline, p95_ms=80_000.0)
            write_agent_result(candidate, p95_ms=80_000.0, gate_passed=False)

            gate = legacy.build_agent_turn_gate(baseline, candidate)

        self.assertFalse(gate["candidateGatePassed"])
        self.assertFalse(gate["passed"])


if __name__ == "__main__":
    unittest.main()
