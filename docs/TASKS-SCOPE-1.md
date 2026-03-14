# RustRAG Tasks — Scope 1

Legend:
- `[ ]` not started
- `[~]` partially implemented / needs hardening
- `[x]` done
- `[P]` can run in parallel

## Phase 0 — Monorepo and docs alignment
- [x] T000 remove embedded `spec-kit` from runtime-repo concept and document separate spec-governance repo approach
- [x] T001 rewrite README and top-level docs for backend/frontend monorepo shape
- [x] T002 define top-level compose/dev stack contract
- [x] T003 align backend docs with monorepo model
- [x] T004 align frontend docs with monorepo model

## Phase 1 — Backend bootstrap
- [x] T010 implement typed config loading
- [x] T011 implement runtime bootstrap
- [x] T012 wire Postgres 18 connection and migrations
- [x] T013 wire Redis connection and runtime checks
- [x] T014 implement health/readiness/version endpoints
- [x] T015 add structured tracing/logging baseline
- [x] T016 add OpenAPI serving/export workflow

## Phase 2 — Auth and core control plane
- [~] T020 design token/auth baseline
- [x] T021 implement workspace persistence and CRUD
- [x] T022 implement project persistence and CRUD
- [x] T023 implement provider account persistence and CRUD
- [x] T024 implement model profile persistence and CRUD
- [~] T025 implement secret handling boundary
- [~] T026 define auth scopes and permission matrix

## Phase 3 — Ingestion control plane
- [x] T030 implement source persistence and CRUD
- [x] T031 implement upload-based document registration
- [~] T032 implement ingestion job state machine
- [x] T033 implement document persistence
- [x] T034 implement chunk persistence
- [~] T035 implement background execution abstraction
- [~] T036 define future URL/git source contracts

## Phase 4 — Retrieval/query foundation
- [x] T040 implement retrieval run model
- [x] T041 implement query request/response contract
- [~] T042 implement citations/reference model
- [~] T043 implement chat/query session model
- [x] T044 implement project-scoped retrieval orchestration baseline
- [~] T045 define entity/relation extraction persistence hooks
- [x] T046 define retrieval debug metadata shape

## Phase 5 — Usage, cost, observability
- [x] T050 implement provider/model usage attribution
- [x] T051 implement cost accounting groundwork
- [x] T052 implement workspace/project usage attribution
- [~] T053 add operational metrics/tracing/logging hooks
- [~] T054 define diagnostics endpoints for jobs and retrieval runs

## Phase 6 — Frontend shell
- [x] T060 implement dashboard shell
- [x] T061 implement workspaces view
- [x] T062 implement projects view
- [x] T063 implement providers/model profiles view
- [x] T064 implement ingestion view
- [x] T065 implement query/chat workspace
- [x] T066 wire typed API client generation path

## Phase 7 — Deployment and scale readiness
- [x] T070 add top-level `docker-compose.yml`
- [x] T071 verify `backend/Dockerfile`
- [x] T072 add `frontend/Dockerfile`
- [~] T073 document horizontal-scale/clustering path
- [~] T074 review idempotency and worker split readiness
- [~] T075 document thousands-of-workspaces assumptions and operational guardrails

## Reconciliation notes (2026-03-13)

### What is clearly implemented already
- Runtime monorepo shape is already documented at root and in backend/frontend READMEs.
- Root-level `docker-compose.yml`, `backend/Dockerfile`, and `frontend/Dockerfile` are present.
- Backend foundation is materially beyond bootstrap:
  - typed config loading
  - app bootstrap and shared state wiring
  - Postgres migrations and Redis ping during startup
  - health/readiness/version routes
  - tracing/logging baseline
  - hand-maintained OpenAPI contract at `backend/contracts/rustrag.openapi.yaml`
- Auth/control plane baseline exists:
  - token minting, bootstrap token route, bearer auth extraction, revoke/list/get flows
  - workspace/project/provider/model-profile CRUD-style list/create flows
  - project default profile update/read flows
- Ingestion and retrieval are partially-to-substantially implemented:
  - sources, ingestion jobs, documents, chunks persistence
  - text ingest and multipart UTF-8 upload ingest
  - retrieval runs, `/v1/query`, lexical + semantic chunk ranking, references/debug persistence
  - embedding generation and pgvector-backed semantic search path
- Usage/cost attribution is already persisted and exposed via read APIs.
- Frontend is no longer just a shell; it already has substantial routed product surfaces for dashboard, workspaces, projects, providers, onboarding, ingestion, chat, graph, API integrations, diagnostics, and design-system reference.

### Highest-priority gaps still visible
- Secret handling remains foundation-level only: provider secrets are stored in `encrypted_secret jsonb`, but no real encryption/key-management/rotation boundary is visible yet.
- Auth is usable but still baseline-level: scopes exist pragmatically, but there is no clearly centralized/complete permission matrix document or mature auth model.
- Ingestion jobs are still synchronous/control-plane oriented, not a real background worker system with durable execution abstraction.
- Ingestion lifecycle is heuristic rather than a richer state machine with attempt history and stronger retry/idempotency semantics.
- Chat/query session persistence is not implemented even though a domain struct exists.
- Citation/reference model is partial: references are derived from chunk ownership/debug payloads rather than a richer first-class citation/provenance model.
- Graph/entity/relation support is only hook-level or heuristic product surface support; dedicated extraction pipelines and persisted provenance-rich graph workflows are not implemented.
- Observability is still baseline: logs/traces exist, but metrics/diagnostics/operator feeds are only partial and mostly read-only.
- Deployment docs are honest about current Docker/runtime blockers, but clustering/horizontal scale and large multi-workspace operational assumptions remain mostly undocumented.
