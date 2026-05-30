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

### Create user

```bash
ironrag-cli create-user <LOGIN> <PASSWORD> [--name "Display Name"]
```

Creates a new user with admin privileges (`iam_admin` grant). The user is automatically added to the default workspace if one exists. Password must be at least 8 characters.

Options:
- `-n, --name` -- display name (defaults to login if omitted)

### Reset password

```bash
ironrag-cli reset-password <LOGIN> <PASSWORD>
```

Updates the password for an existing user and revokes all active sessions, forcing re-authentication. Password must be at least 8 characters.

### Delete user

```bash
ironrag-cli delete-user <LOGIN>
```

Permanently deletes a user and all associated records (sessions, grants, workspace memberships, principal).

### Create API token

```bash
ironrag-cli create-token <LOGIN> [--label "my-token"] [--workspace "my-workspace"] [--permission <PERM>...] [--scope <SCOPE>]
```

Creates an API token owned by the specified user. The plaintext token is displayed once and cannot be retrieved later. Tokens are prefixed with `irt_`.

Options:
- `-l, --label` -- token label (defaults to `api-token`)
- `-w, --workspace` -- limit the token to a specific workspace (by slug or UUID)
- `-p, --permission` -- permission to grant (repeatable). If omitted, defaults to `iam_admin`
- `--scope` -- explicit grant scope: `system`, `workspace:<slug>`, or `library:<slug>`

Available permissions:
- `iam_admin` -- full system administration
- `workspace_admin`, `workspace_read` -- workspace management
- `library_read`, `library_write` -- library and document access
- `document_read`, `document_write` -- document-level access
- `query_run` -- execute queries (ask)
- `ops_read`, `audit_read` -- operational and audit data
- `credential_admin`, `binding_admin` -- integration management
- `connector_admin` -- manage connectors

Scope resolution (when `--scope` is not specified):
- System permissions (`iam_admin`, `ops_read`, `audit_read`) → `system` scope
- Other permissions with `--workspace` → `workspace` scope on that workspace
- Other permissions without `--workspace` → `system` scope (access to all workspaces)

Examples:
```bash
# Full admin token
ironrag-cli create-token admin

# Read-only token for all workspaces
ironrag-cli create-token admin -p library_read -p query_run -l "reader"

# Write token scoped to a specific workspace
ironrag-cli create-token admin -p library_read -p library_write -w default -l "writer"

# Ops monitoring token
ironrag-cli create-token admin -p ops_read -p audit_read -l "monitoring"
```

### List API tokens

```bash
ironrag-cli list-tokens
```

Displays all API tokens with principal ID, label, prefix, status, issue date, and owner.

### Revoke API token

```bash
ironrag-cli revoke-token <TOKEN_PRINCIPAL_ID>
```

Revokes an API token by its principal UUID. Sets the token and principal status to `revoked`.

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
docker exec <container> ironrag-cli create-user admin2 secretpass --name "Second Admin"
docker exec <container> ironrag-cli reset-password admin newpassword123
docker exec <container> ironrag-cli delete-user old-admin

docker exec <container> ironrag-cli create-token admin --label "ci-token" --workspace default
docker exec <container> ironrag-cli list-tokens
docker exec <container> ironrag-cli revoke-token <TOKEN_PRINCIPAL_ID>

docker exec <container> ironrag-cli list-workspaces
docker exec <container> ironrag-cli create-workspace staging --name "Staging"

