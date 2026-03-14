# Backend next-priority checkpoint — 2026-03-13

## Single highest-priority unfinished backend task
**Implement a real durable ingestion worker path and make it the only authoritative ingestion execution flow.**

This is the highest-priority backend gap because the current backend already exposes ingestion-job resources and lifecycle states, but the actual ingestion work still happens inline inside HTTP handlers. In practice, RustRAG has a job-shaped control plane without a real job executor.

## Why this is the top backend priority
- `POST /v1/ingestion-jobs` only creates a row with `status='queued'` and `stage='created'`; nothing in the backend claims or executes queued jobs.
- `POST /v1/content/ingest-text` and `POST /v1/uploads/ingest` bypass the job lifecycle and ingest documents/chunks synchronously in-request.
- This leaves the system with split semantics:
  - **job routes** suggest durable async execution
  - **real ingest routes** do the actual work immediately
- As long as that split exists, retries, readiness, observability, and future resumability are all a bit fake.

## Exact files / routes involved
### HTTP routes
- `backend/src/interfaces/http/ingestion.rs`
  - `GET /v1/ingestion-jobs`
  - `POST /v1/ingestion-jobs`
  - `GET /v1/ingestion-jobs/{id}`
  - `POST /v1/ingestion-jobs/{id}/retry`
- `backend/src/interfaces/http/content.rs`
  - `POST /v1/content/ingest-text`
  - `POST /v1/content/search-chunks`
  - `POST /v1/content/embed-project`
- `backend/src/interfaces/http/uploads.rs`
  - `POST /v1/uploads/ingest`

### Backend execution / persistence
- `backend/src/interfaces/http/content_support.rs`
  - `ingest_plain_text(...)` currently creates document + chunks directly
- `backend/src/infra/repositories.rs`
  - `create_ingestion_job(...)`
  - `list_ingestion_jobs(...)`
  - `get_ingestion_job_by_id(...)`
  - `create_document(...)`
  - `create_chunk(...)`
- `backend/src/app/shutdown.rs`
  - still an explicit placeholder for future worker/background orchestration
- `backend/migrations/0002_ingestion_and_retrieval.sql`
  - current `ingestion_job` schema is too thin for durable execution

## Concrete evidence from current tree
- `backend/src/interfaces/http/ingestion.rs:create_ingestion_job` only persists the job row.
- `backend/src/infra/repositories.rs:create_ingestion_job` inserts queued jobs with fixed `status='queued'` and `stage='created'`.
- `backend/src/interfaces/http/content.rs:ingest_text` calls `ingest_plain_text(...)` directly.
- `backend/src/interfaces/http/uploads.rs:upload_and_ingest` also calls `ingest_plain_text(...)` directly.
- `backend/src/interfaces/http/content_support.rs:ingest_plain_text` creates a document and then chunk rows one by one; it does not create/update an ingestion job.
- `backend/src/app/shutdown.rs` is still only a comment placeholder.

## Recommended implementation sequence
1. **Define one canonical ingestion execution contract**
   - Decide that all ingestion work must flow through `ingestion_job`.
   - Make text upload/text ingest endpoints create a job payload first instead of doing authoritative inline ingestion.

2. **Strengthen `ingestion_job` persistence model**
   - Extend schema with at least:
     - idempotency key
     - retry/parent job linkage
     - attempt count
     - worker/executor id
     - lease/heartbeat timestamps
     - structured payload/result metadata
   - Prefer adding an `ingestion_job_attempt` table or equivalent event log.

3. **Add repository methods for worker semantics**
   - claim next queued job
   - mark validating/running/completed/failed/retryable_failed
   - heartbeat/lease renewal
   - recover abandoned claimed jobs

4. **Build a minimal in-process worker loop**
   - Start with a single worker inside the backend process.
   - Wire startup/shutdown around it (`backend/src/app/*`, especially the currently empty shutdown hook).
   - Make the worker call the existing ingest/chunking logic rather than HTTP handlers.

5. **Refactor `ingest_plain_text(...)` into executor logic**
   - Reuse the chunking/persistence code from `backend/src/interfaces/http/content_support.rs`, but move it under job execution.
   - Job execution must be the place that advances lifecycle state.

6. **Only after that, re-point request routes**
   - `POST /v1/content/ingest-text` should enqueue-or-create a job and return job-aware details.
   - `POST /v1/uploads/ingest` should do the same.
   - `POST /v1/ingestion-jobs/{id}/retry` should create a causally linked retry/attempt, not just a brand-new anonymous queued row.

## What can wait until after this
- richer chat/session persistence
- graph/provenance deepening
- frontend queue UX improvements

Those matter, but the ingestion worker gap is more fundamental because the backend already claims job/lifecycle semantics that it does not yet truly implement.

## Bottom line
If only one backend task gets priority next, it should be this: **turn ingestion from “queued rows plus inline request-path work” into a real durable worker-driven lifecycle with one authoritative execution path.**
