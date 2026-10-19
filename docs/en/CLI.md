# IronRAG CLI

[Overview](./README.md) | [IAM](./IAM.md) | [MCP](./MCP.md)

Command-line tool for IronRAG admin operations. Connects directly to PostgreSQL.

## Build

```bash
cargo build --release -p ironrag-backend --bin ironrag-cli
```

The binary is also included in the Docker image at `/usr/local/bin/ironrag-cli`.

## Configuration

The CLI reads the same environment variables as the backend server. The required variable is `DATABASE_URL` (or the equivalent setting from the application config).

## Commands

### Print CLI version

```bash
ironrag-cli version
```

Prints the CLI build version (matches the `ironrag-backend` crate version).

### List users

```bash
ironrag-cli list-users
```

Displays a table of all users with login, display name, status, and creation date.

### IAM administration

User lifecycle, password reset, grants, and token lifecycle mutations are intentionally not
available in the direct-database CLI. Use the authenticated `/v1/iam` service endpoints so the
human administrator role, resource scope validation, and audit trail are enforced. API tokens
use the documented `inrg_` format.

### List workspaces

```bash
ironrag-cli list-workspaces
```

Displays all workspaces with ID, slug, display name, lifecycle state, and creation date.

### Create workspace

```bash
ironrag-cli create-workspace <SLUG> [--name "Display Name"]
```

Creates a new workspace.

Options:
- `-n, --name` -- display name (defaults to slug if omitted)

### List libraries

```bash
ironrag-cli list-libraries <WORKSPACE>
```

Lists all libraries in a workspace. The workspace can be specified by slug or UUID.

### Create library

```bash
ironrag-cli create-library <WORKSPACE> <SLUG> [--name "Display Name"] [--description "Description"]
```

Creates a new library in the specified workspace.

Options:
- `-n, --name` -- display name (defaults to slug if omitted)
- `-d, --description` -- library description

## Docker usage

```bash
docker exec <container> ironrag-cli list-users
docker exec <container> ironrag-cli list-tokens
docker exec <container> ironrag-cli list-workspaces
docker exec <container> ironrag-cli create-workspace staging --name "Staging"
docker exec <container> ironrag-cli list-libraries default
docker exec <container> ironrag-cli create-library default docs --name "Documentation" --description "Public docs"
docker exec <container> ironrag-cli backfill-acronym-aliases docs
docker exec <container> ironrag-cli backfill-document-parents docs
```

# IronRAG maintenance CLI

`ironrag-maintenance` is the single operator surface for everything
that keeps the IronRAG storage layer healthy: looking at what is on
disk, removing what can be safely removed, recovering what is in a
broken state, and inspecting the durable scheduler that runs the same
sweepers automatically in the worker role.

Two things to keep in mind throughout:

* The destructive commands documented below do not expose a common `--yes`
  confirmation switch. Run the documented `--dry-run` preview where available,
  then invoke the scoped command deliberately.
* The worker container runs a scheduler that walks the same sweepers
  on a rolling cadence. Manual CLI invocation never conflicts with
  it: both go through the same lease table and would block each other
  cleanly if they ever picked the same row.

## When do I run what?

A short decision guide before diving into the per-subcommand pages
below. Each row links to the command to run.

