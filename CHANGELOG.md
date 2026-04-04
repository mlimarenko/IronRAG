# Changelog

## 0.0.4 - 2026-04-04

### Highlights
- Added GitHub Release automation that builds and publishes the canonical Docker Hub images `pipingspace/rustrag-backend` and `pipingspace/rustrag-frontend`.
- Split Compose surfaces into one default prebuilt deployment path, one manual local-build path, and one internal GitLab deployment path.
- Added one-click `install.sh` installation without cloning the repository, with release-tag or `latest` resolution from GitHub.
- Cut query and extraction orchestration over to one typed agent runtime with runtime-backed lifecycle, stage trace, and policy summaries across REST, MCP, and the assistant UI.

### Platform
- Switched the default [`docker-compose.yml`](./docker-compose.yml) to published Docker Hub images so release installs no longer depend on local image builds.
- Renamed the internal deployment surface from `docker-compose.ci.yml` to [`docker-compose-gitlab.yml`](./docker-compose-gitlab.yml) and updated Ansible plus GitLab CI to keep internal deploys aligned.
- Updated root env documentation and release docs around the canonical `docker-compose.yml`, `docker-compose-local.yml`, and `docker-compose-gitlab.yml` split.
- Tracked the canonical [`backend/Cargo.lock`](./backend/Cargo.lock) in release artifacts so clean GitHub checkouts can build the backend Docker image without missing-file failures.
- Added the canonical `backend/src/agent_runtime/` subsystem with typed task contracts, staged execution, explicit policy decisions, and owner-linked runtime persistence.
- Removed query-local lifecycle authority, raw structured task `ChatRequest` callers, hidden extraction parser repair, and transport-specific runtime aliases from the canonical success path.
- Replaced full-library graph rebuilds during ingestion with targeted canonical graph reconciliation so uploads no longer stall behind unnecessary global rebuild work.
- Reworked knowledge generation and operations read models around canonical revision readiness so library health, generation state, and runtime progress no longer depend on empty shadow collections or stale rebuild flags.

### Product
- Assistant execution surfaces now render runtime lifecycle, stage summaries, policy interventions, and explicit policy-rejected or policy-terminated outcomes instead of generic failures.
- Query and extraction inspection now share the same runtime vocabulary for `accepted`, `running`, `completed`, `recovered`, `failed`, and `canceled`.
- Documents, Dashboard, Graph, and auth surfaces were rebalanced around the canonical workbench layout, with calmer empty states, tighter login/bootstrap shells, and more truthful operator feedback.

### Recovery And Policy
- Strict structured-output success is now first-pass only; malformed machine-consumed outputs fail explicitly or surface as bounded recovered outcomes with trace evidence.
- Runtime policy rejections and terminations now leave bounded redacted reason summaries, runtime-execution audit subjects, and explicit operator-visible terminal states.

### Reliability And Performance
- Fixed long-running document ingestion stalls by eliminating graph-persistence races in canonical merge/write paths and by stopping stale worker attempts from owning current document state.
- Restored truthful operator state reporting so idle libraries now surface `healthy` plus `graph_ready` instead of lingering in false `rebuilding` or mixed-status states.
- Revalidated the grounded benchmark matrix at release time with all strict suites passing and the canonical local stack staying green under fresh upload and answer runs.

### Docs
- Added quick-install `curl` flows for latest and version-pinned installs in the English and Russian READMEs.
- Documented the new release-image channel and the manual local source-build path.

## 0.0.3 - 2026-04-03

### Highlights
- Added the full structured preparation pipeline: semantic sections, structure-aware chunks, typed technical facts, grounded graph evidence, and answer verification now feed one readiness model.
- Added canonical URL ingestion for `single_page` and `recursive_crawl`, with one shared pipeline across backend, REST, MCP, and the web UI.
- Completed the first-run bootstrap flow with canonical provider/model bindings for graph extraction, embeddings, answer generation, and vision.

