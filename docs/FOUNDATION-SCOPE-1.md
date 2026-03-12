# RustRAG Foundation Scope 1

## Summary

RustRAG Scope 1 defines the first implementation-ready foundation for a lightweight, API-first, automation-first, multi-workspace RAG platform built as a monorepo.

This scope must cover the complete platform baseline required to start implementation safely:
- backend runtime and contracts
- frontend shell
- auth/token baseline
- workspace/project/provider model
- ingestion control plane
- retrieval/query control plane
- usage/cost groundwork
- observability
- local deployment shape
- scale/clustering readiness constraints

## Repository shape

RustRAG is a monorepo containing:
- `backend/`
- `frontend/`
- top-level `docker-compose.yml`

`spec-kit` is intentionally **not** part of this repository and is expected to live separately.

## Product goals

- one instance with many isolated workspaces
- many projects inside each workspace
- API as the primary product surface
- UI as a client of backend APIs
- OpenAI + DeepSeek support in the first platform wave
- Postgres 18 + Redis as mandatory base infrastructure
- explicit path to horizontal scale and clustering later

## Scope requirements

### 1. Backend foundation
The backend must define and/or implement:
- config model
- runtime bootstrap
- health/readiness/version endpoints
- Postgres connection and migrations
- Redis connection and runtime checks
- tracing/logging setup
- OpenAPI contract baseline

### 2. Core domain/control plane
The backend must define and/or implement:
- workspace
- project
- provider_account
- model_profile
- source
- ingestion_job
- document
- chunk
- entity
- relation
- retrieval_run
- chat_session
- usage/cost records
- auth token model

### 3. Provider support
The backend must define:
- internal provider gateway abstraction
- OpenAI implementation path
- DeepSeek implementation path
- support for multiple credentials per workspace
- support for named model profiles
- room for OpenAI-compatible providers later

### 4. Ingestion control plane
The platform must define:
- source registration
- upload-based ingestion first
- ingestion stage/state model
- document/chunk persistence shape
- job execution abstraction compatible with future worker split

### 5. Retrieval/query control plane
The platform must define:
- query request/response model
- project-scoped retrieval
- citations/references
- retrieval run persistence
- chat/query session model
- retrieval debug metadata shape

### 6. Auth and automation
The foundation must include:
- instance/workspace token baseline
- scopes for automation and admin operations
- API-first operability for all major workflows

### 7. Usage, cost, observability
The foundation must include:
- provider/model usage attribution
- cost-accounting groundwork
- logs/metrics/traces baseline
- diagnostics surface for jobs and retrieval runs

### 8. Frontend shell
The frontend must provide:
- dashboard shell
- workspaces view
- projects view
- providers/model profiles view
- ingestion view
- query/chat view

### 9. Deployment baseline
The monorepo must include:
- `backend/Dockerfile`
- `frontend/Dockerfile`
- top-level `docker-compose.yml`

### 10. Scale-readiness baseline
The design must preserve a path to:
- stateless API replicas
- distributed workers
- idempotent job execution
- workspace isolation at high count

## Non-goals

Scope 1 does not require:
- full production cluster orchestration
- full LightRAG feature parity
- every connector/provider
- final UI polish
- every benchmark-driven storage optimization now

## Acceptance criteria

- repository is a clean backend/frontend monorepo
- spec/governance is decoupled conceptually from runtime repo
- backend and frontend skeletons exist
- top-level compose exists
- backend contracts and migrations exist
- scope explicitly covers auth, providers, ingestion, retrieval, observability, cost, and scale-readiness
