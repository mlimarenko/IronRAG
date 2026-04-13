# IronRAG: content processing pipeline

This document describes the full data path from source ingestion to knowledge graph construction and retrieval. The goal is to have one place where all nuances around different file types, web ingestion, chunking, extraction, merge, and dedup are pinned down, so the map does not need to be rebuilt from code each time.

All code references use `path:line` format, relative to `ironrag/`.

---

## 1. Entry points (HTTP)

All REST routes live in `apps/api/src/interfaces/http/`.

| Source | Method + path | Handler |
|---|---|---|
| Inline document (text/json) | `POST /content/documents` | `interfaces/http/content.rs` (`create_document`) |
| File upload (multipart) | `POST /content/documents/upload` | `interfaces/http/content.rs` (`upload_inline_document`) |
| Append/edit/replace | `POST /content/documents/{id}/append|edit|replace` | `interfaces/http/content.rs` |
| Web ingestion (single + crawl) | `POST /content/web-runs` | `services/ingest/web.rs` |
| Ingest job status | `GET /ingest/jobs`, `GET /ingest/attempts/{id}` | `interfaces/http/ingestion.rs` |

`POST /content/documents/upload` parses multipart, extracts `file_name`, `mime_type`, `file_bytes` and assembles `UploadInlineDocumentCommand`, which then flows into `ContentService::upload_inline_document` (`services/content/service/pipeline.rs:152`).

---

## 2. File types and parsers

File type is detected in `services/shared/extraction/file_extract.rs` via `detect_upload_file_kind()` (extension first, MIME fallback).

| `UploadFileKind` | Extensions / MIME | Parser | Notes |
|---|---|---|---|
| `TextLike` | `txt`, `md`, `json`, `yaml`, source files (rs, py, ts, js, …) | `services/shared/extraction/file_extract/normalization.rs` | Newline normalization, UTF-8/BOM decode, basic cleanup |
| `Pdf` | `application/pdf` | `services/shared/extraction/pdf.rs` (Pdfium) | Per-page text extraction, layout recovery attempt. **OCR is not implemented** — scanned PDFs without a text layer fall through |
| `Image` | `png`, `jpg`, `jpeg`, `gif`, `webp`, `svg` | Vision LLM (via `LlmGateway`) | Image description through a multimodal model. This is **not** OCR but semantic description |
| `Docx` | `application/vnd.openxmlformats-…wordprocessingml…` | `services/shared/extraction/docx.rs` | Structured blocks (paragraphs, headings, tables) |
| `Spreadsheet` | `csv`, `xlsx`, `ods` | `services/shared/extraction/spreadsheet.rs` | Each row → its own structured block with `kind=table_row` |
| `Pptx` | `application/vnd.openxmlformats-…presentationml…` | `services/shared/extraction/pptx.rs` | Slides → blocks |
| `Binary` | other | — | Rejected at admission stage |

All parsers produce a single representation: `Vec<StructuredBlockData>`. This matters because **chunking, embedding, and graph extraction only know about structured blocks**, not the original file format.

---

## 3. Web ingestion

`POST /content/web-runs` creates `CreateWebIngestRunCommand` → `services/ingest/web.rs`.

Two strategies:

- **Single page** — `services/ingest/web/single_page.rs`. One fetch, readability extraction.
- **Recursive crawl** — `services/ingest/web/recursive.rs`. BFS with boundary policy: same-domain / subdomain limit, max depth, max pages.

**Fetch:** via reqwest with 20s timeout, max 10 redirects, custom User-Agent.

**Content extraction:** `services/shared/extraction/html_main_content.rs`:
- `extract_html_canonical_url()` — URL normalization (canonical link, dedup by URL).
- Readability-style boilerplate removal — drops nav, footer, sidebar.
- Returns structured blocks in the same shape as file parsers.

**PDF over URL:** detected by `Content-Type`, downloaded, then routed through the same `pdf.rs`.

**Not specially supported:** GitHub repo (no clone), YouTube transcripts, API docs (treated as plain HTML).

**Deduplication:** by canonical URL — re-fetching the same URL does not create a new `content_document`.

---

## 4. Storage model

### Postgres tables (`apps/api/migrations/0001_init.sql`)