### Product
- Expanded Documents with preparation status, prepared segments, typed technical facts, web-ingest diagnostics, and shared graph coverage surfaces.
- Simplified Documents, Dashboard, Graph, Assistant, Admin, and auth/bootstrap layouts so empty, sparse, and loading states are calmer and more truthful.
- Added assistant verification surfaces, evidence panels, and clipboard image paste through the same upload path as files.

### Platform
- Normalized graph extraction around one English `snake_case` relation vocabulary and rebuilt graph reconciliation around Unicode-safe canonical identity keys.
- Expanded the AI catalog and price matrix to the canonical provider set and aligned runtime/bootstrap configuration with the actual deployment flow.
- Folded cached-input pricing, preparation checkpoints, and web-ingest schema into one canonical `0001_init.sql` baseline for clean installs.
- Added stricter grounded-query benchmark suites plus a neutral in-repo Wikipedia corpus for release validation.
- Added canonical workspace/library deletion and cleaned benchmark/demo/test fixtures of organization-specific sample data.

### Fixes
- Fixed readiness and graph coverage drift so readable-but-sparse documents no longer appear fully graph-ready.
- Fixed exact technical answers and follow-up handling so the assistant stays grounded in prepared evidence and active chat context.
- Fixed mixed-script entity merge failures, DeepSeek `json_schema` fallback handling, and graph rebuild gaps after clean graph regeneration.
- Fixed web-ingest URL handling, bootstrap recovery, clean README bootstrap behavior, bootstrap provider defaults, canonical backend Docker builds in local Compose and GitLab CI, verification warning noise, and multiple Documents/auth layout regressions.
- Fixed grounded-query runtime drift on the neutral release corpus by improving deterministic multi-document role matching and exact-literal benchmark normalization.

## 0.0.2 - 2026-03-31

### Highlights
- Added the dedicated Assistant surface with preserved chat history, attachments, grounded context, and responsive layouts.
- Added the Admin `MCP` section with setup snippets for Codex, Cursor, Claude Code, VS Code, and generic HTTP clients.
- Added the grounded-query benchmark harness with canonical local execution paths and scheduled/manual CI support.
- Added the canonical web-ingest run model for `single_page` and `recursive_crawl` URL ingestion.

### Product
- Consolidated the shell and primary pages into one responsive surface model across `home`, `documents`, `graph`, `admin`, `assistant`, `swagger`, and `404`.
- Reworked Documents into a sortable table-first workbench with sticky filters, compact headers, and inspector-first destructive actions.
- Reworked Graph around one canvas path with restored curved edges, better targeting, improved layout transitions, and responsive side panels.
- Reworked Assistant into a chat-first flow with stable session routing, sticky composer, cleaner evidence presentation, and a compact session rail.
- Reworked Admin into a consistent control-plane workbench for Access, Operations, AI setup, Pricing, and MCP setup.

### Platform
- Switched Assistant product UX to one canonical deep retrieval mode and increased retrieval depth/context budget for cross-document synthesis.
- Reworked grounded query execution so answer generation, debug evidence, graph references, and benchmark validation consume one context-bundle path.
- Tightened exact-literal handling for API-style documents so URLs, methods, parameters, endpoints, and other technical literals survive retrieval and answering.
- Reworked readiness semantics so `processing`, `search-ready`, `graph-sparse`, and `graph-ready` stay consistent across dashboard, documents, and graph.
- Routed web-page ingestion through the same canonical content, readiness, and graph pipeline as uploaded files.

### Fixes
- Fixed document list status modeling, multipart upload handling, direct `fileName` responses, tolerant PNG decoding, and end-to-end upload flows for supported formats.
- Fixed graph cursor behavior, node selection and dragging, sparse/error state rendering, and dense-cluster hit accuracy.
- Fixed assistant session rollover, composer viewport regressions, shallow “please upload documents” failure modes, and runtime context gaps for latest-document/library-summary questions.
- Fixed graph query/runtime regressions across relation traversal, provenance lookup, lexical recall, and exact-literal answer paths.
- Fixed link-ingest defaults so recursive crawl is opt-in and partial completion or cancellation is visible across REST, UI, and MCP.

## 0.0.1

- Initial release.
