# RustRAG Backend

The backend is being rewritten around one destructive greenfield schema and one canonical knowledge plane. Fresh deployments are expected to come entirely from [0001_init.sql](/home/leader/sources/RustRAG/rustrag/backend/migrations/0001_init.sql) plus ArangoDB bootstrap; no legacy migration chain, backfill pass, or compatibility alias is authoritative anymore.

## Greenfield Bootstrap

Fresh environments are expected to start with:

- canonical Postgres control-plane tables only: `catalog_*`, `iam_*`, `ai_*`, `ingest_*`, `query_*`, `billing_*`, `ops_*`, `audit_*`, `async_operation`
- canonical ArangoDB knowledge collections only: `knowledge_document`, `knowledge_revision`, `knowledge_chunk`, `knowledge_entity`, `knowledge_relation`, `knowledge_evidence`, `knowledge_context_bundle`, and typed `knowledge_*_edge` collections
- seeded AI catalog rows from the initial migration:
  - `3` providers
  - `7` models
  - `12` prices
- zero default workspaces, libraries, or connectors

Recommended destructive bootstrap settings:

```bash
export RUSTRAG__DESTRUCTIVE_FRESH_BOOTSTRAP_REQUIRED=true
export RUSTRAG__DESTRUCTIVE_ALLOW_LEGACY_STARTUP_SIDE_EFFECTS=false
export RUSTRAG__LEGACY_UI_BOOTSTRAP_ENABLED=false
export RUSTRAG__LEGACY_BOOTSTRAP_TOKEN_ENDPOINT_ENABLED=false
export RUSTRAG__BOOTSTRAP_TOKEN=bootstrap-local
```

The first administrator is claimed explicitly through:

- `POST /v1/iam/bootstrap/claim`

This route is one-time only and must emit one canonical `audit_event`.

## Fresh-Bootstrap Proof

For a real fresh-database proof against a local Postgres server:

```bash
RUSTRAG__DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres \
cargo test -q --test greenfield_bootstrap -- --ignored
```

That harness creates a temporary database, applies the canonical migration, verifies seed rows, proves one-time bootstrap claim behavior, and checks that discovery reads do not create catalog rows as a side effect.

## Required Quality Gate

Every backend change must keep these commands green:

```bash
cargo fmt --all --check
cargo check -q
cargo test -q
```

For schema-wide work, the code gate is not enough. Fresh-database bootstrap evidence is also required; see [backend-change-gate.md](/home/leader/sources/RustRAG/rustrag/docs/operations/backend-change-gate.md).

## Local Runtime

Backend is expected to run against the compose stack from the repo root with Postgres, Redis, and ArangoDB:

```bash
docker compose up --build -d backend
```

Additional local operating notes live in:

- [README.md](/home/leader/sources/RustRAG/rustrag/README.md)
- [backend-change-gate.md](/home/leader/sources/RustRAG/rustrag/docs/operations/backend-change-gate.md)
