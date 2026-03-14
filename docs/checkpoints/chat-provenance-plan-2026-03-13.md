# Chat / Provenance / Graph checkpoint — 2026-03-13

## Current state
- Retrieval/query is real and persists `retrieval_run` rows with answer text plus debug payload; live `/v1/query` also persists usage/cost attribution (`backend/src/interfaces/http/retrieval.rs`, `backend/src/infra/repositories.rs`).
- The stored retrieval shape is still retrieval-run-centric: `retrieval_run` has `query_text`, `response_text`, `top_k`, `model_profile_id`, and `debug_json`, but no first-class chat session/message linkage (`backend/migrations/0002_ingestion_and_retrieval.sql`).
- A `ChatSession` domain struct exists, but it is only a stub and is not wired to migrations, repositories, HTTP routes, or frontend state (`backend/src/domains/retrieval.rs`).
- Citation/provenance is partial: API DTOs expose `CitationReferenceDto` and `RetrievalDebugMetadataDto`, but references are basically labels + optional chunk/document/source ids and score, with most evidence derived from debug/chunk ownership rather than a durable provenance model (`backend/src/interfaces/http/product/shared.rs`, `backend/src/interfaces/http/product/projects.rs`, `backend/src/interfaces/http/product/documents.rs`).
- Graph persistence hooks exist at schema level (`entity`, `relation`, `relation.provenance_json`), but the graph product currently builds an in-memory view from chunk metadata and retrieval debug hints instead of loading canonical persisted graph records or extraction runs (`backend/src/interfaces/http/graph_product.rs`).
- Frontend chat is single-run state only: Pinia keeps one draft and one current response/detail; no session list, message timeline, reload, or resume flow exists (`frontend/src/stores/chat.ts`, `frontend/src/pages/ChatPage.vue`).
- Frontend graph page is still a static/product-honest preview and is not wired to the live graph endpoints that already exist in the backend contract/handlers (`frontend/src/pages/GraphPage.vue`, `backend/contracts/rustrag.openapi.yaml`).

## Main gaps
1. **Chat/query session persistence is the highest-priority missing layer.** Right now every query is an isolated retrieval run, so there is no durable conversation container, no message history, and no way to attach later provenance improvements to a stable session/message model.
2. **Citation/provenance is not first-class.** Important evidence lives in `debug_json` and ad hoc reference labels, which makes auditing, filtering, UI rendering, and later graph linkage brittle.
3. **Graph/entity-relation support is still hook-level.** The schema suggests eventual persistence, but the current pipeline does not create canonical entities/relations from ingestion/query extraction and does not link them back to messages/citations in a provenance-rich way.
4. **Frontend still reflects the old limitation.** Chat is run-oriented rather than session-oriented, and Graph UI is mostly explanatory even though backend graph APIs now exist.

## Next implementation steps (ordered)

### 1) Land minimal durable chat session + message persistence
- Add DB tables for `chat_session` and `chat_message` (or equivalent) scoped by workspace/project.
- Persist a session on first query, then persist at least user message + assistant message per run.
- Add `retrieval_run.session_id` and preferably `retrieval_run.assistant_message_id` / `user_message_id` linkage so retrieval diagnostics stay attached to the produced turn.
- Extend `/v1/query` to accept either `session_id` or `create_session` semantics and return session/message ids in the response.
- Add list/get endpoints for sessions and messages so frontend can resume prior conversations.

Why first: it is the control point everything else wants to hang off. Without stable session/message ids, provenance and graph links stay bolted onto one-off runs.

### 2) Promote citations/provenance from debug payload to structured records
- Introduce a first-class citation/provenance table (for example `citation`, `message_citation`, or `retrieval_evidence`) keyed to `retrieval_run` and/or assistant message.
- Store one row per supporting chunk/reference with normalized fields: chunk/document/source ids, rank/score, provider/model context, evidence kind, quote/snippet, offsets if available, and provenance payload for extraction details.
- Keep `debug_json` for raw diagnostics, but make API/UI read from structured evidence first and use debug as supplemental detail only.
- Update query response/detail DTOs and project/document product surfaces to use the structured model.

