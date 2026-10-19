#!/usr/bin/env python3
"""Check what a cargo test run actually executed, suite by suite.

The fast target checks the `it` binary's total inside a workspace run.
`BACKEND_INTEGRATION_SUITES` gates the ignored cases in the `it` binary,
whose database cases are all `#[ignore]`d and run in the integration job.
`BACKEND_LIB_IGNORED_SUITES` gates the `--lib` binary, whose database cases are
`#[ignore]`d for the same reason but live in the unit-test binary because they
exercise private items. Each target reads one run's output rather than invoking
cargo once per suite.

The check exists because a filter that matches nothing still exits 0, printing
`test result: ok. 0 passed`. That is how forty-six tests were once "added to
CI" and never ran, so the exit code alone cannot tell "everything passed" from
"nothing ran". Every entry states how many tests its module must report, and
any deviation -- zero, fewer, or more -- fails.

The unexpected-module check exists for the opposite drift: a renamed or
re-pointed module that the run still executes, but that no entry declares. The
per-entry check alone would keep passing while the declared counts silently
describe a different module than the one that ran, so a module the run executed
without being declared fails too.
"""

from __future__ import annotations

import argparse
import re
import sys
from collections import Counter
from pathlib import Path

# `test services::query::execution::semantic_rerank::binding_tests::case ... ok`
# The module is everything before the last `::`; the case name never contains
# one, so a nested module path is captured whole.
RESULT_LINE = re.compile(r"^test (.+)::([^:]+) \.\.\. ok$", re.MULTILINE)
CARGO_SECTION = re.compile(r"^[ \t]*(?:Running .+|Doc-tests .+)$", re.MULTILINE)
SUMMARY_LINE = re.compile(
    r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; "
    r"(\d+) ignored; (\d+) measured; (\d+) filtered out(?:;|$)", re.MULTILINE
)
RUNNING_TESTS = re.compile(r"^running (\d+) tests?$", re.MULTILINE)


def running_binary(header: str) -> str | None:
    """Extract Cargo's binary identity without suffix-wide backtracking."""
    description, separator, path = header.strip().partition(" (")
    if not separator or not description.startswith("Running ") or not path.endswith(")"):
        return None
    filename = path[:-1].replace("\\", "/").rsplit("/", 1)[-1]
    filename = filename.removesuffix(".exe")
    name, separator, digest = filename.rpartition("-")
    if not separator or re.fullmatch(r"[^/\\ ()]+", name) is None:
        return None
    if re.fullmatch(r"[0-9a-f]+", digest) is None:
        return None
    return name


def report_total(binary: str, total: int, log: str) -> tuple[list[str], bool]:
    headers = list(CARGO_SECTION.finditer(log))
    sections = []
    for index, header in enumerate(headers):
        if running_binary(header.group()) == binary:
            end = headers[index + 1].start() if index + 1 < len(headers) else len(log)
            sections.append(log[header.end():end])
    if len(sections) != 1:
        return [f"FAIL {binary}: expected one Cargo binary section, got {len(sections)}"], False
    announcements = RUNNING_TESTS.findall(sections[0])
    if not announcements:
        return [f"FAIL {binary}: missing test count announcement"], False
    announced = int(announcements[0])
    # A test may spawn this same binary with a filter. Its own harness summary
    # appears inside the parent section, but describes fewer selected tests.
    summaries = [
        summary for summary in SUMMARY_LINE.findall(sections[0])
        if sum(map(int, summary[1:5])) == announced
    ]
    if len(summaries) != 1:
        return [f"FAIL {binary}: expected one test summary, got {len(summaries)}"], False
    status, passed, failed, _, _, _ = summaries[0]
    if status != "ok" or int(failed) != 0 or int(passed) != total:
        return [f"FAIL {binary}: expected {total} passing tests, got {passed} passed and {failed} failed ({status})"], False
    return [f"ok {binary}: {passed} of {total} tests passed"], True


def executed_per_module(log: str) -> Counter[str]:
    return Counter(match.group(1) for match in RESULT_LINE.finditer(log))


def parse_expectations(raw: str) -> tuple[dict[str, int], list[str]]:
    expected: dict[str, int] = {}
    problems: list[str] = []
    for entry in raw.split():
        module, separator, count = entry.rpartition(":")
        if not separator or not module:
            problems.append(f"FAIL {entry}: entries must read '<suite>:<test count>'")
            continue
        if not count.isdigit():
            problems.append(f"FAIL {module}: expected test count '{count}' is not a number")
            continue
        if int(count) == 0:
            problems.append(f"FAIL {module}: expected test count must be greater than zero")
            continue
        expected[module] = int(count)
    return expected, problems


def report(raw_expectations: str, log: str) -> tuple[list[str], bool]:
    expected, lines = parse_expectations(raw_expectations)
    ok = not lines
    executed = executed_per_module(log)
    for module, count in sorted(expected.items()):
        ran = executed.get(module, 0)
        if ran == count:
            lines.append(f"ok {module}: {ran} of {count} tests passed")
        else:
            lines.append(f"FAIL {module}: expected {count} passing tests, got {ran}")
            ok = False
    unexpected = sorted(set(executed) - set(expected))
    if unexpected:
        lines.append(
            "FAIL unexpected modules executed but not declared: " + ", ".join(unexpected)
        )
        ok = False
    return lines, ok


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    expectation = parser.add_mutually_exclusive_group(required=True)
    expectation.add_argument("--suites", help="Space-separated <suite>:<test count> entries")
    expectation.add_argument("--total", type=int, help="Expected passing tests in one binary")
    parser.add_argument("--binary", help="Cargo test binary name for --total")
    parser.add_argument("log", help="Cargo output path, or - for stdin")
    args = parser.parse_args()
    if args.total is not None and (args.total < 1 or not args.binary):
        parser.error("--total requires a positive count and --binary")
    if args.binary and args.total is None:
        parser.error("--binary requires --total")
    log = sys.stdin.read() if args.log == "-" else Path(args.log).read_text(encoding="utf-8")
    lines, ok = (
        report_total(args.binary, args.total, log)
        if args.total is not None
        else report(args.suites, log)
    )
    for line in lines:
        print(line, file=sys.stderr if line.startswith("FAIL") else sys.stdout)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