| Symptom | Start here |
|---|---|
| "Disk is filling up, what is eating it?" | [`audit storage-summary`](#audit-storage-summary) |
| "Retrieval looks slow; suspect bad indexes" | [`audit index-bloat`](#audit-index-bloat) |
| "A document never shows up in answers even though ingest reported success" | [`audit null-head-docs`](#audit-null-head-docs), then [`repair null-heads`](#repair-null-heads) |
| "A lifecycle webhook is dead-lettered and blocks catalog deletion" | [`audit webhook-outbox`](#audit-webhook-outbox), then requeue it with [`repair webhook-outbox-dead-letter`](#repair-webhook-outbox-dead-letter) or explicitly discard it with [`repair webhook-outbox-dead-letter-resolve`](#repair-webhook-outbox-dead-letter-resolve) |
| "Superseded vectors make the hot ANN too large" | [`gc superseded-vectors`](#gc-superseded-vectors) |
| "`runtime_graph_evidence` is bigger than the rest of the DB" | [`gc stale-evidence`](#gc-stale-evidence) |
| "Failed ingest left lots of `null-head` documents" | [`repair null-heads-auto`](#repair-null-heads-auto) |
| "ingest_stage_event is months old, queries are slow" | [`retention stage-events`](#retention-stage-events) |
| "Changed the embedding model for a library; vectors need rebuilding" | [`rebuild vector-plane`](#rebuild-vector-plane) |
| "Want to re-embed an entire library" | [`rebuild vector-plane`](#rebuild-vector-plane) |
| "Graph went out of sync with documents" | [`rebuild runtime-graph`](#rebuild-runtime-graph) |
| "Want to see what the background scheduler is doing right now" | [`lease summary`](#lease-summary) |
| "A sweeper went dead-letter, need to retry after fixing root cause" | [`lease clear-failure`](#lease-clear-failure) |

## Build

```bash
cargo build --release -p ironrag-backend --bin ironrag-maintenance
```

Ships in the Docker image at `/usr/local/bin/ironrag-maintenance`.

## Configuration

Reads the same environment variables as the backend server. The required
database setting is `DATABASE_URL`.

## Schema rollout

IronRAG starts a fresh database from one baseline,
`apps/api/migrations/0001_init.sql`. A 0.5.x database cannot be upgraded in
place to 0.6.0; recreate it and re-ingest sources. SQLx checks the checksum of
an already applied baseline at startup, so editing that file does not migrate a
live database. `make migration-check` checks the source file against
`scripts/ops/baseline-checksum.txt`; it does not change a database.

For a later baseline change on an existing database, first establish its
recorded schema revision and the exact predecessor required by the intended
operator transition. Recreate the database when no reviewed transition covers
it. The supported `ironrag-maintenance migrate <transition>` commands preview
their own guarded hop without `--apply`; add `--apply` only after reviewing the
preconditions, backups, new binary and release gates. Each command checks its
predecessor objects, applies its own DDL and compare-and-sets the SQLx checksum
for migration 1. Do not apply its SQL body directly or stamp the checksum by
hand. Historical index steps that require `psql`, including
`scripts/ops/apply-knowledge-structured-block-fk-indexes.sql`, are separate
operator actions; the maintenance CLI does not execute them. Follow the
predecessor and target instructions in the transition's SQL or implementation
before running a later hop. For the manual FK-index step, verify all three
index definitions, then use the SQL file's compare-and-set template with the
independently verified predecessor and target SHA-384 values; require exactly
one updated migration row.

The revision-admission transition requires a reviewed import-admission
certificate for every predecessor readable revision. Its dry run accepts the
manifest, its independently reviewed raw SHA-256, the accepted recovery
snapshot ID, source baseline SHA-384 and operator identity; only the same
command with `--apply` changes the schema. Do not infer filenames or source
identities from mutable document labels, and do not run this transition while
old lifecycle writers are active. The JSON manifest contains
`recoverySnapshotId`, `sourceBaselineSha384`, `admittedBy` and `revisions`.
Each revision supplies its `revisionId`, `documentId`, `workspaceId`,
`libraryId`, `storageKey` and a `replacement` with exact `checksum`,
`byteSize`, `mimeType`, `fileName`, and explicit nullable `title`,
`languageCode`, `sourceUri`, `documentHint`. `storageKey` is required but may
be null. Every row also requires `sourceEvidence`:

- `{"kind":"source_blob"}` requires the retained revision storage key and
  verifies the actual source size and SHA-256.
- `{"kind":"snapshot_revision","knowledgeRevisionPresent":true,
  "normalizedTextSha256":"<lowercase SHA-256>","textChecksum":"<retained checksum>",
  "textState":"text_readable","textReadableAt":"<retained RFC3339 timestamp>"}` binds the captured
  knowledge row and actual normalized-text UTF-8 digest when original storage
  is absent. These fields are explicit nullable values where retained metadata
  is absent. With no knowledge row, use `knowledgeRevisionPresent:false` and
  null for the other four fields. This preserves historical head metadata;
  it does not invent a projection or verify unavailable original bytes.

Both variants require the exact scope and replacement descriptor, explicit
import filename, independently pinned snapshot and raw certificate hash.
Missing or extra readable revisions are rejected. With no readable revisions,
the certificate may be omitted.

```bash
ironrag-maintenance migrate content-visibility-intent-schema \
  --import-admission-manifest /path/to/reviewed-admission.json \
  --import-admission-sha256 '<reviewed raw certificate SHA-256>' \
  --accepted-recovery-snapshot-id '<accepted snapshot ID>' \
  --source-baseline-sha384 '<reviewed source baseline SHA-384>' \
  --admitted-by '<reviewed operator identity>' --json
```

Review the dry-run result, then repeat the same command with `--apply` only
after authorizing the coordinated rollout.

`apps/api/ops/schema-rollout.pending` fences release, promotion and GitOps
deployment while an operator transition is outstanding. It records the issue
and target baseline SHA-384. Keep it until the intended live transition and
API/worker readiness have been verified; removing it is not proof that a
database was migrated. Back up the schema and migration history before the
window. A checksum change makes an old binary unready, so coordinate the
transition with deployment rather than changing it while image builds are
pending.

## `audit` — read-only inspection

The audit family answers "what is going on" without changing anything.
Safe to run any time, including while ingest and retrieval are
serving traffic.

### `audit storage-summary`

**What it tells you.** Top Postgres tables by total on-disk size,
with live/dead tuple counts and the most recent autovacuum
timestamp.

**When to run.** Disk filling up. Postgres queries getting slow. As
the first step of any "why is this so big?" investigation.

**Example.**

```bash
ironrag-maintenance audit storage-summary --limit 20 --json
```

Sample reading: a `runtime_graph_evidence` row at 24 GB means that
table is your single biggest reclaim target — pair with
[`gc stale-evidence`](#gc-stale-evidence) to clean it.

### `audit index-bloat`

**What it tells you.** Per-index size and scan count for the
configured set of write-heavy tables. The `idx_scan` column shows how
often the index was actually used; a sizable index with zero scans
is a candidate for `DROP INDEX` review.

**When to run.** Suspect bloat. Considering a `REINDEX` window. Need
data to justify a dropped index in a PR.

**Example.**

```bash
ironrag-maintenance audit index-bloat --min-size-mb 100 --json
```

Restrict to a smaller table set with `--tables=table_a,table_b` when
you want to focus on one subsystem.

### `audit null-head-docs`

**What it tells you.** Documents whose `content_document_head`
carries neither a readable nor an active revision. These are
documents the retrieval path ignores — usually because the ingest
pipeline failed before producing a usable revision. The output
includes `recovery_attempts_count` and `dead_letter_at` so you can
see how the rate-limited auto-recovery has been spending its budget.

**When to run.** Users report that an uploaded document never shows
up in answers. After a known ingest incident, before deciding how
many docs to recover.

**Example.**

```bash
ironrag-maintenance audit null-head-docs --library <uuid> --limit 100 --json
```

If the listed docs have `dead_letter_at IS NOT NULL`, recovery has
already been exhausted — see
[`repair clear-recovery-dead-letter`](#repair-clear-recovery-dead-letter)
before retrying.

### `audit webhook-outbox`

**What it tells you.** A bounded, redacted lifecycle-outbox inventory. It defaults to
`--state dead-letter`, supports `--library`, and returns at most 500 rows per keyset page. Payloads,
event IDs, URLs, credentials, headers, lease identities, and raw errors are excluded. Valid states
also include `resolved`; the output exposes only typed `last_error_code` /
`resolution_reason_code` and their non-secret timestamps.

**Example.**

```bash
ironrag-maintenance audit webhook-outbox --state dead-letter --limit 100 --json
```

When `has_more` is true, continue with both fields from `next_cursor` as
`--before-created-at` and `--before-id`. The cursor is based on immutable creation order; because
state-filter membership is live, start a fresh pass after rows are requeued while paging. See
[webhook operations](./WEBHOOK.md#lifecycle-outbox-dead-letter-operations)
for the full safety contract.

---

## `gc` — garbage collection

The gc family removes derived data that is no longer reachable from a
readable or authoritatively leased active document revision. Every gc subcommand acquires the
per-library graph advisory lock and refuses to run while any ingest job
for the library is in
the `queued` / `leased` / `paused` queue state — so concurrent ingest
cannot lose data to the sweeper.

### `gc superseded-vectors`

**What it does.** Deletes chunk vectors from every physical PostgreSQL
dimension shard when their revision is neither readable nor the active
revision owned by the exact leased content-mutation job and its latest leased
unfinished attempt. A stale active pointer alone does not keep a vector hot.
It preserves source revisions, content chunks, knowledge chunks,
and stored query references. Content and knowledge head projections
must agree or the operation fails closed. Documents whose head is null
on both pointers are skipped by default.

Each delete transaction is capped at 10,000 rows. The background scheduler
commits at most one such batch per class run and re-queues remaining work for
the next scheduler tick. A durable keyset cursor follows the existing logical
vector B-tree and is stored in the maintenance lease. It is reset whenever the
library `source_truth_version` changes outside the sweep, so a concurrent head
or vector mutation cannot leave newly stale rows behind the cursor.
Graph/evidence reclamation continues under the separate scheduler-only
`gc.runtime-graph` class; its lease, retries, and failures are independent from
vector reclamation. That class also commits at most 10,000 physical delete
mutations per tick, counting graph/evidence roots together with dependent
context-bundle reference rows. Its durable cursor stores only the current
cleanup phase; every tick re-evaluates roots against current evidence before
locking and deleting a complete closure. A single indivisible closure above
the limit fails explicitly instead of overrunning the budget or partially
removing citations. The limit bounds committed delete mutations, not query
planning, index reads, or lock waits. Delete commit and lease-cursor update are
at-least-once: ownership loss between them replays the same idempotent phase.

**When to run.** Disk pressure on PostgreSQL storage, especially after
many revision replacements. Use the preview to measure the reclaimable
logical rows; physical HNSW/table shrink requires a separate rebuild.

**Example.**

```bash
# Safe preview: count what would be removed without issuing destructive deletes.
ironrag-maintenance gc superseded-vectors --dry-run --json

# Real run, single library.
ironrag-maintenance gc superseded-vectors --library <uuid>

# Aggressive mode: also remove vectors for null-head documents. Source and
# chunk history still remain available for repair.
ironrag-maintenance gc superseded-vectors --library <uuid> --include-null-head
```

### `gc stale-evidence`

**What it does.** Deletes `runtime_graph_evidence` rows whose
revision is no longer the readable/active head of the source
document, plus rows whose `chunk_id` points at a chunk that no longer
exists. Both lanes skip rows whose
document has an active ingest job in flight.

**When to run.** When `audit storage-summary` flags
`runtime_graph_evidence` as the largest table, especially after a
revision-heavy period.

**Example.**

```bash
ironrag-maintenance gc stale-evidence --library <uuid> --json
```

Sample output: `stale_revision_rows: 157645` means 157k rows in
`runtime_graph_evidence` were tied to obsolete revisions. The
companion `phantom_chunk_rows` counter shows how many were tied to
already-deleted chunks.

---

## `repair` — bring broken state back to the successful-ingest shape

The repair family is for *recovery*, not removal. It writes new rows
to bring objects whose state diverged back into the shape
the ingest pipeline produces on success.

### `repair null-heads`

**What it does.** For every document with `readable_revision_id IS
NULL AND active_revision_id IS NULL` that has at least one revision
with persisted chunks, promote the most recent chunk-bearing
revision to the head. Uses the same `promote_document_head` code
path the ingest pipeline uses on success, so the outcome is
indistinguishable from a fresh successful ingest. Idempotent.

**When to run.** One-shot, single-pass recovery after a known
incident. Best when you want the operation to attack every eligible
document immediately without any rate limiting.

**Example.**

```bash
ironrag-maintenance repair null-heads --library <uuid> --json
```

### `repair null-heads-auto`

**What it does.** Same recovery action as `repair null-heads`, but
the per-document outcome is recorded on `content_document_head`
so a flaky upstream cannot burn the recovery budget on a single
document:

* A document touched in the last hour is skipped (cooldown).
* On success, `recovery_attempts_count` is reset and
  `last_recovery_attempt_at = now()`.
* On failure, `recovery_attempts_count` is incremented if the new
  error matches `last_recovery_error_code`; otherwise the counter
  resets to 1.
* Three consecutive same-error failures stamp `dead_letter_at` and
  the document is excluded from future auto runs until an operator
  clears the mark.

**When to run.** Re-driving recovery against the same library in
loops or under cron. Anywhere a flaky external dependency could
flip the same document into "failed" repeatedly — the rate-limit
turns that into a manageable backlog instead of a tight retry storm.

**Example.**

```bash
ironrag-maintenance repair null-heads-auto --library <uuid> --json
```

Sample output: `"promoted": 24, "failed": 0, "dead_lettered": 0,
"cooldown_skipped": 0` means 24 docs were recovered cleanly on this
pass; subsequent passes within the next hour will skip those 24.

### `repair clear-recovery-dead-letter`

**What it does.** Clears `dead_letter_at` and the recovery counters
on `content_document_head` for one document. The next
`repair null-heads-auto` pass will pick it up again.

**When to run.** Only after you have diagnosed and fixed the root
cause of the failure that drove the document into dead-letter.

**Example.**

```bash
ironrag-maintenance repair clear-recovery-dead-letter --document <uuid>
```

### `repair webhook-outbox-dead-letter`

**What it does.** Atomically changes one exact outbox UUID from `dead_letter` to `pending`, clears
attempt/lease/error state, and makes it due now. It does not send HTTP; the ordinary worker handles
delivery. Audit output exposes only a stable typed `last_error_code`, never raw transport errors or
payloads. Missing rows or any state mismatch fail without modification.

```bash
ironrag-maintenance repair webhook-outbox-dead-letter --outbox <uuid> --json
```

### `repair webhook-outbox-dead-letter-resolve`

**What it does.** Permanently changes one exact UUID from `dead_letter` to the separate terminal
state `resolved` without claiming delivery. It records a database-clock timestamp and a bounded,
typed reason in both the outbox and the durable redacted audit log. `dispatched` and every other
state are protected by the compare-and-set. Catalog deletion treats `resolved` as non-blocking;
the audit event survives the catalog cascade.

The reason must be 1–64 ASCII bytes in lowercase `snake_case`; free-form text is rejected. Use this
only when delivery is intentionally no longer required. Use the requeue command above when the
receiver should still get the event. The command refuses to mutate state unless the operator also
passes `--acknowledge-not-delivered`.

```bash
ironrag-maintenance repair webhook-outbox-dead-letter-resolve \
  --outbox <uuid> --reason-code receiver_retired --acknowledge-not-delivered --json
```

---

### `repair knowledge-projection-metadata`

**What it does.** Copies drifted metadata from authoritative content to existing
knowledge projections in the same workspace and library, only for active content
with `deleted_at IS NULL`. Document fields: `external_key`, `parent_document_id`,
`document_role`, `document_state`, `deleted_at`. Revision fields: `revision_number`,
`revision_kind`, `source_uri`, `document_hint`, `mime_type`, `checksum`, `title`,
`byte_size`. A knowledge-side tombstone does not delete active content: this repair
restores the projection to `active` with a NULL `deleted_at`.

**Limits.** Does not change content, heads, readiness or chunks; does not insert
missing projections or purge orphans. No provider calls or scheduled backfill.
Apply holds the existing library lifecycle lock and advances source-truth
generation only when rows change. `--dry-run` counts eligible drift without writing;
an unchanged rerun reports zero updated rows.

```bash
ironrag-maintenance repair knowledge-projection-metadata --library <uuid> --dry-run --json
ironrag-maintenance repair knowledge-projection-metadata --library <uuid> --json
ironrag-maintenance repair knowledge-projection-metadata --library <uuid> --dry-run --json
```

Omit `--library` to reconcile every library.

---

## `retention` — TTL-based history sweepers

The retention family deletes rows from INSERT-only history tables
that have aged past their configured retention window. Every sweep is
batched (10 000 rows per DELETE, 100 ms pause between batches) so
concurrent ingest writers stay responsive.

### `retention stage-events`

**What it does.** Deletes `ingest_stage_event` rows older than
`--older-than-days`. The supporting index `idx_ingest_stage_event_recorded_at` is part of the
baseline, so the predicate is an index range scan, not a sequential scan under
`AccessExclusiveLock`.

**When to run.** When `audit storage-summary` flags
`ingest_stage_event` as oversized. Routinely once
`/v1/ingest/...` history queries get slow.

**Example.**

```bash
# 90-day retention window
ironrag-maintenance retention stage-events --older-than-days 90 --json

# Be cautious before going aggressive in dev/test
ironrag-maintenance retention stage-events --older-than-days 30 --json
```

---

## `rebuild` — heavy operator-only passes

The rebuild family is intentionally *never* wired into the recurring
scheduler. These passes consume significant provider budget or hold
long-running database resources; operators must trigger them
explicitly with full context.

### `rebuild vector-plane`

**What it does.** Reconciles PostgreSQL pgvector material with the
source library's active vector binding and rebuilds library vector
material that must move to the matching `(library, dim)` relation.

**When to run.** When changing the embedding dimension for a library
or scope that already has vector material. The source library tells
the rebuilder which active binding and dimension to use.

**Example.**

```bash
ironrag-maintenance rebuild vector-plane --source-library <uuid>
```

### `rebuild runtime-graph`

**What it does.** Re-runs the runtime-graph projection for
one library or for every library. Batch mode tolerates per-library
`StateConflict` errors and surfaces a non-zero exit at the end so
operator scripts can detect partial completion.

**When to run.** Graph went visibly out of sync with documents
(e.g. document deletions were applied but graph edges still
reference them). After a schema migration that touched the graph
projection.

**Example.**

```bash
# Single library
ironrag-maintenance rebuild runtime-graph --library <uuid>

# Every library (batch mode)
ironrag-maintenance rebuild runtime-graph
```

---

## `lease` — scheduler internals

The background scheduler tracks every (class, scope) maintenance
unit in a `maintenance_job_run` row. The lease subcommands inspect
that table and manage failure recovery.

### `lease show`

**What it tells you.** The current lease row for every (class,
scope) the scheduler is tracking, including who holds it right now,
what its `next_due_at` is, and the last error if any.

**When to run.** Investigating why the scheduler is not picking up
a class. After a dead-letter alert.

**Example.**

```bash
ironrag-maintenance lease show --class gc.superseded-vectors --json
ironrag-maintenance lease show --state dead_letter --json
```

### `lease summary`

**What it tells you.** Per-class summary of pending / leased /
dead-letter counts. Compact overview of the scheduler state.

**When to run.** Healthy-state quick check; suitable for cron-style
monitoring.

**Example.**

```bash
ironrag-maintenance lease summary --json
```

### `lease clear-failure`

**What it does.** Resets a dead-letter lease row back to `pending`
so the scheduler will pick it up on the next tick. Use after the
root cause has been fixed.

**Example.**

```bash
# Per-library class
ironrag-maintenance lease clear-failure --class gc.superseded-vectors --library <uuid>

# Instance-scope class (no --library flag)
ironrag-maintenance lease clear-failure --class retention.stage-events
```

### `lease reap-stale`

**What it does.** Reaps leased rows whose heartbeat is older than
the configured threshold. The scheduler also reaps on every tick;
this command is the manual lever for when an operator suspects
something is stuck.

**Example.**

```bash
ironrag-maintenance lease reap-stale --stale-after-secs 300
```

---

## Docker usage

```bash
docker exec <container> ironrag-maintenance audit storage-summary --json
docker exec <container> ironrag-maintenance gc superseded-vectors --dry-run
docker exec <container> ironrag-maintenance lease summary --json
```
