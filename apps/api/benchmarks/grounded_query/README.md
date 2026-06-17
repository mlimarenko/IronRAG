# Grounded Query Benchmarks

These benchmarks run against a live IronRAG API and use only the synthetic
fixtures in this directory.

Set the live target before running a matrix:

```bash
export IRONRAG_SESSION_COOKIE="..."
export IRONRAG_BENCHMARK_WORKSPACE_ID="..."
```

Run the default grounded matrix:

```bash
make benchmark-grounded
```

Run the golden answer matrix:

```bash
make benchmark-golden
```

Both targets write `matrix.result.json`, one `*_suite.result.json` file per
suite, and append a compact rank-metric record to
`rank_metrics_trend.jsonl` under the configured output directory.

Rank metrics are emitted for cases with known relevance. Document relevance
defaults to each case's `expectedDocumentsContains` values. Optional
`rank_relevance.json` entries can add or override `relevantDocuments` and add
`relevantChunks` content markers for stable chunk ranking checks.

The runner reports `hit@1`, `hit@3`, `hit@5`, `hit@10`, and `MRR` per case and
aggregates them under each suite summary and the matrix summary.
The same compact summary is appended to `rank_metrics_trend.jsonl`, making
successive runs comparable without overwriting older evidence.

Compare two output directories:

```bash
python3 apps/api/benchmarks/grounded_query/compare_benchmarks.py \
  tmp-grounded-baseline tmp-grounded-candidate
```
