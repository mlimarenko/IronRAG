# Changelog

## 0.0.3 - Unreleased

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
- Fixed web-ingest URL handling, bootstrap recovery, clean README bootstrap behavior, bootstrap provider defaults, verification warning noise, and multiple Documents/auth layout regressions.
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
