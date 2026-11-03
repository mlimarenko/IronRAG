# Isolated lexical replay

This opt-in experiment calls the real `PgSearchStore` without `AppState`, API
workers, embeddings or generation. It does not install a product backend or
change production SQL, migrations, RRF or other retrieval channels.

## Treatments

| Arm | Matching and ranking |
| --- | --- |
| `production_fts` | Actual `ChunkLexicalQuery`, `UserQuery`, default `simple` configuration, including title lanes, identifier policy, boosts, diversification and relaxation |
| `raw_fts` | PostgreSQL `websearch_to_tsquery('simple', ...)` and `ts_rank_cd`, without those policies |
| `raw_bm25` | Native `pg_search` parser and BM25 score, without those policies |

Both diagnostic arms see the same unaccented text as the generated production
FTS column: content plus normalized text, with empty window, section and heading
fields. Each selected UTF-8 fixture is one ready chunk. This bypasses extraction
and ordinary chunking, so the result is not an ingest or end-to-end RAG benchmark.
The production reference uses admitted source revisions and readable heads from
the ordinary fresh-database migration, not an imitation search schema.

PostgreSQL's `simple` configuration and ParadeDB's default tokenizer do not have
identical identifier or phrase semantics. Native ordinary query tokens are quoted
and joined with `AND`; explicit Boolean probes use minus exclusion. On the
measured `0.26.2` extension, `AND NOT` did not implement the intended exclusion.
The manifest retains both query strings. Even the diagnostic pair compares
engines/analyzers, not just scoring formulas. The production/raw comparison
also changes search policy; do not attribute that difference to BM25 alone.

## Disposable target

Use a new local PostgreSQL 18 database with `vector` and actual `pg_search`
`0.26.2`, preloaded through `shared_preload_libraries=pg_search`. The adjacent
Dockerfile pins the official base and extension packages used by IRG-285.
Package licenses remain installed; `pg_search` is AGPLv3. Apt dependency
repositories are not snapshotted, so retain the resulting image digest and
package list with each run. Do not trust a version tag instead of `extversion`.

Run the container with a unique name and new storage, a loopback-only dynamic
published port, finite CPU/memory limits, and a host-source-restricted `pg_hba.conf`.
Do not mount application data, use host networking, or reuse a shared database.
The runner requires an explicit password-free
`postgresql://postgres@127.0.0.1:<port>/irg285_<run>?sslmode=disable` DSN and refuses
nonempty databases. It never reads `DATABASE_URL` or an application setting.
Create a new database for each run; no reset/drop command is built in.

## Run from the repository root

```sh
python3 apps/api/benchmarks/grounded_query/lexical_experiment.py prepare \
  --output /data/artifacts/<run>/manifest.json

CARGO_TARGET_DIR=/data/targets/<worktree> cargo build --locked \
  -p ironrag-backend --example lexical_replay

/data/targets/<worktree>/debug/examples/lexical_replay \
  --dsn 'postgresql://postgres@127.0.0.1:<port>/irg285_<run>?sslmode=disable' \
  --manifest /data/artifacts/<run>/manifest.json \
  --output /data/artifacts/<run>/native.json

python3 apps/api/benchmarks/grounded_query/lexical_experiment.py evaluate \
  --manifest /data/artifacts/<run>/manifest.json \
  --receipt /data/artifacts/<run>/native.json \
  --output /data/artifacts/<run>/evaluation.json
```

Preparation freezes the eleven UTF-8 hardening fixtures, reuses the suite's
queries and relevance registry, and adds Boolean/identifier controls. PDF/PNG
fixtures and their dependent cases are explicitly excluded. The evaluator uses
the existing `compute_rank_metrics`, not the live answer comparator. It refuses
incomplete arm/query pairs, mismatched identities/checksums and nonfinite data.
Duplicate chunks invalidate either raw arm. If the unchanged production store
returns duplicate chunks, its complete native trace is retained and identified
by result index, its status is `invalid_reference`, and its aggregate rank metrics
are withheld. That does not turn the malformed product reference into an accepted
search result or discard the independently valid diagnostic pair.
The overall report then says `partial_diagnostic`, not that all three arms passed.

Each query gets two warmups and fifteen measured repetitions per arm, with
rotating arm order and identical ordered results required across repetitions.
Latency includes the client/database round trip and result decoding, but not
hashing, setup or index construction. Report warm median/p95 separately from
setup. The small corpus does not establish production latency or capacity.
No OS/page-cache flush is attempted. Raw index sizes and build times are
diagnostics; production index maintenance is included in migration/seed work.

Retain source SHA, binary SHA-256, migration and manifest hashes, image/container
identities, actual extension versions, native receipts, evaluator output and
container resource observations outside git. Resource totals for the combined
experiment are not per-arm CPU measurements. Final-answer, UI and MCP quality
remain **not measured**; adoption remains **not decided** under IRG-255.
