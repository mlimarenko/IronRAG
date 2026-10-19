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

## Filtered HNSW query bounds

Vector relations are shared by dimension; library, embedding profile, and
vector kind are logical filters inside the relation. pgvector therefore runs
filtered HNSW searches with strict iterative scanning and explicit bounds:

```text
IRONRAG_PG_HNSW_BUILD_BUDGET_BYTES=3000000000
IRONRAG_PG_HNSW_EF_SEARCH=400
IRONRAG_PG_HNSW_MAX_SCAN_TUPLES=50000
IRONRAG_PG_HNSW_SCAN_MEM_MULTIPLIER=2
IRONRAG_PG_HNSW_EXACT_FALLBACK_MAX_ROWS=10000
```

The runtime clamps these values to `1..10000`, `1..1000000`, `1..64`, and
`0..100000` respectively. If an approximate scan returns fewer rows than the
requested limit even though the logical-lane manifest contains enough rows,
IronRAG emits a structured warning. Lanes no larger than the exact-fallback
threshold receive one exact retry; `0` disables that retry. Larger lanes remain
bounded and report the underfill instead of risking an unbounded sequential
distance sort.

Raise scan tuples or the memory multiplier only after comparing answer quality,
query p95, and PostgreSQL memory. The scan-memory multiplier scales pgvector's
use of `work_mem`, so increasing it across many concurrent searches can consume
substantial database memory.

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
over-subscribed. The default caps are deliberately conservative:

```text
IRONRAG_INGESTION_MAX_PARALLEL_JOBS_GLOBAL=4
IRONRAG_INGESTION_MAX_PARALLEL_JOBS_PER_WORKSPACE=2
IRONRAG_INGESTION_MAX_PARALLEL_JOBS_PER_LIBRARY=1
```

The provider gateway applies a second, endpoint-scoped backpressure layer:

```text
IRONRAG_PROVIDER_CONCURRENCY_MAX_OUTBOUND=16
IRONRAG_PROVIDER_CONCURRENCY_QUERY_RESERVED=4
IRONRAG_PROVIDER_CONCURRENCY_ACQUIRE_TIMEOUT_MS=30000
IRONRAG_PROVIDER_RATE_LIMIT_REQUESTS_PER_SECOND=8
IRONRAG_PROVIDER_RATE_LIMIT_BURST=16
IRONRAG_PROVIDER_RETRY_MAX_ATTEMPTS=3
IRONRAG_PROVIDER_RETRY_INITIAL_BACKOFF_MS=200
IRONRAG_PROVIDER_RETRY_MAX_BACKOFF_MS=5000
```

Ingest can use only the shared 12 permits, so it cannot starve interactive
queries. Each endpoint also has an independent token bucket. Retry delays use
bounded exponential backoff with jitter and respect valid `Retry-After`
headers up to the configured maximum. Permit and token waits are bounded and
fail closed. `0`/`0` disables a budget; a concurrency reserve equal to the
maximum is rejected at startup because it would deadlock ingest.

The budget is **per process**, not global. Every API replica, every worker,
and every `ironrag-maintenance` or `ironrag-cli` invocation keeps its own
in-memory registry. The rate a provider actually sees is therefore
`running processes × IRONRAG_PROVIDER_RATE_LIMIT_REQUESTS_PER_SECOND`; size
that product against the provider's quota, not the single-process number.

Setting `IRONRAG_PROVIDER_CONCURRENCY_MAX_OUTBOUND=0` does not opt an
endpoint out of the registry, because the rate limiter is on by default and
needs an entry of its own. Such endpoints count against
`IRONRAG_PROVIDER_CONCURRENCY_REGISTRY_MAX_ENTRIES`, which fails closed when
it is full. Set both the concurrency maximum and the requests-per-second to
`0` for an endpoint that must stay entirely unmetered.

Each worker also applies a memory-derived local claim cap from its cgroup soft
limit before it asks Postgres for more ingest jobs. This keeps
memory-heavy extraction or graph-merge jobs from stacking in one process on a
swapless host. Raising `IRONRAG_WORKER_REPLICAS` or the ingest caps is useful
only when the host, worker memory limit, database connection budget, and
provider concurrency budget are raised together.

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

