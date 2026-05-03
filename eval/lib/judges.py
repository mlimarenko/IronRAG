"""Ragas LLM-as-judge wrappers.

Per ADR-001 (`.omc/plans/adr-evaluation-binding.md`), the judge LLM is the same provider
that backs `AiBindingPurpose::QueryAnswer`. The Ragas LLM is configured from the
IRONRAG_EVAL_JUDGE_PROVIDER + IRONRAG_EVAL_JUDGE_MODEL env vars so CI can pin the judge
to match the answer model under test.
"""

from __future__ import annotations

import os
from dataclasses import dataclass

from datasets import Dataset
from ragas import evaluate
from ragas.metrics import answer_relevancy, context_precision, faithfulness


@dataclass(frozen=True)
class RagasResult:
    faithfulness: float
    context_precision: float
    answer_relevancy: float
    per_sample: list[dict[str, float]]


def run_ragas(
    questions: list[str],
    answers: list[str],
    contexts: list[list[str]],
    references: list[str] | None = None,
) -> RagasResult:
    payload = {
        "question": questions,
        "answer": answers,
        "contexts": contexts,
    }
    metrics = [faithfulness, answer_relevancy]
    if references:
        payload["ground_truth"] = references
        metrics.append(context_precision)
    dataset = Dataset.from_dict(payload)
    result = evaluate(dataset, metrics=metrics)
    df = result.to_pandas()
    return RagasResult(
        faithfulness=float(df["faithfulness"].mean()),
        context_precision=float(df["context_precision"].mean()) if "context_precision" in df else 0.0,
        answer_relevancy=float(df["answer_relevancy"].mean()),
        per_sample=df.to_dict(orient="records"),
    )


def judge_provider() -> str:
    return os.environ.get("IRONRAG_EVAL_JUDGE_PROVIDER", "anthropic")


def judge_model() -> str:
    return os.environ.get("IRONRAG_EVAL_JUDGE_MODEL", "claude-sonnet-4-6")
