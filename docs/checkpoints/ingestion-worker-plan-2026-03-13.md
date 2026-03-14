# Ingestion / Worker Maturity Checkpoint — 2026-03-13

## Current state
- RustRAG has a real ingestion control plane, but not a real durable worker system yet.
- Backend exposes ingestion resources via `GET/POST /v1/ingestion-jobs`, `GET /v1/ingestion-jobs/{id}`, and `POST /v1/ingestion-jobs/{id}/retry`.
- `ingestion_job` is persisted in Postgres with only a lightweight row shape: `id`, `project_id`, `source_id`, `trigger_kind`, `status`, `stage`, `requested_by`, `error_message`, `started_at`, `finished_at`, `created_at`.
- Job creation is currently enqueue-in-name-only: `create_ingestion_job(...)` inserts a row with fixed initial state `status='queued'`, `stage='created'`, but there is no executor loop, claim/lease model, attempt table, or worker heartbeat/update path visible in the backend.
- Actual text and upload ingestion flows are still inline request/response handlers:
  - `/v1/content/ingest-text` calls `ingest_plain_text(...)` directly.
  - `/v1/uploads/ingest` parses multipart data and also calls `ingest_plain_text(...)` directly.
- `ingest_plain_text(...)` immediately creates a document, splits text, and inserts chunks in-process inside the HTTP request path. It does not create/update an ingestion job record.
- The frontend is honest about this gap: it explicitly says uploads may ingest immediately without surfacing a separate job record, and the upload queue is client-side only.

## Concrete gaps
1. **No durable worker architecture**
   - No background runner, queue consumer, scheduler, claim/lock/lease semantics, or crash recovery flow.
   - `backend/src/app/shutdown.rs` is still a placeholder for a future worker/background subsystem.
   - Current `queued` jobs can exist with no code path that actually advances them.

2. **Job model is too thin for real execution**
   - No attempt history table.
   - No parent/retry linkage between original job and retried job.
   - No worker identity, lease expiration, heartbeat timestamp, next_retry_at, or backoff fields.
   - No structured stage transition log or event history.
   - `status`/`stage` are free-form strings rather than strongly enforced DB-level workflow semantics.

3. **Retry is scaffold-level, not operationally safe**
   - Retryability is a heuristic: only `partial` and `retryable_failed` are treated as retryable.
   - Retrying just inserts a fresh queued row with the same project/source/trigger context.
   - No deduplication, no attempt counter, no cooldown/backoff, no max-attempt policy, no causal link to the failed run.

4. **Idempotency is largely absent**
   - `ingest_plain_text(...)` always creates a new document and new chunks.
   - `document` has `external_key` and `checksum`, but no uniqueness constraint or upsert path is visible in the schema/repository layer.
   - Repeated requests or retries can create duplicate documents/chunks for the same logical payload.
   - Upload ingestion derives `external_key` from filename or a generated UUID, which is not enough for durable replay-safe ingestion semantics.

5. **Inline ingestion is not transactionally modeled as a job lifecycle**
   - The request path does not create a job, transition through validating/running/completed/failed, and finalize observability state.
   - Partial failure behavior is weak: document/chunk inserts happen one by one; there is no visible compensating logic, resumability marker, or stage checkpointing.

6. **State surface is ahead of implementation**
   - OpenAPI and UI expose lifecycle values like `queued`, `validating`, `running`, `partial`, `completed`, `retryable_failed`, `canceled`, `failed`.
   - The persisted/runtime implementation does not show the machinery needed to reliably produce or maintain those states.

## Risks
- **Zombie queue risk:** jobs can accumulate in `queued` with no durable consumer to execute them.
- **Duplicate content risk:** retries, client resubmits, or network replay can create duplicate documents/chunks.
- **Inconsistent operator picture:** UI/control-plane suggests a job system, while the real ingest path is mostly synchronous and bypasses job tracking.
- **Weak crash recovery:** if the process dies mid-ingest, there is no attempt lease, resumable stage state, or durable recovery contract.
- **Hard future migration:** the longer inline ingestion remains the real path, the more product surfaces and assumptions will calcify around non-durable semantics.
- **Readiness/reporting drift:** project readiness currently looks at latest ingestion-job status, but successful inline upload/text ingestion can happen without corresponding job history.

## Highest-priority implementation sequence
1. **Make one ingestion execution path authoritative**
   - Define a canonical ingestion job payload and execution contract.
   - Route text/upload ingestion through job creation first, even if an in-process worker executes it initially.
   - Stop having “job path” and “real inline path” diverge.

2. **Harden the persistence model before scaling execution**
   - Extend `ingestion_job` with durable execution fields: attempt count, parent/retry-of id, idempotency key, executor/worker id, lease/heartbeat timestamps, terminal reason, and structured payload/result metadata.
   - Add an `ingestion_job_attempt` table or equivalent event log for per-attempt history.
   - Add DB constraints/indexes for safe lookup and replay control.

3. **Add idempotency and dedup semantics**
   - Define idempotency keys per ingestion request/job.
   - Add dedup policy for documents using `(project_id, source_id, external_key)` and/or checksum-based rules.
   - Replace blind inserts with explicit upsert/replace/version semantics.

4. **Build a minimal durable worker loop**
   - Implement claim-next-job / lease / heartbeat / finalize transitions in the backend process first.
   - Ensure restart-safe recovery for abandoned claimed jobs.
   - Add bounded retry/backoff logic driven by persisted state, not UI/manual hope.

5. **Move stage transitions into explicit executor code**
   - `queued -> validating -> running -> completed|partial|retryable_failed|failed|canceled`
   - Persist stage/error updates as the worker progresses.
   - Make job detail meaningful for operators.

6. **Only then enrich UX/diagnostics**
   - Progress reporting, attempt history, remediation hints, and resumable upload/work queue UX should follow the durable backend semantics, not precede them.

## Bottom line
RustRAG is past bootstrap on ingestion control-plane UX, but the worker side is still mostly scaffold/honest placeholder territory. The maturity gap is not “a bit more retry logic”; it is the absence of a single durable execution architecture with idempotent persistence and real worker state transitions.