#!/usr/bin/env python3
"""Sprint A runner: load eval set → call IronRAG → score with Ragas + artifact gate → CI exit code.

Usage:
    python3 run_eval.py --set spec-kit/eval-sets/<library>/synthetic_v1.json --mode quick

Mode `quick` enforces pre-commit thresholds; `full` enforces post-deploy thresholds.
Refuses to run without both public + private perf-baseline output paths set per v4 plan
two-tier rule.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

from lib.artifact_gate import check_sample, coverage_ratio
from lib.ironrag_client import IronRagClient
from lib.judges import judge_model, judge_provider, run_ragas


THRESHOLDS = {
    "quick": {"faithfulness": 0.85, "context_precision": 0.70, "answer_relevancy": 0.75, "artifact_coverage": 0.85},
    "full": {"faithfulness": 0.90, "context_precision": 0.80, "answer_relevancy": 0.80, "artifact_coverage": 0.90},
}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--set", dest="set_path", required=True)
    parser.add_argument("--mode", choices=("quick", "full"), default="quick")
    parser.add_argument("--public-out", dest="public_out", required=True)
    parser.add_argument("--private-out", dest="private_out", required=True)
    parser.add_argument("--label", dest="label", required=True, help="short identifier (e.g. 'pre-rrf-baseline')")
    args = parser.parse_args()

    eval_set = json.loads(Path(args.set_path).read_text(encoding="utf-8"))
    library_id = eval_set["library_id"]
    samples = eval_set["samples"]
    if args.mode == "quick":
        samples = samples[:20]

    client = IronRagClient()
    questions, answers, contexts, references = [], [], [], []
    artifact_checks = []
    for s in samples:
        result = client.grounded_answer(library_id, s["query"])
        questions.append(s["query"])
        answers.append(result.answer_text)
        contexts.append(result.retrieved_contexts)
        references.append(s["reference_answer"])
        artifact_checks.append(
            check_sample(
                sample_id=s["sample_id"],
                answer_text=result.answer_text,
                citation_chunk_ids=result.citation_chunk_ids,
                expected_artifacts=s.get("reference_artifacts", []),
                reference_chunk_ids=s.get("reference_chunk_ids", []),
            )
        )

    ragas = run_ragas(questions, answers, contexts, references)
    artifact_cov = coverage_ratio(artifact_checks)
    metrics = {
        "faithfulness": ragas.faithfulness,
        "context_precision": ragas.context_precision,
        "answer_relevancy": ragas.answer_relevancy,
        "artifact_coverage": artifact_cov,
    }
    thresholds = THRESHOLDS[args.mode]
    failed = [k for k, v in metrics.items() if v < thresholds[k]]

    write_reports(
        public_path=Path(args.public_out),
        private_path=Path(args.private_out),
        label=args.label,
        mode=args.mode,
        metrics=metrics,
        thresholds=thresholds,
        sample_count=len(samples),
        failed=failed,
        artifact_checks=artifact_checks,
        ragas_per_sample=ragas.per_sample,
        eval_set_path=args.set_path,
    )

    if failed:
        print(f"FAIL: metrics below threshold: {failed}", file=sys.stderr)
        return 1
    print(f"PASS ({args.mode}): {metrics}")
    return 0


def write_reports(
    *,
    public_path: Path,
    private_path: Path,
    label: str,
    mode: str,
    metrics: dict[str, float],
    thresholds: dict[str, float],
    sample_count: int,
    failed: list[str],
    artifact_checks: list,
    ragas_per_sample: list[dict[str, float]],
    eval_set_path: str,
) -> None:
    public_path.parent.mkdir(parents=True, exist_ok=True)
    private_path.parent.mkdir(parents=True, exist_ok=True)
    now = datetime.now(timezone.utc).isoformat()
    common = (
        f"# {label}\n\n"
        f"- Mode: {mode}\n"
        f"- Samples: {sample_count}\n"
        f"- Judge: {judge_provider()} / {judge_model()}\n"
        f"- Generated: {now}\n\n"
        f"## Metrics vs thresholds\n\n"
        + "\n".join(
            f"- **{k}**: {metrics[k]:.3f} (threshold {thresholds[k]:.2f}) — {'PASS' if k not in failed else 'FAIL'}"
            for k in thresholds
        )
        + "\n"
    )
    public_path.write_text(common, encoding="utf-8")
    private_path.write_text(
        common
        + f"\n## Eval set\n\n- Path: `{eval_set_path}`\n\n## Per-sample (artifact gate)\n\n"
        + "\n".join(
            f"- {c.sample_id}: artifacts {c.artifacts_in_answer}/{c.artifacts_total}, cited {c.cited_chunks_present}/{c.cited_chunks_required}, {'PASS' if c.passed else 'FAIL'}"
            for c in artifact_checks
        )
        + "\n\n## Per-sample (Ragas)\n\n```json\n"
        + json.dumps(ragas_per_sample, indent=2, ensure_ascii=False)
        + "\n```\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    sys.exit(main())
