# Changelog

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
- `cargo clippy -p rustrag-backend --all-targets -- -D warnings` — pass
- `cargo test -p rustrag-backend` — 381 tests, 0 failures
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
- `cargo check -p rustrag-backend --tests` — pass
- `cargo clippy -p rustrag-backend --all-targets -- -D warnings` — pass
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
- Switched the default [`docker-compose.yml`](./docker-compose.yml) to published Docker Hub images so release installs no longer depend on local image builds.
- Historical note: the legacy deployment surface `docker-compose-gitlab.yml` was removed (deleted); active compose files are now [`docker-compose.yml`](./docker-compose.yml) and [`docker-compose-local.yml`](./docker-compose-local.yml).
- Updated root env documentation and release docs around the canonical `docker-compose.yml` and `docker-compose-local.yml` split.
- Tracked the canonical workspace [`Cargo.lock`](./Cargo.lock) in release artifacts so clean GitHub checkouts can build the API Docker image without missing-file failures.
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