```
catalog_library (id, …, extraction_prompt)
    │
    ├──> content_document (id, library_id, external_key, document_state)
    │       │
    │       ├──> content_revision (id, document_id, revision_number, mime_type,
    │       │       byte_size, title, source_uri, storage_key, parent_revision_id)
    │       │       │
    │       │       └──> content_chunk (id, revision_id, chunk_index,
    │       │               normalized_text, text_checksum, token_count, …)
    │       │
    │       └──> content_document_head (document_id → active_revision_id)
    │
    ├──> runtime_graph_node (line 1028)
    │       (id, library_id, canonical_key, label, node_type,
    │        aliases_json, summary, metadata_json, support_count,
    │        projection_version, …)
    │
    └──> runtime_graph_edge (line 1046)
            (id, from_node_id, to_node_id, relation_type, canonical_key,
             support_count, metadata_json, projection_version, …)
```

**Content versioning:** `revision_number` increments, `parent_revision_id` builds the chain. `content_document_head.active_revision_id` points at the current version.

**Source byte storage:** `content_revision.storage_key` → blob storage (S3-like, implementation in `infra/storage/`).

**Graph versioning:** `projection_version` (bigint). Each rebuild creates a new `runtime_graph_snapshot`; the graph is read by `(library_id, projection_version)`. The active version is `active_projection_version()` (`services/graph/projection.rs:83`).

### Arango stores

Structured blocks and graph extraction candidates (before promotion to Postgres) live in Arango (`arango_document_store`, `arango_graph_store`). The persistent source of truth is Postgres `runtime_graph_*`; Arango is staging.

---

## 5. Chunking

`services/shared/extraction/chunking.rs`, `StructuredChunkingProfile`:

```rust
StructuredChunkingProfile {
    max_chars: 2_800,
    overlap_chars: 280,   // ~10% overlap
}
```

Algorithm `build_structured_chunk_windows`:
- Iterates `Vec<StructuredBlockData>` in document order.
- **Heading-aware:** a heading opens a new chunk.
- **Table-aware:** table rows are either grouped into one chunk or each goes separately with `chunk_kind="table_row"` (for small tables).
- **Code-aware:** large code blocks are pre-split on semantic boundaries.
- **Overlap:** the last blocks of the previous chunk are duplicated into the start of the next.
- **Near-duplicate detection:** `mark_near_duplicates()` via simhash, to avoid spawning chunks for identical sections.

Chunks are saved as `content_chunk` rows; each gets a `text_checksum` (SHA256 of the normalized text). This checksum drives **diff-aware ingest** — see section 9.

---

## 6. Embedding

- **Model:** configured via the provider catalog. Default seed in migrations is `text-embedding-3-large` (OpenAI).
- **Storage:** pgvector column (exact name depends on the migration). Accessed through `services/query/search.rs`.
- **Timing:** **async**, a separate job pipeline stage `embed_chunk` (see section 8). Does not block ingestion.
- **Usage:** hybrid retrieval (vector top-K + graph evidence).

---

## 7. Graph extraction (key stage)

### 7.1. Job pipeline stages

`services/ingest/service.rs:21-30`:

```
extract_content       → pull text from file/URL
prepare_structure     → assemble structured blocks
chunk_content         → slice into chunks
embed_chunk           → async embedding
extract_technical_facts
extract_graph         ← here
verify_query_answer
finalizing
web_discovery / web_materialize_page  (web ingest only)
```

Each stage is its own `IngestJob` with a lease-based attempt (`services/ingest/worker.rs`).

### 7.2. What goes into the LLM

Request type is `GraphExtractionRequest` (`services/graph/extract/types.rs:14-25`):

```rust
pub struct GraphExtractionRequest {
    pub library_id: Uuid,
    pub document: DocumentRow,
    pub chunk: ChunkRow,
    pub structured_chunk: GraphExtractionStructuredChunkContext,
    pub technical_facts: Vec<GraphExtractionTechnicalFact>,
    pub revision_id: Option<Uuid>,
    pub activated_by_attempt_id: Option<Uuid>,
    pub resume_hint: Option<GraphExtractionResumeHint>,
    pub library_extraction_prompt: Option<String>,
    pub sub_type_hints: GraphExtractionSubTypeHints,
}
```

`GraphExtractionSubTypeHints` is defined in `types.rs:32-52` as `{ by_node_type: Vec<GraphExtractionSubTypeHintGroup> }` with an `is_empty()` helper. The field is **not Option** — an empty value (`GraphExtractionSubTypeHints::default()`) is valid; it simply renders to nothing and the prompt section is omitted.

