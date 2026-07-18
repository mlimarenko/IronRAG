#!/usr/bin/env python3
"""Reject file-wide lint and type-checker suppression directives.

Targeted item- or line-level suppressions remain available when they carry a
reason.  This gate deliberately has no debt baseline: a file-wide muzzle is a
policy violation regardless of when it entered the repository.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import re
import subprocess
import sys
import tokenize
from typing import Iterable, Sequence


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SOURCE_SUFFIXES = frozenset({".cjs", ".js", ".jsx", ".mjs", ".py", ".rs", ".ts", ".tsx"})

# These are generated products, not hand-maintained source.  The boundaries
# are exact so a new generated surface must be reviewed here explicitly.
GENERATED_PREFIXES = (
    Path("apps/web/src/shared/api/generated"),
)
GENERATED_FILES = frozenset(
    {
        Path("apps/web/public/mockServiceWorker.js"),
        Path("apps/web/public/swagger-ui-bundle.js"),
    }
)

RUST_FILE_ALLOW = re.compile(r"^[ \t]*#![ \t]*\[[ \t]*allow[ \t]*[(\]]", re.MULTILINE)
ESLINT_FILE_DISABLE = re.compile(
    r"^[ \t]*/\*[ \t]*eslint-disable(?:[ \t]|\*/)",
    re.MULTILINE,
)
PYTHON_FILE_DIRECTIVE = re.compile(
    r"^#\s*(?:"
    r"type\s*:\s*ignore(?:\[[^\]]+\])?\s*$|"
    r"mypy\s*:\s*(?:ignore-errors\b|.*disable_error_code\s*=)|"
    r"ruff\s*:\s*noqa\b|"
    r"flake8\s*:\s*noqa\b|"
    r"noqa\s*$|"
    r"pylint\s*:\s*(?:skip-file\b|disable\s*=)|"
    r"pyre-ignore-all-errors\b|"
    r"pyright\s*:\s*.*(?:typeCheckingMode\s*=\s*off|report[A-Za-z]+\s*=\s*(?:false|none))"
    r")",
    re.IGNORECASE,
)


@dataclass(frozen=True, slots=True)
class Violation:
    path: Path
    line: int
    rule: str


def _relative_to_repository(path: Path) -> Path | None:
    try:
        return path.resolve().relative_to(REPOSITORY_ROOT)
    except ValueError:
        return None


def is_generated(path: Path) -> bool:
    relative = _relative_to_repository(path)
    if relative is None:
        return False
    if relative in GENERATED_FILES:
        return True
    return any(relative == prefix or prefix in relative.parents for prefix in GENERATED_PREFIXES)


def repository_source_files() -> list[Path]:
    completed = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=REPOSITORY_ROOT,
        check=True,
        capture_output=True,
    )
    paths = []
    for raw_path in completed.stdout.split(b"\0"):
        if not raw_path:
            continue
        path = REPOSITORY_ROOT / raw_path.decode("utf-8", errors="surrogateescape")
        if path.suffix in SOURCE_SUFFIXES and path.is_file() and not is_generated(path):
            paths.append(path)
    return sorted(paths)


def _regex_violations(path: Path, content: str, pattern: re.Pattern[str], rule: str) -> list[Violation]:
    return [
        Violation(path=path, line=content.count("\n", 0, match.start()) + 1, rule=rule)
        for match in pattern.finditer(content)
    ]


def _python_violations(path: Path, content: str) -> list[Violation]:
    violations = []
    try:
        tokens = tokenize.generate_tokens(iter(content.splitlines(keepends=True)).__next__)
        for token in tokens:
            if (
                token.type == tokenize.COMMENT
                and token.start[1] == 0
                and PYTHON_FILE_DIRECTIVE.match(token.string)
            ):
                violations.append(
                    Violation(path=path, line=token.start[0], rule="python-file-suppression")
                )
    except (SyntaxError, tokenize.TokenError) as error:
        raise ValueError(f"cannot tokenize Python source {path}: {error}") from error
    return violations


def scan_file(path: Path) -> list[Violation]:
    content = path.read_text(encoding="utf-8")
    if path.suffix == ".rs":
        return _regex_violations(path, content, RUST_FILE_ALLOW, "rust-crate-allow")
    if path.suffix == ".py":
        return _python_violations(path, content)
    if path.suffix in {".cjs", ".js", ".jsx", ".mjs", ".ts", ".tsx"}:
        return _regex_violations(path, content, ESLINT_FILE_DISABLE, "eslint-file-disable")
    return []


def scan_paths(paths: Iterable[Path]) -> list[Violation]:
    violations = []
    for path in paths:
        violations.extend(scan_file(path))
    return sorted(violations, key=lambda item: (str(item.path), item.line, item.rule))


def _display_path(path: Path) -> str:
    relative = _relative_to_repository(path)
    return relative.as_posix() if relative is not None else str(path)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="*", type=Path, help="files to scan; defaults to repository source")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    paths = [path.resolve() for path in args.paths] if args.paths else repository_source_files()
    try:
        violations = scan_paths(paths)
    except (OSError, ValueError) as error:
        print(f"blanket-suppression scan failed: {error}", file=sys.stderr)
        return 2

    if not violations:
        print(f"blanket-suppression scan passed ({len(paths)} source files)")
        return 0

    for violation in violations:
        print(
            f"{_display_path(violation.path)}:{violation.line}: {violation.rule}: "
            "replace the file-wide suppression with the narrowest item-level directive and a reason",
            file=sys.stderr,
        )
    print(f"blanket-suppression scan failed ({len(violations)} violation(s))", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