Why second: it removes the current “important truth hidden in debug JSON” problem and gives the session model something durable to render.

### 3) Turn graph hooks into a real persisted extraction pipeline
- Define extraction-run semantics separate from retrieval runs (ingestion-time, query-time, or both).
- Populate canonical `entity` and `relation` rows from chunk metadata/extraction output instead of rebuilding the graph only in memory.
- Persist provenance on entities/relations back to source chunks/documents and, where applicable, to the query/session/message that surfaced them.
- Refactor graph product handlers to read persisted entity/relation state first, using heuristic/chunk-derived fallback only as an explicitly temporary path.

Why third: the graph surface already has schema and endpoints, but it is still not a trustworthy system-of-record pipeline.

### 4) Move frontend chat/graph onto the real persisted model
- Chat: add session list, create/resume behavior, message timeline, and retrieval/evidence panels per assistant turn.
- Graph: replace the static preview with live calls to graph summary/search/entity/subgraph endpoints, but keep explicit empty/partial states when persisted graph coverage is absent.
- Ensure citations shown in chat can deep-link to source chunks/documents and, later, related graph entities/relations.

## Affected files / modules
- Backend schema/migrations:
  - `backend/migrations/0002_ingestion_and_retrieval.sql` (currently has `entity`, `relation`, `retrieval_run`, but no chat-session tables)
- Backend domain/repository/query path:
  - `backend/src/domains/retrieval.rs`
  - `backend/src/infra/repositories.rs`
  - `backend/src/interfaces/http/retrieval.rs`
  - `backend/src/interfaces/http/product/shared.rs`
  - `backend/src/interfaces/http/product/projects.rs`
  - `backend/src/interfaces/http/product/documents.rs`
  - `backend/src/interfaces/http/graph_product.rs`
  - `backend/contracts/rustrag.openapi.yaml`
- Frontend chat/graph surfaces:
  - `frontend/src/boot/api.ts`
  - `frontend/src/stores/chat.ts`
  - `frontend/src/pages/ChatPage.vue`
  - `frontend/src/pages/GraphPage.vue`
  - optionally `frontend/src/stores/graph.ts`
- Scope tracking/docs:
  - `docs/TASKS-SCOPE-1.md`

## Validation steps
- Backend migration tests / local migration run succeeds with new session + provenance tables.
- API flow test:
  1. create or resume a chat session
  2. submit two queries into the same session
  3. verify session/message history returns both turns in order
  4. verify each assistant turn links to one retrieval run and structured citation rows
- Retrieval detail test verifies legacy `debug_json` still exists, but structured citations are present and preferred.
- Graph validation test verifies persisted entities/relations can be created from chunk/extraction input and then returned by graph summary/search/detail endpoints without relying solely on in-memory chunk scanning.
- Frontend validation:
  - reload `/chat` and resume a prior session
  - inspect evidence per turn
  - open `/graph` and confirm it uses live API data with honest partial/empty states

## Risks / caveats
- There is a modeling choice to make early: whether citations belong primarily to `retrieval_run`, assistant `chat_message`, or both. Picking one poorly will create churn later.
- Session persistence will force API contract changes on `/v1/query`; keep the first version additive if possible.
- If graph extraction is done at query time only, canonical graph quality will be noisy and biased toward what users asked. Ingestion-time extraction is likely the better long-term source of truth.
- Refactoring the graph endpoint from heuristic chunk scanning to persisted records needs care so existing demo/preview behavior does not regress to empty responses overnight.
- Provenance can get large quickly; normalize core fields and keep bulky/raw extraction payloads in bounded JSON blobs rather than duplicating everything everywhere.
