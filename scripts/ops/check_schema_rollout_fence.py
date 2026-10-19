#!/usr/bin/env python3
"""Refuse a release mutation while the schema-rollout fence marker exists.

The marker at ``apps/api/ops/schema-rollout.pending`` blocks publication.
Its contents are never read. Any existing marker or untrusted parent path
refuses the action; absence permits it but does not verify the database.
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

DEFAULT_MARKER = (
    Path(__file__).resolve().parents[2] / "apps/api/ops/schema-rollout.pending"
)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Refuse a release mutation while the schema-rollout fence is up.",
    )
    parser.add_argument(
        "--marker",
        type=Path,
        default=DEFAULT_MARKER,
        help="path to the schema-rollout fence marker (default: %(default)s)",
    )
    args = parser.parse_args(argv)
    marker = args.marker

    try:
        os.lstat(marker)
    except FileNotFoundError:
        # Absent is a permit only when the parent is a trusted existing directory;
        # an untrusted or missing parent refuses instead of reporting "absent".
        if marker.parent.is_dir():
            return 0
        print(
            "schema rollout fence up: marker parent is not a directory",
            file=sys.stderr,
        )
        return 1
    except OSError:
        print("schema rollout fence up: marker is unreadable", file=sys.stderr)
        return 1

    print("schema rollout fence up: marker exists", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
