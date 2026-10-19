# Vector lifecycle observability runbook

This runbook defines the production observation contract for vector lifecycle.
It does not add a second lifecycle state model: the MVCC-consistent inventory
remains the source for vector state, and the durable maintenance lease remains
the source for GC completion state. Do not alert on an absent series until the
inventory and scheduler hooks are deployed and OpenTelemetry export is enabled
for that deployment.

## Safe activation boundary

Telemetry is opt-in. Set `IRONRAG_OTEL_ENABLED=true` only together with an
approved OTLP endpoint and metrics exporter. Do not put endpoint credentials,
raw database identifiers, document contents, vector IDs, or error text into
metric labels, dashboards, tickets, or command history.

The collector's OTLP metrics flow through the platform collector to the metrics
backend. They are not the `/metrics` HTTP handler, so an ingestion-path failure
must be ruled out before absence is interpreted as a quiet GC.

## Metric ownership and labels

The worker runs one supervised inventory task at startup and every 30 minutes.
Each sample is one global read-only classification over every shared vector
shard, with a 120-second total budget and a 30-second statement timeout. The
classification CTE is evaluated once per shard in SQL, rather than repeating
the scan for each library. Its cost still depends on the data size. Inventory work
does not block maintenance dispatch and is cancelled with the owning scheduler.
A failed observation preserves the previous last-success timestamp and is logged;
it does not fabricate a failed GC run.

The four chunk categories use the classification CTE embedded from
`scripts/ops/vector-lifecycle-inventory.sql`, also used by the operator collector.
A nullable `library_id` keeps the same query usable for a single-library operator
run without a second query path. Reclaimable backlog is a global total with the
real oldest vector timestamp across every shard; an orphan is not automatically
reclaimable. No durable GC completion leaves the last-success series unset,
never a fabricated zero. An empty backlog overwrites the age with zero using
the same stable class label. Alert on age only when `backlog_vectors > 0`;
mutable state labels would retain an obsolete nonempty-backlog series.

GC metrics are emitted after the durable lease transition. Telemetry error
normalization does not change the persisted error code or sweeper retry policy.
The freshness threshold is measured against one global sample per 30 minutes,
so it tests collector and scheduler health rather than a rotation completing.

All lifecycle metrics use the `ironrag.maintenance` meter. No series carries
`ironrag.library.id`: the catalog is unbounded and a per-library label overflows
the SDK's finite series ceiling. A shard is exactly
`(ironrag.vector.kind, ironrag.vector.dimensions)`. GC also carries a stable
maintenance class and a bounded outcome, reason, or error code. A per-library
drill-down is operator SQL and structured logs (`library_id` + bounded
`error.code`), never a metric label. Correlation identifiers belong in the
associated trace or structured event, never in a metric label.

| Family | Scope and meaning |
|---|---|
| `ironrag.maintenance.vector_lifecycle.inventory.vector_rows` | Global chunk-vector count per shard, split by canonical lifecycle category. |
| `ironrag.maintenance.vector_lifecycle.inventory.logical_embedding_value_bytes` | Sum of embedding value-datum bytes for the same shard and category. It is a logical value size, not table or index storage. |
| `ironrag.maintenance.vector_lifecycle.inventory.physical_shard_bytes` | Exact shared relation footprint for one shard. It has no library or lifecycle-category label. |
| `ironrag.maintenance.vector_lifecycle.inventory.last_success_unix_seconds` | Last successful global inventory observation. It is label-free. |
| `ironrag.maintenance.vector_lifecycle.gc.runs`, `candidate_documents`, `deleted_vectors`, `skipped_runs`, `errors`, `duration_seconds` | Aggregate GC outcomes. A skipped run has a bounded reason; an error has only its stable error code. |
| `ironrag.maintenance.vector_lifecycle.gc.backlog_vectors`, `backlog_oldest_age_seconds`, `last_success_unix_seconds` | Global GC backlog and the latest durable completion across all libraries. |

The category values are exactly `currentReadable`,
`protectedActiveIngest`, `supersededReadableHistory`, and
`inconsistentOrOrphan`. They apply only to chunk vectors. Entity vectors are
outside revision-lifecycle categories and must not be put in an invented
`unknown` series.

Operational label values are finite as well: `outcome` is `success`,
`skipped_active_ingest`, `skipped_library_busy`, or `failed`; `skip.reason`
is `active_ingest` or `library_busy`; and `deletion.reason` is `null_head` or
`superseded_readable_history`.
`error.code` is one of `catalog_lookup`, `lease_complete`, `lease_cursor`,
`lease_fail`, `lease_requeue`, `postgres`, `runtime_graph`,
`unsupported_class_scope`, or `vector_plane`. A new value is a schema change
and must be declared, reviewed, and
gated before it can be emitted.

## Bytes: do not double count

Vector relations and indexes are shared by libraries. Therefore,
`physical_shard_bytes` is grouped only by shard and summed once. Never attach a
library label to it, and never combine it with the logical embedding value as
though the two were additive.

For a library view, use `logical_embedding_value_bytes`. For a storage view,
use `physical_shard_bytes`. A library's logical vectors may be only part of one
shared physical relation, and no proportional physical-byte estimate is valid.

## Alert and triage rules

- Calculate superseded share only from vector-row categories in the same
  `(kind, dimensions)` scope. A physical-byte metric is not its denominator.
- Backlog age and missing success are global. The last-success gauge is the
  latest completion of ANY library, so it is never a claim that every library is
  drained. For per-library completion and error state, use operator SQL and the
  structured logs keyed by `library_id` and bounded `error.code`.
- Use bounded `error.code` as the alert grouping key. Find correlated job-run
  and trace evidence through approved operator tooling; never add error text to
  a label just to make the alert more descriptive.
- Distinguish `skipped_runs{skip.reason="active_ingest"}` and
  `skipped_runs{skip.reason="library_busy"}` from a failed run. The first is
  a protective ingest skip; the second is catalog-row contention. Neither is
  a deletion failure.
- Do not manually delete vector rows, relations, or HNSW indexes as an alert
  response. Resolve the scoped maintenance failure and let the durable
  scheduler retry safely.

An operator dashboard may show physical footprint, GC activity, and read
latency/cache context together, but every panel needs a live source.

## Investigation sequence

The database integration test `postgres_gc_removes_only_superseded_vectors`
checks the actual seeded preview and page report counts directly; a successful
zero-count no-op fails it. Captured SDK tests check emitted names, units and
attributes against the metric schema without inspecting source text.
The five ignored `vector_inventory::tests` cases exercise the runtime read path,
read-only timeout, global sum versus single-library scope isolation, orphan
classification, global backlog age and latest/NULL/absent last-success against
private migrated databases.

1. Read the global inventory and backlog series for the maintenance class, plus
   the exact shard labels when present. Confirm the latest-success and backlog
   values were sampled in the same 30-minute tick.
2. Separate an OpenTelemetry ingestion outage from a scoped maintenance run
   that stopped succeeding; record the two cases independently.
3. Use the durable maintenance run and its correlated trace or structured event
   to inspect the bounded error code and retry state. The global last-success is
   the newest completion of ANY library and proves the scheduler is alive, not
   that every library is drained: drill into a single library with operator SQL
   and its `library_id`-keyed logs.
4. If inventory reports `inconsistentOrOrphan`, use the canonical read-only
   lifecycle inventory procedure to establish relation and category facts before
   scheduling a repair.