The 16-connection per-process cap is gone: `IRONRAG_DATABASE_MAX_CONNECTIONS`
is divided across API+worker replicas and that quotient is the process budget.
Outermost retrieval turns also pass a bounded admission semaphore
(`IRONRAG_QUERY_TURN_MAX_CONCURRENT` default 32, queue 80, 15s wait). A burst
that exceeds the queue is rejected with retryable HTTP 503
`query_admission_rejected` instead of waiting until the 180s turn budget or
exhausting the SQLx pool. Matching-terms retrieval uses `pg_trgm` expression
indexes on `lower(normalized_text)`, `lower(content_text)`, and
`lower(coalesce(window_text, ''))`. The candidate join is a `UNION` of
per-column `LIKE` arms driven from bound `'%term%'` patterns; do not fold
those arms into one `OR` and then join requested revisions first — that plan
ignores `gin_trgm` and seq-filters every chunk in the revision set. Do not
reintroduce a `simple` tsquery prefilter, which misses substring matches
inside URL/`key=value` lexemes.

Bundled Postgres defaults stay conservative for a 1 GiB container:
`max_connections=100`, `shared_buffers=128MB`, `work_mem=4MB`. Raise the
container memory limit before raising `shared_buffers` or `work_mem`.

### Sizing Postgres memory to your hardware

Scale the three compose variables together from the database container's
memory limit (`IRONRAG_DB_MEMORY_LIMIT`):

| DB container limit | `IRONRAG_POSTGRES_SHARED_BUFFERS` | `IRONRAG_POSTGRES_WORK_MEM` | `IRONRAG_POSTGRES_SHM_SIZE` |
| --- | --- | --- | --- |
| 1 GiB (default) | 128MB | 4MB | 256mb |
| 4 GiB | 1GB | 8MB | 1g |
| 6 GiB | 3GB | 12MB | 2g |
| 12 GiB | 6GB | 16MB | 2g |
| 24 GiB+ | 8–12GB | 32MB | 4g |

The rules behind the table:

- **`shared_buffers` ≈ 40–50 % of the container limit.** The rest of the
  cgroup holds each backend's private memory and the kernel page cache for
  reads that miss the buffer pool (page cache is charged to the container's
  cgroup too, but it is reclaimable, so it does not OOM the container).
- **`work_mem` is charged per sort/hash node per backend**, so it multiplies
  by concurrency in a way `shared_buffers` does not. At
  `max_connections=300`, 32 MB puts the worst case far past a 6 GiB
  container; prefer growing `shared_buffers` first.
- **`shm_size` sizes `/dev/shm`**, which holds dynamic shared memory for
  parallel queries — its demand follows `work_mem` × parallel workers, not
  `shared_buffers`. 2 GiB is ample for the defaults; raise it if parallel
  aggregates start failing with `could not resize shared memory segment`.
- Whatever you raise, verify the effective values from inside the server —
  these settings arrive on the postmaster command line, so `ALTER SYSTEM`
  cannot change them and `pg_settings.source` says `command line`:

  ```sql
  select name, setting, source from pg_settings
  where name in ('shared_buffers', 'work_mem', 'max_connections');
  ```

The goal of all of it is the cache hit ratio (query above): on a stage node
whose corpus dwarfed its memory, moving `shared_buffers` from 1 GB to 3 GB and
tuning the async-I/O settings below carried a 100-concurrent-turn benchmark
from 34 to 52 completions inside the same admission limits. Prefer a larger
`shared_buffers` over a larger `work_mem` once concurrency is high. At `max_connections=300`, a 32 MB `work_mem` puts
the worst case far past a 6 GiB container.

### Concurrent query serving is bound by disk reads, not CPU

Once the corpus no longer fits in `shared_buffers`, retrieval latency under
concurrency is dominated by physical reads, and the symptom is misleading: the
**application** looks idle. On a stage node serving 100 concurrent turns the
backend used 0.18 of 8 cores while Postgres accumulated ~2300 s of statement
time — it was blocked on the disk, not computing. CPU utilisation cannot
distinguish "waiting on the network" from "waiting on a read"; compare
accumulated statement time against provider call time before concluding a
workload is provider-bound.

Check the cache hit ratio before tuning anything else:

```sql
select round(100.0 * sum(heap_blks_hit)
             / nullif(sum(heap_blks_hit + heap_blks_read), 0), 1) as hit_pct
from pg_statio_user_tables;
```

Below ~95 % means the working set does not fit and every query pays disk
latency. Raising `shared_buffers` narrows the gap but cannot close it when the
database is tens of times larger than the container.

### PostgreSQL 18 asynchronous I/O

PostgreSQL 18 reads sequential scans, **bitmap heap scans**, and vacuum
asynchronously. IronRAG's trigram and full-text lanes are bitmap heap scans, so
they are exactly the shape that benefits, and the shipped defaults
(`io_method=worker`, `io_workers=3`, `effective_io_concurrency=16`) are sized
for a small host.

