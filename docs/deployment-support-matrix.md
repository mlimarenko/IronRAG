# RustRAG Deployment Support Matrix

| Path | Status | Evidence / Notes | Next action |
|------|--------|------------------|-------------|
| Backend local build | supported | `cargo build --release` and backend validation path are working | keep validating per release |
| Backend container build | supported | backend image is built/published via GitLab CI and intended as the deploy artifact | re-check after packaging changes |
| Frontend local build | supported | `npm run check` passes locally | keep validating per release |
| Frontend container build | blocked | verified again on 2026-03-13: containerized `npm install` / `esbuild` path fails with `EACCES` in this host runtime | investigate container runtime / permissions workaround |
| Local compose runtime | blocked on this host | latest target stack is PostgreSQL 18 + pgvector (`pgvector/pgvector:0.8.2-pg18-trixie`) + Redis 8.4, but host AppArmor `docker-default` denies Unix socket creation inside containers | fix Docker/AppArmor policy or use an alternate local runtime strategy |
| Host deploy path (`/opt/docker/rustrag`) | supported baseline | `ansible/deploy.yml` deploys against a host-managed compose stack at `/opt/docker/rustrag`, expects `docker-compose.yml` + `.env` in place, writes `.images.env`, then pulls/restarts the selected compose service from `DEPLOY_IMAGE`; current GitLab jobs target `backend`/`frontend` via `RUSTRAG_BACKEND_IMAGE` / `RUSTRAG_FRONTEND_IMAGE` | keep host compose files and CI variables documented together |
| Production-like upgrade path | conditional | host deploy flow exists, but upgrade/rollback checklist is still not fully formalized beyond image pin + compose restart | add explicit rollback and post-deploy validation runbook |
