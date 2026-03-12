# RustRAG Release Quality Gates

## Backend gates
- `cd backend && cargo fmt --check`
- `cd backend && cargo clippy --all-targets --all-features -- -D warnings`
- `cd backend && cargo test`

## Frontend gates
- `cd frontend && npm run api:check`
- `cd frontend && npm run lint`
- `cd frontend && npm run format:check`
- `cd frontend && npm run typecheck`
- `cd frontend && npm run build`

## Platform gates
- supported deployment/build paths revalidated and reflected in `docs/deployment-support-matrix.md`
- blocked paths explicitly documented in `docs/enterprise-blockers.md`
- runtime docs updated to match actual support status

## Story gates
- changed workflows validated end-to-end at story/checkpoint level
- operator-visible loading/empty/error/degraded states reviewed for affected frontend routes
