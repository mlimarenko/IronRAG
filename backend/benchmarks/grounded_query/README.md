# Grounded Query Live Benchmark

Live benchmark harness for real RustRAG ingestion + retrieval + grounded answer quality.

Suites:
- `grad_api_suite.json`: baseline literal-fidelity and grounded-answer regression gate.
- `grad_agent_workflows_suite.json`: stricter graph-backed agent workflow suite with higher chunk/entity/relation thresholds.

Both suites:
- upload the same three API-spec PDFs from `/home/leader/Nextcloud/Personal/GRAD/`
- wait until documents become readable
- keep polling until the pipeline goes quiet or the timeout expires
- run fixed grounded questions
- score:
  - top retrieved document correctness
  - whether retrieved chunk text contains required facts
  - whether the final answer preserves those facts
  - whether the query execution actually uses graph references when the case requires it
  - whether the system refuses to invent unsupported APIs when the case is intentionally absent from the corpus

Usage:

```bash
cd backend
RUSTRAG_SESSION_COOKIE='<session-cookie>' \
python3 benchmarks/grounded_query/run_live_benchmark.py \
  --workspace-id 019d203e-9bb1-7042-b514-57fdcdaebe01 \
  --output /tmp/rustrag-grounded-benchmark.json
```

Strict mode for regression gates:

```bash
cd backend
RUSTRAG_SESSION_COOKIE='<session-cookie>' \
python3 benchmarks/grounded_query/run_live_benchmark.py \
  --workspace-id 019d203e-9bb1-7042-b514-57fdcdaebe01 \
  --strict
```

Recommended interpretation:
- `topDocumentPassRate` isolates ranking quality.
- `retrievalPassRate` shows whether chunk retrieval exposes the required facts.
- `answerPassRate` shows whether the answer stage preserves grounded facts without hallucinating over them.
- `graphUsagePassRate` shows whether execution still routes through chunk/entity/relation references instead of silently falling back to a graph-blind path.
- `strictCasePassRate` is the real regression-gate score: document ranking + retrieval facts + final answer + required graph participation.

If retrieval passes but answer fails, the main problem is answer fidelity, not chunk discovery.
If answer passes but graph usage fails, the system is still vulnerable to regressing into a chunk-only assistant while the benchmark looks green.

Fast rerun on an existing ready corpus:

```bash
cd backend
RUSTRAG_SESSION_COOKIE='<session-cookie>' \
python3 benchmarks/grounded_query/run_live_benchmark.py \
  --workspace-id 019d203e-9bb1-7042-b514-57fdcdaebe01 \
  --library-id <existing-library-id> \
  --skip-upload \
  --output /tmp/rustrag-grounded-benchmark.json
```

Canonical local regression path from the repo root:

```bash
RUSTRAG_SESSION_COOKIE='<session-cookie>' \
RUSTRAG_BENCHMARK_WORKSPACE_ID='019d203e-9bb1-7042-b514-57fdcdaebe01' \
RUSTRAG_BENCHMARK_LIBRARY_ID='<existing-library-id>' \
make benchmark-grounded-all
```

This writes one JSON result per suite into `tmp-grounded-benchmarks/`.
