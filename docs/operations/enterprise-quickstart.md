# Enterprise Scope 2 Quickstart

## Source of truth
- spec: `../spec-kit/specs/002-rustrag-enterprise/spec.md`
- plan: `../spec-kit/specs/002-rustrag-enterprise/plan.md`
- tasks: `../spec-kit/specs/002-rustrag-enterprise/tasks.md`

## Validation
- backend: `cd backend && make quality`
- frontend: `cd frontend && npm run enterprise:check`
- monorepo: `make enterprise-validate`

## Deployment baseline
- local/dev path: repository-root `docker-compose.yml`
- host/runtime path: `/opt/docker/rustrag`
- deploy automation: `ansible/deploy.yml`
- expected host-managed files before deploy: `/opt/docker/rustrag/docker-compose.yml` and `/opt/docker/rustrag/.env`
- image pin file managed by deploy automation: `/opt/docker/rustrag/.images.env`
- default compose project used by deploy automation: `rustrag`
- the deploy playbook restarts one compose service at a time; current GitLab CI/CD jobs target `backend` and `frontend`
- current GitLab CI/CD image env vars are `RUSTRAG_BACKEND_IMAGE` and `RUSTRAG_FRONTEND_IMAGE`
- GitLab CI/CD deploy jobs export `DEPLOY_IMAGE` and rely on `CI_REGISTRY`, `CI_REGISTRY_USER`, and `CI_REGISTRY_PASSWORD`

## Current implementation priority
1. setup and foundational operational-state scaffolding
2. workspace governance maturity
3. ingestion reliability and retry semantics
4. grounded query transparency
5. diagnostics and operator-console maturity
6. deployment support-matrix hardening
