# 058 Operator UI Verification Notes

## Scope

Validation of operator-facing truth for Documents/Graph/Admin surfaces based on canonical backend runtime outputs and release-validation artifacts.

## Verified truth sources

- Document processing and readiness:
  - `GET /v1/ingest/jobs/{job_id}`
  - `GET /v1/content/documents?libraryId=...`
- Graph population/quality:
  - `GET /v1/knowledge/libraries/{library_id}/entities`
  - `GET /v1/knowledge/libraries/{library_id}/relations`
- Billing transparency:
  - `GET /v1/billing/executions/ingest_attempt/{attempt_id}`
  - `GET /v1/billing/executions/ingest_attempt/{attempt_id}/provider-calls`
  - `GET /v1/billing/executions/ingest_attempt/{attempt_id}/charges`
- MCP/agent operability:
  - `GET /v1/mcp/capabilities`
  - `POST /v1/mcp`

## Evidence highlights

- 9/9 fixtures reached `graphReady = true`.
- Graph contains meaningful extracted terms (`acme`, `beta`, `berlin`, `rustrag`, `budget 2026`).
- Billing returned `200` for every validated attempt, including explicit zero-cost behavior for one PDF attempt.
- MCP flow completed through search/upload/status/read steps without blocking errors.

## Notes

- This checkpoint validates operator UI truth via backend contracts and release artifacts.
- Optional manual visual walkthrough can be executed as a separate UX sign-off pass if needed before release cut.