docker exec <container> ironrag-cli list-libraries default
docker exec <container> ironrag-cli create-library default docs --name "Documentation" --description "Public docs"
```

# IronRAG maintenance CLI

`ironrag-maintenance` is the single operator surface for everything
that keeps the IronRAG storage layer healthy: looking at what is on
disk, removing what can be safely removed, recovering what is in a
broken state, and inspecting the durable scheduler that runs the same
sweepers automatically in the worker role.

Two things to keep in mind throughout:

* Every destructive command refuses to run by default. Either it is
  opt-in (`--dry-run` is off and a flag like `--yes` is required), or
  it acquires a per-library advisory lock and refuses to run while
  ingest is in flight.
* The worker container runs a scheduler that walks the same sweepers
  on a rolling cadence. Manual CLI invocation never conflicts with
  it: both go through the same lease table and would block each other
  cleanly if they ever picked the same row.

## When do I run what?

A short decision guide before diving into the per-subcommand pages
below. Each row links to the canonical command.

| Symptom | Start here |
|---|---|
| "Disk is filling up, what is eating it?" | [`audit storage-summary`](#audit-storage-summary) |
| "Retrieval looks slow; suspect bad indexes" | [`audit index-bloat`](#audit-index-bloat) |
| "A document never shows up in answers even though ingest reported success" | [`audit null-head-docs`](#audit-null-head-docs), then [`repair null-heads`](#repair-null-heads) |
| "I deleted libraries last week, did Arango free their space?" | [`audit orphan-libraries`](#audit-orphan-libraries) |
| "Old chunks pile up after we replace document revisions" | [`gc stale-chunks`](#gc-stale-chunks) |
| "`runtime_graph_evidence` is bigger than the rest of the DB" | [`gc stale-evidence`](#gc-stale-evidence) |
| "Confirmed orphan footprint in Arango, want to purge" | [`gc orphan-libraries --yes`](#gc-orphan-libraries) |
| "Failed ingest left lots of `null-head` documents" | [`repair null-heads-auto`](#repair-null-heads-auto) |
| "ingest_stage_event is months old, queries are slow" | [`retention stage-events`](#retention-stage-events) |
| "Changed the embedding model; vectors need to move to new shard" | [`migrate vector-per-dim`](#migrate-vector-per-dim) |
| "JSONL chunks have no temporal bounds yet" | [`migrate chunk-temporal-bounds`](#migrate-chunk-temporal-bounds) |
| "Chunk vectors are on the shared per-dim shard; want to move them into per-library shards" | [`migrate chunk-vector-per-library`](#migrate-chunk-vector-per-library) |
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

Reads the same environment variables as the backend server (chiefly
`DATABASE_URL` and the ArangoDB credentials).

## Replacement for legacy maintenance binaries

`ironrag-maintenance` consolidates the per-task maintenance binaries
that used to ship alongside `ironrag-backend`. The old binary names
are gone — invoke the new subcommand instead.

| Removed binary | Replacement subcommand |
|---|---|
| `ironrag-gc-stale-chunks` | `ironrag-maintenance gc stale-chunks` |
| `ironrag-audit-orphan-data` | `ironrag-maintenance audit orphan-libraries` (read-only) + `ironrag-maintenance gc orphan-libraries --yes` (destructive) |
| `ironrag-promote-null-heads` | `ironrag-maintenance repair null-heads` |
| `ironrag-vector-migrate-to-per-dim` | `ironrag-maintenance migrate vector-per-dim` |
| `ironrag-vector-rebuild` | `ironrag-maintenance rebuild vector-plane --source-library <uuid>` |
| `ironrag-backfill-chunk-temporal-bounds` | `ironrag-maintenance migrate chunk-temporal-bounds` |
| `rebuild_runtime_graph` | `ironrag-maintenance rebuild runtime-graph` |

---

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

### `audit orphan-libraries`

**What it tells you.** ArangoDB rows whose `library_id` does not
match a live PostgreSQL `catalog_library` row. Such rows are left
behind by older deletion paths and pre-cascade-fix code; the report
counts orphan rows per library and per knowledge collection.

**When to run.** Periodic hygiene, especially after library deletions
on older deployments. Always read-only — the destructive purge is a
separate command.

**Example.**

```bash
ironrag-maintenance audit orphan-libraries --json
```

A non-empty report is the canonical pre-flight check for
[`gc orphan-libraries --yes`](#gc-orphan-libraries).

---

## `gc` — garbage collection

The gc family removes content that the canonical heads no longer
reach. Every gc subcommand acquires the per-library graph advisory
lock and refuses to run while any ingest job for the library is in
the `queued` / `leased` / `paused` queue state — so concurrent ingest
cannot lose data to the sweeper.

### `gc stale-chunks`

**What it does.** Deletes chunks from ArangoDB (and their vectors
across every per-dim shard plus the legacy single-dim collection)
whose revision is no longer the readable or active head of their
document. The default is conservative: documents whose head is null
on both pointers (failed ingest) are skipped so a recoverable doc
isn't erased outright.

**When to run.** Disk pressure on the Arango volume, especially after
many revision replacements. Suspect "we kept old chunks around"
behaviour.

**Example.**

```bash
# Safe preview: count what would be removed without issuing destructive AQL.
ironrag-maintenance gc stale-chunks --dry-run --json

# Real run, single library.
ironrag-maintenance gc stale-chunks --library <uuid>

# Aggressive mode: also wipe failed-ingest docs (chunks/vectors only;
# the document row stays). Only use when you have decided the doc
# is unrecoverable.
ironrag-maintenance gc stale-chunks --library <uuid> --include-null-head
```

### `gc stale-evidence`

**What it does.** Deletes `runtime_graph_evidence` rows whose
revision is no longer the readable/active head of the source
document, plus rows whose `chunk_id` points at a chunk that has
already been swept by `gc stale-chunks`. Both lanes skip rows whose
document has an active ingest job in flight.

**When to run.** When `audit storage-summary` flags
`runtime_graph_evidence` as the largest table. Usually after a
revision-heavy period or after running `gc stale-chunks`, since the
two sweepers complement each other.

**Example.**

```bash
ironrag-maintenance gc stale-evidence --library <uuid> --json
```

Sample output: `stale_revision_rows: 157645` means 157k rows in
`runtime_graph_evidence` were tied to obsolete revisions. The
companion `phantom_chunk_rows` counter shows how many were tied to
already-deleted chunks.

### `gc orphan-libraries`

**What it does.** Destructive companion to `audit orphan-libraries`.
Wipes every ArangoDB row whose `library_id` no longer matches a live
PostgreSQL `catalog_library` row. Refuses to run without `--yes`.

**When to run.** Only after `audit orphan-libraries` has been
reviewed and you accept the list it reports.

**Example.**

```bash
# Preview first
ironrag-maintenance audit orphan-libraries --json

