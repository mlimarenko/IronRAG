# ArangoDB Reset Inventory

This document is the destructive cutover inventory for the ArangoDB-centered rewrite.

## No-Legacy Assumptions

- No backward compatibility is preserved.
- No separate graph projection database survives in the target architecture.
- No `runtime_*`, `ui_*`, `mcp_*`, `project`, or `collection` names survive as canonical core-domain vocabulary.
- Postgres remains control plane only.
- PostgreSQL runs on ordinary `postgres:18-alpine`, not `pgvector`.
- Canonical Postgres identifiers use `uuidv7()` defaults.
- ArangoDB becomes the canonical knowledge plane for document-derived knowledge.

## Old Knowledge Ownership To Remove

### Postgres runtime and projection truth

- `content_*`
- `extract_*`
- `graph_*`
- `search_*`
- `runtime_graph_*`
- `runtime_query_*`
- `mcp_mutation_receipt`
- `mcp_audit_event`
- Postgres `vector` extension
- Postgres tables `search_chunk_embedding`
- Postgres tables `search_graph_node_embedding`

### Legacy graph projection layer

- the removed legacy graph-database adapter file
- `backend/src/infra/graph_store.rs`
- `backend/src/infra/vector_search.rs`
- all services that treat a separate graph database as the graph read model

### Glue-heavy application surfaces

- `backend/src/services/query_runtime.rs`
- `backend/src/services/query_intelligence.rs`
- `backend/src/services/chat_sessions.rs`
- `backend/src/interfaces/http/mcp_memory.rs`
- `backend/src/services/mcp_memory.rs`
- `backend/src/interfaces/http/ui_auth.rs`
- `backend/src/interfaces/http/ui_shell.rs`
- `backend/src/infra/ui_queries.rs`

## Target Postgres Control Plane Tables

- `catalog_workspace`
- `catalog_library`
- `catalog_library_connector`
- `iam_principal`
- `iam_api_token`
- `iam_grant`
- `iam_workspace_membership`
- `ai_provider_catalog`
- `ai_model_catalog`
- `ai_price_catalog`
- `ai_provider_credential`
- `ai_model_preset`
- `ai_library_model_binding`
- `ai_binding_validation`
- `ingest_job`
- `ingest_attempt`
- `ingest_stage_event`
- `query_session`
- `query_turn`
- `query_execution`
- `billing_provider_call`
- `billing_library_usage_rollup`
- `ops_library_state`
- `audit_event`
- `audit_event_subject`
- `async_operation`

## Target ArangoDB Knowledge Collections

### Document collections

- `knowledge_document`
- `knowledge_revision`
- `knowledge_chunk`
- `knowledge_library_generation`

### Vector collections

- `knowledge_chunk_vector`
- `knowledge_entity_vector`

### Graph collections

- `knowledge_entity`
- `knowledge_entity_candidate`
- `knowledge_relation`
- `knowledge_relation_candidate`
- `knowledge_evidence`

### Retrieval and context collections

- `knowledge_context_bundle`
- `knowledge_retrieval_trace`

### Edge collections

- `knowledge_document_revision_edge`
- `knowledge_revision_chunk_edge`
- `knowledge_relation_subject_edge`
- `knowledge_relation_object_edge`
- `knowledge_evidence_supports_entity_edge`
- `knowledge_evidence_supports_relation_edge`
- `knowledge_bundle_chunk_edge`
- `knowledge_bundle_entity_edge`
- `knowledge_bundle_relation_edge`
- `knowledge_bundle_evidence_edge`

## Cutover Principle

The target system is not a migration bridge. The final runtime owns knowledge in one place: ArangoDB.
