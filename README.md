# RustRAG Runtime Monorepo

RustRAG is a lightweight, API-first, automation-first, multi-workspace retrieval platform.

This repository is the runtime monorepo and contains:
- `backend/` — Rust backend service
- `frontend/` — frontend shell
- `docker-compose.yml` — local development composition
- `ansible/deploy.yml` — production deployment playbook for the host-side compose stack in `/opt/docker/rustrag`

The spec/governance repository lives separately in `../spec-kit/`.
The active enterprise maturity implementation program is tracked in `../spec-kit/specs/002-rustrag-enterprise/`.

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
> - the backend image is now built as a multi-stage Docker image, so CI can publish a self-contained backend artifact to a registry without relying on a host-built binary
> - the frontend passes local `npm run check`, but `docker compose build frontend` currently fails in this host/runtime on containerized `npm install` (`esbuild` postinstall -> `spawn sh EACCES`); for now treat local frontend build artifacts and local verification as the supported workaround until the container runtime issue is fixed
> - the intended latest local stack is PostgreSQL 18 + pgvector and Redis 8.4; current containerized startup failures on this host are caused by AppArmor `docker-default` denying Unix socket creation inside containers, not by an application-level incompatibility with the latest stack
> - backend image build validation on this host now fails later, inside Docker build networking, because the builder cannot resolve `index.crates.io`; that is a host/runtime networking issue rather than a packaging-layout issue

## Deployment shape

- local development uses the repository-root `docker-compose.yml`
- host deployment is intentionally split from the repo checkout: the target host keeps a compose project in `/opt/docker/rustrag`
- `ansible/deploy.yml` expects `/opt/docker/rustrag/docker-compose.yml` and `/opt/docker/rustrag/.env` to already exist on the target host
- GitLab CI/CD deploy jobs export `DEPLOY_IMAGE` and pass through `CI_REGISTRY`, `CI_REGISTRY_USER`, and `CI_REGISTRY_PASSWORD`; the playbook writes the selected image tag to `/opt/docker/rustrag/.images.env` under the service-specific image env var (`RUSTRAG_BACKEND_IMAGE` or `RUSTRAG_FRONTEND_IMAGE` in the current pipeline)
- deployment then runs `docker compose pull <service>` and `docker compose up -d --no-deps --no-build <service>` inside compose project `rustrag`; current GitLab jobs target services `backend` and `frontend`

## Repository status

The backend already includes a substantial foundation slice:
- runtime bootstrap
- auth baseline
- control plane resources
- knowledge ingest/query baseline
- usage/cost persistence
- OpenAI/DeepSeek gateway baseline

The frontend is still a shell and will catch up to the backend contracts incrementally.
