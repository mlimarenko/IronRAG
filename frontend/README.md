# Frontend Ask AI Notes

The Graph page now uses a chat-first `Ask AI` rail instead of the older diagnostics-heavy sidebar.

## Retrieval UX

- Query mode chips describe retrieval strategy, not model choice: `Documents`, `Local`, `Global`, `Hybrid`, `Mixed`
- Assistant history preserves persisted planning, rerank, grouped references, and warning metadata when older chats are reopened
- Node detail and document detail both surface graph-quality truth: canonical summaries, extraction recovery, and reconciliation scope
- Grouped references keep one visible title/excerpt per support cluster while preserving support ids for drill-down

## Main flow

- Session lifecycle and settings edits use `/v1/chat/sessions`, `/v1/chat/sessions/{id}`, and `/v1/chat/sessions/{id}/messages`.
- When a saved assistant message has a persisted `executionId`, the frontend can hydrate the full persisted query detail from `/v1/query/executions/{executionId}` to restore grouped references and planning truth.

## Chat behavior

- Every new chat is seeded with a default grounded system prompt.
- The system prompt and preferred retrieval mode are stored per chat session.
- The composer keeps one recommended mode visible and moves secondary mode switching behind a lighter control.
- Live asks insert an optimistic user message and a pending assistant placeholder before the backend response arrives.
- Reopened chats prefer persisted query detail over stale flattened history payloads when richer metadata is available.

## Contracts

Frontend API types are generated from [../backend/contracts/rustrag.openapi.yaml](/home/leader/sources/RustRAG/rustrag/backend/contracts/rustrag.openapi.yaml).

Regenerate them after any assistant or chat payload change:

```bash
cd frontend
npm run api:generate
```

## Validation

Typical frontend checks for this surface:

```bash
cd frontend
npm run lint
npm run typecheck
npm run build
```
