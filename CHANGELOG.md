# Changelog

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
- **IRONRAG rename**: release-facing configuration now uses `IRONRAG_*` naming instead of `RUSTRAG_*`.

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
- **Dead `legacy_`* bootstrap flags removed**: `legacy_ui_bootstrap_enabled`, `legacy_bootstrap_token_endpoint_enabled`, `allow_legacy_startup_side_effects` deleted from config and 5 integration tests. `legacy_ui_bootstrap_admin` renamed to `ui_bootstrap_admin`.
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
