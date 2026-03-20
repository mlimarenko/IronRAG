# Schema Reset Inventory

Date: 2026-03-20

This document tracks the destructive greenfield reset inventory for the backend.

## Canonical Vocabulary

- `workspace`
- `library`
- `document`
- `revision`
- `connector`
- `principal`
- `grant`
- `conversation`
- `query`

Forbidden synonyms for the same aggregate:

- `project`
- `collection`

Forbidden core-domain prefixes:

- `runtime_`
- `ui_`
- `mcp_`

## Aggregate Reset Inventory

### Delete outright

- `entity`
- `relation`
- `mcp_audit_event`
- `runtime_vector_target`

### Merge into canonical catalog and IAM

- `project` -> `catalog_library`
- `source` -> `catalog_library_connector`
- `workspace_member` -> `iam_workspace_membership`
- `project_access_grant` + `api_token.scope_json` -> `iam_grant`

### Merge into canonical content and ingest

- `document` + `document_revision` + `document_mutation_workflow` -> `content_document` + `content_revision` + `content_document_head` + `content_mutation` + `content_mutation_item`
- `ingestion_job` + `ingestion_job_attempt` + `runtime_ingestion_run` -> `ingest_job` + `ingest_attempt`

### Merge into canonical extract, graph, and search

- `runtime_extracted_content` -> `extract_content`
- `runtime_graph_extraction` and related resume/recovery rows -> `extract_chunk_result` + candidate rows + `extract_resume_cursor`
- `runtime_graph_node` + `runtime_graph_edge` + `runtime_graph_evidence` -> `graph_*`
- `chunk.embedding` + `chunk_embedding` + `runtime_vector_target` -> `search_*`

### Merge into canonical query, billing, ops, and audit

- `retrieval_run` + `runtime_query_execution` -> `query_execution`
- `usage_event` + `cost_ledger` + runtime accounting rows -> `billing_*`
- `mcp_mutation_receipt` -> `ops_async_operation`
- runtime snapshot tables -> `ops_library_state` + `ops_library_warning`
- MCP-only audit -> `audit_event` + `audit_event_subject`

## Notes

- This inventory is intentionally destructive.
- No schema-level compatibility aliases or dual-write layers are planned.
- Fresh bootstrap must succeed from the canonical baseline alone, including seeded provider, model, and price catalogs.

## Authoritative Migration State

- The backend migration directory is now intentionally greenfield-only.
- `backend/migrations/0001_init.sql` is the only authoritative schema migration for fresh deployments.
- Legacy migration files `0002..0021` were deleted after their historical intent was either dropped or folded into the canonical baseline.
- A fresh bootstrap is expected to produce:
  - `3` seeded `ai_provider_catalog` rows
  - `7` seeded `ai_model_catalog` rows
  - `12` seeded `ai_price_catalog` rows
- A fresh bootstrap must not produce:
  - `workspace`
  - `project`
  - `runtime_ingestion_run`
  - `mcp_audit_event`
