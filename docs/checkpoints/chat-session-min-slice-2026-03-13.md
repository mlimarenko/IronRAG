# Chat session minimal shippable slice — 2026-03-13

## Decision
Did **not** land code changes for chat/session persistence in this pass.

Reason: the safe implementation boundary is backend-wide rather than a one-file edit. A minimally honest slice would touch:
- DB migration(s)
- repository row structs + create/list/get functions
- `/v1/query` request/response contract
- retrieval persistence path
- OpenAPI contract and generated frontend client
- frontend chat store/page if we want the new IDs to be exercised end-to-end

At the same time, the working tree already has unrelated in-flight edits in:
- `backend/contracts/rustrag.openapi.yaml`
- `frontend/src/contracts/api/generated.ts`
- `frontend/src/pages/GraphPage.vue`
- several frontend/i18n/router files
- `docs/TASKS-SCOPE-1.md`

That makes an opportunistic API-contract change higher conflict than is reasonable for a scout/safe-slice pass.

## Repo state assessed
### What is already true
- Retrieval runs are real and persisted through `backend/src/interfaces/http/retrieval.rs` -> `backend/src/infra/repositories.rs` -> `retrieval_run`.
- `/v1/query` is still **single-turn**. `QueryRequest` has only `project_id`, `query_text`, model ids, and `top_k`; `QueryResponse` returns only retrieval-run-centric fields.
- `ChatSession` exists only as an unused domain stub in `backend/src/domains/retrieval.rs`.
- `backend/migrations/0002_ingestion_and_retrieval.sql` has `entity`, `relation`, and `retrieval_run`, but no `chat_session`, `chat_message`, or structured evidence table.
- Frontend chat (`frontend/src/stores/chat.ts`, `frontend/src/pages/ChatPage.vue`) is wired around one current response + one retrieval detail payload, not a resumable timeline.

### Why a code change was not the right move here
A backend-only schema addition without wiring query flow would leave dead tables and no validated path.
A query-contract change without regenerating/settling the already-modified OpenAPI/generated-client files risks stepping on in-flight work.
A full vertical slice is larger than a safe low-conflict patch for this pass.

## Next minimal shippable slice (recommended)
Target a **backend-first additive slice** that introduces durable session/message storage while keeping current chat UI functional.

### Scope
1. **Migration:** add `chat_session` and `chat_message` tables.
   - `chat_session`: `id`, `workspace_id`, `project_id`, `title`, `created_at`, `updated_at`
   - `chat_message`: `id`, `session_id`, `project_id`, `role`, `content`, `retrieval_run_id nullable`, `created_at`
2. **Retrieval linkage:** add nullable `session_id` to `retrieval_run`.
   - Defer `assistant_message_id` / `user_message_id` until follow-up if needed.
3. **Repository layer:** add create/get/list helpers for sessions/messages and update `create_retrieval_run` to accept optional `session_id`.
4. **HTTP additive contract:** extend `/v1/query` with optional `session_id` and optional `create_session`/`session_title` semantics.
   - If `session_id` absent, create a new session automatically.
   - Persist one `user` message before model execution and one `assistant` message after retrieval run persistence.
   - Return `session_id`, `user_message_id`, and `assistant_message_id` in `QueryResponse`.
5. **Read endpoints:**
   - `GET /v1/chat/sessions?project_id=...`
   - `GET /v1/chat/sessions/{id}/messages`
6. **Frontend:** do **not** attempt the full session UX yet.
   - Only make generated types compile and optionally stash returned `session_id` in store for same-page follow-up.
   - Defer session list/timeline UI to the next slice.

## Why this is the right next slice
- Gives provenance a stable container (`session` + `message`) without forcing the structured citation model in the same change.
- Keeps `/v1/query` additive instead of breaking existing callers.
- Leaves current retrieval diagnostics intact.
- Enables a small validation story: two queries in one session, then list messages.

## Explicitly deferred from this slice
- Structured citation/provenance tables (`retrieval_evidence`, `message_citation`, etc.)
- Graph extraction/persistence refactor
- Full frontend session list/resume UX
- Message-level token/cost attribution refactoring
- Rich session title generation

## Conflict risks to watch
1. **OpenAPI/generated client churn**
   - `backend/contracts/rustrag.openapi.yaml` and `frontend/src/contracts/api/generated.ts` are already modified in the tree.
   - Coordinate before regenerating or hand-editing generated types.
2. **Chat page assumptions**
   - Current page/store assumes one response/detail pair. Returning extra IDs is safe; replacing response shape semantics is not.
3. **Migration ordering**
   - Current retrieval schema lives in `0002_ingestion_and_retrieval.sql`; adding session tables likely needs a new migration rather than editing old ones if environments already exist.
4. **Workspace authorization**
   - Session read/list endpoints must enforce project/workspace access the same way retrieval endpoints do.

## Validation plan for the recommended slice
1. Run migrations cleanly on a fresh DB.
2. Submit `POST /v1/query` without `session_id` and verify:
   - a session is created
   - one user message exists
   - one assistant message exists
   - retrieval run links to the session
3. Submit a second `POST /v1/query` with returned `session_id` and verify message ordering.
4. `GET /v1/chat/sessions?project_id=...` returns the session.
5. `GET /v1/chat/sessions/{id}/messages` returns four messages for the two-turn flow.
6. Existing retrieval detail endpoint still works unchanged.

## Test/validation status from this scout pass
- Attempted targeted Rust test runs for `retrieval` / `chat`, but they were still running at handoff time and not used as a basis for code changes.
- No production code was modified in this pass; only this checkpoint note was added.