# Then purge
ironrag-maintenance gc orphan-libraries --yes --json
```

---

## `repair` — bring broken state back to canonical

The repair family is for *recovery*, not removal. It writes new rows
to bring objects whose state diverged back into the canonical shape
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
the per-document outcome is now recorded on `content_document_head`
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

---

## `retention` — TTL-based history sweepers

The retention family deletes rows from INSERT-only history tables
that have aged past their canonical retention window. Every sweep is
batched (10 000 rows per DELETE, 100 ms pause between batches) so
concurrent ingest writers stay responsive.

### `retention stage-events`

**What it does.** Deletes `ingest_stage_event` rows older than
`--older-than-days`. The supporting index
`idx_ingest_stage_event_recorded_at` shipped in migration 0017 so
the predicate is an index range scan, not a sequential scan under
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

## `migrate` — one-shot data migrations

The migrate family is idempotent: a second run after success is a
no-op. These are NOT recurring — they exist for the canonical
"convert old shape to new" path and are removed from the operator
catalog once a deployment is fully migrated.

### `migrate vector-per-dim`

**What it does.** Moves rows from the legacy single-dim
`knowledge_chunk_vector` / `knowledge_entity_vector` collections
into per-dim shards (`knowledge_*_vector_d<dim>`). Walks every
distinct vector length present in the legacy collection and creates
the matching shard on demand.

**When to run.** Once per cluster, after upgrading to the per-dim
schema. Re-runs are safe if migration was interrupted.

**Example.**

```bash
ironrag-maintenance migrate vector-per-dim --json
```

### `migrate chunk-temporal-bounds`

**What it does.** Backfills `occurred_at` / `occurred_until` on
chunks whose `normalized_text` carries the canonical JSONL temporal
header but whose columns are still NULL. Cursor-paginated by chunk
id so re-runs after a crash naturally continue.

**When to run.** Once per cluster, after upgrading to the schema
that introduced the temporal columns.

**Example.**

```bash
# Preview without writing
ironrag-maintenance migrate chunk-temporal-bounds --dry-run --json

# Real run, single library
ironrag-maintenance migrate chunk-temporal-bounds --library <uuid> --json
```

### `migrate chunk-vector-per-library`

**What it does.** Drains chunk vectors from the shared per-dim shard
(`knowledge_chunk_vector_d<dim>`) into per-`(library, dim)` shards
(`knowledge_chunk_vector_d<dim>_l<library>`). Walks every distinct
`(library_id, dim)` pair present in the shared shard and writes the
rows into the matching per-library shard, creating the shard on demand
if it does not exist yet. Idempotent: a second run after migration
completes is a no-op. Entity vectors are not moved by this command —
they remain on the shared per-dim shard.

**When to run.** Once per cluster, after upgrading to the per-library
shard schema. Re-runs after an interrupted migration are safe.

**Example.**

```bash
# Drain the shared per-dim shard into per-library shards
ironrag-maintenance migrate chunk-vector-per-library

# Same, with a JSON summary instead of human-readable output
ironrag-maintenance migrate chunk-vector-per-library --json
```

---

## `rebuild` — heavy operator-only passes

The rebuild family is intentionally *never* wired into the recurring
scheduler. These passes consume significant provider budget or hold
long-running ArangoDB resources; operators must trigger them
explicitly with full context.

### `rebuild vector-plane`

**What it does.** Reconciles the instance-wide ArangoDB vector index
dimensions with the source library's active vector binding and
rebuilds all library vector material that must share those indexes.

**When to run.** When changing the embedding dimension globally
(e.g. swapping from 1536-dim to 3072-dim across the cluster). The
source library tells the rebuilder which dimension is the new
canonical.

**Example.**

```bash
ironrag-maintenance rebuild vector-plane --source-library <uuid>
```

### `rebuild runtime-graph`

**What it does.** Re-runs the canonical runtime-graph projection for
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
ironrag-maintenance lease show --class gc.stale-chunks --json
ironrag-maintenance lease show --state dead_letter --json
```

### `lease summary`

**What it tells you.** Per-class summary of pending / leased /
completed / failed / dead-letter counts. Compact overview of the
scheduler state.

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
ironrag-maintenance lease clear-failure --class gc.stale-chunks --library <uuid>

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
docker exec <container> ironrag-maintenance gc stale-chunks --dry-run
docker exec <container> ironrag-maintenance lease summary --json
```