Constructed in `services/content/service/pipeline.rs:68` (`build_canonical_graph_extraction_request`), called from `services/content/service/revision.rs:1082` per chunk inside a parallel stream.

### 7.3. Prompt structure

`services/graph/extract/prompt.rs:70` — `build_graph_extraction_prompt_plan()`. The prompt is assembled as a set of named sections `[name]\nbody`, in order:

1. **`task`** (`prompt.rs:90`) — main instruction: extract entities + relations, resolve coreferences, prefer specific relation types over `mentions`.
2. **`entity_types`** (`prompt.rs:107`) — **hardcoded static list** of 10 types: `person`, `organization`, `location`, `event`, `artifact`, `natural`, `process`, `concept`, `attribute`, `entity`. No dynamic loading.
3. **`examples`** (`prompt.rs:121`) — two examples (API docs + infrastructure).
4. **`schema`** (`prompt.rs:130`) — JSON schema requirement. Explicitly notes that `sub_type` is a **freeform specialization** (framework, database, algorithm, …). `relation_type` comes from `canonical_relation_type_catalog()` (`services/graph/identity.rs`).
5. **`rules`** (`prompt.rs:137`) — critical rules: no markdown fences, no empty summaries, no `mentions` when something specific fits.
6. **`document`** (`prompt.rs:146`) — document label + chunk ordinal.
7. **`domain_context`** (`prompt.rs:156`) — section path.
8. **`library_context`** (`prompt.rs:162-167`) — optional per-library instruction from `catalog_library.extraction_prompt`.
9. **`sub_type_hints`** (`prompt.rs:168-170`) — lists observed `sub_type`s per `node_type` for the current library, with a "prefer existing if applicable" instruction. The section is emitted only when hints are non-empty. Renderer is `render_sub_type_hints` (`prompt.rs:293`). See section 14.
10. **`structured_chunk`** (`prompt.rs:167`) — chunk kind, section path, heading trail, support block count.
11. **`technical_facts`** (`prompt.rs:171`) — rendered typed facts (if any).
12. **`downgrade`** (`prompt.rs:177`) — if running recovery with downgrade.
13. **`recovery`** + **`previous_output`** (`prompt.rs:186-207`) — for retry attempts.
14. **`chunk_segment_*`** — the actual chunk text, sliced into 1-3 segments depending on downgrade level.

### 7.4. Adaptive downgrade

On repeated extraction failures (`resume_hint.downgrade_level > 0`) the prompt is shrunk:
- `downgrade_level=1`: size limit / 2, max 2 chunk-text segments.
- `downgrade_level=2`: limit / 3, max 1 segment.

This gives the LLM a second chance on problematic chunks with reduced context.

### 7.5. Response parsing

