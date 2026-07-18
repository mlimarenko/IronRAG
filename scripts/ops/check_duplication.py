#!/usr/bin/env python3
"""Run jscpd and enforce an exact, ratcheting clone fingerprint baseline."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
from tempfile import TemporaryDirectory
from typing import Any, Mapping, Sequence


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CONFIG = REPOSITORY_ROOT / ".jscpd.json"
DEFAULT_BASELINE = REPOSITORY_ROOT / "scripts/ops/jscpd-baseline.json"
DEFAULT_JSCPD = REPOSITORY_ROOT / "apps/web/node_modules/.bin/jscpd"
BASELINE_SCHEMA_VERSION = 1


@dataclass(frozen=True, slots=True)
class CloneRecord:
    fingerprint: str
    description: str


@dataclass(frozen=True, slots=True)
class DuplicationResult:
    records: tuple[CloneRecord, ...]
    statistics: Mapping[str, Any]
    config_sha256: str


def _sha256(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def _normalized_fragment(root: Path, file_info: Mapping[str, Any]) -> tuple[str, str, int]:
    relative_path = Path(str(file_info["name"]))
    source_path = (root / relative_path).resolve()
    try:
        canonical_path = source_path.relative_to(root.resolve()).as_posix()
    except ValueError as error:
        raise ValueError(f"jscpd reported a path outside the repository: {relative_path}") from error
    if not source_path.is_file():
        raise ValueError(f"jscpd reported a missing source file: {canonical_path}")

    lines = source_path.read_text(encoding="utf-8").splitlines()
    start = int(file_info["start"])
    end = int(file_info["end"])
    if start < 1 or end < start or end > len(lines):
        raise ValueError(f"invalid jscpd source span {canonical_path}:{start}-{end}")
    fragment = "\n".join(lines[start - 1 : end])
    normalized = re.sub(r"\s+", "", fragment).encode("utf-8")
    return canonical_path, _sha256(normalized), start


def clone_records(report: Mapping[str, Any], root: Path = REPOSITORY_ROOT) -> tuple[CloneRecord, ...]:
    duplicates = report.get("duplicates")
    if not isinstance(duplicates, list):
        raise ValueError("jscpd report has no duplicates array")

    raw_occurrences: dict[tuple[str, str], set[int]] = {}
    clone_occurrences: list[tuple[str, tuple[str, str, int], tuple[str, str, int]]] = []
    for duplicate in duplicates:
        first = _normalized_fragment(root, duplicate["firstFile"])
        second = _normalized_fragment(root, duplicate["secondFile"])
        raw_occurrences.setdefault(first[:2], set()).add(first[2])
        raw_occurrences.setdefault(second[:2], set()).add(second[2])
        clone_occurrences.append((str(duplicate["format"]), first, second))

    ordinals = {
        (path, fragment_hash, start): ordinal
        for (path, fragment_hash), starts in raw_occurrences.items()
        for ordinal, start in enumerate(sorted(starts), start=1)
    }

    records = []
    for clone_format, first, second in clone_occurrences:
        occurrence_ids = sorted(
            [
                f"{first[0]}:{first[1]}:{ordinals[first]}",
                f"{second[0]}:{second[1]}:{ordinals[second]}",
            ]
        )
        canonical = json.dumps(
            {"format": clone_format, "occurrences": occurrence_ids},
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
        fingerprint = _sha256(canonical)
        description = f"{first[0]}:{first[2]} <-> {second[0]}:{second[2]} ({clone_format})"
        records.append(CloneRecord(fingerprint=fingerprint, description=description))

    fingerprints = [record.fingerprint for record in records]
    if len(fingerprints) != len(set(fingerprints)):
        raise ValueError("jscpd report produced ambiguous clone fingerprints")
    return tuple(sorted(records, key=lambda record: record.fingerprint))


def run_jscpd(jscpd: Path, config: Path, root: Path = REPOSITORY_ROOT) -> Mapping[str, Any]:
    if not jscpd.is_file():
        raise FileNotFoundError(f"jscpd is not installed at {jscpd}; run 'npm ci --prefix apps/web'")
    with TemporaryDirectory(prefix="ironrag-jscpd-") as output_directory:
        completed = subprocess.run(
            [
                str(jscpd),
                "--config",
                str(config),
                "--reporters",
                "json",
                "--output",
                output_directory,
                "--no-colors",
                "--no-tips",
            ],
            cwd=root,
            check=False,
            text=True,
            capture_output=True,
        )
        if completed.returncode != 0:
            detail = completed.stderr.strip() or completed.stdout.strip()
            raise RuntimeError(f"jscpd exited with {completed.returncode}: {detail}")
        report_path = Path(output_directory) / "jscpd-report.json"
        if not report_path.is_file():
            raise RuntimeError("jscpd did not write jscpd-report.json")
        return json.loads(report_path.read_text(encoding="utf-8"))


def analyze(jscpd: Path, config: Path, root: Path = REPOSITORY_ROOT) -> DuplicationResult:
    report = run_jscpd(jscpd, config, root)
    statistics = report.get("statistics", {}).get("total")
    if not isinstance(statistics, dict):
        raise ValueError("jscpd report has no total statistics")
    return DuplicationResult(
        records=clone_records(report, root),
        statistics=statistics,
        config_sha256=_sha256(config.read_bytes()),
    )


def baseline_document(result: DuplicationResult) -> dict[str, Any]:
    statistic_keys = (
        "clones",
        "duplicatedLines",
        "duplicatedTokens",
        "lines",
        "percentage",
        "percentageTokens",
        "sources",
        "tokens",
    )
    missing_keys = [key for key in statistic_keys if key not in result.statistics]
    if missing_keys:
        raise ValueError(f"jscpd report is missing statistics: {', '.join(missing_keys)}")
    return {
        "schemaVersion": BASELINE_SCHEMA_VERSION,
        "configSha256": result.config_sha256,
        "cloneFingerprints": [record.fingerprint for record in result.records],
        "statistics": {
            key: result.statistics[key]
            for key in statistic_keys
        },
    }


def compare_with_baseline(result: DuplicationResult, baseline: Mapping[str, Any]) -> list[str]:
    errors = []
    if baseline.get("schemaVersion") != BASELINE_SCHEMA_VERSION:
        errors.append("unsupported duplication baseline schema")
    if baseline.get("configSha256") != result.config_sha256:
        errors.append("jscpd policy changed; review it and regenerate the exact baseline")

    expected = baseline.get("cloneFingerprints")
    if not isinstance(expected, list) or any(not isinstance(item, str) for item in expected):
        errors.append("duplication baseline has no valid cloneFingerprints array")
        return errors

    expected_set = set(expected)
    if len(expected_set) != len(expected):
        errors.append("duplication baseline contains duplicate fingerprints")
    current_by_id = {record.fingerprint: record for record in result.records}
    current_set = set(current_by_id)
    added = sorted(current_set - expected_set)
    removed = sorted(expected_set - current_set)
    if added:
        errors.append(f"{len(added)} new or changed clone pair(s) detected")
        errors.extend(f"new clone: {current_by_id[fingerprint].description}" for fingerprint in added[:20])
        if len(added) > 20:
            errors.append(f"{len(added) - 20} additional new clone pair(s) omitted")
    if removed:
        errors.append(
            f"{len(removed)} clone pair(s) were removed; regenerate the baseline now to ratchet the debt down"
        )
    return errors


def _summary(result: DuplicationResult) -> str:
    statistics = result.statistics
    return (
        f"jscpd analyzed {statistics['sources']} files: {statistics['clones']} clone pairs, "
        f"{statistics['duplicatedLines']} duplicated lines ({float(statistics['percentage']):.4f}%)"
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jscpd", type=Path, default=DEFAULT_JSCPD)
    parser.add_argument("--config", type=Path, default=DEFAULT_CONFIG)
    parser.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    parser.add_argument(
        "--write-baseline",
        action="store_true",
        help="replace the exact fingerprint baseline after reviewing every reported change",
    )
    parser.add_argument("--copy-report", type=Path, help="copy the machine-readable baseline/report summary")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        result = analyze(args.jscpd.resolve(), args.config.resolve())
        document = baseline_document(result)
        if args.copy_report:
            args.copy_report.parent.mkdir(parents=True, exist_ok=True)
            args.copy_report.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
        if args.write_baseline:
            args.baseline.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
            print(f"wrote exact duplication baseline: {args.baseline}")
            print(_summary(result))
            return 0
        baseline = json.loads(args.baseline.read_text(encoding="utf-8"))
        errors = compare_with_baseline(result, baseline)
    except (OSError, ValueError, RuntimeError) as error:
        print(f"duplication gate failed: {error}", file=sys.stderr)
        return 2

    print(_summary(result))
    if errors:
        for error in errors:
            print(f"duplication gate: {error}", file=sys.stderr)
        return 1
    print("duplication gate passed: no clone fingerprint changed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
