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
│   └── fixtures/    upload-path smoke fixtures
├── *.json           suite definitions
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

## Direct scripts

```bash
python3 apps/api/benchmarks/grounded_query/run_live_benchmark.py --help
python3 apps/api/benchmarks/grounded_query/compare_benchmarks.py old.json new.json
```

## Result contract

Benchmark runs write to `tmp-grounded-benchmarks/` by default and include:

- per-case pass/fail details,
- `failedChecks` for each broken assertion,
- suite-level `failureReasonCounts`,
- latency and evidence metadata for each case.

The goal is not only pass/fail. The output should tell you whether a drop came from retrieval, answer assembly, evidence selection, or verification.