`services/graph/extract/parse.rs`:
- `extract_json_payload()` — pulls JSON out of the response (tolerant of ```json fences).
- `parse_entity_candidate()` — parses an entity (`parse.rs:222-228` for `sub_type`):
  ```rust
  let sub_type = value.get("sub_type")
      .and_then(serde_json::Value::as_str)
      .map(str::trim)
      .filter(|s| !s.is_empty())
      .map(ToString::to_string);
  ```
- `parse_relation_candidate()` — parses a relation, validates `relation_type` against the catalog.
- `refine_entity_type()` — post-processing: fixes obviously wrong types via heuristics.

Output is `GraphExtractionCandidateSet { entities, relations }`.

---

## 8. Merge / upsert into the graph

### 8.1. Entry point

`services/graph/merge.rs:130` — `merge_chunk_graph_candidates()`. Takes a `GraphMergeScope` (`library_id`, `projection_version`, `revision_id`, `attempt_id`), the document, the chunk, and the candidates.

### 8.2. Identity key

`services/graph/identity.rs`:
- `canonical_node_key(node_type, label)` → `"{node_type_slug}:{normalized_label}"`.
- `normalize_graph_identity_component()` — lowercase, trim, Unicode NFKC, punctuation stripping.

**`sub_type` is NOT part of the identity key.** Two candidates with the same `(node_type, label)` but different `sub_type` collapse into one `runtime_graph_node`.

### 8.3. Node upsert

`services/graph/merge.rs:457-504` — `upsert_graph_node()`:

```rust
let canonical_key = canonical_node_key(node_type, label);
let existing = get_runtime_graph_node_by_key(pool, library_id, canonical_key, projection_version);
let support_count = existing.map_or(1, |row| row.support_count.max(1));

let mut metadata = merge_graph_quality_metadata(
    existing.map(|row| &row.metadata_json),
    extraction_recovery,
    summary,
);
if let Some(st) = sub_type {
    metadata.as_object_mut().map(|obj| {
        obj.insert("sub_type".to_string(), Value::String(st.to_string()))
    });
}

upsert_runtime_graph_node(...)
```

**Field-level conflict policy:**

| Field | Policy |
|---|---|
| `aliases_json` | Union (normalized + dedup) |
| `summary` | Last-wins if new is non-empty |
| `support_count` | `max(existing, 1)` + reconcile pass increment |
| `metadata_json.sub_type` | **Last-wins (overwrite)**. The previous value is dropped |
| other `metadata_json` | Merged via `merge_graph_quality_metadata()` |

### 8.4. Edge upsert

`services/graph/merge.rs:506` — `upsert_graph_edge()`. Identity by `(from_canonical, relation_type, to_canonical)`. Same reconciliation for `support_count` via `reconcile_merge_support_counts()`.

---

## 9. Diff-aware ingest (re-use)

`services/content/service/revision.rs:982` — `build_chunk_reuse_plan()`.

When a new revision of a document is created: for each new chunk, check whether the **parent** revision had a chunk with the same `text_checksum`. If yes — take the existing `runtime_graph_extraction` records, copy them under the new `chunk_id`, and **the LLM is not invoked**.

This is a critical optimization for documents edited in pieces — most chunks remain unchanged and reuse the previous graph extraction.

---

## 10. Entity resolution (post-hoc dedup)

`services/graph/entity_resolution.rs`.

**Trigger:** `resolve_after_ingestion()` runs after ingestion when the library has ≥ 50 nodes (efficiency threshold).

**Match algorithms (deterministic, no LLM, no embedding):**
1. **ExactAlias** — one node's label exactly matches another's alias.
2. **NormalizedPrefix** — after stripping known suffixes (`_database`, `_db`, `_framework`, `_system`, …).
3. **Acronym** — known abbreviations table (`pg` ↔ `postgresql`, `k8s` ↔ `kubernetes`, `jwt` ↔ `json web token`).

**Merge process:**
- One node is keep, one is remove.
- The remove node's edges are redirected to keep.
- The remove node's label is added to keep's `aliases_json`.
- `support_count` is summed.

**What is lost:** the remove node's `metadata_json` (including `sub_type`!) **is not transferred**. This is a known asymmetry with the upsert path — there it's last-wins, here it's silent loss. Sub_type hints (section 14) attack the root cause — making the LLM emit converging values from the start, not patching things up after the fact.

---

## 11. Retrieval

`services/query/search.rs` + `services/query/execution/retrieve.rs`.

**Hybrid scheme:**
- **Vector search** — pgvector similarity over chunk embeddings.
- **Graph search** — traversal of `runtime_graph_node` / `runtime_graph_edge` from matched entities.
- **Fusion ranking** — combined score over relevance, support_count, metadata.

**`sub_type` is NOT used in retrieval** — it is a purely annotative field in `metadata_json`, not indexed and not filtered on.

---

## 12. Job runner and resilience

### 12.1. Lease lifecycle

`services/ingest/worker.rs` — lease-based worker pool:
- `AdmitIngestJobCommand` creates an `IngestJob` with `queue_state='queued'`.
- `claim_next_queued_ingest_job` (`jobs.rs`) atomically transitions `queued → leased` using `for update skip locked` — multiple workers never race for the same row. The `active_leases` CTE counts **all** currently-leased jobs against the global / workspace / library caps; a previous heartbeat-freshness filter introduced a TOCTOU that let fresh claims race ahead of the per-library cap, so it was removed (zombie leases are handled by the reaper, the claim query only enforces limits).
- `LeaseAttemptCommand` creates an `ingest_attempt` with `attempt_state='leased'` and an initial `heartbeat_at=now()`.
- `HeartbeatAttemptCommand` refreshes `heartbeat_at` every `settings.ingestion_worker_heartbeat_interval_seconds` (default 15s).
- `FinalizeAttemptCommand` records `success | failed | canceled` plus `failure_class` / `failure_code` / `retryable`.
- `extraction_recovery.rs` — retry logic with adaptive downgrade.

Parallelism is two-dimensional:
- **Cross-document (dispatcher)** — `settings.ingestion_max_parallel_jobs_per_library` (default 16) is the static ceiling. Actual concurrency additionally drops under memory pressure via `ingestion_memory_soft_limit_mib`: before each claim the dispatcher reads worker RSS and refuses to start a new job when the process is over the soft limit. The soft limit auto-resolves from the cgroup (or `/proc/meminfo`) to 90% of container memory when config is `0` (`shared::telemetry::resolve_memory_soft_limit_mib`).
- **Per-document (graph extract fan-out)** — `settings.ingestion_graph_extract_parallelism_per_doc` (default 8) controls the `buffer_unordered` concurrency of per-chunk graph-extract LLM calls inside one job. Decoupled from the cross-doc limit so heavy docs get proper chunk parallelism without pushing cross-doc pressure up.

### 12.2. Stale lease reaper (periodic)

`services/ingest/worker/runtime.rs` — `run_canonical_lease_recovery_loop`:
- Every `CANONICAL_LEASE_RECOVERY_INTERVAL = 15s` it finds attempts with `queue_state='leased'` + `attempt_state='leased'` + `heartbeat_at < now() - CANONICAL_STALE_LEASE_SECONDS (60s)` and returns the job to the queue (`jobs.rs` — `recover_stale_canonical_leases`).
- The attempt is marked `attempt_state='failed'`, `failure_class='lease_expired'`, `failure_code='stale_heartbeat'`, `retryable=true`.
- Catches the "provider hung for minutes" scenario — the LLM call pins the task, heartbeat cannot tick, and after 60s the reaper frees the job for the next worker to retry.

### 12.x. LLM transport retry schedule

Retryable provider failures (timeouts, transient 4xx/5xx — 408, 409, 425, 429, 500, 502, 503, 504, 520–524, 529 — plus `reqwest` transport errors) are retried with a fixed schedule: **1s, 3s, 10s, 30s, 90s** (`TRANSPORT_RETRY_SCHEDULE_SECS` in `integrations/llm/streaming.rs`). `llm_transport_retry_attempts` default 5 matches the schedule length; `runtime_graph_extract_recovery_max_attempts` default 4 adds an outer retry layer around per-chunk graph extraction, so a chunk survives up to 4 × (134 s backoff ladder) of transient provider failure before surfacing as an error.

### 12.3. Startup lease sweep (one-shot at worker pool boot)

`services/ingest/worker/runtime.rs` — `reclaim_orphaned_leases_on_startup`, called inside `run_ingestion_worker_pool` **before the dispatcher starts claiming new jobs**:

```rust
pub(super) async fn run_ingestion_worker_pool(...) {
    ...
    reclaim_orphaned_leases_on_startup(&state).await;
    let lease_recovery_handle = tokio::spawn(run_canonical_lease_recovery_loop(...));
    ...
}
```

Uses a **shorter** threshold — `CANONICAL_STARTUP_LEASE_RECOVERY_SECONDS = 30s`. Rationale: at pool boot we know *this* process holds zero leases, so any `leased` row in the DB is either owned by a live sibling worker or orphaned. 30s = 2× heartbeat interval — a healthy sibling cannot exhibit that gap, so orphans are caught almost immediately without risk of stealing active leases.

**What it closes:** after a restart of the backend / worker / full stack, documents that were in-flight no longer look "stuck in processing" for up to a minute. The startup sweep returns them to the queue within seconds of boot, and the next `claim_next_queued_ingest_job` cycle picks them up.

Visible in the logs on boot:
```
WARN  startup lease sweep: reclaimed orphaned canonical ingest leases after worker pool boot
      recovered=9 threshold_seconds=30
```

### 12.4. Cancel flow (including active leases)

`cancel_jobs_for_document` (`infra/repositories/ingest_repository/jobs.rs:601` — previously `cancel_queued_jobs_for_document`, canonically renamed) is the **single** cancel SQL:

```sql
UPDATE ingest_job
SET queue_state = 'canceled', completed_at = now()
WHERE mutation_id IN (...)
  AND queue_state IN ('queued', 'leased')
  AND completed_at IS NULL
```

Covers **both** queued and leased. For queued this is an atomic terminal. For leased, setting `queue_state='canceled'` is a **signal to the worker** that cooperates with a cancel-aware pipeline abort.

**Heartbeat-loop observer** (`worker.rs:execute_canonical_ingest_job`):
- A `JobCancellationToken { canceled: Arc<AtomicBool> }` is created next to the heartbeat task.
- Each heartbeat tick, after writing `heartbeat_at`, issues `get_ingest_job_by_id`; if it sees `queue_state='canceled'`, it calls `token.mark_canceled()` and exits the loop.
- A pre-lease guard reads the queue state once immediately after the attempt is created, to catch a cancel that fired between claim and the first heartbeat tick.
- Latency: ≤ `ingestion_worker_heartbeat_interval_seconds` (default 15s) from `UPDATE queue_state='canceled'` to worker observation.

**Pipeline guards** (`worker.rs:run_canonical_ingest_pipeline`):

```rust
async fn run_canonical_ingest_pipeline(..., cancellation: &JobCancellationToken) {
    cancellation.check(job.id)?;          // extract_content
    ...
    cancellation.check(job.id)?;          // prepare_structure / chunk_content / …
    ...
    cancellation.check(job.id)?;          // embed_chunk
    ...
    cancellation.check(job.id)?;          // extract_graph
    ...
    cancellation.check(job.id)?;          // finalize readiness
    ...
}
```

`cancellation.check(job_id)` is called between stages. If the flag is set it returns `anyhow::Error::new(JobCanceledByRequest { job_id })` and the pipeline stops. A mid-stage cancel (during an LLM call) waits for the current stage to finish — acceptable, LLM calls are already bounded by provider timeouts.

**Finalize branch** (`worker.rs:execute_canonical_ingest_job`, `Err` arm):
- The `error.downcast_ref::<JobCanceledByRequest>()` check runs **first**.
- Finalize with `attempt_state='canceled'`, `failure_class='content_mutation'`, `failure_code='canceled_by_request'`, `retryable=false`.
- Returns `Ok(())` so the outer handler does NOT call `fail_canonical_ingest_job`.

**Fail handler guard** (`worker/failure.rs:44`):
- `fail_canonical_ingest_job` now skips `queue_state IN ('completed', 'canceled')` — a race guard: even if somewhere a `JobCanceledByRequest` error fails to downcast, the user's cancel is never clobbered.

**HTTP entry:** `POST /content/documents/batch-cancel` (`interfaces/http/content/batch.rs:175`). Takes a `document_ids` array and loops calling `cancel_jobs_for_document`. The "Cancel Processing" UI button in `DocumentsOverlays.tsx` activates when `selectedCount > 0`.

### 12.5. Retry flow (batch reprocess)

The **"Retry Processing"** UI button routes to `POST /content/documents/batch-reprocess` (`interfaces/http/content/batch.rs:237`). The handler loops over `reprocess_single_document`, which:

1. **Force-resets stale inflight** via `force_reset_inflight_for_retry` (`services/content/service/document.rs`). This is what distinguishes a user-initiated retry from the automatic reconciliation path: a retry is an explicit "stop whatever was happening and start over":
   - `cancel_jobs_for_document(document_id)` cancels every queued and leased job for this document.
   - If `latest_mutation` is still `accepted`/`running`, `reconcile_failed_ingest_mutation` is called with `failure_code='superseded_by_retry'`, which:
     - Flips `async_operation` → `failed`
     - Flips `mutation_items` → `failed`
     - Flips `mutation` → `failed`
     - Re-promotes `content_document_head.latest_mutation_id`
   - Terminal mutations (`failed`/`canceled`/`applied`) are left untouched.

2. **Admits a new mutation** via `admit_mutation(operation_kind='reprocess')`. New mutation, new ingest job, new revision; the same `content_source_kind`, `storage_key`, `source_uri` are propagated through `build_reprocess_revision_metadata` — which is why web-captured pages, uploaded files, and inline documents all retry through a single canonical path.

3. **Diff-aware reuse** automatically skips reprocessing for unchanged chunks (see section 9). Retrying the same content → SHA256 checksums match → `build_chunk_reuse_plan` copies existing `runtime_graph_extraction` rows without LLM calls. Retry is cheap on API usage but still guarantees that anything the previous run never finished is processed fresh (the old attempts are already finalized, the new worker starts clean).

**Why this fixes stalled documents (the primary use case):**

Stalled = `queue_state='leased'` + stale `heartbeat_at` + `mutation_state='accepted'`. Automatic `reconcile_stale_inflight_mutation_if_terminal` refuses to fix this state (it waits for `job_state='failed'`), so the old `ensure_document_accepts_new_mutation` raised `ConflictingMutation` and retry silently incremented `failed_count` in the response. The new `force_reset_inflight_for_retry` terminates the stale mutation explicitly, and admission then proceeds normally.

**Frontend toast with real counts** (`DocumentsPage.tsx:handleBulkReprocess`): parses `BatchReprocessResponse { reprocessed_count, failed_count, results }`. All ok → success toast, all failed → error toast with the first error message, partial → warning toast `"Reprocessing X, Y could not be retried: {error}"`. The previous version always rendered "Reprocessing N documents" regardless of outcome.

### 12.6. Resilience matrix

| Scenario | Coverage | How |
|---|---|---|
| Worker process restarted | ✅ | Startup sweep on pool boot, 30s threshold |
| Backend process restarted | ✅ | Same — worker is part of backend |
| Provider hung for >1 min | ✅ | Periodic reaper, 60s threshold + LLM retry schedule |
| Manual cancel of queued job | ✅ | SQL atomically → `canceled` |
| Manual cancel of leased job | ✅ | SQL → `canceled`, heartbeat observer, pipeline check, canceled finalize |
| Cancel mid-LLM call | ⚠️ | Waits for current stage to finish (bounded by provider timeout) |
| Delete document → auto-cancel | ✅ | `cancel_jobs_for_document_with_executor` inside the delete transaction |
| Retry of stalled document | ✅ | `force_reset_inflight_for_retry` before `admit_mutation` |
| Retry of web-captured document | ✅ | Same path, `content_source_kind` preserved, diff-aware skips unchanged chunks |

---

## 13. Libraries (catalog)

`catalog_library` — Postgres table. Field `extraction_prompt: Option<String>` is a per-library instruction injected into the graph extraction prompt as the `library_context` section.

Loaded in `services/content/service/revision.rs:948`:

```rust
let library_extraction_prompt = catalog_repository::get_library_by_id(...)
    .await.ok().flatten()
    .and_then(|row| row.extraction_prompt);
```

Passed into `build_canonical_graph_extraction_request(..., library_extraction_prompt)`.

The same call site is used to load **sub_type hints** (section 14).

---

## 14. Sub_type flow and vocabulary-aware extraction

### 14.1. Current storage state

- `GraphEntityCandidate.sub_type: Option<String>` (`services/graph/extract/types.rs:66`).
- The LLM returns it in JSON, parsed in `parse.rs:223-228`.
- Stored under `runtime_graph_node.metadata_json -> 'sub_type'` via `merge.rs:483-487`.
- Not part of identity. Not part of retrieval. Pure annotation.

### 14.2. The problem

`sub_type` is intentionally freeform — a deliberate decision so the same graph model works across domains (development, medicine, retail, law, …) without a hard global catalog. The downside: at every extraction the LLM invents a value **from scratch**, with no awareness of which `sub_type`s already live in the graph. The result: for one entity you see variants `relational_database`, `rdbms`, `relational_db` that later collapse into a single node, but `sub_type` flickers.

### 14.3. The fix: vocabulary-aware extraction

Instead of a separate table or materialized view — aggregate on the fly from `runtime_graph_node.metadata_json` and pass into the prompt as a **soft hint**.

**Repo method:** `infra/repositories/runtime_graph_repository.rs:432` — `list_observed_sub_type_hints(pool, library_id, projection_version) -> Vec<RuntimeGraphSubTypeHintRow>`. Top-N per `node_type` (default 15) is applied by the caller.

SQL:

```sql
SELECT node_type,
       metadata_json->>'sub_type' AS sub_type,
       COUNT(*) AS occurrences
FROM runtime_graph_node
WHERE library_id = $1
  AND projection_version = $2
  AND metadata_json ? 'sub_type'
  AND length(metadata_json->>'sub_type') > 0
GROUP BY node_type, metadata_json->>'sub_type'
ORDER BY node_type, occurrences DESC, sub_type
```

In code, top-N (default 15) per `node_type`.

**Wired into the request:** `GraphExtractionRequest.sub_type_hints: GraphExtractionSubTypeHints`. Populated by the helper `load_sub_type_hints_for_extraction` (`services/content/service/revision.rs:1369`), called from `revision.rs:958` next to `library_extraction_prompt`. Same call site, same per-library scope, same projection version (via `resolve_projection_scope`). One SQL aggregate per revision; the result is shared across chunks via clone in the parallel stream (`revision.rs:1063`). A SQL or snapshot failure does not fail ingest — the helper logs a warning and returns `default()`.

**Rendered into the prompt:** the `sub_type_hints` section is inserted in `build_graph_extraction_prompt_plan` (`services/graph/extract/prompt.rs:168-170`) **after** `library_context` and **before** `structured_chunk`. Renderer is `render_sub_type_hints` (`prompt.rs:293`). Format:

```
[sub_type_hints]
Observed sub_types in this library (prefer one of these if it fits;
create a new sub_type only if none match):
- artifact: framework (47), database (32), library (28), microservice (19), …
- attribute: http_status_code (12), latency_ms (8), config_key (6), …
- concept: paradigm (9), pattern (7), …
```

The model stays free — this is a soft hint, not a hard enum. But with an anchor present, ~80% of common cases converge naturally.

**Scope is per library.** Vocabularies from different domains do not mix, otherwise the very idea of freeform per-domain sub_type breaks.

**Performance.** One SQL aggregate before extraction starts for the whole revision (not per chunk). If the graph grows to a size where this becomes a bottleneck, add an expression index `(library_id, projection_version, node_type, (metadata_json->>'sub_type'))`. **Do not optimize earlier.**

### 14.4. What is NOT in this iteration

- **Embedding snap-to-nearest on the write path** — deferred. Will appear only if sub_type hints prove insufficient at eliminating cross-entity near-duplicates.
- **Offline reconciliation job** — deferred. For historical noise accumulated before hints.
- **Persistent sub_type alias set per node** — deferred. The current last-wins-on-upsert + silent-loss-on-resolution policy is not addressed in this iteration; vocabulary-aware extraction at the source should reduce collision frequency enough to make post-hoc merge a rare case.
- **LLM judge during dedup** — out of scope.

If hints still leave visible problems after this lands, the next step will be exactly "proper sub_type merge" (alias set + most-frequent-wins) on both paths (`upsert_graph_node` + entity resolution merge).

---

## 15. File map (quick index)

| Area | Files |
|---|---|
| HTTP entry points | `apps/api/src/interfaces/http/content.rs`, `interfaces/http/ingestion.rs` |
| File parsing | `services/shared/extraction/{file_extract.rs, pdf.rs, docx.rs, spreadsheet.rs, pptx.rs, html_main_content.rs}` |
| Web ingestion | `services/ingest/web.rs`, `services/ingest/web/{single_page.rs, recursive.rs}` |
| Chunking | `services/shared/extraction/chunking.rs` |
| Job runner | `services/ingest/{service.rs, worker.rs, extraction_recovery.rs}` |
| Content service | `services/content/service/{pipeline.rs, revision.rs}` |
| Graph extraction | `services/graph/extract/{types.rs, prompt.rs, parse.rs}` |
| Graph merge | `services/graph/merge.rs`, `services/graph/identity.rs` |
| Entity resolution | `services/graph/entity_resolution.rs` |
| Graph projection | `services/graph/projection.rs` |
| Retrieval | `services/query/{search.rs, execution/retrieve.rs}` |
| Repositories | `infra/repositories/{runtime_graph_repository.rs, catalog_repository.rs}` |
| Schema | `apps/api/migrations/0001_init.sql` (`runtime_graph_node` line 1028) |

---

## 16. Known limitations

- **OCR is not implemented.** Scanned PDFs without a text layer fall through; image OCR is replaced by semantic description via Vision LLM (different semantics).
- **No special handling for GitHub repos / YouTube / API docs** — everything goes through generic HTML readability.
- **`sub_type` is dropped during entity resolution merge** — see section 10. Mitigated by vocabulary-aware extraction (section 14); a full fix is deferred.
- **`relation_type` catalog is hard** (`canonical_relation_type_catalog()`); new types require code changes. This is a deliberate design choice.
- **Entity resolution does not use embeddings** — string-level matching only. May miss semantically close but lexically distinct entities.
