RustRAG Runtime Monorepo

RustRAG is a local-first graph-RAG runtime for document ingestion, graph projection, and grounded querying.

Repository layout

- backend/ — Rust API + worker runtime
- frontend/ — Vue 3 + Quasar operator shell
- docker-compose.yml — supported local stack
- scripts/smoke/ — live-provider validation flows
- All documentation and checkpoints: ../rustrag.spec-kit/.specify/memory/ (see README.md there)

Supported local stack

PostgreSQL, Redis, Neo4j, backend, frontend, nginx ingress.

  docker compose up --build -d nginx

Default login: admin / rustrag
Ingress: http://127.0.0.1:19000
API: http://127.0.0.1:19000/v1
Neo4j Browser: http://127.0.0.1:19000/browser/

Validation

  cd backend && cargo check && cargo test -q
  cd frontend && npm install && npm run api:generate && npm run lint && npm run typecheck && npm run build

Live-provider smoke: see ../rustrag.spec-kit/.specify/memory/operations/provider-smoke.md and model-pricing-catalog.md; scripts/smoke/runtime-openai.sh, runtime-deepseek.sh.
