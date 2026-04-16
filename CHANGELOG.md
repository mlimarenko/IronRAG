# Changelog

## 0.3.0 — 2026-04-16

Large-library performance release. On a 4900-document reference library the documents page drops from 26 MB / 1.2 s to 3 KB gzip / 50 ms per page, graph from 60 MB / 1.7 s to 2 MB zstd / 60 ms on Redis hit, `get_library_summary` from a broken 11 s to a correct 1.3 s, and batch rerun moves from a 5 s sync handler that timed out beyond a hundred docs to an async job with polling.

### AI catalog & admin UX

- AI model compatibility is now classified canonically across OpenAI, Qwen, DeepSeek, and Ollama from one binding-purpose model. Hosted DeepSeek remains text-only; multimodal and embedding roles are exposed only where the provider or discovered model family actually supports them.
- `GET /v1/ai/models` no longer performs live provider `/models` discovery on every admin-page load. Generic reads now resolve against the persisted catalog plus visible credentials, while provider discovery sync runs on credential save and on narrow credential-specific refreshes only. In local profiling the AI admin read path dropped from multi-second stalls to low-millisecond responses.
- The Admin `AI` tab was redesigned for high-cardinality catalogs: scope cards, provider inventory, binding cards, searchable credentials and presets, and lazy Ollama model checks in the editor. Fixed the library-override crash caused by the missing `selectedCredentialModelSet` binding state.

### Document operations & ingest hardening

- `GET /v1/content/documents` now honors the documents UI's largest page size: the hard clamp moved from 200 to 1000 rows so bulk-selection and batch actions no longer silently operate on only the first 200 matches of a filtered result set.
- The documents surface now supports true "select all matching" expansion across paginated filtered results, keeps failed uploads visible after a batch completes, and avoids clearing operator-visible per-file errors immediately after acceptance.
- Upload MIME admission is now sniffing-first for unknown text filenames: generic binary MIME plus unknown suffixes like `.env.backup.20260116-162838` are accepted when the payload is clearly text, while explicitly unsupported declared formats such as `video/mp4` still fail fast.

### Documents list

- `GET /v1/content/documents` is now keyset-paginated: `cursor`, `limit` (50/200), `search` (ILIKE), `sortBy`, `sortOrder`, opt-in `includeTotal`. Response is a slim `ContentDocumentListItem` with server-derived `status` and `readiness`; heavy fields moved to the per-document detail endpoint used by the inspector.
- Backend list path is a single Postgres CTE with LATERAL joins replacing the previous 6-call batch prefetch. The `(library_id, created_at DESC, id DESC)` keyset index and a GIN trigram index for `search` over `lower(external_key)` are part of the canonical `0001_init.sql` baseline.
- `get_library_summary` now uses a single aggregate SQL (`aggregate_library_document_readiness`) instead of iterating the document list. Fixed a Cartesian `LEFT JOIN ingest_job` that was inflating counts ~4000× during integration.
- Frontend `DocumentsPage` rewritten around server pagination + `IntersectionObserver` infinite scroll. Debounced server search and sort. Client-side status/readiness filters and local `slice()` paging removed — no mixed client/server filtering.

### Graph topology

- New `GET /v1/knowledge/libraries/{id}/graph` replaces `/graph-topology`. Wire format is compact NDJSON: sections `meta → id_map → docs → nodes → edges → doc_links → end`, UUIDs emitted once through a `u32` id map, nodes use short field keys, edges are 4-tuples `[from, to, rel, support]`. Fields the frontend never reads (`metadata_json`, workspace/library/timestamp columns, `normalized_assertion`, …) are stripped from the wire. No truncation — the full graph is always delivered.
- Redis cache by `graph:{library_id}:v{projection_version}` with a 24 h TTL, invalidated from every `upsert_runtime_graph_snapshot` call site in `projection.rs` and `rebuild.rs`.
- Frontend `getGraphTopologyStream` parses the NDJSON through a `ReadableStream` and surfaces `onProgress` to `GraphPage` for a live loading indicator.

### Observability

- `/metrics` Prometheus endpoint via `axum-prometheus` (`axum_http_requests_total`, `axum_http_requests_duration_seconds` labelled by `method`, `endpoint`, `status`). Published on `127.0.0.1:9464` in the local dev compose; in prod compose it stays off the host and is scraped via `docker exec`.
- `#[tracing::instrument]` on the hot paths: `list_documents`, `prefetch_document_summary_data`, `get_graph_topology`, the runtime-graph repository functions, `batch_reprocess_documents`, `reprocess_single_document`. Spans carry `library_id`, relevant counts, and `elapsed_ms`.

### Worker — heartbeat starvation fix

- Root cause of the repeated `lease_expired / stale_heartbeat` failures: `projection::project_canonical_graph`, `projection::project_targeted_canonical_graph`, and the per-chunk `serde_json::from_value::<GraphExtractionCandidateSet>` inside `rebuild.rs`'s `buffer_unordered(8)` were fully synchronous CPU-bound paths on tokio worker threads with no yield points. They pegged every tokio worker at 100 %, starving the heartbeat task so leases expired while the worker was still alive.
- Fix: those CPU hot spots now run inside `tokio::task::spawn_blocking`. The heartbeat task moved onto a dedicated `heartbeat_postgres: PgPool` (`min_connections=1`, `max_connections=2`) so the main pool can be fully checked out without blocking heartbeats. New stage-level timeout `IRONRAG_RUNTIME_GRAPH_EXTRACT_STAGE_TIMEOUT_SECONDS` (default 600) wraps `materialize_revision_graph_candidates` + `reconcile_revision_graph` via `tokio::time::timeout`, surfacing `stage_timeout` failures through the existing "degraded to readable" branch instead of silently holding a lease.

### Batch rerun — async job with polling

- `POST /v1/content/documents/batch-reprocess` now returns `202 Accepted` with `{ batchOperationId, total, libraryId, workspaceId }`. A spawned task admits the child mutations via `buffer_unordered(N)` (default 4, `IRONRAG_BATCH_REPROCESS_PARALLELISM`) bounded by `IRONRAG_BATCH_REPROCESS_TIMEOUT_SECS` (default 3600). Library consistency is enforced up front — every `documentId` must belong to the same library.
- `ops_async_operation` carries a nullable `parent_async_operation_id` FK directly in the canonical `0001_init.sql` baseline. This is the canonical single-table batch-ops mechanism — any future batch endpoint reuses the same parent/child shape.
- `GET /v1/ops/operations/{id}` returns the parent row plus `progress { total, completed, failed, inFlight }` via a single `LEFT JOIN` + FILTER aggregate. For any parent with children, `status` and `completedAt` are derived on read from progress counts (`processing` while any child pending, `failed` on any child failure, else `ready`) — no race between the spawned admit phase and child lifecycle.
- Old `BATCH_MAX_DOCUMENTS = 1000` cap replaced by a DoS sanity cap of 100 000 ids.
- Frontend shows a progress banner that polls `opsApi.getAsyncOperation(id)` with 1.5 s → 5 s adaptive backoff, refreshes the document list on terminal states, and auto-dismisses 4 s after completion.

### Library backup / restore (tar.zst)

