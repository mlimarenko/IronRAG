# Backend Change Gate

`make backend-change-gate` is the canonical merge gate for backend work in RustRAG.

## Default merge gate

Run this from the repo root:

```bash
cd /home/leader/sources/RustRAG/rustrag
make backend-change-gate
```

Today that gate covers:

- `cargo fmt --all --check`
- `cargo check -q`
- `cargo test -q`

This gate is mandatory for every backend change.

## Additional gate for schema-wide refactors

When a change rewrites the authoritative schema, baseline migration, or canonical domain vocabulary, `make backend-change-gate` is necessary but not sufficient.

Those changes must also produce destructive fresh-bootstrap evidence:

1. Start from an empty database or a temporary database created for the run.
2. Apply the canonical migration set from `backend/migrations/`.
3. Bootstrap the canonical ArangoDB knowledge collections, edges, search view, and named graph.
4. Prove the expected seeded catalog rows exist.
5. Prove no default workspace, library, or connector is created as a side effect.
6. Prove the one-time bootstrap-admin claim works exactly once and writes a canonical `audit_event`.
7. Prove legacy tables from the removed baseline do not appear in the fresh schema.
8. Prove no separate graph-database projection truth is required to answer a grounded knowledge query; the Arango-backed knowledge plane remains the source of truth.

The current executable proof for that flow is:

```bash
RUSTRAG__DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres \
cargo test -q --test greenfield_bootstrap -- --ignored
```

## When the code gate is not enough

Some pipeline changes can pass the compile and unit suite while still regressing runtime truth under real backlog. In those cases, `make backend-change-gate` is necessary but not sufficient.

Treat the following categories as requiring main-stack acceptance reruns:

- queue claim or worker-slot logic
- ingestion worker lifecycle, lease recovery, or heartbeat behavior
- upload admission, multipart parsing, or extractor dispatch
- graph extraction progress, recovery, or throughput persistence
- collection accounting, settlement, or warning semantics
- runtime or UI diagnostics payloads used by operators
- graph provider-call request shaping, retry semantics, or provider failure classification
- Documents workspace grouping, notice ordering, or summary hierarchy
- MCP memory discovery, search, read, mutation, or audit-review behavior

## Required acceptance reruns for pipeline-sensitive backend changes

When a change touches any of the categories above, run the following in addition to `make backend-change-gate`:

1. Mixed-backlog isolation rerun on the main compose stack.
   Confirm a newly uploaded library reaches active execution while an older library still owns long-running graph work.
2. Mixed-format rejection probes.
   Confirm file-level rejection truth, operator action text, and honest use of `invalid_file_body`.
3. Mixed 50-document corpus rerun.
   Confirm live accounting, graph throughput, and early collection diagnostics.
4. 280-document rerun through final settlement.
   Confirm final settled totals, stage and format rollups, and residual limitations.
5. Graph diagnostics stability soak.
   Confirm `/v1/runtime/libraries/{library_id}/graph/diagnostics` remains readable with no transient `500` during active backlog.
6. Documents workspace UX review.
   Confirm the page still reads as one primary summary rail, one secondary diagnostics strip, one grouped notice region, and one table region without duplicated progress/spend panels.
7. MCP permission matrix rerun.
   Confirm read-only, write-capable, and admin-review tokens see the correct tool surface and rejection behavior.
8. MCP continuation-read rerun.
   Confirm large readable documents reconstruct losslessly from ordered `continuationToken` windows.
9. External MCP client smoke rerun.
   Confirm a real MCP client can initialize, discover, search, read, write, and review audit evidence through `/v1/mcp`.

## Evidence expectations

Pipeline-sensitive backend changes are not complete until they produce or update a checkpoint with:

- exact commands
- real timings
- queue-isolation evidence
- upload-admission outcomes
- graph-diagnostics soak evidence
- provider-failure classification evidence including `requestShapeKey`, `upstreamStatus`, and retry outcome
- Documents workspace UX observations for idle, active backlog, and degraded states
- MCP token matrix and tool-visibility outcomes
- continuation-read evidence
- external MCP client smoke transcript or exact commands
- sampled MCP audit evidence with request-id attribution
- live and settled collection totals
- honest pass/fail notes against the last accepted baseline

For `052-pipeline-hardening`, the canonical validation sequence lives in [quickstart.md](/home/leader/sources/RustRAG/rustrag.spec-kit/specs/052-pipeline-hardening/quickstart.md).
