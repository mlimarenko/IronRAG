#!/usr/bin/env python3
"""Scan every tracked or non-ignored worktree file with redacted Gitleaks output."""

from __future__ import annotations

import argparse
from pathlib import Path
import shutil
import subprocess
import sys
from tempfile import TemporaryDirectory


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]


def selected_paths(root: Path) -> tuple[Path, ...]:
    completed = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=root,
        check=True,
        stdout=subprocess.PIPE,
    )
    relative_paths: list[Path] = []
    for raw_path in completed.stdout.split(b"\0"):
        if not raw_path:
            continue
        relative_path = Path(raw_path.decode("utf-8"))
        source = root / relative_path
        if not source.exists() and not source.is_symlink():
            continue
        resolved_parent = source.parent.resolve()
        try:
            resolved_parent.relative_to(root.resolve())
        except ValueError as error:
            raise ValueError(f"worktree path escapes repository: {relative_path}") from error
        relative_paths.append(relative_path)
    return tuple(relative_paths)


def copy_selected_tree(root: Path, destination: Path, paths: tuple[Path, ...]) -> None:
    for relative_path in paths:
        source = root / relative_path
        target = destination / relative_path
        target.parent.mkdir(parents=True, exist_ok=True)
        if source.is_symlink():
            # Scan the versioned link payload as data. Recreating the symlink
            # would let a tracked absolute link make the scanner walk outside
            # the repository, while a broken relative link would be skipped.
            target.write_text(source.readlink().as_posix(), encoding="utf-8")
        else:
            shutil.copy2(source, target)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gitleaks", default="gitleaks", help="Gitleaks executable")
    parser.add_argument("--timeout", type=int, default=90, help="scanner timeout in seconds")
    parser.add_argument(
        "--max-target-megabytes",
        type=int,
        default=20,
        help="maximum individual source-file size",
    )
    return parser


def main() -> int:
    args = build_parser().parse_args()
    if args.timeout <= 0 or args.max_target_megabytes <= 0:
        raise ValueError("timeout and maximum target size must be positive")

    paths = selected_paths(REPOSITORY_ROOT)
    with TemporaryDirectory(prefix="ironrag-gitleaks-worktree-") as temporary_directory:
        scan_root = Path(temporary_directory)
        copy_selected_tree(REPOSITORY_ROOT, scan_root, paths)
        completed = subprocess.run(
            [
                args.gitleaks,
                "dir",
                "--redact",
                "--no-banner",
                "--no-color",
                "--ignore-gitleaks-allow",
                "--max-target-megabytes",
                str(args.max_target_megabytes),
                "--timeout",
                str(args.timeout),
                str(scan_root),
            ],
            cwd=REPOSITORY_ROOT,
            check=False,
        )
    if completed.returncode == 0:
        print(f"worktree secret scan passed ({len(paths)} files)")
    return completed.returncode


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"worktree secret scan failed: {error}", file=sys.stderr)
        raise SystemExit(2) from error