- **Format redesign: NDJSON → tar.zst.** The old NDJSON stream truncated in browsers because the frontend buffered the entire response via `fetch().blob()`. The new format is a streaming tar archive compressed with zstd level 3 (~13× on NDJSON text). Archive layout: `manifest.json` → chunked NDJSON per table/collection (64 MiB soft cap per part) → raw blob entries → `summary.json`. `tokio-tar` + `async-compression` write into a `tokio::io::duplex` pair piped into `Body::from_stream` with natural back-pressure.
- **Streaming Arango cursor.** New `ArangoClient::query_json_batches` yields cursor batches via an async callback instead of merging the entire result into memory. The old path OOM-killed the backend on `knowledge_structured_block` (545 k rows / ~1 GB JSON). Export now streams through a bounded `mpsc` channel (depth 2) between the cursor producer task and the tar writer.
- **Edge `library_id` propagation.** Every Arango edge insert now carries `library_id` (10 wrapper methods, 12 call sites, 8 files). Persistent indexes on `library_id` for all 15 edge collections. Snapshot export edge query uses `FILTER edge.library_id == @lid` instead of the old per-edge `DOCUMENT()` lookup — edge export stage dropped from **11.3 s → <2 ms** on the Artix fixture.
- **Batched import.** `PgBatcher` flushes every 1 000 rows via `jsonb_populate_recordset`; `ArangoBatcher` flushes via `FOR doc IN @docs INSERT`. Artix full restore (107 k pg + 466 k arango rows) dropped from **>5 min (nginx 504)** to **~78 s**.
- **Bulk Arango timeout.** `query_json_bulk` and `query_json_batches` carry a 10-minute per-request timeout override. The canonical 15 s client timeout was truncating edge cleanup and large-batch inserts.
- **Replace mode.** `POST /snapshot?overwrite=replace` clears the library footprint (reverse-FK Postgres delete, Arango edge/doc purge, blob stash) before restoring. Import trusts the archive manifest for include kinds — no duplicate selector on the request.
- **Simplified include scope.** `IncludeKind` collapsed from `content / runtime_graph / knowledge / blobs` to **`library_data` + `blobs`**. A library is one atomic domain unit; the old split leaked storage tiers into the UI. Back-compat shim maps old tokens to `library_data`. UI dialog: one always-on "Library data" card + one "Include source files" checkbox.
- Export throughput on Artix (24 k nodes, 82 k edges, 445 k structured blocks): **6.1 s / 42 MiB** (was 17.4 s before edge indexes, truncated before tar.zst).

### Graph rendering performance

- **Web Worker layout offload.** `applyGraphLayout` runs in a dedicated module worker (`graphLayout.worker.ts`, ~70 KB chunk) for graphs ≥ 3 000 nodes. The main thread builds Graphology + inits Sigma while the worker computes coordinates off-thread; positions are returned via a `Float32Array` transferable buffer (zero-copy). Artix first canvas paint: **~1.6 s** (was browser "page is slowing down" warning).
- **Hidden-edge precompute.** The `hiddenIds → hiddenEdgeIds` derivation moved from the per-hover reducer effect (O(M) on every hover commit) to a dedicated effect keyed only on `hiddenIds` change. Hover branches read a pre-built `Set<edgeId>` ref — O(1) per edge per frame.
- **O(degree) selection.** Click-mode connected-edge lookup uses `graph.edges(selectedId)` (O(degree)) instead of `graph.forEachEdge` (O(M)). On Artix: ~8 edges vs 82 k scan per click.
- **Instant layout at density.** Layout transitions skip the 280 ms per-frame interpolation at ≥ 5 000 nodes — the animation burned 1.5 M `setNodeAttribute` calls/sec with no visual value at that density.
- **Labels disabled at extreme density.** `renderLabels: false` at > 15 000 nodes eliminates Sigma's label collision pass (the dominant per-frame cost). `selectProminentGraphLabelIds` O(N log N) sort also skipped in that regime.
- **Byte-buffered NDJSON parser.** `getGraphTopologyStream` uses a `Uint8Array` ring buffer with direct 0x0A scan instead of the old `pending += chunk; pending.slice()` pattern, preventing O(N²) string churn at 100 k+ node topologies.

### MCP performance

- **N+1 eliminated from capability snapshot.** `visible_workspaces` used to loop N workspaces issuing one `load_visible_library_contexts` per workspace. New `visible_catalog` loads workspaces + all libraries in 2 concurrent queries via `tokio::try_join!` and groups in memory. `mcp.capabilities`: 132 ms → **50 ms**; `mcp.initialize`: 116 ms → **53 ms**.

### Release-readiness tooling

- **`scripts/ops/release-check.py`** — consolidated pre-release smoke + perf suite. 24 checks covering auth, catalog, content, knowledge, snapshot export, and MCP tools. Per-check latency budgets with pass/warn/fail verdicts, top-10 latency table, machine-readable JSON output. Replaces ad-hoc curl scripts.

### Extraction pipeline refactor

- **tree-sitter AST extraction for 15 languages.** Code identifier extraction switched from substring heuristics (`"fn "`, `"class "`, `"const "`) to real AST parsing via tree-sitter. Supported: Python, JavaScript, TypeScript, Bash, Rust, Go, Java, C, C#, Ruby, PHP, Swift, Scala, YAML, Proto. Each grammar tested with per-language unit tests. Heuristic fallbacks removed entirely — if tree-sitter doesn't support the language, no code identifier fact is produced.
- **Structural config parsing (JSON, YAML, TOML).** Config key extraction via `serde_json`, `serde_yaml`, `toml_edit` replaces the old `split_once(':')` / `split_once('=')` heuristic that produced false positives on prose. Only declared config-language blocks are parsed.
- **URL and version parsing via real crates.** `url::Url::parse` (RFC 3986) replaces `starts_with("http://")`. `semver::Version::parse` replaces manual digit splitting.
- **Block-family dispatch.** The extraction loop dispatches extractors by block kind instead of running all 14 on every line. CodeBlock → identifiers + env vars + config keys; EndpointBlock → full HTTP surface; Table → endpoints + config + params; Prose → URLs + versions + error codes.
- **Parser-derived confidence.** Confidence now derived from the parser that produced the fact (AST node → 0.98, structural parse → 0.97, keyword heuristic → 0.94) instead of hardcoded values per block kind.
- **Typed QuestionIntent.** `QuestionIntent` enum with 10 intents (Endpoint, Parameter, HttpMethod, Version, ErrorCode, EnvVar, ConfigKey, Protocol, BasePrefix, Port) and a canonical bilingual keyword table. Replaces scattered `contains()` chains across 4 files.
- **Fact-grounded deterministic answers.** Endpoint and parameter answers now require matching technical facts in the evidence store, not substring parsing of chunk text.
- **Branded identifier heuristics removed.** Heading phrase matching and catalog link guessing deleted from the pipeline — entity extraction is the LLM's job, not the fact store's.

### Token scopes & MCP

- **System-scope tokens.** New scope level for tokens with no workspace restriction — full admin across all workspaces. UI scope selector: System / Workspace / Library.
- **Auto-inference of workspace_id / library_id.** MCP tools and the query API auto-fill IDs from the token scope when exactly one workspace or library is accessible. `list_documents.libraryId` is now optional; query session `workspaceId` is optional.
- **Leaner MCP initialize response.** Removed `tokenId`, `tools` list (duplicate of tools/list), `generatedAt` from the initialize JSON-RPC response. 833 → 270 chars.
- **Permission hierarchy in token mint UI.** Grouped cards (Admin, Workspace, Library & Content, Operations) with lucide icons and human-readable labels. Implied permissions show as checked+disabled. `connector_admin` added. Labels explicitly mention import/export capabilities.

### Query execution performance

