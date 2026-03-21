# ArangoDB Cutover Delete List

These files, modules, and conceptual layers are the delete inventory for the ArangoDB rewrite.

## Already removed

- the legacy graph-database adapter file
- `backend/src/infra/vector_search.rs`
- `backend/src/infra/repositories/search_repository.rs`
- `backend/src/interfaces/http/ui_auth.rs`
- `backend/src/interfaces/http/ui_shell.rs`
- `backend/src/infra/ui_queries.rs`
- `backend/src/services/chat_sessions.rs`
- `backend/src/domains/ui_admin.rs`
- `backend/src/domains/ui_identity.rs`
- `backend/src/domains/ui_chat.rs`
- `backend/src/domains/ui_graph.rs`

The baseline is already on ordinary `postgres:18-alpine` with `uuidv7()` defaults and no Postgres `vector` extension.

## Still pending

## Infrastructure

- `backend/src/infra/graph_store.rs`
- `backend/src/infra/repositories/content_repository.rs`
- `backend/src/infra/repositories/extract_repository.rs`
- `backend/src/infra/repositories/graph_repository.rs`
- Postgres `vector` extension and all Postgres-side embedding storage
- Postgres tables `search_chunk_embedding` and `search_graph_node_embedding`

## Services

- `backend/src/services/query_runtime.rs`
- `backend/src/services/query_intelligence.rs`
- `backend/src/services/pricing_catalog.rs`
- `backend/src/services/collection_settlement.rs`
- `backend/src/services/queue_isolation.rs`
- `backend/src/services/terminal_settlement.rs`
- `backend/src/services/graph_diagnostics_snapshot.rs`
- `backend/src/services/operator_warning.rs`
- `backend/src/services/mcp_memory.rs`
- all services that treat a separate graph database as the source of graph truth

## HTTP surfaces

- `backend/src/interfaces/http/mcp_memory.rs`
- any route that exposes runtime projection state as canonical truth

## Domains

- `backend/src/domains/mcp_memory.rs`
- `backend/src/domains/runtime_graph.rs`
- `backend/src/domains/runtime_query.rs`
- `backend/src/domains/pricing_catalog.rs`
- `backend/src/domains/query_intelligence.rs`
- `backend/src/domains/ui_admin.rs`
- `backend/src/domains/ui_chat.rs`
- `backend/src/domains/ui_documents.rs`
- `backend/src/domains/ui_graph.rs`
- `backend/src/domains/ui_identity.rs`

## Operational rule

Deletion is not optional cleanup. These files represent knowledge ownership that no longer fits the target architecture.
The canonical PostgreSQL baseline is ordinary `postgres:18-alpine` with `uuidv7()` defaults, not a `pgvector` image.
