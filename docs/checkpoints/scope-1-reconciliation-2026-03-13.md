# Scope 1 Reconciliation Checkpoint

## Date
2026-03-13

## Purpose
Reconcile `docs/TASKS-SCOPE-1.md` against the actual runtime monorepo state without changing source code.

## Summary
The task file was stale relative to the repository.

Actual repo status is materially ahead of the old checklist in three major ways:
- monorepo/runtime docs alignment is already done
- backend implementation is well beyond bootstrap and already covers auth/control-plane/ingestion/retrieval/usage groundwork
- frontend is no longer just a shell; it already contains multiple routed operator/product surfaces wired around backend contracts

## Confirmed implemented areas

### Monorepo and deployment shape
- root `README.md` documents the runtime monorepo and separate `spec-kit` governance repo
- root `docker-compose.yml` exists and defines local postgres/redis/backend/frontend stack
- `backend/Dockerfile` and `frontend/Dockerfile` both exist

### Backend foundation already present
- typed config loading in `backend/src/app/config.rs`
- runtime bootstrap and shared state in `backend/src/app/mod.rs` and `backend/src/app/state.rs`
- Postgres migrations and Redis startup verification in `backend/src/infra/persistence.rs`
- health/readiness/version endpoints in `backend/src/interfaces/http/health.rs`
- tracing/logging baseline in `backend/src/shared/telemetry.rs`
- hand-maintained OpenAPI contract in `backend/contracts/rustrag.openapi.yaml`

### Auth and control plane already present
- token creation/list/get/revoke plus bootstrap-token path
- bearer auth context with workspace access checks and basic scopes
- workspace/project/provider/model-profile persistence and create/list endpoints
- project default-profile read/update endpoints

### Ingestion/retrieval groundwork already present
- source, ingestion job, document, chunk, retrieval-run tables and repository helpers
- upload/text ingest flows
- chunk persistence and chunk listing/search
- embedding generation path and pgvector-backed semantic search path
- `/v1/query` orchestration with retrieval debug metadata, references, and usage/cost persistence

### Usage/cost already present
- `usage_event` and `cost_ledger` persistence
- list/detail/summary endpoints for usage and cost data
- workspace/project attribution already stored in persistence model

### Frontend already beyond shell
Routes/pages now exist for:
- dashboard
- workspaces
- projects
- providers
- onboarding
- ingestion
- chat
- graph
- API integrations
- diagnostics
- design system

Typed API generation is also wired from backend OpenAPI into frontend generated types.

## Highest-priority remaining gaps
- real secret-management boundary is still missing; current provider secret handling is not hardened
- auth/scopes are practical but not fully formalized as a mature permission matrix
- ingestion execution is not yet a real background worker architecture
- retry/idempotency semantics remain heuristic
- chat session persistence is still missing
- citations/provenance are partial rather than first-class
- graph extraction/entity-relation workflows are still mostly hooks/heuristics rather than dedicated persisted pipelines
- observability/diagnostics are partial and operator-facing remediation is still thin
- clustering/scale assumptions and operational guardrails remain underdocumented

## Known honest blocker still reflected in docs
- frontend container build path is still documented as blocked in this environment by `esbuild` postinstall `spawn sh EACCES`