- **O(1) edge lookup.** `QueryGraphIndex.edges` switched from `Vec` with linear `.find()` to `HashMap<Uuid, GraphViewEdgeWrite>`. On the Artix fixture (82 k edges), `map_edge_hit` dropped from ~41 M comparisons to ~500 hash lookups per query.
- **Batched extract candidate inserts.** `replace_extract_node_candidates` and `replace_extract_edge_candidates` switched from per-row INSERT to `QueryBuilder::push_values()` bulk INSERT … RETURNING. On a 5 k-doc library with ~50 candidates per chunk, this eliminates millions of sequential round-trips during ingest.
- **Batched audit subject inserts.** `append_audit_event` subject loop switched to single-statement `push_values` INSERT.
- **Extract candidate indexes.** New migration `0003_extract_candidate_indexes.sql` adds `CREATE INDEX CONCURRENTLY` on `extract_node_candidate(chunk_result_id)` and `extract_edge_candidate(chunk_result_id)` — the tables lacked FK indexes and every per-chunk DELETE/SELECT was a full table scan.

### AI catalog bootstrap

- **Pre-seeded model presets for all providers.** `seed_all_provider_presets` runs at bootstrap and creates presets for OpenAI, DeepSeek, Qwen, and Ollama (4 purposes × 4 providers = 16 presets) regardless of whether API keys are configured. Operators can immediately assign bindings after adding a credential — no manual preset creation required.
- **Preset lookup fix.** `select_runtime_preset` no longer falls back to a single-model match when the canonical name doesn't match. The old behavior returned the wrong preset when two purposes (e.g. ExtractGraph + QueryAnswer) shared the same model (qwen3:0.6b), preventing the second preset from being created.

### Code quality

- **Named constants for vector kinds.** `VECTOR_KIND_CHUNK` / `VECTOR_KIND_ENTITY` (8 sites). `FACT_FETCH_MULTIPLIER` / `FACT_FETCH_MIN`. Domain status constants `ASYNC_OP_STATUS_*`, `MUTATION_KIND_*`, `GRAPH_STATUS_*` (15+ sites).
- **`execution_id_of()` helper.** Deduplicates 5-site `.expect(...)` pattern.
- **ServiceRole enum dispatch.** Config string matching replaced with existing `ServiceRole::Api/Worker/Startup` enum.
- **N+1 query eliminated in document lifecycle.** Batch `list_ingest_attempts_by_jobs` / `list_ingest_stage_events_by_jobs` via `WHERE job_id = ANY($1)` — 2 queries regardless of job count.
- **Sequential awaits → `tokio::try_join!`** in AI catalog service.
- **Dead code removal.** Query execution scaffolding, branded identifier heuristics, unused types.
- **In-place dedup.** BTreeSet roundtrip pattern (4 sites) → `sort_unstable() + dedup()`.
- **MCP library_ids capped** at 50 per request.

### Platform bits

- `tower-http::compression` layer (gzip + br + zstd) added globally.
- `nginx.conf.template`: `proxy_buffering off` and `proxy_request_buffering off`.
- New deps: `tokio-tar`, `async-compression`, `tokio-util`, `tree-sitter` 0.25 + 15 grammar crates, `serde_yaml`, `toml_edit`.
- All backend + frontend deps updated to latest compatible versions.

## 0.2.3 — 2026-04-14

### Diff-aware reuse correctness & graph fail-fast

- **Diff-reuse no longer poisons the new revision's lifecycle**. `materialize_revision_graph_candidates` synthesizes a `runtime_graph_extraction` record for every chunk whose text checksum matches the parent revision, but used to clone `raw_output_json` verbatim — including the parent revision's `lifecycle.revision_id`. The downstream `reconcile_revision_graph` filter then dropped every reused record because `extraction_lifecycle.revision_id != Some(current_revision_id)`, leaving the merge with **0 records → 0 nodes → 0 edges** even though all 100 chunks looked "ready" in the table. Fix: rewrite `raw_output_json.lifecycle.revision_id` to the current revision before persisting the synthetic record. Triggered for any retry/edit on a CSV-style document where every row is structurally identical.
- **Fail-fast graph pipeline**. `run_inline_post_chunk_pipeline` used to swallow a "0 contributions" reconcile by recording `extract_graph` as `completed` with `graphReady=false` and degrading to `graph_state='processing'`, leaving the document permanently stuck in a misleading half-state with no error visible. The pipeline now returns `ApiError` whenever `materialize` errors, `reconcile` errors, or `graph_ready` is false; the ingest job is marked `failed` and the revision's `graph_state='failed'`. Single canonical path — no `degradedToReadable` shim.
- **Honest stage timings**. The "embed_chunk" stage used to fire a fake "deferred" `started`/`completed` pair before `materialize` even ran, then re-emit `completed` after reconcile finished, which made `merge_stages` report the full wall clock (~19.6 s) for both `Extract Graph` AND `Embed Chunk`. The pipeline now uses two independent timers: `extract_elapsed_ms` covers only the LLM materialization phase, `embed_elapsed_ms` covers only the reconcile phase, and the `embed_chunk` stage event is emitted **only** when `embedding_usage` is actually present (no no-op fallback row).
- **Single source of truth for `Extract Graph` model name**. `materialize_revision_graph_candidates` now reads `provider_kind` and `model_name` directly from the active extract-graph binding (`graph_runtime_context.provider_profile.indexing`), so the pipeline event always carries the binding model — even when 100 % of chunks are reused and the per-chunk LLM call never ran. Removed the `provider_kind` / `model_name` fields from `ChunkExtractAggregate` since they were dead code under the new scheme.

### Document status priority chain

- **In-flight job beats stale readiness**. Both `interfaces/http/ops.rs::map_document_status` and `apps/web/src/pages/documents/mappers.ts::mapApiDocument` checked `readiness_is_ready` before checking `queue_state`, so a document with previous successful readiness and a freshly enqueued retry job stayed visually "Ready" until the retry actually finished — operators couldn't tell if their click had registered. The priority chain is now: terminal failure → `canceled`/`failed` → `leased` (Processing/Blocked/Retrying/Stalled) → `queued` → `graph_ready`/`readable` → `graph_sparse` → zombie `completed`. Backend and frontend mappers stay symmetric.

### Documents page live updates

- **Quiet 5-second background refresh**. The documents page used to poll only when at least one document was in `processing` state, on a 15-second interval, and replaced the entire `documents` array on each tick — which left selected-row inspector data stale until the user clicked the row again. Polling now runs unconditionally on a 5-second cadence whenever the page is mounted, refreshes both the table rows and the selected document's full inspector payload (lifecycle, segments, facts) in place, and never resets row selection or any inspector sub-state. `silent=true` keeps the spinner and error banner out of the polling path.

### Ingest throughput & worker stability

- **Parallel per-chunk graph merge**. `reconcile_revision_graph` used to walk `latest_records_by_chunk` sequentially, paying ~130 sequential DB round-trips per chunk × N chunks per doc — pinning all 4 tokio runtime worker threads at 100 % CPU and starving the heartbeat / dispatcher / cancel-poll tasks. Wrapped the chunk loop in `stream::iter().buffer_unordered(8)` with `Arc`-shared pool / quality_guard / document / merge_scope captures, and consume `latest_records_by_chunk.into_values()` so `record.normalized_output_json` is `mem::take`'d straight into the deserializer (no per-chunk deep clone of `serde_json::Value`). Empirically reproduced on prod data: worker CPU dropped from **502 % → 7 %** (62× less starvation), tokio threads moved from `state=R` (running) to `state=S` (sleeping on futex), pipeline throughput unblocked.
- **Bulk `runtime_graph_evidence` insert**. New `bulk_create_runtime_graph_evidence_for_chunk` repository function batches every per-chunk evidence row (typically 50+ for 10 entities + 10 relations) into a single `INSERT ... SELECT FROM unnest(...)` round-trip. `merge_chunk_graph_candidates` collects `GraphEvidenceTarget`s into a `Vec` during the per-entity / per-relation walk and flushes once at the end. Replaces ~50 sequential single-row INSERTs per chunk with one bulk insert.
- **TLS `close_notify` retry coverage**. The transport retry classifier was missing the rustls "peer dropped TLS session mid-response" pattern. Production worker would see `error decoding response body: ... peer closed connection without sending TLS close_notify` from the LLM provider, fail to recognize it as retryable, and surface `extract_graph` as failed after ~51 s at the outer recovery layer. Added `peer closed connection` and `close_notify` to `is_retryable_transport_error_text` so these cases now go through the canonical `[1, 3, 10, 30, 90]` s schedule.

