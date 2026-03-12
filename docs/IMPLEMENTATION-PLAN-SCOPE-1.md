# RustRAG Implementation Plan — Scope 1

## Milestones

### M0 — Monorepo restructuring
- remove `spec-kit/` from this runtime repo model
- establish backend/frontend monorepo shape
- add top-level compose
- update README/docs

### M1 — Backend bootstrap
- config/runtime bootstrap
- Postgres/Redis bootstrap
- health/readiness/version
- tracing/logging
- OpenAPI baseline

### M2 — Control plane core
- auth/token baseline
- workspace CRUD
- project CRUD
- provider account CRUD
- model profile CRUD

### M3 — Ingestion control plane
- source CRUD
- upload registration
- ingestion job state machine
- document/chunk persistence

### M4 — Retrieval/query foundation
- retrieval run model
- query API
- citations/references
- chat/query session model

### M5 — Usage/cost and observability
- usage attribution
- cost groundwork
- operational diagnostics

### M6 — Frontend shell
- dashboard/workspaces/projects/providers/ingestion/chat shell
- typed API integration path

### M7 — Scale-readiness pass
- idempotency review
- worker split readiness
- clustering path note
- thousands-of-workspaces assumption review
