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

## Container DNS

The Compose stack gives application containers explicit recursive DNS defaults
so outbound provider endpoints resolve consistently even when the host's Docker
daemon inherited a local resolver that containers cannot reach. Override them
when your environment requires a private resolver:

```bash
IRONRAG_DOCKER_DNS_PRIMARY=192.0.2.53 \
  IRONRAG_DOCKER_DNS_SECONDARY=192.0.2.54 \
  docker compose up -d
```

## Host profiles

| Profile | Host | Corpus shape | Notes |
| --- | --- | --- | --- |
| Evaluation | 4 vCPU, 8–12 GiB RAM, 50+ GB disk | Largest library up to ~25k chunks | Good for trials and demos. Avoid large high-dimensional vector rebuilds on this tier. |
| Standard | 4–8 vCPU, 16 GiB RAM, 100–150 GB disk | Largest library up to ~250k chunks; total corpus may be larger across many libraries | Matches the default Compose memory budget. Suitable for normal self-hosted use when rebuilds are occasional. |
| Large | 8–16 vCPU, 24–32 GiB RAM, 250+ GB disk | Largest library ~250k–1M chunks, high ingest concurrency, or full vector rebuilds | Raise the `IRONRAG_*_MEMORY_LIMIT` caps (see above) or equivalent Helm resource overrides. |

## Runtime graph prewarm

Runtime graph projection prewarm is disabled by default:

```text
IRONRAG_RUNTIME_GRAPH_PROJECTION_PREWARM_ENABLED=false
IRONRAG_RUNTIME_GRAPH_PROJECTION_PREWARM_MAX_LIBRARIES=0
```

Leave it disabled on constrained or multi-library hosts. Lazy per-library graph
loading keeps steady query service available without allocating every active
library graph at API startup. Enable prewarm only when the host has enough free
RAM for the largest active graph projections and first-turn graph latency is a
known operational bottleneck. When enabling it, use
`IRONRAG_RUNTIME_GRAPH_PROJECTION_PREWARM_MAX_LIBRARIES` to cap startup memory
exposure before raising API memory limits.

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
baseline light enough for small corpora and weak hosts. To spread ingest work
more independently (large back-catalog imports, many libraries loading at once),
run more workers from a single variable:

```bash
IRONRAG_WORKER_REPLICAS=4 docker compose up -d
```

On Kubernetes set `worker.replicaCount` in
[`charts/ironrag/values.yaml`](../../charts/ironrag/values.yaml). The Helm chart
also reserves one API rollout surge slot in the DB budget so API rolling updates
can stay available without opening surprise Postgres backends.

If you scale API replicas outside Helm, set `IRONRAG_API_REPLICAS` to the number
of API processes that can be alive at once. For rolling updates, include the
surge process in that count. Compose defaults to one API and one worker; Helm
derives the API count from `api.replicaCount + 1` and the worker count from
`worker.replicaCount`.

Scale-out is safe at any time: workers claim queued jobs with
`SELECT … FOR UPDATE SKIP LOCKED` under per-library, per-workspace and global
concurrency caps, so two workers never pick up the same job and no library is
over-subscribed. The default queue caps are:

```text
IRONRAG_INGESTION_MAX_PARALLEL_JOBS_GLOBAL=16
IRONRAG_INGESTION_MAX_PARALLEL_JOBS_PER_WORKSPACE=8
IRONRAG_INGESTION_MAX_PARALLEL_JOBS_PER_LIBRARY=4
```

On Helm, set the same ceilings with
`app.ingestionMaxParallelJobsGlobal`,
`app.ingestionMaxParallelJobsPerWorkspace`, and
`app.ingestionMaxParallelJobsPerLibrary`.

Those are deployment-wide ceilings, not a guarantee that one worker process can
run 16 jobs at once. Each worker also applies a memory-derived local claim cap
from its cgroup soft limit before it asks Postgres for more canonical ingest
jobs. This keeps memory-heavy extraction or graph-merge jobs from stacking in
one process on a swapless host. Raising the ingest caps is useful only when the
host, worker memory limit, worker replica count, database connection budget, and
provider concurrency budget are sized together.

For a 16 GiB host that should keep the default `16 / 8 / 4` queue caps busy,
prefer several moderate workers over one very large worker. A typical starting
point is:

```bash
IRONRAG_WORKER_REPLICAS=4 \
  IRONRAG_WORKER_MEMORY_LIMIT=2560M \
  IRONRAG_DATABASE_MAX_CONNECTIONS=50 \
  docker compose up -d
```

With those values, each worker has enough memory and DB pool headroom to lease
roughly four jobs, while the database queue still enforces 16 globally, 8 per
workspace, and 4 per library. If the visible leased count stops below 16, check
whether all available queued jobs are in one workspace or library before raising
caps.

Each worker keeps its own `IRONRAG_WORKER_MEMORY_LIMIT` budget, so size the
host for `replicas × worker memory` on top of the database and backend.
Docling-backed PDF, office, and OCR extraction also requires enough hard cgroup
headroom for one local extractor process. If the worker cap is too small,
ingestion fails the affected document with `docling_insufficient_memory` before
spawning the extractor; raise `IRONRAG_WORKER_MEMORY_LIMIT` for extractor-heavy
imports rather than relying on retries.
`IRONRAG_DATABASE_MAX_CONNECTIONS` is a deployment-wide app connection budget;
the runtime divides it across the expected API and worker processes and caps
each worker's local claim loop to the DB slots it can service. Adding workers
only increases DB-backed ingest parallelism when that deployment-wide DB budget
has enough headroom for the larger process count. With the default budget, extra
workers mostly spread work and memory isolation across processes; query serving
scales with the backend, not the worker count.

See also: [AI bindings](./AI-BINDINGS.md) (embedding dimension changes),
[Helm chart](../../charts/ironrag/values.yaml) (resource presets),
[README — deployment](../../README.md#other-deployment-options).
