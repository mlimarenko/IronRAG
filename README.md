# RustRAG Runtime Monorepo

RustRAG is a local-first graph-RAG runtime for document ingestion, graph projection, revision-aware document lifecycle, and grounded querying.

## Layout

- `backend/` — Rust API, worker runtime, retrieval pipeline
- `frontend/` — Vue 3 + Quasar operator UI
- `docker-compose.yml` — supported local stack
- `scripts/smoke/` — live-provider validation flows
- `../rustrag.spec-kit/.specify/memory/` — architecture notes, checkpoints, and operational guidance

## Retrieval Intelligence

- Query modes are product-level retrieval strategies, not different LLMs: `document`, `local`, `global`, `hybrid`, `mix`
- Query planning reuses cached intent keywords when the question and source-truth version still match
- `hybrid` and `mix` expand candidate pools, apply rerank, then assemble bounded mixed context
- Persisted query detail now preserves planning, rerank, context-assembly, grouped references, and warning state
- Canonical graph summaries and selective reconciliation keep graph-facing answers aligned with append, replace, and delete mutations

## Local Stack

```bash
docker compose up --build -d nginx
```

- Ingress: `http://127.0.0.1:19000`
- API: `http://127.0.0.1:19000/v1`
- Default UI login: `admin / rustrag`

## Validation

```bash
make backend-change-gate
cd frontend && npm run api:generate && npm run lint && npm run typecheck && npm run build
```
