# IronRAG benchmarks

IronRAG keeps its grounded-query datasets in `apps/api/benchmarks/grounded_query/`. The benchmark corpus is test data, not operator documentation; the commands and evaluation contract live here.

## Corpus layout

```text
apps/api/benchmarks/grounded_query/
├── corpus/
│   ├── wikipedia/   general knowledge articles
│   ├── docs/        technical docs and contract fixtures
│   ├── code/        code and config files
│   ├── documents/   PDF, DOCX, PPTX fixtures
│   ├── graph/       multi-hop graph topology fixtures
│   └── fixtures/    upload-path smoke fixtures
├── *.json           suite definitions
├── rank_relevance.json
├── run_live_benchmark.py
└── compare_benchmarks.py
```

## Suites

| Suite | Purpose |
|---|---|
| `api_baseline_suite` | single-document retrieval quality |
| `workflow_strict_suite` | multi-document grounded QA |
| `layout_noise_suite` | extraction robustness on noisy layouts |
| `graph_multihop_suite` | graph-backed traversal quality |
| `multiformat_surface_suite` | multi-format upload and extraction |
| `technical_contract_suite` | exact technical literals: endpoints, parameters, absent capabilities, transport comparisons |
| `golden_*_suite` | broader programming, infrastructure, protocol, code, and multi-format coverage |

`technical_contract_suite` is the exact-literal quality gate. Run it whenever query retrieval, grounding, MCP search/read behavior, or answer assembly changes.

## Running the benchmarks

```bash
export IRONRAG_SESSION_COOKIE="..."
export IRONRAG_BENCHMARK_WORKSPACE_ID="..."

make benchmark-grounded-seed
make benchmark-grounded-all
make benchmark-grounded-technical
make benchmark-golden
```

`make benchmark-grounded` uses the `IRONRAG_BENCHMARK_SUITES` matrix and writes
to `tmp-grounded-benchmarks/`. `make benchmark-golden` switches to the broader
`golden_*_suite` matrix and writes to `tmp-golden-benchmarks/`.

For release candidates, pair the public suites with a private live smoke on
representative operator data. Keep the private prompts, document labels, and
expected strings outside git; publish only sanitized aggregate evidence such as
HTTP status, lifecycle state, verifier state, answer length, source count,
matched structural markers, and whether forbidden generic markers were absent.
Setup/procedure changes should include at least:

- one broad multi-variant setup request that must answer instead of clarifying;
- one focused setup request that must stay on the focused evidence path;
- one versioned procedure request that must retrieve a transition
  procedure instead of a adjacent transition or compatibility page;
- one application-update procedure request that must expose an ordered
  grounded sequence.

## Direct scripts

```bash
python3 apps/api/benchmarks/grounded_query/run_live_benchmark.py --help
python3 apps/api/benchmarks/grounded_query/compare_benchmarks.py old.json new.json
```

`compare_benchmarks.py` accepts two result directories and reports pass/fail
movement, graph topology deltas, and retrieval rank-metric deltas.

## Result contract

Benchmark runs write to `tmp-grounded-benchmarks/` by default and include:

- per-case pass/fail details,
- `failedChecks` for each broken assertion,
- suite-level `failureReasonCounts`,
- suite-level and matrix-level `summary.rankMetrics`,
- appended `rank_metrics_trend.jsonl` records in the output directory,
- latency and evidence metadata for each case.

The goal is not only pass/fail. The output should tell you whether a drop came from retrieval, answer assembly, evidence selection, or verification.

## Retrieval rank metrics

For cases with known relevance, the live runner records ordered retrieval
quality separately from answer correctness:

- document rank metrics use the expected document checks from each suite unless
  `rank_relevance.json` overrides them with explicit `relevantDocuments`;
- chunk rank metrics use optional `relevantChunks` marker strings matched
  against retrieved chunk text;
- each metric family reports `hit@1`, `hit@3`, `hit@5`, `hit@10`, `MRR`, and
  `caseCount`.

The relevance data is synthetic fixture data only. Private or operator-specific
corpora should stay outside this repository; publish only sanitized aggregate
evidence.

`searchQuery` is the exact input sent to the question-agnostic search endpoint.
When a document-rank expectation depends on a public subject disambiguator from
the benchmark question, keep that disambiguator in `searchQuery` instead of
expecting search to infer it.

## Large-document ingest smoke

Large private ingest corpora are not stored in this public repository. When
validating changes to Docling, chunking, embedding, graph extraction, or worker
leases, run the private large-document smoke and record only sanitized evidence
in public docs:

- all files reached `ready`;
- resumed jobs reused completed PDF page-range units;
- graph topology was non-empty after finalization;
- encoding scanners found no mojibake in persisted graph labels or page units;
- document UI showed stage progress, model, duration, calls, and cost;
- public `make check` passed.
