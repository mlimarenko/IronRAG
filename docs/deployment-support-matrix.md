# RustRAG Deployment Support Matrix

| Path | Status | Evidence / Notes | Next action |
|------|--------|------------------|-------------|
| Backend local build | supported | `cargo build --release` and backend validation path are working | keep validating per release |
| Backend container build | supported | `docker compose build backend` verified | re-check after packaging changes |
| Frontend local build | supported | `npm run check` passes locally | keep validating per release |
| Frontend container build | blocked | verified again on 2026-03-13: containerized `npm install` / `esbuild` path fails with `EACCES` in this host runtime | investigate container runtime / permissions workaround |
| Local compose runtime | blocked on this host | latest target stack is PostgreSQL 18 + pgvector (`pgvector/pgvector:0.8.2-pg18-trixie`) + Redis 8.4, but host AppArmor `docker-default` denies Unix socket creation inside containers | fix Docker/AppArmor policy or use an alternate local runtime strategy |
| Production-like upgrade path | conditional | not yet fully formalized; requires explicit upgrade/rollback checklist | add runbook and validation |
