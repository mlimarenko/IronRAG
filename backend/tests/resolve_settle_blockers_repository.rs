mod resolve_settle_blockers_support;

use std::{fs, path::PathBuf};

use resolve_settle_blockers_support::{
    sample_documents_workspace, sample_graph_diagnostics_snapshot, sample_terminal_outcome,
};
use rustrag_backend::{
    domains::runtime_graph::{
        RuntimeGraphProjectionLockState, RuntimeGraphProjectionWriteState,
        RuntimeGraphWriteFailureKind,
    },
    infra::repositories::{
        runtime_graph_projection_lock_state_key, runtime_graph_projection_write_state_key,
        runtime_graph_write_failure_kind_key,
    },
};

fn load_repository_source() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("infra").join("repositories.rs");
    let source = fs::read_to_string(&path);
    assert!(source.is_ok(), "expected repositories source at {}", path.display());
    source.unwrap_or_default()
}

#[test]
fn repository_source_keeps_terminal_and_graph_snapshot_helpers() {
    let source = load_repository_source();

    assert!(source.contains("pub struct DocumentsWorkspaceProjectionRows"));
    assert!(source.contains("pub async fn load_runtime_collection_terminal_outcome"));
    assert!(source.contains("pub async fn upsert_runtime_collection_terminal_outcome"));
    assert!(source.contains("pub async fn load_runtime_graph_diagnostics_snapshot"));
    assert!(source.contains("pub async fn upsert_runtime_graph_diagnostics_snapshot"));
    assert!(source.contains("pub struct RuntimeGraphProjectionScopeRow"));
    assert!(source.contains("pub struct RuntimeGraphProjectionScopeCountersRow"));
    assert!(source.contains("pub async fn create_runtime_graph_projection_scope"));
    assert!(source.contains("pub async fn update_runtime_graph_projection_scope"));
    assert!(source.contains("pub async fn list_active_runtime_graph_projection_scopes_by_project"));
    assert!(source.contains("pub async fn load_runtime_graph_projection_scope_counters"));
    assert!(source.contains("pub async fn load_documents_workspace_projection_rows"));
}

#[test]
fn repository_source_keeps_provider_failure_checkpoint_fields() {
    let source = load_repository_source();

    assert!(source.contains("pub async fn load_runtime_provider_failure_snapshot"));
    assert!(source.contains("pub async fn record_runtime_graph_progress_failure_classification"));
    assert!(source.contains("provider_failure_class"));
    assert!(source.contains("request_shape_key"));
    assert!(source.contains("request_size_bytes"));
}

#[test]
fn repository_source_keeps_projection_scope_counter_from_clause() {
    let source = load_repository_source();

    assert!(source.contains("with scope_rows as ("));
    assert!(source.contains("from scope_rows"));
}

#[test]
fn support_samples_keep_non_empty_workspace_and_terminal_truth() {
    let workspace = sample_documents_workspace();
    let terminal = sample_terminal_outcome();
    let graph = sample_graph_diagnostics_snapshot();

    assert_eq!(workspace.primary_summary.terminal_state, "live_in_flight");
    assert!(workspace.table_document_count > 0);
    assert_eq!(terminal.failed_document_count, 3);
    assert_eq!(graph.retrying_projection_count, 1);
}

#[test]
fn projection_scope_key_helpers_keep_stable_contract_values() {
    assert_eq!(
        runtime_graph_projection_lock_state_key(
            &RuntimeGraphProjectionLockState::RetryingContention
        ),
        "retrying_contention"
    );
    assert_eq!(
        runtime_graph_projection_write_state_key(&RuntimeGraphProjectionWriteState::Completed),
        "completed"
    );
    assert_eq!(
        runtime_graph_write_failure_kind_key(
            &RuntimeGraphWriteFailureKind::GraphPersistenceIntegrity
        ),
        "graph_persistence_integrity"
    );
}
