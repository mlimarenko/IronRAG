# Auth / Access-Control Gap Plan

## Date
2026-03-13

## Scope reviewed
- `docs/TASKS-SCOPE-1.md`
- `backend/src/interfaces/http/auth.rs`
- `backend/src/interfaces/http/{workspaces,projects,providers,ingestion,documents,content,chunks,uploads,retrieval,usage,integrations}.rs`
- `backend/contracts/rustrag.openapi.yaml`
- `backend/docs/security/secrets-and-provider-accounts.md`
- `backend/migrations/0001_init_core.sql`

## Confirmed issues
1. **Workspace-boundary enforcement is inconsistent on read/list surfaces.**
   - Multiple list endpoints are still public and return cross-workspace inventory without `AuthContext` checks: `/v1/workspaces`, `/v1/projects`, `/v1/provider-accounts`, `/v1/model-profiles`, `/v1/sources`, `/v1/ingestion-jobs`, `/v1/documents`, `/v1/retrieval-runs`.
   - `openapi` explicitly documents many of these with `security: []` and even notes that some read/list endpoints are public.
   - Even some authenticated read endpoints miss object-to-workspace checks after scope validation:
     - `backend/src/interfaces/http/documents.rs:create_document` does not verify the target project's workspace.
     - `backend/src/interfaces/http/content.rs:ingest_text`, `search_chunks`, and `embed_project_chunks` do not verify that the caller can access the target project/workspace.
     - `backend/src/interfaces/http/uploads.rs:upload_and_ingest` does not verify workspace access for the submitted `project_id`.
     - `backend/src/interfaces/http/chunks.rs:list_chunks` requires scope but does not confirm the referenced document/project belongs to the caller workspace.
     - `backend/src/interfaces/http/usage.rs` scopes access but does not constrain rows to the caller workspace/project before returning usage/cost data.
2. **Scopes exist, but the permission model is still route-local and incomplete.**
   - Scope checks are hard-coded per handler via `require_any_scope`, with no central permission matrix or helper describing route-to-scope and route-to-resource rules.
   - `AuthContext` only has `require_any_scope` and `require_workspace_access`; it does not provide reusable helpers for project/document/job/token ownership checks, so handlers repeat or skip boundary verification.
   - `docs/TASKS-SCOPE-1.md` already marks `T026 define auth scopes and permission matrix` as partial, and the code confirms that status.
3. **OpenAPI/docs drift currently normalizes insecure behavior instead of describing the intended boundary.**
   - `backend/contracts/rustrag.openapi.yaml` marks many inventory/list routes as public (`security: []`) even where the rest of the backend already models workspace-scoped auth.
   - The contract descriptions do not call out the missing project/document/job ownership enforcement on several handlers, so generated clients/docs can imply safer behavior than the implementation actually guarantees.
4. **Secret boundary remains unfinished and tied to the same auth hardening pass.**
   - `provider_account.encrypted_secret jsonb` exists in schema, but the reviewed HTTP surfaces and security doc still describe future hardening (key management, rotation, audit trail, allowlists) rather than implemented controls.
   - This is not the first fix to do, but it is still an auth/access-control adjacent unfinished area from `T025`.

## Highest-priority implementation steps
1. **Close the workspace-boundary holes first.**
   - Require bearer auth on all non-bootstrap control-plane inventory/read routes that expose tenant data.
   - For every route taking `workspace_id`, `project_id`, `document_id`, `ingestion_job_id`, `retrieval_run_id`, or token id, resolve the owning workspace and enforce `auth.require_workspace_access(...)` before returning or mutating data.
   - Add small shared helpers like `load_project_and_authorize`, `load_document_project_and_authorize`, `load_ingestion_job_project_and_authorize`, and `authorize_project_query_scope` to eliminate missed checks.
2. **Centralize the permission matrix in code.**
   - Define a single source of truth for scopes per capability/route family (workspace admin, providers admin, projects write, documents read/write, query run, usage read).
   - Replace ad-hoc arrays in handlers with named policy helpers/constants so scope intent is auditable and testable.
   - Decide and document whether list endpoints should support: instance-admin global view, workspace-token own-workspace-only view, or both.
3. **Reconcile OpenAPI and docs after the auth behavior is fixed.**
   - Remove `security: []` from tenant data routes that should require auth.
   - Update route descriptions to state actual workspace scoping and any allowed cross-workspace behavior for `instance_admin`.
   - Regenerate/refresh frontend client expectations after contract changes.
4. **Finish the provider secret-handling boundary.**
   - Introduce explicit secret write/read/update flow boundaries, encryption/key source handling, redaction guarantees, and auditability.
   - Add provider-account auth rules alongside any future per-project/provider allowlists described in `backend/docs/security/secrets-and-provider-accounts.md`.

## Affected files / modules
- `backend/src/interfaces/http/auth.rs`
- `backend/src/interfaces/http/workspaces.rs`
- `backend/src/interfaces/http/projects.rs`
- `backend/src/interfaces/http/providers.rs`
- `backend/src/interfaces/http/ingestion.rs`
- `backend/src/interfaces/http/documents.rs`
- `backend/src/interfaces/http/content.rs`
- `backend/src/interfaces/http/chunks.rs`
- `backend/src/interfaces/http/uploads.rs`
- `backend/src/interfaces/http/retrieval.rs`
- `backend/src/interfaces/http/usage.rs`
- `backend/src/interfaces/http/integrations.rs`
- `backend/contracts/rustrag.openapi.yaml`
- `backend/docs/security/secrets-and-provider-accounts.md`
- `docs/TASKS-SCOPE-1.md`

## Validation steps
- Add handler-level tests proving workspace tokens cannot read or mutate resources from another workspace for each resource family: projects, providers, documents, chunks, ingestion jobs, retrieval runs, usage, integrations.
- Add tests that authenticated workspace tokens only see their own rows on list endpoints, while `instance_admin` behavior is explicitly covered.
- Add negative tests for `document_id` / `project_id` query parameters that currently bypass ownership checks.
- Diff implemented route auth against `backend/contracts/rustrag.openapi.yaml` and fail CI if a route’s declared security/scoping no longer matches the handler policy.
- Smoke test generated frontend API usage against the updated contract.

## Risks / caveats
- Tightening public list endpoints will likely break any current frontend/bootstrap flows that quietly relied on anonymous reads; sequence the contract/client update with the backend change.
- Instance-admin versus workspace-token behavior is partly implicit today; hardening without deciding that model first will create churn.
- Centralizing policy helpers will touch many handlers at once, so partial rollout can leave mixed semantics unless done as one auth pass.
- Secret-boundary work depends on operational decisions outside the HTTP layer (key source, rotation strategy, audit retention), so it should follow the boundary-enforcement fix rather than block it.