### Dashboard / documents page consolidation

- **Single source of truth for "Failed" counts**. The dashboard `failed_documents` and the documents-page filter pill used different mappers and disagreed by an order of magnitude (dashboard reported 142 failed, the documents page reported 4829 for the same library). The backend `map_document_status` in `interfaces/http/ops.rs` now mirrors the frontend `mapApiDocument` rules: `queue_state='canceled'` and zombie completions (`queue_state='completed'` with no readable / sparse readiness) both map to `DocumentStatus::Failed`, matching the frontend bucket. Both surfaces now report the same count.
- **Empty graph view fix**. `get_graph_workbench` and the graph topology endpoint relied on `runtime_graph_state` to derive an entity's `entity_state`, but the function read `metadata.extraction_recovery_status` first — which contains values like `clean`, `recovered`, `partial` describing how the node was extracted, **not** whether it is admitted. With "active" hard-coded as the filter target, ~99 % of nodes were dropped: a large library's graph displayed as "0 nodes / Graph is empty". Fixed by removing `extraction_recovery_status` from the state derivation chain — entity_state now resolves from `metadata.entity_state` / `metadata.relation_state` and falls back to `"active"`.

### Graph viewer interactivity (frontend)

- **Edges no longer disappear during pan/zoom** — `hideEdgesOnMove: false`. The previous `hideEdgesOnMove: denseGraph` made the graph feel broken on every cursor move.
- **Hover ≠ click**. Hover used to dim every other node and re-color edges; now hover only highlights the node + neighbors, and edges are never touched. Click owns the focus mode (selected node + neighbors keep color, every other node fades to white, only edges incident to the selection light up).
- **Dwell-time hover (140 ms)**. Hover state commits only after the cursor pauses on a node — fast sweeps across a dense graph never trigger the expensive `sigma.refresh()` (~120 ms on 25 k nodes). The dwell gate keeps `HOVER_FPS` at the 60 fps baseline during sweeps.
- **DOM tooltip card anchored to node viewport**. Hover shows a floating card with the full node label, neighbor count, and the first 12 neighbor names. The card is positioned via `sigma.graphToViewport(node.x, node.y)` and re-anchored on every camera `updated` event, so it stays glued to the node during pan/zoom instead of trailing the cursor.
- **Density-aware label rendering**. `labelRenderedSizeThreshold` scales 8 → 10 → 14 → 20 with `nodes.length` and `labelGridCellSize` jumps 100 → 240 on dense graphs. `hideLabelsOnMove` is now true for every graph above 5 k nodes (was 140). Hover no longer `forceLabel`-s neighbors on dense graphs.
- **Pre-computed neighbor index + label lookup**. `useMemo<Map<string, Set<string>>>` rebuilt only when `nodes` / `edges` change; hover and click both read it as O(1) instead of walking the graphology adjacency list each time.
- **Layout spacing tuned for `autoRescale`**. `layoutBands`, `layoutSectors`, `layoutRings` now spread cells aggressively (`orderRoot * 1.4` for bands, `× 4` inner radius for sectors, `× 1.6` ring gaps) so even after Sigma compresses 25 k nodes into the viewport the cells stay visually distinct.
- **Density-aware node radius**. Per-node `size` shrinks with the visible node count (3..13 → 2..7 → 1.4..4 → 1..2.6 across density tiers) so dense graphs do not paint as a solid color block.

### Local profiling environment

- Reproduced the 0.2.2 symptoms on a local stack seeded from a production pg_dump of a large reference library (~5 k docs, ~25 k graph nodes, ~80 k edges, ~230 k evidence rows). Root-cause analysis behind the 0.2.3 changes and the reproduction harness used to validate the merge-loop and viewer fixes (Playwright + Chromium hover-FPS measurement) captured in `tmp/prod-0.2.2-perf-analysis.md`.

## 0.2.2 — 2026-04-13

### Ingest resilience

- Startup lease sweep (`reclaim_orphaned_leases_on_startup`) requeues orphan `leased` rows before the dispatcher claims anything, so a backend/worker restart no longer freezes in-flight docs. Steady-state reaper tightened to 15s interval / 60s stale threshold.
- Dispatcher claim query now counts **all** `queue_state='leased'` rows against the global / workspace / library limits — the previous `heartbeat_at` freshness filter introduced a TOCTOU that let the per-library cap be bypassed, stacking parallel docs past the cgroup.
- Cancel now reaches active leases. `cancel_jobs_for_document` flips both `ingest_job → canceled` and `ingest_attempt → canceled` in one CTE; the heartbeat loop polls `queue_state` and signals a `JobCancellationToken` that pipeline stages check between steps, so a cancel is observed in ≤15 s.
- Retry unsticks stalled documents (`force_reset_inflight_for_retry`), falls back to the latest `content_revision` when the head promote never fired, and tombstones orphans with zero revisions. Web retries re-fetch the source URL via `WebIngestService::refetch_document_source`; uploaded docs reuse their stored source. Diff-aware chunk reuse is preserved across retries.

### Worker memory footprint

- Vision LLM image bytes moved instead of cloned (`describe_extracted_images` takes `Vec<ExtractedImage>` by value, drops each image after its call). `extraction_plan` is moved into `CanonicalExtractedContent` instead of deep-cloned. Per-chunk graph-extract uses `Arc<…>` for shared state and `try_fold` instead of `buffer_unordered + try_collect` so nothing accumulates across a document's chunk stream.
- PDF extractor parses the `lopdf::Document` once (was twice), `drop`s it before vision phase, and caps images at 30 / 150 MB per doc. HTML extractor truncates the decoded source to 4 MB at a tag boundary before `scraper::Html::parse_document` so pathological Confluence exports can't blow the `html5ever` arena. Both paths warn but never reject a document.
- Noisy third-party `tracing` crates (`scraper`, `html5ever`, `selectors`, `hyper`, `h2`, `reqwest`, `rustls`, `sqlx`, `mio`, `tower`, `tonic`, `tungstenite`) are clamped to `warn` in `telemetry::init` regardless of `IRONRAG_LOG_FILTER`, so `debug` no longer amplifies HTML parse events into gigabytes of log allocation.
- `build_chunk_reuse_plan` stores `Arc<RuntimeGraphExtractionRecordRow>` in all three HashMaps so the diff-reuse path refcount-bumps instead of deep-cloning `raw_output_json` + `normalized_output_json` three times per doc.

### Throughput & CPU

