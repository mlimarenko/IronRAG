# Greenfield DB Schema Audit

Date: 2026-03-20

Scope: [backend](/home/leader/sources/RustRAG/rustrag/backend)

Assumption: no legacy compatibility is preserved. The target is an ideal schema and domain model as if the system were being written from scratch today. Destructive changes, renames, table drops, aggregate redesign, and baseline reset are all allowed.

## Implemented State Snapshot

The rewrite has already locked in these baseline decisions:

- PostgreSQL runs on ordinary `postgres:18-alpine`.
- `backend/migrations/0001_init.sql` no longer enables the Postgres `vector` extension.
- canonical identifiers in the baseline use `uuidv7()`.
- Postgres-side embedding tables `search_chunk_embedding` and `search_graph_node_embedding` were removed from the baseline.
- `backend/src/infra/vector_search.rs` and `backend/src/infra/repositories/search_repository.rs` are already gone.
- the legacy graph-database adapter file is already gone.
- `backend/src/interfaces/http/ui_auth.rs`, `backend/src/interfaces/http/ui_shell.rs`, and `backend/src/infra/ui_queries.rs` are already gone.
- `backend/src/services/chat_sessions.rs` and the dead `domains::legacy` / retrieval-only domain files are already gone.
- dead backend UI domain models `ui_admin`, `ui_identity`, `ui_chat`, and `ui_graph` are already gone.

The remaining work is not to debate these choices. The remaining work is to finish removing the still-live services and routes that still speak in runtime-era vocabulary.

## Verdict

The current schema is not one coherent model. It is a historical accumulation of:

- product nouns: `workspace`, `project`, `library`, `collection`
- transport nouns: `ui_*`, `mcp_*`
- implementation nouns: `runtime_*`
- legacy domain nouns: `entity`, `relation`

The result is four concurrent naming systems and several duplicated truth models:

- two graph models
- two query/retrieval models
- multiple runtime health and settlement models
- separate MCP-only audit instead of a system audit
- separate token/UI auth models instead of one principal/grant model
- historical init plus repair migrations instead of a real baseline

The correct response is not incremental cleanup. The correct response is a new schema baseline with a new vocabulary and new aggregate boundaries.

## Core Principles For Schema V2

1. One canonical product vocabulary.
2. One canonical aggregate per domain concept.
3. One canonical source of truth for state.
4. No transport-specific tables.
5. No `runtime_` catch-all prefix in persisted model names.
6. No procedural table names when an entity name exists.
7. No polymorphic `kind + id` references when FK-safe typed links are possible.
8. No hidden semantic state inside `jsonb` if it affects invariants or workflows.
9. No create-on-read behavior.
10. No baseline migrations with backfills, repairs, or conditional DDL.

## Target Naming Convention

All persistent tables should use:

- `<domain>_<entity>`
- singular nouns
- domain prefixes only, not transport or implementation prefixes

Allowed domains:

- `catalog_`: tenant containers and owned configuration
- `iam_`: identity, sessions, tokens, grants
- `ai_`: provider credentials, model presets, validation, pricing
- `content_`: logical documents, revisions, content chunks, mutations
- `ingest_`: connectors, jobs, runs, attempts, stage events
- `extract_`: extracted text and extraction state
- `graph_`: canonical graph, evidence, projections, summaries
- `query_`: conversations, turns, query executions, references, intent cache
- `search_`: embeddings and vector index targets
- `billing_`: raw usage and priced charges
- `ops_`: derived operational state and warnings only
- `audit_`: system-wide immutable audit trail

Forbidden prefixes for new schema:

- `runtime_`
- `ui_`
- `mcp_`

Forbidden product nouns in new schema if `library` is chosen:

- `project`
- `collection`

## Product Vocabulary Reset

The product language should be:

- `workspace`
- `library`
- `document`
- `revision`
- `connector`
- `query`
- `conversation`
- `principal`
- `grant`

The word `project` should be removed from domain, database, API, and code naming. It is already semantically replaced by `library` in MCP and runtime behavior.

