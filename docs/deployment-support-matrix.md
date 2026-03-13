# RustRAG Deployment Support Matrix

| Path | Status | Evidence / Notes | Next action |
|------|--------|------------------|-------------|
| Backend local build | supported | `cargo build --release` and backend validation path are working | keep validating per release |
| Backend container build | supported | `docker compose build backend` verified | re-check after packaging changes |
| Frontend local build | supported | `npm run check` passes locally | keep validating per release |
| Frontend container build | blocked | verified again on 2026-03-12: `npm install` in container fails during `esbuild` postinstall with `spawn sh EACCES` | investigate container runtime / permissions workaround |
| Local compose runtime | conditional | backend/deps path is usable; frontend container path remains blocked | split supported compose guidance by service |
| Production-like upgrade path | conditional | not yet fully formalized; requires explicit upgrade/rollback checklist | add runbook and validation |
