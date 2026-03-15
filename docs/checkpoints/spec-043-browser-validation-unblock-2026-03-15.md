# Spec 043 browser-validation unblock — 2026-03-15

## Scope
Unblock honest local browser validation for spec 043 tasks T170-T181 without reintroducing setup/admin flow into the primary UX.

## Practical blocker found
- The backend already implements `POST /v1/auth/bootstrap-token`.
- The frontend already has an auth/session panel that can mint a token for protected flows.
- Local runtime wiring did **not** reliably provide a bootstrap token in compose/local docs, so browser validation of protected flows was blocked or dependent on out-of-band operator setup.
- There is still **no dedicated browser automation harness** in package scripts, so validation remains a documented manual browser matrix unless/until a real e2e path is added.

## Minimal unblock applied
1. `docker-compose.yml`
   - backend now receives `bootstrap_token: ${RUSTRAG_BOOTSTRAP_TOKEN:-bootstrap-local}`
   - this keeps local compose honest and deterministic without changing production contracts
2. `backend/docs/operations/local-development.md`
   - documents `RUSTRAG_BOOTSTRAP_TOKEN=bootstrap-local` for local runtime
   - adds a direct curl verification step for `/v1/auth/bootstrap-token`
3. `backend/README.md`
   - documents that bootstrap token config is required for honest local frontend/browser validation of protected flows

## What this unblocks
- Local/developer browser validation can now mint a real temporary session token against the local backend instead of relying on fake auth or undocumented operator setup.
- This specifically unblocks practical execution of the spec 043 browser matrix around:
  - T170 first-run from cold open
  - T171 default landing into Documents
  - T175 Ask available with ready content
  - T176 Ask remains available after later failed ingest when older content exists
  - T177 recent Ask session resume
  - T179 no primary path depends on setup/admin pages
  - T180 workspace/library switch updates Documents and Ask
  - T181 advanced-only paths stay secondary

## What remains blocked / not solved here
- No repo-native Playwright/Cypress/manual-browser-runner script exists yet; browser validation is still a documented manual/devtools exercise.
- Upload success/failure/processing validation (T172-T174) still depends on a working local ingest path and realistic backend data setup.
- If provider-backed query validation is required beyond shell/navigation/auth/readiness checks, operator credentials for the configured model provider are still needed.
- On this host, compose runtime verification is additionally blocked by an existing Postgres migration-history mismatch: backend exits with `migration 5 was previously applied but is missing in the resolved migrations`. This is an environment/database state problem, not a bootstrap-token wiring problem.

## Verification performed
- `npm --prefix frontend run check` ✅
- `cargo test -q` ✅
- `docker compose up -d postgres redis backend` ✅ started services, but backend crashed during startup
- `docker compose logs --tail=80 backend` ⚠️ showed startup failure from migration-history mismatch before HTTP bind
- direct `curl` verification of `/v1/health` and `/v1/auth/bootstrap-token` could not complete because backend never reached serving state

## Honest next step
Run the local stack with `RUSTRAG_BOOTSTRAP_TOKEN=bootstrap-local`, mint a browser session token via the existing auth panel or curl, then execute the spec 043 browser matrix and record outcomes in rollout notes/tasks.
