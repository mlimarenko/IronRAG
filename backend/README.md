# RustRAG Backend

Backend API service for RustRAG.

This backend is part of the `RustRAG/` runtime monorepo and is governed by specs/constitution in the separate `spec-kit/` repository.

## Current foundation status

Implemented baseline includes:
- config/runtime bootstrap
- Postgres 18 + Redis wiring
- health/readiness/version endpoints
- API token minting + bearer auth baseline
- workspace/project/provider/model-profile control plane
- source/ingestion-job/document control plane
- text ingest and multipart text-like upload ingest
- chunk persistence and chunk inspection endpoints
- provider-aware query path for OpenAI/DeepSeek-compatible chat execution
- embedding generation baseline
- usage/cost persistence and read APIs

## Current API surface

Runtime:
- `GET /v1/health`
- `GET /v1/ready`
- `GET /v1/version`

Auth:
- `POST /v1/auth/tokens`

Control plane:
- `GET/POST /v1/workspaces`
- `GET/POST /v1/projects`
- `GET/POST /v1/provider-accounts`
- `GET/POST /v1/model-profiles`
- `GET/POST /v1/sources`
- `GET/POST /v1/ingestion-jobs`
- `GET/POST /v1/documents`

Knowledge path:
- `POST /v1/content/ingest-text`
- `POST /v1/uploads/ingest`
- `POST /v1/content/search-chunks`
- `GET /v1/chunks`
- `POST /v1/content/embed-project`
- `GET/POST /v1/retrieval-runs`
- `POST /v1/query`

Usage / cost:
- `GET /v1/usage-events`
- `GET /v1/cost-ledger`

## Development

### Local config
Copy `.env.example` into your local runtime environment and set provider credentials as needed.

### Check build
```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

### Compose from monorepo root
```bash
cd ..
make backend-build
cd frontend && npm install && npm run build && cd ..
docker compose up --build backend postgres redis
```

> Notes:
> - the backend container image is built as a multi-stage Docker image and no longer depends on a prebuilt host binary, which makes CI/registry publishing sane again
> - `docker compose build backend` should remain the primary validation path for backend image packaging; on this host the current failure mode is Docker builder DNS resolution to `index.crates.io`, not the image layout itself
> - `docker compose build frontend` is currently blocked in this host/runtime by containerized `npm install` failing during `esbuild` postinstall with `spawn sh EACCES`; use local frontend checks/build as the current workaround

## Notes

This backend is still in foundation stage:
- semantic retrieval is baseline-level and not yet pgvector-native
- provider secret handling is still foundation-level and should be hardened further
- OpenAPI contract needs continuous refresh as endpoints evolve
