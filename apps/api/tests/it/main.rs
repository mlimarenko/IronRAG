//! Every integration test in one crate.
//!
//! Cargo compiles each file directly under `tests/` as its own crate, which
//! meant sixty-five separate binaries, each statically linking the whole
//! backend — three quarters of a gigabyte apiece — and compiling the shared
//! `support` helpers once per file that used them. Nothing needed that: no test
//! here touches process-global state, so sharing one process costs nothing and
//! saves sixty-four links.
//!
//! Add a test file by dropping it in this directory and declaring it below.

// Shared helpers, compiled once instead of once per test file.
#[path = "support/content_lifecycle_support.rs"]
mod content_lifecycle_support;
#[path = "support/database_url.rs"]
mod database_url;
#[path = "support/graph_reconcile_support.rs"]
mod graph_reconcile_support;
#[path = "support/http_response_support.rs"]
mod http_response_support;
#[path = "support/iam_token_support.rs"]
mod iam_token_support;
#[path = "support/ingest_ai_fixture.rs"]
mod ingest_ai_fixture;
#[path = "support/ingest_ai_support.rs"]
mod ingest_ai_support;
#[path = "support/knowledge_seed.rs"]
mod knowledge_seed;
#[path = "support/mcp_tool_call_support.rs"]
mod mcp_tool_call_support;
#[path = "support/postgres_activity.rs"]
mod postgres_activity;
#[path = "support/structured_document_fixtures.rs"]
mod structured_document_fixtures;
#[path = "support/temp_postgres_database.rs"]
mod temp_postgres_database;
#[path = "support/web_ingest_fixture.rs"]
mod web_ingest_fixture;
#[path = "support/web_ingest_support.rs"]
mod web_ingest_support;

// The tests themselves.
mod ai_catalog_repository;
mod ai_catalog_service;
mod ai_config_generation_fencing;
#[cfg(feature = "test-support")]
mod atomic_admission;
mod audit_events;
mod billing_rollups;
mod bootstrap_stack;
#[cfg(feature = "test-support")]
mod cancellation_class;
#[cfg(feature = "test-support")]
mod canonical_read_visibility;
mod catalog_repository;
mod content_document_graph_coverage;
mod content_lifecycle;
mod content_repository;
mod content_revision_lineage;
mod credential_encryption;
mod document_external_key_reclaim;
mod extract_graph;
mod extract_repository;
mod governance_auth;
mod graph_atomicity_fencing;
mod graph_projection_repository;
mod graph_rebuild_guard_lifecycle;
mod graph_reextract_repository;
mod graph_snapshot_fencing;
#[cfg(feature = "test-support")]
mod graph_summary_executor_fencing;
#[cfg(feature = "test-support")]
mod graph_visibility_race;
mod greenfield_bootstrap;
mod greenfield_contracts;
mod greenfield_support;
mod grounded_benchmark_technical_suite;
mod iam_user_repository;
mod incremental_graph_reconcile;
mod ingest_attempts;
mod ingest_cancellation;
mod ingest_preparation_fencing;
mod ingest_publication;
mod ingest_repository;
mod ingest_unit_fencing;
mod knowledge_lifecycle;
mod knowledge_revision_states;
mod knowledge_search;
mod login_rate_limit;
mod mcp_grounded_answer_contract;
mod mcp_knowledge;
mod mcp_memory_contracts;
mod mcp_memory_permissions;
mod mcp_memory_reads;
mod mcp_memory_search;
#[cfg(feature = "test-support")]
mod mcp_public_writers;
mod model_pricing;
mod observability_smoke;
mod oidc_sso;
mod ops_state;
mod provider_batch_resolution;
mod query_compiler_openai_smoke;
mod query_grounding;
mod revision_prefix_fetch;
mod runtime_graph_extraction_repository;
mod runtime_graph_node_hints;
#[cfg(feature = "test-support")]
mod runtime_orphan_reconcile;
mod snapshot_transfer;
mod stranded_fan_out_repair;
mod structured_block_repository;
mod structured_preparation;
mod terminal_graph_state_repair;
mod web_ingest_contracts;
#[cfg(feature = "test-support")]
mod web_ingest_recursive_crawl;
mod web_ingest_repository;
#[cfg(feature = "test-support")]
mod web_ingest_single_page;
#[cfg(feature = "test-support")]
mod web_ingest_visibility;
#[cfg(feature = "test-support")]
mod webhook_delivery;
mod webhook_outbound;
mod workspace_restore_atomicity;
