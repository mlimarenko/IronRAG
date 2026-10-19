# Grounded Query Benchmarks

One self-contained benchmark registry. Every suite lives under the
`golden_` prefix, every corpus file is referenced by at least one suite,
and everything runs against a live IronRAG API using only the synthetic
fixtures in this directory.

## The registry

| Suite | Purpose |
|---|---|
| golden_programming | Core programming-corpus answer quality |
| golden_infrastructure | Infrastructure-corpus answer quality |
| golden_protocols | Protocol-corpus answer quality |
| golden_code | Source-code corpus: declarations, structure, literals |
| golden_multiformat | Business documents: docx, pdf, xlsx, pptx content QA |
| golden_hardening | Hard classes: numeric facts with units (regex), compound literals, cross-document hops, config comments, JSONL records, Russian corpus, near-twin distractors, wide tables, deep declaration reach, pdf/png extracts |
| golden_api_baseline | Broad API-surface baseline over the mixed corpus |
| golden_workflow_strict | Ordered procedures and strict workflow answers |
| golden_layout_noise | Answers under noisy/laid-out corpus (incl. wikipedia noise) |
| golden_graph_multihop | Graph-lane multi-hop reasoning topologies |
| golden_upload_surface | Upload smoke over every binary format (docx, pdf, pptx, png, jpg) |
| golden_technical_contract | Technical-contract literal fidelity |

The full registry is wired as `IRONRAG_GOLDEN_SUITES` in the repository
Makefile; `IRONRAG_BENCHMARK_SUITES` defaults to the same list.

## Corpus layout

- `corpus/docs/` — markdown documents (incl. one Russian runbook)
- `corpus/code/` — source-code fixtures (py, go, rs, yaml, txt, …)
- `corpus/documents/` — binary business documents (docx, pdf, xlsx, pptx)
- `corpus/fixtures/` — upload-surface fixtures (png, jpg, pdf, docx, pptx),
  runtime configs (yaml), structured records (jsonl)
- `corpus/graph/` — multi-hop graph topologies
- `corpus/wikipedia/` — public-derived noise/OCR material

Do not add a corpus file without referencing it from a suite: unreferenced
files are dead weight and get removed.

## Case discipline (probe contract)

- Every `answerRequiredAll` / `answerRequiredAny` string must exist
  verbatim in the fixture that `expectedDocumentsContains` points to.
- `searchRequiredAll` must hold for the expected top document — prefer a
  token unique to that document.
- Numeric facts with units belong in `answerRequiredRegexAny` with
  patterns tolerant to inline markdown (`[*_\`]*` between number and
  unit); regexes run case-insensitively over the raw answer text.
- `allowedVerificationStates` may only contain successful states
  (`verified`, `partially_supported`); stricter (verified-only) is fine.
- No natural-language keyword dictionaries; anchors are identifiers,
  literals, and document-local structure.

## Running

Set the live target:

```bash
export IRONRAG_SESSION_COOKIE="..."
export IRONRAG_BENCHMARK_WORKSPACE_ID="..."
```

Seed a fresh library with the full registry corpus, then run it:

```bash
make benchmark-golden-seed
make benchmark-golden IRONRAG_BENCHMARK_LIBRARY_ID=<library-id>
```

Per-dimension convenience targets: `benchmark-grounded-noisy-layout`,
`benchmark-grounded-multihop`, `benchmark-grounded-technical`.

Each run writes `matrix.result.json`, one `*_suite.result.json` per
suite, and appends rank metrics (`hit@k`, `MRR`) to
`rank_metrics_trend.jsonl` in the output directory. Compare two runs:

```bash
python3 apps/api/benchmarks/grounded_query/compare_benchmarks.py \
  <baseline-dir> <candidate-dir>
```

Certification protocol: paired runs on a quiet host (the runner enforces
PSI/swap-IO eligibility), deterministic bindings (temperature 0.0,
top_p 1.0), compare failure SETS across the pair — totals alone prove
nothing.