## Schema V2 By Domain

### Catalog

- `catalog_workspace`
- `catalog_library`
- `catalog_library_connector`

Responsibilities:

- workspace identity
- library identity
- connector definitions attached to libraries

Remove:

- `project`
- `source`

### IAM

- `iam_principal`
- `iam_user`
- `iam_session`
- `iam_api_token`
- `iam_api_token_secret`
- `iam_grant`
- `iam_workspace_membership`

Responsibilities:

- all actors represented as principals
- UI and token auth sharing one authorization model
- resource-scoped grants for workspace, library, document, provider credential

Remove or merge:

- `workspace_member`
- `project_access_grant`
- `api_token.scope_json`

### AI

- `ai_provider_credential`
- `ai_model_preset`
- `ai_library_model_binding`
- `ai_provider_validation`
- `ai_price_catalog`

Responsibilities:

- workspace-scoped provider secrets
- reusable model presets
- library-scoped active AI binding
- validation history
- pricing catalog

Remove or merge:

- `provider_account`
- `model_profile`
- `runtime_provider_profile`

### Content

- `content_document`
- `content_document_head`
- `content_revision`
- `content_chunk`
- `content_mutation`
- `content_mutation_scope`

Responsibilities:

- logical document identity
- immutable revisions
- active head pointer
- chunks derived from active or revision-scoped content
- mutation lifecycle

Remove or redesign:

- revision-like columns on `document`
- `document_mutation_workflow`

### Ingest

- `ingest_job`
- `ingest_attempt`
- `ingest_stage_event`

Responsibilities:

- queue/lease/admission
- attempt lifecycle
- stage transitions

Merge:

- `ingestion_job`
- `ingestion_job_attempt`
- `runtime_ingestion_run`

### Extract

- `extract_content`
- `extract_graph_chunk`
- `extract_graph_resume`
- `extract_graph_progress`

Responsibilities:

- normalized extracted text
- graph extraction attempts per chunk
- resumable extraction state
- provider progress and failure telemetry

Merge or redesign:

- `runtime_graph_extraction`
- `runtime_graph_extraction_resume_state`
- `runtime_graph_extraction_recovery_attempt`
- `runtime_graph_progress_checkpoint`

### Graph

- `graph_projection`
- `graph_node`
- `graph_edge`
- `graph_node_evidence`
- `graph_edge_evidence`
- `graph_summary`
- `graph_rejected_candidate`
- `graph_document_contribution`

Responsibilities:

- one canonical graph model
- one projection root
- typed evidence relations
- graph summaries as derived artifacts

Delete:

- `entity`
- `relation`

Redesign:

- `runtime_graph_evidence(target_kind, target_id)`
- per-row `projection_version`

### Query

- `query_session`
- `query_turn`
- `query_execution`
- `query_reference`
- `query_enrichment`
- `query_intent_cache`

Responsibilities:

- conversation state
- user and assistant turns
- query execution state
- references
- optional enrichment trace
- optional intent cache

Merge or delete:

- `retrieval_run`
- `chat_message.retrieval_run_id`
- duplicated query execution artifacts in `debug_json`

### Search

- `search_chunk_embedding`
- optional `search_graph_embedding`

Responsibilities:

- embeddings as explicit search infrastructure
- no polymorphic vector target rows

Delete or redesign:

- `chunk.embedding`
- `chunk_embedding.embedding_json`
- `runtime_vector_target`

### Billing

- `billing_usage`
- `billing_charge`
- `billing_provider_call_cost`
- `billing_execution_cost`

Responsibilities:

- raw usage events
- priced charge records
- per-call and per-execution cost summaries

Merge or redesign:

- `usage_event`
- `cost_ledger`
- `runtime_attempt_stage_accounting`
- `runtime_attempt_cost_summary`

### Ops

- `ops_library_state`
- `ops_library_rollup`
- `ops_library_warning`
- optional `ops_library_queue`

Responsibilities:

- derived operational read model only
- no canonical truth outside domain aggregates

Merge or delete:

