# RustRAG Tasks — Scope 1

Legend:
- `[ ]` not started
- `[x]` done
- `[P]` can run in parallel

## Phase 0 — Monorepo and docs alignment
- [ ] T000 remove embedded `spec-kit` from runtime-repo concept and document separate spec-governance repo approach
- [ ] T001 rewrite README and top-level docs for backend/frontend monorepo shape
- [ ] T002 define top-level compose/dev stack contract
- [P] T003 align backend docs with monorepo model
- [P] T004 align frontend docs with monorepo model

## Phase 1 — Backend bootstrap
- [ ] T010 implement typed config loading
- [ ] T011 implement runtime bootstrap
- [ ] T012 wire Postgres 18 connection and migrations
- [ ] T013 wire Redis connection and runtime checks
- [ ] T014 implement health/readiness/version endpoints
- [ ] T015 add structured tracing/logging baseline
- [P] T016 add OpenAPI serving/export workflow

## Phase 2 — Auth and core control plane
- [ ] T020 design token/auth baseline
- [ ] T021 implement workspace persistence and CRUD
- [ ] T022 implement project persistence and CRUD
- [ ] T023 implement provider account persistence and CRUD
- [ ] T024 implement model profile persistence and CRUD
- [ ] T025 implement secret handling boundary
- [P] T026 define auth scopes and permission matrix

## Phase 3 — Ingestion control plane
- [ ] T030 implement source persistence and CRUD
- [ ] T031 implement upload-based document registration
- [ ] T032 implement ingestion job state machine
- [ ] T033 implement document persistence
- [ ] T034 implement chunk persistence
- [ ] T035 implement background execution abstraction
- [P] T036 define future URL/git source contracts

## Phase 4 — Retrieval/query foundation
- [ ] T040 implement retrieval run model
- [ ] T041 implement query request/response contract
- [ ] T042 implement citations/reference model
- [ ] T043 implement chat/query session model
- [ ] T044 implement project-scoped retrieval orchestration baseline
- [P] T045 define entity/relation extraction persistence hooks
- [P] T046 define retrieval debug metadata shape

## Phase 5 — Usage, cost, observability
- [ ] T050 implement provider/model usage attribution
- [ ] T051 implement cost accounting groundwork
- [ ] T052 implement workspace/project usage attribution
- [ ] T053 add operational metrics/tracing/logging hooks
- [P] T054 define diagnostics endpoints for jobs and retrieval runs

## Phase 6 — Frontend shell
- [ ] T060 implement dashboard shell
- [ ] T061 implement workspaces view
- [ ] T062 implement projects view
- [ ] T063 implement providers/model profiles view
- [ ] T064 implement ingestion view
- [ ] T065 implement query/chat workspace
- [P] T066 wire typed API client generation path

## Phase 7 — Deployment and scale readiness
- [ ] T070 add top-level `docker-compose.yml`
- [ ] T071 verify `backend/Dockerfile`
- [ ] T072 add `frontend/Dockerfile`
- [ ] T073 document horizontal-scale/clustering path
- [ ] T074 review idempotency and worker split readiness
- [P] T075 document thousands-of-workspaces assumptions and operational guardrails