| Setting | Default | Suggested for a disk-bound node | Why |
| --- | --- | --- | --- |
| `io_workers` | 3 | 8 | Three workers cannot keep a concurrent bitmap-scan workload's read queue full. |
| `effective_io_concurrency` | 16 | 256 (SSD/NVMe) | Read-ahead depth for bitmap heap scans. |
| `maintenance_io_concurrency` | 16 | 64 | Same, for vacuum and index builds. |
| `random_page_cost` | 4.0 | 1.1 (SSD/NVMe) | The 4.0 default encodes seek time on **rotating** disks and pushes the planner away from index scans. Confirm the device first: `lsblk -d -o NAME,ROTA` (`ROTA=0` is solid state). |
| `track_io_timing` | off | on | Without it `pg_stat_io` and `EXPLAIN (BUFFERS)` cannot attribute read time. |

`io_method=io_uring` differs from `worker`, but **Docker's default
seccomp profile blocks the io_uring syscalls**: Postgres refuses to start with
`could not setup io_uring queue: Operation not permitted`. Using it requires a
custom seccomp profile on the container, which is a deliberate security
decision — `worker` with more `io_workers` needs no such exemption.

Recovering from a failed `io_method` is awkward, because the server will not
start to accept an `ALTER SYSTEM` that undoes it. Edit
`postgresql.auto.conf` in the data volume directly while the container is down.

### Expression indexes lose their statistics on REINDEX

An index built on an expression carries its own statistics, and the planner has
nothing else to estimate that expression's selectivity with. `REINDEX INDEX
CONCURRENTLY` builds a *new* index and swaps it in, so those statistics are
gone until the next `ANALYZE` — autovacuum may not come for hours, and the
window is silent.

Measured on a stage node, one `LIKE ANY(...)` over a concatenated-text trigram
index with no expression statistics: the planner estimated **140 rows** where
the scan returned **47 724**, a 341x underestimate that propagates into every
join above it. A plain `ANALYZE` on the table brought the estimate to 10 676 —
still low, but within a factor the planner can work with.

```sql
-- expression indexes whose statistics are missing
select t.relname as table, c.relname as index
from pg_class c
join pg_index i on i.indexrelid = c.oid
join pg_class t on t.oid = i.indrelid
where i.indexprs is not null
  and not exists (select 1 from pg_stats s where s.tablename = c.relname);
```

Always follow a `REINDEX` of an expression index with `ANALYZE` on its table.

Note that `reltuples` on a GIN index is not a reliable health signal: values
far above the table's row count are normal for indexes over large concatenated
expressions, and reindexing may raise the number rather than lower it. Judge
these indexes by the planner's row estimates, not by `reltuples`.

## Compose deployment variants

All variants live in the single `docker-compose.yml`, selected by env and one
profile (see the file header for the full list):

```bash
# With S3-compatible storage (bundled s4core), via the `s4` profile
COMPOSE_PROFILES=s4 \
  IRONRAG_CONTENT_STORAGE_PROVIDER=s3 \
  IRONRAG_DEPENDENCY_OBJECT_STORAGE_MODE=bundled \
  IRONRAG_CONTENT_STORAGE_TOPOLOGY=shared_cluster \
  docker compose up -d

# Local source build. IRONRAG_VERSION is the build identity
# baked into the image and reported by /v1/version; the build fails without it
# instead of shipping a placeholder. `make docker-local-build` derives it from
# git automatically.
IRONRAG_VERSION=$(git describe --always --abbrev=40 --dirty --match '') \
  IRONRAG_BACKEND_IMAGE=ironrag-backend:local \
  IRONRAG_FRONTEND_IMAGE=ironrag-frontend:local \
  docker compose build backend frontend
IRONRAG_BACKEND_IMAGE=ironrag-backend:local \
  IRONRAG_FRONTEND_IMAGE=ironrag-frontend:local \
  docker compose up -d
```

### Helm (Kubernetes)

```bash
helm upgrade --install ironrag charts/ironrag \
  --namespace ironrag --create-namespace \
  --set-string app.providerSecrets.provider_alpha="${PROVIDER_API_KEY}" \
  --wait --timeout 20m
```

By default the chart deploys the API, worker, and web images with the
`v<appVersion>` image tag derived from `Chart.appVersion`. Override
`api.image.tag`, `worker.image.tag`, and `web.image.tag` only when pinning a
different published image. Bundled dependencies match Docker Compose:
`pgvector/pgvector:pg18` and `redis:8.10.1`.

See also: [AI bindings](./AI-BINDINGS.md) (embedding dimension changes),
[Install](./INSTALL.md), [Helm chart](../../charts/ironrag/values.yaml).