- `runtime_library_queue_slice`
- `runtime_collection_settlement_snapshot`
- `runtime_collection_settlement_rollup`
- `runtime_collection_warning_snapshot`
- `runtime_collection_terminal_outcome`
- `runtime_graph_diagnostics_snapshot`

### Audit

- `audit_event`
- `audit_event_subject`

Responsibilities:

- immutable system-wide audit for all transports and actors
- one audit model for UI, REST, MCP, worker, bootstrap

Delete:

- `mcp_audit_event`

Generalize:

- `mcp_mutation_receipt` into shared operation receipt if still needed

## Major Semantic Errors In Current Model

1. `0001_init.sql` is not an init migration.
It already contains backfill, repair, `ALTER`, and data mutation logic.

2. `project` and `library` describe the same domain aggregate.
This breaks ubiquitous language and pollutes every table and API.

3. `collection` is a third name for the same aggregate.
It appears in settlement tables and creates false domain boundaries.

4. `runtime_` is used as a domain prefix even though it is only an implementation detail.

5. `ui_` and `mcp_` tables encode client transport in schema.
Transport is not a domain.

6. `entity/relation` and `runtime_graph_node/edge` are two graph models.

7. `retrieval_run` and `runtime_query_execution` are two query execution models.

8. `ingestion_job`, `ingestion_job_attempt`, and `runtime_ingestion_run` split queue truth from runtime truth.

9. `document`, `document_revision`, and `document_mutation_workflow` all carry partially overlapping lifecycle state.

10. `runtime_graph_evidence` and `runtime_query_reference` both use polymorphic `kind + id`.

11. `chunk.embedding`, `chunk_embedding`, and `runtime_vector_target` create more than one embedding model.

12. `runtime_provider_profile` duplicates the semantics of `model_profile` and library defaults.

13. Runtime health is spread across too many snapshot and diagnostics tables.

14. Important domain semantics are persisted inside `jsonb` fields named `debug_json`, `payload_json`, `result_json`, and assorted `*_snapshot_json`.

15. Authorization is split across role labels, access levels, token kind, scope JSON, and ad hoc policy functions.

16. Persisted audit exists only for MCP.

17. Discovery code creates default resources.

18. Some provider and ingestion listing endpoints are readable without auth.

19. Several rows duplicate both `workspace_id` and `project_id` without hard invariants.

20. Query and chat state are coupled procedurally in handlers instead of by a clean domain model.

## High-Priority Changes

### P0

1. Replace the entire migration baseline with a new DDL-only `0001_init.sql`.
2. Choose one product noun: `library`, and delete `project`/`collection` from schema vocabulary.
3. Replace `runtime_` naming with domain naming.
4. Merge `ingestion_job` and `runtime_ingestion_run`.
5. Redesign document lifecycle around logical document, revision, and head.
6. Delete legacy `entity/relation`.
7. Replace polymorphic graph evidence with typed evidence tables.
8. Replace MCP-only audit with system-wide audit.
9. Replace token scope JSON with explicit grants.
10. Add token-scoped library/document grants.
11. Eliminate create-on-read.
12. Close unauthenticated read endpoints for provider and ingestion state.
13. Remove transport prefixes from schema.
14. Remove duplicated embedding models.
15. Add strict enum/check constraints for all persisted states.

### P1

16. Merge runtime health snapshots into one ops state model.
17. Replace `retrieval_run` plus `runtime_query_execution` with one query execution model.
18. Replace flat `chat_message(role, content)` with typed conversation turns.
19. Separate conversation configuration from conversation state.
20. Move intent cache to a minimal cache model or external cache.
21. Split provider credentials, reusable model presets, and library bindings.
22. Rebuild billing around raw usage plus priced charges.
23. Remove semantic state from JSON payloads where invariants depend on it.
24. Remove duplicated scope columns or enforce composite invariants.
25. Replace per-target vector polymorphism with explicit search embeddings.
26. Make audit append-only and correlation-aware for all transports.
27. Remove self-auditing recursion from audit review.
28. Separate auth validation, usage touch, and audit recording.

