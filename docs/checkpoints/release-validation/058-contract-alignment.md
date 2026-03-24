# 058 Contract Alignment

## Scope

Feature: `058-release-e2e-validation`

## FR -> Surface mapping

- **FR-001 (format matrix)**  
  - `scripts/release-validation/config.json`
  - `scripts/release-validation/generate-fixtures.py`
  - `scripts/release-validation/run.mjs`
- **FR-002 (stage/status visibility)**  
  - `GET /v1/ingest/jobs/{job_id}`
  - `scripts/release-validation/lib/ingest-client.mjs`
  - `scripts/release-validation/sql/stage-flow.sql`
- **FR-003 (workspace/library ownership truth)**  
  - `GET /v1/content/documents?libraryId=...`
  - `GET /v1/knowledge/libraries/{library_id}/entities`
  - `GET /v1/knowledge/libraries/{library_id}/relations`
- **FR-004 (graph meaningfulness checks)**  
  - `GET /v1/knowledge/libraries/{library_id}/entities`
  - `GET /v1/knowledge/libraries/{library_id}/relations`
  - `scripts/release-validation/lib/semantic-evaluator.mjs`
- **FR-005 (billing traceability to execution)**  
  - `GET /v1/billing/executions/{executionKind}/{executionId}`
  - `GET /v1/billing/executions/{executionKind}/{executionId}/provider-calls`
  - `GET /v1/billing/executions/{executionKind}/{executionId}/charges`
  - canonical owner kind: `ingest_attempt`
- **FR-006 (performance + verdict)**  
  - `scripts/release-validation/sql/stage-latency.sql`
  - `scripts/release-validation/lib/verdict-reducer.mjs`
  - `scripts/release-validation/run.mjs`
- **FR-007 (MCP real workflow)**  
  - `GET /v1/mcp/capabilities`
  - `POST /v1/mcp` JSON-RPC (`initialize`, `tools/list`, `tools/call`)
  - `scripts/release-validation/lib/mcp-client.mjs`
- **FR-008 (defects/fixes/revalidation evidence)**  
  - `docs/checkpoints/release-validation/058-validation-evidence.md`
  - `docs/checkpoints/release-validation/058-backend-gate.md`
- **FR-009 (caller-visible behavior captured in contract)**  
  - `backend/contracts/rustrag.openapi.yaml`

## NFR coverage notes

- **Reproducibility / auditability**: deterministic fixtures + persisted JSON/MD reports.
- **Error/operator clarity**: validator captures explicit status/failure fields and blocking issues.
- **Latency/SLA visibility**: per-stage SQL diagnostics + verdict reducer thresholds.
- **MCP stability/retry semantics**: initialize/list/search/upload/status/read workflow validated.

## Contract clarifications confirmed

- Billing cost rollup path supports `executionKind` values: `query_execution`, `ingest_attempt`.
- Zero-cost ingestion attempts return `200` with deterministic zero totals (not ambiguous `404`).
