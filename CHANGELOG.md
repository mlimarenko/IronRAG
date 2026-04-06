# Changelog

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
