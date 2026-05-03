#!/usr/bin/env python3
"""Bootstrap a synthetic eval set from a IronRAG library via Ragas TestsetGenerator.

Output goes to spec-kit/eval-sets/<library-slug>/synthetic_<N>.json (internal). The public
ironrag/eval/ tree never receives synthesized samples — they may contain corpus-specific
phrasing. Operator must manually spot-check the output before promoting it to a CI gate.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from lib.ironrag_client import IronRagClient


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--library-id", required=True)
    parser.add_argument("--out", required=True, help="path under spec-kit/eval-sets/...")
    parser.add_argument("--samples", type=int, default=50)
    args = parser.parse_args()

    if not args.out.startswith("spec-kit/eval-sets/"):
        print(
            "refusing to write outside spec-kit/eval-sets/ (CLAUDE.md data policy)",
            file=sys.stderr,
        )
        return 2

    client = IronRagClient()
    chunks = export_library_chunks(client, args.library_id)
    samples = synthesize(chunks, target_count=args.samples)
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(
        json.dumps(
            {
                "eval_set_version": "1.0",
                "library_id": args.library_id,
                "samples": samples,
            },
            indent=2,
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )
    print(f"wrote {len(samples)} samples → {args.out}")
    print("HUMAN ACTION REQUIRED: spot-check before promoting to CI", file=sys.stderr)
    return 0


def export_library_chunks(client: IronRagClient, library_id: str) -> list[dict]:
    raise NotImplementedError(
        "implement against IronRAG export endpoint when /v1/knowledge/libraries/{id}/chunks lands"
    )


def synthesize(chunks: list[dict], target_count: int) -> list[dict]:
    raise NotImplementedError(
        "wire Ragas TestsetGenerator against `chunks` once chunk-export is reachable"
    )


if __name__ == "__main__":
    sys.exit(main())