- Per-library dispatcher ceiling raised to 16 with a **memory-aware throttle**: before each claim, `fill_available_job_slots` reads worker RSS and holds new claims when the process is over `ingestion_memory_soft_limit_mib`. The soft limit auto-resolves to 90 % of the detected cgroup (or `/proc/meminfo`) memory — any container size gets a sensible cap with zero manual tuning; an explicit positive value overrides.
- Per-document graph-extract fan-out decoupled from the cross-doc limit via the new `ingestion_graph_extract_parallelism_per_doc` (default 8). Heavy docs get proper chunk parallelism without raising `per_library`.
- LLM transport retries now follow a fixed `[1, 3, 10, 30, 90]` second schedule (5 retries, 134 s total) covering timeouts and the retryable 4xx/5xx set (408, 409, 425, 429, 500, 502, 503, 504, 520–524, 529). `llm_transport_retry_attempts` bumped 3 → 5 and `runtime_graph_extract_recovery_max_attempts` 2 → 4 so both layers have room to run.
- Synchronous extractors (`extract_pdf`, `extract_docx`, `extract_tabular`, `extract_pptx`, `extract_html_main_content`) now run on `tokio::task::spawn_blocking`, freeing the async runtime for concurrent I/O. Postgres connection pool raised 20 → 64 so 16 parallel jobs don't contend on connections. Heartbeat loop now respects `ingestion_worker_heartbeat_interval_seconds` instead of using a hardcoded constant.

### Vocabulary-aware graph extraction

- New `list_observed_sub_type_hints` aggregates existing `metadata_json->>'sub_type'` values per `node_type` for the library + projection version. `build_graph_extraction_prompt_plan` renders them as a `sub_type_hints` section so the model converges on in-use vocabulary instead of inventing fresh near-duplicates.

### Document status taxonomy (UI)

- Canonical `DocumentStatus` with strict priority: `canceled` → `failed` → `ready` → `ready_no_graph` → `blocked` / `retrying` / `stalled` → `processing` → `queued`. Timer only ticks for in-flight states. `documentStatusSortRank` + `Finished` column give operators proper severity sort and completion time.
- Filter pills rebuilt around four buckets: **All | In Progress | Needs Attention | Ready | Failed**. Retry toasts surface real outcomes with accurate skipped / failed counts; documents that no longer exist come back as `skipped` instead of `failed`. `queue_state='completed'` + `readiness='processing'` zombies are reclassified as `failed` with an explicit reason.
- Inspector cleanup: dropped the legacy "Re-ingest from URL" button, summary truncated to 280 chars with a "Show full" toggle, `source_uri` shown for `web_page` docs instead of the `viewpage.action` basename.

### Documentation

- `docs/{en,ru}/PIPELINE.md` refreshed for the new parallelism model, memory-aware throttle, auto-resolved soft limit, LLM retry schedule, and updated lease thresholds.

## 0.2.1 — 2026-04-12

- Fixed web snapshot persistence for long percent-encoded page and attachment URLs.
- Fixed graph edge rendering during zoom and selection so focused neighbors stay visible above unrelated nodes.
- Fixed web-ingested documents to render as `Web page` instead of leaking raw source/file extensions into the type column.
- Added canonical token minting for an explicit target workspace plus multi-library grants in one token, fixed library-scoped discovery so tokens only enumerate selected libraries, and closed the workspace-scoped `iam_admin` auth bypass.
- Hid unfinished connector-admin permissions from user-facing token issuance surfaces while keeping connector support on the backend roadmap.

## 0.2.0 — 2026-04-12

### Highlights

- Rebranded the shipped product from `RustRAG` to `IronRAG` across env vars, packages, images, charts, OpenAPI, and release-facing docs.
- Added a full-screen document editor for text, `docx`, and `xlsx` uploads with table-aware markdown editing and automatic reprocessing after save.
- Unified `xlsx`, `docx`, `pdf`, and `csv` table handling on one canonical extraction path with grounded row and column-summary semantics.
- Added a full document lifecycle inspector with per-stage duration, model/provider identity, token usage, and cost from the canonical billing path.
- Rebuilt the in-app assistant on one canonical MCP-tool agent loop and added a DeepSeek bootstrap preset for first-run provider setup.
- Added diff-aware ingest reuse so unchanged chunks can skip repeated graph extraction on document replacement and edit flows.

### Breaking Changes

- **Schema reset**: the database baseline was consolidated to one canonical `0001_init.sql`; legacy execution and accounting paths were removed.
- **Assistant/MCP cutover**: the standalone `ask` shortcut and parallel special-case assistant flow were removed; assistant Q&A now runs only through the canonical MCP tool loop.
- **IRONRAG rename**: release-facing configuration now uses `IRONRAG_`* naming instead of `RUSTRAG_*`.

### Platform

- Billing and pipeline cost rollups now come from one canonical source of truth, including vision and embedding calls.
- System-level IAM grants now authorize correctly across workspaces, libraries, and documents.
- Document delete and revision-head writes were hardened so canonical Postgres state can commit cleanly even when read-model cleanup degrades to warnings.
- Grounding guardrails now refuse conflicting or insufficiently supported answers instead of shipping hallucinated output.
- Added `ironrag-cli` for users, tokens, workspaces, libraries, and scoped permission management.
- Deployment surfaces were aligned around the split `web` / `api` / `worker` / `startup` topology for Docker and Helm.

### Refactor

- Split major backend hotspots into focused graph-store, MCP, AI catalog, ingest, graph-service, and query submodules while deleting legacy release artifacts and dead code.
- Trimmed the MCP protocol to the canonical tool surface only: no fake resources capability, no legacy aliases, and permission-filtered `tools/list`.
- Removed silent error swallowing and cleaned up the release line to a zero-warning backend build.

### Validation

- Added release-gate coverage for end-to-end pipeline quality and graph mutation correctness.
- Revalidated the Docker and Helm deployment paths for the `0.2.0` release line.

## 0.1.3 — 2026-04-10

### Performance — Ingestion Parallelism

- **Parallel embedding batches** (`ingest/runtime.rs`): node and edge embedding now sends batches in parallel via `futures::stream::buffer_unordered`. New env var `IRONRAG_INGESTION_EMBEDDING_PARALLELISM=4` (default) controls how many embed batches run concurrently per job. For a 200-chunk document this is ~4x faster end-to-end.
- **Per-library job isolation** (`ingest_repository::claim_next_queued_ingest_job`): SQL claim now optionally caps the number of `leased` jobs per library. New env var `IRONRAG_INGESTION_MAX_JOBS_PER_LIBRARY=0` (0 = unlimited) prevents one busy library from starving others when many docs are queued at once.
- **Parallel web crawl fetches** (`ingest/web::discover_recursive_scope`): the BFS frontier is now drained in waves of N candidates and HTTP fetches run in parallel via `buffer_unordered`, while DB writes stay sequential per result for canonical/seen-set determinism. New env var `IRONRAG_WEB_INGEST_CRAWL_CONCURRENCY=4`.
- All three knobs are independent and tunable per deployment via env vars. Defaults are conservative (4) and tested against the local stack.

### Refactor — Bootstrap Flow Cleanup

- **Removed legacy `/iam/bootstrap/claim` endpoint** entirely: route, handler, `BootstrapClaimRequest/Response`, `BootstrapClaimCommand/Outcome`, service method, OpenAPI paths and schemas, integration test, and the `bootstrap_token` / `bootstrap_claim_enabled` config fields with their `IRONRAG_BOOTSTRAP_TOKEN` env var. Single canonical bootstrap surface is now `/iam/bootstrap/setup` only.
- **Display name optional** in bootstrap setup: backend already accepted `Option<String>` and falls back to login; frontend no longer validates it as required and passes `undefined` when empty.
- **Required field markers** in `LoginPage`: red `*` next to required labels (Admin login, Password) plus `(optional)` hint next to Display name; matching `login.optional` i18n key in en/ru locales.
- **Cursor pointer everywhere**: added `cursor: pointer` to all `button:not(:disabled)`, `[role="button"]`, `a[href]`, `label[for]`, `summary`, `select` via `@layer base` global rule in `index.css`, plus baked into the `Button` cva so the shadcn variant gets it explicitly. Disabled controls get `cursor: not-allowed`.

