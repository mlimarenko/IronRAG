# Capacity planning

IronRAG sizing is driven by **chunks**, graph density, embedding dimension, and
ingest / vector-rebuild concurrency. Source document count alone is a weak
predictor: many small libraries are cheaper to query than one very large
library, and steady query serving needs much less RAM than OCR, graph
extraction, or vector index rebuilds.

Trial deployments can be smaller, but the default Docker Compose stack
targets a **16 GiB** host. On larger hosts, raise the per-role memory caps
via env (no separate overlay file):

```bash
IRONRAG_DB_MEMORY_LIMIT=6144M \
  IRONRAG_BACKEND_MEMORY_LIMIT=4096M \
  IRONRAG_WORKER_MEMORY_LIMIT=4096M \
  docker compose up -d
```

## Host profiles

| Profile | Host | Corpus shape | Notes |
| --- | --- | --- | --- |
| Evaluation | 4 vCPU, 8–12 GiB RAM, 50+ GB disk | Largest library up to ~25k chunks | Good for trials and demos. Avoid large high-dimensional vector rebuilds on this tier. |
| Standard | 4–8 vCPU, 16 GiB RAM, 100–150 GB disk | Largest library up to ~250k chunks; total corpus may be larger across many libraries | Matches the default Compose memory budget. Suitable for normal self-hosted use when rebuilds are occasional. |
| Large | 8–16 vCPU, 24–32 GiB RAM, 250+ GB disk | Largest library ~250k–1M chunks, high ingest concurrency, or full vector rebuilds | Raise the `IRONRAG_*_MEMORY_LIMIT` caps (see above) or equivalent Helm resource overrides. |

## Disk planning

```text
database disk ~= chunks * (10–20 KB content+graph)
              + vector rows * embedding_dim * component_size * index_factor
              + original file storage
              + 20–30% headroom
```

`component_size` is `4` bytes for `vector(dim)` at `dim <= 2000`, and `2`
bytes for `halfvec(dim)` at `dim > 2000`. As a rule of thumb, 1536-dim vectors
and 3072-dim half-vectors both store about 6 KB of raw vector payload per row.
HNSW and SQL indexes add overhead, so use `index_factor = 2–3` for planning.

| Embedding size | Raw vector payload per 100k rows | Raw vector payload per 1M rows |
| --- | ---: | ---: |
| 384-dim `vector` | ~150 MB | ~1.5 GB |
| 1536-dim `vector` | ~600 MB | ~6 GB |
| 3072-dim `halfvec` | ~600 MB | ~6 GB |

For example, a corpus with one million chunks and 3072-dim embeddings needs
about 6 GB of raw chunk-vector payload before vector indexes, graph rows,
source files, WAL, and backup space. A multi-library corpus with several
million total chunks can still fit the standard RAM profile if the largest
active library is moderate and rebuilds are scheduled carefully; disk grows
with the total corpus.

## Vector rebuild spikes

Vector index rebuilds are the main memory spike. If you switch an embedding
binding to a different dimension or rebuild a high-dimensional shard, run it in
a maintenance window and raise the memory caps for million-row shards.

## Scaling ingest workers

Ingest — extraction, chunking, embedding, and graph build — runs in a separate
**worker** service. The default stack runs **one** worker, which keeps the
baseline light enough for small corpora and weak hosts. To ingest faster (large
back-catalog imports, many libraries loading at once), run more workers from a
single variable:

```bash
IRONRAG_WORKER_REPLICAS=4 docker compose up -d
```

On Kubernetes set `worker.replicaCount` in
[`charts/ironrag/values.yaml`](../../charts/ironrag/values.yaml).

Scale-out is safe at any time: workers claim queued jobs with
`SELECT … FOR UPDATE SKIP LOCKED` under per-library, per-workspace and global
concurrency caps, so two workers never pick up the same job and no library is
over-subscribed. Each worker keeps its own `IRONRAG_WORKER_MEMORY_LIMIT` budget,
so size the host for `replicas × worker memory` on top of the database and
backend. Adding workers speeds up ingest only; query serving scales with the
backend, not the worker count.

See also: [AI bindings](./AI-BINDINGS.md) (embedding dimension changes),
[Helm chart](../../charts/ironrag/values.yaml) (resource presets),
[README — deployment](../../README.md#other-deployment-options).