### P2

29. Replace placeholder chat title semantics with optional user labels.
30. Replace persisted prompt-state derivations with computed values.
31. Rename `source` to `ingest_source` or `catalog_library_connector`.
32. Rename procedural tables to entity names.
33. Reduce snapshot tables to read models only.
34. Split large repository/services by domain boundaries that mirror the new schema.

## Current To Target Rename Examples

- `workspace` -> `catalog_workspace`
- `project` -> `catalog_library`
- `source` -> `catalog_library_connector` or `ingest_source`
- `ui_user` -> `iam_user`
- `ui_session` -> `iam_session`
- `workspace_member` -> `iam_workspace_membership`
- `project_access_grant` -> `iam_library_grant`
- `api_token` -> `iam_api_token`
- `provider_account` -> `ai_provider_credential`
- `model_profile` -> `ai_model_preset`
- `runtime_provider_profile` -> `ai_library_model_binding`
- `document` -> `content_document`
- `document_revision` -> `content_revision`
- `chunk` -> `content_chunk`
- `document_mutation_workflow` -> `content_mutation`
- `ingestion_job` -> `ingest_job`
- `ingestion_job_attempt` -> `ingest_attempt`
- `runtime_ingestion_run` -> `ingest_attempt` or `ingest_run` depending on final merge
- `runtime_ingestion_stage_event` -> `ingest_stage_event`
- `runtime_extracted_content` -> `extract_content`
- `runtime_graph_extraction` -> `extract_graph_chunk`
- `runtime_graph_node` -> `graph_node`
- `runtime_graph_edge` -> `graph_edge`
- `runtime_graph_evidence` -> `graph_node_evidence` and `graph_edge_evidence`
- `runtime_graph_canonical_summary` -> `graph_summary`
- `chat_session` -> `query_session`
- `chat_message` -> `query_turn`
- `retrieval_run` -> merged into `query_execution`
- `runtime_query_execution` -> `query_execution`
- `runtime_query_reference` -> `query_reference`
- `runtime_query_enrichment` -> `query_enrichment`
- `query_intent_cache_entry` -> `query_intent_cache`
- `usage_event` -> `billing_usage`
- `cost_ledger` -> `billing_charge`
- `runtime_attempt_stage_accounting` -> `billing_provider_call_cost`
- `runtime_attempt_cost_summary` -> `billing_execution_cost`
- `runtime_collection_settlement_snapshot` -> `ops_library_state`
- `runtime_collection_settlement_rollup` -> `ops_library_rollup`
- `runtime_collection_warning_snapshot` -> `ops_library_warning`
- `mcp_audit_event` -> merged into `audit_event`
- `mcp_mutation_receipt` -> shared `ops_operation_receipt` if retained

## Deletions In Greenfield Schema

Delete outright rather than preserve:

- `entity`
- `relation`
- `mcp_audit_event`
- transport-specific audit logic
- `retrieval_run` as a separate aggregate if `query_execution` remains
- `chunk.embedding` or `chunk_embedding`, one of them must die
- `runtime_vector_target` if explicit embedding tables replace it

## Destructive Baseline Reset Strategy

1. Finalize vocabulary first.
2. Finalize aggregate boundaries second.
3. Design schema v2 from the target model, not from current migrations.
4. Write a new `0001_init.sql` from scratch.
5. Move old migrations to a legacy folder and stop treating them as canonical.
6. For existing environments, use explicit reset or one-shot migration/export-import outside the new baseline.

## Shortlist For Approval

Approve in bundles:

1. Vocabulary reset:
   `project -> library`, remove `collection`, remove `runtime/ui/mcp` prefixes.
2. Identity/auth/audit reset:
   unified `iam_*` and `audit_*`.
3. Content/ingest reset:
   document/revision/head and merged ingest attempt model.
4. Graph/search reset:
   one graph model, one embedding model, typed evidence.
5. Query/conversation reset:
   one query execution model and typed conversation turns.
6. Ops reset:
   one derived library state model instead of many snapshot tables.