### Refactor — Code Quality + Hierarchy

- `**shared/` restructure**: 7 files moved into `shared/extraction/` (chunking, file_extract, structured_document, text_render, technical_facts) and `shared/web/` (ingest, url_identity). Root keeps only canonical primitives.
- `**services/` restructure**: 43 flat files reorganized into 8 domain folders — `graph/`, `query/`, `content/`, `ingest/`, `mcp/`, `ops/`, `iam/`, `knowledge/`. ~80 import sites updated.
- `**query/execution.rs` split started**: 7909-line megafile reduced to 6346 in `mod.rs` plus 5 extracted submodules — `embed`, `hyde_crag`, `technical_literals`, `verification`, `port_answer` (-20%, 1631 lines moved out).
- *Dead `legacy`_ bootstrap flags removed**: `legacy_ui_bootstrap_enabled`, `legacy_bootstrap_token_endpoint_enabled`, `allow_legacy_startup_side_effects` deleted from config and 5 integration tests. `legacy_ui_bootstrap_admin` renamed to `ui_bootstrap_admin`.
- **Silent error swallowing fixed**: 14 `let _ = state...` audit/append sites converted to explicit `if let Err(e) = ... { warn!(stage=..., error=%e, ...); }` with structured logging. 31 cosmetic `let _ = HashSet::insert()` cleaned up.
- **Frontend `any` elimination**: 52 `any` removed from `pages/{Documents,Graph,Assistant,Admin}.tsx`, all `apiFetch<any>` calls in `api/*.ts` typed against `Raw`* interfaces, `ApiError.body` typed as `ApiErrorBody`. Added 11 new typed interfaces.
- **Observability**: HyDE/CRAG/multimodal extraction stages emit structured `stage=` tracing per the constitution severity convention.
- **Backend test debt cleanup**: split `content_lifecycle.rs` into a reusable `tests/support/content_lifecycle_support.rs` fixture plus a dedicated lineage test file, bringing the main lifecycle test back under the 1000-line cap; removed stale unused worker imports in web-ingest integration tests.

### Deploy

