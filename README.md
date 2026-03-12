# RustRAG Runtime Monorepo

RustRAG is a lightweight, API-first, automation-first, multi-workspace retrieval platform.

This repository is the runtime monorepo and contains:
- `backend/` — Rust backend service
- `frontend/` — frontend shell
- `docker-compose.yml` — local development composition

The spec/governance repository lives separately in `../spec-kit/`.

## Foundation principles

- one instance, many workspaces
- many independent projects per workspace
- Postgres 18 + Redis foundation
- OpenAI + DeepSeek support in the initial platform wave
- strong API contracts for automation and UI
- usage/cost visibility from early stages
- clear path to clustering and horizontal scale

## Quick start

```bash
make backend-build
cd frontend && npm install && npm run build && cd ..
docker compose up --build
```

> Notes:
> - the backend image currently packages a host-built release binary due to a Dockerized Cargo build permission issue in the current environment
> - the frontend passes local `npm run check`, but `docker compose build frontend` currently fails in this host/runtime on containerized `npm install` (`esbuild` postinstall -> `spawn sh EACCES`); for now treat local frontend build artifacts and local verification as the supported workaround until the container runtime issue is fixed

## Repository status

The backend already includes a substantial foundation slice:
- runtime bootstrap
- auth baseline
- control plane resources
- knowledge ingest/query baseline
- usage/cost persistence
- OpenAI/DeepSeek gateway baseline

The frontend is still a shell and will catch up to the backend contracts incrementally.
