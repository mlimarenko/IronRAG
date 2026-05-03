# IronRAG Evaluation Harness

Sanitized, public Ragas-based evaluation harness for IronRAG retrieval and answer quality.

Tracks: `faithfulness`, `context_precision`, `answer_relevancy`, plus the IronRAG-specific
`artifact_coverage` gate that asserts benchmark answers contain the concrete artifacts
(banks, parameters, paths) listed in the ground-truth eval set.

## Two-tier eval data

Per `CLAUDE.md` data policy, this directory holds only the runner. Ground-truth eval sets
(corpus-specific questions, reference answers, expected chunks) live in the internal
`spec-kit/eval-sets/<library>/` tree. The runner accepts the eval-set path on the command
line; it does not embed corpus-specific data.

Synthetic ground-truth templates with non-corpus stand-in fixtures live at
`spec-kit/eval-sets/synthetic_v1.template.json` for shape reference.

## Targets

```
make eval-quick    # 20-sample faithfulness + context_precision; pre-commit gate
make eval-full     # full eval set; post-deploy gate
make eval-gen      # bootstrap synthetic testset via Ragas TestsetGenerator
make eval-baseline # capture baseline before a v0.4 sprint lands
```

Thresholds (CI-gate values, configurable via env):

| Metric              | Quick | Full  |
|---------------------|------:|------:|
| `faithfulness`      | 0.85  | 0.90  |
| `context_precision` | 0.70  | 0.80  |
| `answer_relevancy`  | 0.75  | 0.80  |
| `artifact_coverage` | 0.85  | 0.90  |

## Binding

Per `.omc/plans/adr-evaluation-binding.md`, the LLM-as-judge calls resolve through the
existing `AiBindingPurpose::QueryAnswer` binding. No new enum variant is added in v0.4.
Eval cost is filterable via `billing_execution_cost.tags::JSONB`
(planned in Sprint B1 follow-up F-3).

## Setup

```
python3 -m venv .venv && . .venv/bin/activate
pip install -r requirements.txt
```

The runner expects an IronRAG backend reachable at `IRONRAG_EVAL_BASE_URL`
(default `http://127.0.0.1:19000`) and an admin token at `IRONRAG_EVAL_TOKEN`.

## Output

Per the v4 plan §two-tier perf reporting, each run writes BOTH:

- Public sanitized `ironrag/docs/perf/YYYY-MM-DD-<change>.md` (no corpus names, no doc names)
- Internal `spec-kit/perf/YYYY-MM-DD-<change>.md` (full corpus-specific numbers)

The runner refuses to write only one — both or neither.