- Added the canonical Helm chart for `web`, `api`, `worker`, and one `startup` job.
- Added `docker-compose-s4.yml` for the bundled stack with [s4core](https://github.com/s4core/s4core) and S3 storage.
- Kept `docker-compose.yml` as the classic bundled stack with filesystem storage.

### Changed

- Split runtime into `api`, `worker`, and `startup`.
- Moved migrations and bootstrap out of serving pods into the startup authority.
- Added the canonical storage contract: `filesystem` or S3-compatible object storage.
- Added real source links for documents and grounded answers.
- Fixed replace/delete cleanup so query-chunk references from superseded or deleted revisions are removed instead of leaving stale graph/query state behind.
- Fixed deleted document detail responses to stop exposing stale readiness, prepared counts, and source download links after terminal deletion.
- Made `/v1/ready` report actual deployment, dependency, storage, and topology state.
- Fixed first-run OpenAI bootstrap validation for the current chat-completions API.
- Fixed Swagger/OpenAPI rendering by removing a duplicate YAML key and adding a duplicate-key test.
- Split the documents page into smaller modules and added a staged 1000-line file limit in pre-commit.

### Validation

- `docker compose config` for filesystem and `s4core` profiles — pass
- `helm lint` and `helm template` — pass
- live upload, query, source-download, and Swagger/OpenAPI checks on the Minikube Helm release — pass

## 0.1.2 — 2026-04-08

### Highlights

- **Sigma.js WebGL graph renderer**: replaced Canvas2D with Sigma.js for rendering 11K+ nodes and 54K+ edges via WebGL. 7 layout algorithms (cloud, circle, rings, lanes, clusters, islands, spiral), node dragging, connected-edge overlay, pointer cursor on hover.
- **Entity sub-type extraction**: LLM pipeline now extracts freeform `sub_type` for entities (e.g., person→engineer, artifact→framework). Flows through ArangoDB storage, API, and frontend legend.
- **Vertical graph legend**: left-side collapsible legend with clickable types and sub-types, counts, show-all/invert/hide controls.
- **Documents page tabs**: split into Documents and Web Ingest tabs with independent views, filter bar with status icons and counts, total cost inline.
- **Full dependency upgrade**: React 18→19, TypeScript 5→6, Vite 5→8, Tailwind CSS 3→4, Zod 3→4, ESLint 9→10, plus 50+ other packages updated.
- **Dashboard cleanup**: removed duplicated status layers, consolidated the main library overview, and made dashboard tiles actionable with direct deep-links into filtered documents and graph views.
- **Truthful operational metrics**: fixed dashboard totals and graph counters so document counts, failed counts, nodes, and edges reflect the full active library instead of truncated recent slices.
- **Admin operations clarity**: replaced raw `degraded` signaling with explicit operator guidance, recommended next actions, and direct navigation to failed documents or graph troubleshooting paths.
- **Audit usability upgrade**: added server-backed audit pagination, result/surface filters, and free-text search in the admin panel.

### Graph

- **Sigma.js WebGL renderer**: replaces Canvas2D. Handles 11K nodes / 54K edges at interactive frame rates via GPU-accelerated rendering.
- **7 layout algorithms**: cloud (force-directed jitter), circle (scaled), rings (concentric by type), lanes (horizontal rows by type), clusters (Vogel-disc per type), islands (BFS connected components), spiral (golden-angle, degree-sorted).
- **Connected-edge overlay**: selected node's edges render on a separate Canvas2D overlay on top of all other edges, with curved arrows and blue highlight.
- **Edge z-index**: `zIndex: 2` for connected edges in Sigma's edge reducer ensures visual priority.
- **Node dragging**: `downNode` + `mousemovebody` + `mouseup` events with camera lock during drag.
- **Pointer cursor**: cursor changes to pointer on node hover via `enterNode`/`leaveNode` events.
- **Vertical legend**: collapsible left-side panel with type counts, clickable sub-types, show-all/invert/hide-legend buttons.
- **Layout toolbar**: monochrome icon buttons (⬡○◎≡⬢◇✺) with active state highlight using `bg-primary`.

### Pipeline

- **Entity sub-type extraction**: added `sub_type: Option<String>` to `GraphEntityCandidate`. LLM prompt updated with sub_type in schema and few-shot examples (framework, database, microservice, http_status_code, etc.).
- **Sub-type storage**: `candidate_sub_type` / `entity_sub_type` fields added to ArangoDB entity documents. Schema-less — no migration needed, `#[serde(default)]` handles old documents.
- **Sub-type API**: `entitySubType` returned in entity list/detail HTTP responses via `metadata_json`.

### Frontend

- **Documents page tabs**: split into Documents tab (table + filters + pagination + upload) and Web Ingest tab (run list + add link). Independent views, shared inspector panel.
- **Filter bar redesign**: status filter buttons now include icons (⏱ processing, ✓ ready, ⚠ sparse, ✕ failed) and count badges. Total cost moved inline into the filter bar.
- **Web ingest status fix**: `COMPLETED_PARTIAL` now treated as terminal state, no longer triggers "in progress" banner.
- **Graph sub-type filtering**: `hiddenSubTypes` state allows hiding individual sub-types from the graph. Sub-type badges are clickable in the legend.
- **Node inspector**: shows sub-type below the canonical type with translated label.
- **i18n**: added 8 new keys (showLegend, hideLegend, showAll, invert, resetFilter, subType, tabs.documents, tabs.webIngest) in both en.json and ru.json.

### Dependencies

- **React** 18.3 → 19.2, **TypeScript** 5.8 → 6.0, **Vite** 5.4 → 8.0
- **Tailwind CSS** 3.4 → 4.2 (migrated from JS config to CSS-based `@theme`, PostCSS removed, `@tailwindcss/vite` plugin)
- **Zod** 3.25 → 4.3, **ESLint** 9.32 → 10.2, **react-router-dom** 6.30 → 7.14
- **recharts** 2.15 → 3.8, **sonner** 1.7 → 2.0, **lucide-react** 0.462 → 1.7
- **Rust**: tokio 1.51.0→1.51.1, zip 8.5.0→8.5.1 (minor patches)
- 50+ other npm packages updated to latest versions

### Backend

- Batch document endpoints, audit pagination, URL-backed document pagination.
- Dashboard totals and graph counters fixed to reflect full active library.

## 0.1.1 — 2026-04-07

### Highlights

- **Universal entity taxonomy**: 10 domain-agnostic entity types (`person`, `organization`, `location`, `event`, `artifact`, `natural`, `process`, `concept`, `attribute`, `entity`) designed to work across any domain — programming, medicine, law, finance, biology, engineering, and beyond. Domain-specific granularity via `sub_type` metadata.
- **Pipeline intelligence upgrade**: graph extraction v6 with few-shot examples, relation catalog expanded from 49 to 88 canonical types, semantic chunking (2800 chars, 10% overlap, heading-aware), boilerplate detection, quality scoring, entity resolution, document summaries, and post-extraction type refinement.
- **Hybrid search**: BM25 + vector cosine similarity merged via Reciprocal Rank Fusion (RRF) with field-weighted scoring (heading boost 1.5x, quality score multiplier).
- **21 MCP tools**: added `ask` (grounded Q&A), `search_entities`, `get_graph_topology`, `list_relations`, `list_documents`, `delete_document`. Token-efficient responses with `includeReferences=false` by default.
- **Canvas2D graph renderer**: replaced SVG with Canvas2D for rendering 10K+ nodes and 50K+ edges. Zero React re-renders during pan/zoom. Viewport culling, level-of-detail labels, adaptive edge budget.
- **Bulk document actions**: batch delete, cancel processing, and reprocess via UI selection mode and REST endpoints.

### Pipeline

- Graph extraction prompt v6 with comprehensive entity type guidance, coreference resolution rules, and 2 few-shot examples.
- Relation catalog expanded from 49 to 88 canonical types: `calls`, `implements`, `extends`, `authenticates`, `contains`, `returns`, `validates`, `transforms`, `deployed_on`, `inherits_from`, `imports`, and 27 more.
- Post-extraction type refinement: regex-based pass auto-reclassifies env vars, URL paths, HTTP methods, file paths, and status codes from generic `entity` to specific types.
- Post-extraction mentions reduction: summary-based heuristic upgrades `mentions` to `uses`, `depends_on`, `contains`, `defines`, `provides`, `authenticates`, and other specific types when the summary text implies a concrete relationship.
- Semantic chunking: increased default from 1,600 to 2,800 chars, added 10% overlap between adjacent chunks, heading-aware splitting (headings always start new chunks).
- Boilerplate detection: nav links, breadcrumbs, cookie banners, copyright notices filtered from chunking.
- Chunk quality scoring: 0.0-1.0 score based on text length, word diversity, heading/code/table presence.
- SimHash near-duplicate detection for chunk deduplication.
- Document-level summary generation from structured blocks during ingestion.
- Entity resolution service: deterministic merge by exact alias, normalized prefix, and acronym detection.
- Graph extraction parallelism bumped from max 4 to max 8 concurrent chunks.
- Query expansion: 24 synonym groups for automatic search term broadening.
- Extended technical fact extraction: 8 new fact kinds (environment variables, version numbers, database names, configuration keys, error codes, rate limits, dependency declarations, code identifiers).
- Entity summary upsert changed from last-write-wins to longest-wins.
- Verification feedback loop: warnings from answer verification now flow into the response instead of being silently discarded.
- Error handling: replaced silent `let _ =` patterns in ingestion worker with proper error logging for `promote_document_head`, entity resolution, and document summary generation.

### MCP

- Added `ask` tool: grounded Q&A in a single call (replaces 3-call workflow of create_session + create_turn + get_execution).
- Added `list_documents` tool: browse library contents with optional status filter.
- Added `delete_document` tool: complete CRUD lifecycle for agents.
- Added `search_entities` tool: search knowledge graph entities by label.
- Added `get_graph_topology` tool: graph structure with truncation limits (default 200 entities / 500 relations).
- Added `list_relations` tool: explore graph relationships ordered by support count.
- `search_documents` and `read_document` responses now default to `includeReferences=false`, reducing token usage by ~80%.
- Fixed `list_relations` description (was misleading about query parameter).
- Updated MCP.md and MCP-RU.md with all 21 tools organized by category.

### Frontend

- **Graph page**: Canvas2D renderer replaces SVG. Handles 10K+ nodes at interactive frame rates. Edge labels for selected node connections. 10 distinct entity type colors with updated legend and type filter dropdown. Level-of-detail label rendering. Adaptive edge budget scaling with zoom.
- **Graph page**: all pan/zoom/hover interactions use refs instead of React state — zero re-renders during interaction.
- **Documents page**: selection mode with checkboxes, select-all, and sticky bulk action toolbar (delete, cancel processing, retry). i18n translations for en/ru.
- **Assistant page**: Markdown renderer for answer messages (code blocks, tables, lists). Removed cosmetic attachment UI.
- **Dashboard page**: renders API metrics; web ingest activity strip wired.
- GraphNodeType contract: 10 universal entity types across contracts crate, API mapping, and frontend.

### Backend

- Batch document endpoints: `POST /content/documents/batch-delete`, `batch-cancel`, `batch-reprocess` (max 100 per call).
- Ingestion worker: skip-deleted document guard, skip-cancelled job guard.
- ArangoDB relation type bug fix: `predicate` field now correctly maps to `relationType` in REST API responses.
- BM25 field-weighted scoring: heading_trail matches boosted 1.5x, section_path 1.3x, quality_score multiplier.
- quality_score persisted to ArangoDB `KnowledgeChunkRow`.
- Async reranking infrastructure (rerank_structured_query made async).
- RRF hybrid search fusion in `merge_chunks`.

### Benchmarks

- Golden benchmark corpus: 72 files across 5 semantic directories (wikipedia, docs, code, documents, fixtures).
- 10 benchmark suites with 102 test cases covering: Wikipedia recall, cross-document QA, noisy layouts, graph traversal, multiformat upload, programming docs, infrastructure docs, protocols, code comprehension, and PDF/DOCX/PPTX extraction.
- Code-only dataset: 8 large real-world code files (Go, TypeScript, Python, Rust, Kubernetes operator, React, Terraform, Docker Compose) with 20 comprehension questions.
- Multiformat dataset: 5 generated documents (2 DOCX, 1 PPTX, 2 PDF) with 12 extraction questions.
- Compare benchmarks tool for side-by-side result analysis.
- `make benchmark-golden` target runs all 5 golden suites.

### Documentation

- README.md and README-RU.md: updated pipeline diagram, features list, roadmap (0.1.1 done items), MCP tool table.
- MCP.md and MCP-RU.md: all 21 tools documented with descriptions and required parameters.
- Benchmark README: restructured corpus description with directory layout table.
- `.env.example`: added Redis URL, rerank flags, web crawl defaults, fixed model names.

### Schema

- Migration `0002_document_summaries.sql`: added `content_document_head.document_summary` and `catalog_library.ai_summary` columns.

### Gates (all green)

- `cargo fmt --all` — pass
- `cargo clippy -p ironrag-backend --all-targets -- -D warnings` — pass
- `cargo test -p ironrag-backend` — 381 tests, 0 failures
- `npx tsc --noEmit` (strict mode) — pass
- `npx vite build` — pass

## 0.1.0 — 2026-04-06

### Highlights

- **Full frontend rewrite**: replaced Leptos/Thaw Rust UI with React + shadcn/ui + Tailwind stack. Production-grade UI with i18n (en/ru), interactive knowledge graph, document management, AI assistant with evidence panel, and admin panel.
- **Backend canonicalization**: eliminated all legacy `project_id` vocabulary, collapsed dual-head revision semantics, removed 63 clippy dead-code warnings, achieved green `make backend-lint` for the first time.
- **Billing pipeline**: added cost tracking for graph extraction and query execution. Per-document and per-library cost summaries displayed in UI.
- **Entity references in queries**: answers now include matched knowledge graph entities with labels and types.
- **Source attribution**: segment references include document title and source URI, enabling users to trace answers back to specific documents and web pages.
- **Docker-first deployment**: separate frontend and backend images, nginx reverse proxy, one-command `docker compose up -d`.

### Architecture

- Replaced `apps/web` Leptos crate with React SPA (Vite + TypeScript strict mode).
- Removed `vendor/thaw-0.5.0-beta` patched UI library and all Leptos/leptos_axum dependencies.
- Frontend served as separate nginx container; API proxied through nginx reverse proxy.
- Added `apps/web/Dockerfile` for multi-stage Node build → nginx static serve.
- System admin now bypasses workspace/library discovery authorization.
- Shell bootstrap loads libraries from all visible workspaces.

### Backend

- Renamed all `project_id` to `library_id` across 17 repository functions, 5 service files, all callers.
- Replaced `DocumentRow`/`ChunkRow` shadow vocabulary: `current_revision_id` → `active_revision_id`, `active_status` → `document_state`, `active_mutation_kind/status` → `mutation_kind/status`.
- Added `ContentDocumentHead::effective_revision_id()` and `latest_revision_id()` methods replacing ad-hoc fallback patterns.
- Removed dead code: entire `document_accounting.rs`, 30+ unused functions from `ingestion_worker.rs`, `runtime_ingestion.rs`, `graph_extract.rs`.
- Added billing capture for graph extraction embeddings and query execution embeddings.
- Rewrote billing queries to aggregate costs from all execution kinds (graph_extraction + ingest_attempt).
- Added PostgreSQL-based entity search fallback for query pipeline when ArangoDB entities are empty.
- Added `documentTitle` and `sourceUri` to `PreparedSegmentReference` in query responses.

### Frontend

- 8 pages: Dashboard, Documents, Graph, AI Assistant, Admin, Swagger (live OpenAPI), Login, 404.
- API layer with typed clients: `auth`, `documents`, `dashboard`, `query`, `knowledge`, `admin`, `billing`.
- i18n with `react-i18next`: full English and Russian translations across all pages.
- Interactive knowledge graph: force-directed layout, 8 layout modes, draggable nodes, curved edges, adjacency-based selection highlighting, adaptive labels, document-entity connections.
- Document inspector: file info, web source with clickable URL, preparation summary, actions (upload, append, replace, download, delete, re-ingest).
- Web ingest: run history with expandable page lists, re-ingest with parameter editing.
- AI Assistant: session management, evidence panel with document titles and source URIs, entity/fact/relation references, verification state.
- Admin: AI provider/credential/preset management, library binding configuration, token management, MCP setup with dynamic origin URLs, audit log, pricing management.
- Session persistence via localStorage (workspace/library selection survives page refresh).
- Toast notifications for all operations with actual API error messages.
- TypeScript strict mode enabled with zero errors.

### Deployment

- `docker-compose-local.yml`: 7 services (postgres, redis, arangodb, backend, worker, frontend, nginx).
- `docker-compose.yml`: production config with pre-built images.
- `install.sh`: one-command installation from GitHub releases.
- Nginx reverse proxy: `/v1/` → backend, `/` → frontend, `/mcp` redirect, SPA fallback.

### Gates (all green)

- `cargo fmt --all` — pass
- `cargo check -p ironrag-backend --tests` — pass
- `cargo clippy -p ironrag-backend --all-targets -- -D warnings` — pass
- `cargo test --workspace` — pass
- `make check` / `make check-strict` / `make enterprise-validate` — pass
- Frontend `npx tsc --noEmit` (strict mode) — pass
- Frontend `npx vite build` — pass

## 0.0.4 - 2026-04-04

### Highlights

- Added GitHub Release automation around the published Docker Hub release channel used by the Rust-only stack.
- Split Compose surfaces into one default prebuilt deployment path, one manual local-build path, and one internal GitLab deployment path.
- Added one-click `install.sh` installation without cloning the repository, with release-tag or `latest` resolution from GitHub.
- Cut query and extraction orchestration over to one typed agent runtime with runtime-backed lifecycle, stage trace, and policy summaries across REST, MCP, and the assistant UI.

### Platform

- Switched the default `[docker-compose.yml](./docker-compose.yml)` to published Docker Hub images so release installs no longer depend on local image builds.
- Historical note: the legacy deployment surface `docker-compose-gitlab.yml` was removed (deleted); active compose files are now `[docker-compose.yml](./docker-compose.yml)` and `[docker-compose-local.yml](./docker-compose-local.yml)`.
- Updated root env documentation and release docs around the canonical `docker-compose.yml` and `docker-compose-local.yml` split.
- Tracked the canonical workspace `[Cargo.lock](./Cargo.lock)` in release artifacts so clean GitHub checkouts can build the API Docker image without missing-file failures.
- Added the canonical `apps/api/src/agent_runtime/` subsystem with typed task contracts, staged execution, explicit policy decisions, and owner-linked runtime persistence.
- Replaced full-library graph rebuilds during ingestion with targeted canonical graph reconciliation.
- Reworked knowledge generation and operations read models around canonical revision readiness.

### Product

- Assistant execution surfaces now render runtime lifecycle, stage summaries, policy interventions, and explicit policy-rejected or policy-terminated outcomes instead of generic failures.
- Documents, Dashboard, Graph, and auth surfaces were rebalanced around the canonical workbench layout.

### Reliability And Performance

- Fixed long-running document ingestion stalls by eliminating graph-persistence races in canonical merge/write paths.
- Restored truthful operator state reporting so idle libraries now surface `healthy` plus `graph_ready`.

## 0.0.3 - 2026-04-03

### Highlights

- Added the full structured preparation pipeline: semantic sections, structure-aware chunks, typed technical facts, grounded graph evidence, and answer verification.
- Added canonical URL ingestion for `single_page` and `recursive_crawl`.
- Completed the first-run bootstrap flow with canonical provider/model bindings.

## 0.0.2 - 2026-03-31

### Highlights

- Added the dedicated Assistant surface with preserved chat history, attachments, grounded context, and responsive layouts.
- Added the Admin `MCP` section with setup snippets for Codex, Cursor, Claude Code, VS Code, and generic HTTP clients.
- Added the grounded-query benchmark harness.
- Added the canonical web-ingest run model.

## 0.0.1

- Initial release.

