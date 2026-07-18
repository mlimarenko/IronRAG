use crate::{
    domains::ops::{
        OpsAsyncOperation, OpsAsyncOperationStatus, OpsLibraryState, OpsLibraryWarning,
    },
    interfaces::http::ops::{build_attention_items_bounded, map_attention_item},
};
use chrono::Utc;
use ironrag_contracts::{
    documents::{DocumentReadiness, DocumentStatus, DocumentSummary},
    graph::{GraphStatus, GraphSurface},
};
use serde_json::json;
use uuid::Uuid;

#[test]
fn async_operation_serializes_using_canonical_camel_case_fields() -> Result<(), serde_json::Error> {
    let operation = OpsAsyncOperation {
        id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Some(Uuid::now_v7()),
        operation_kind: "content_mutation".to_string(),
        status: OpsAsyncOperationStatus::Ready,
        surface_kind: Some("rest".to_string()),
        subject_kind: Some("content_mutation".to_string()),
        subject_id: Some(Uuid::now_v7()),
        parent_async_operation_id: None,
        failure_code: None,
        created_at: Utc::now(),
        completed_at: Some(Utc::now()),
    };

    let serialized = serde_json::to_value(&operation)?;

    assert!(serialized.get("completedAt").is_some());
    assert!(serialized.get("completed_at").is_none());
    assert_eq!(serialized.get("status"), Some(&json!("ready")));
    Ok(())
}

fn ops_state(overrides: impl FnOnce(&mut OpsLibraryState)) -> OpsLibraryState {
    let mut state = OpsLibraryState {
        library_id: Uuid::now_v7(),
        queue_depth: 0,
        running_attempts: 0,
        readable_document_count: 0,
        failed_document_count: 0,
        degraded_state: "ready".to_string(),
        latest_knowledge_generation_id: None,
        knowledge_generation_state: None,
        last_recomputed_at: Utc::now(),
    };
    overrides(&mut state);
    state
}

fn graph_surface(overrides: impl FnOnce(&mut GraphSurface)) -> GraphSurface {
    let mut graph = GraphSurface {
        library_id: Uuid::now_v7(),
        status: GraphStatus::Ready,
        convergence_status: None,
        warning: None,
        node_count: 0,
        relation_count: 0,
        edge_count: 0,
        graph_ready_document_count: 0,
        graph_sparse_document_count: 0,
        typed_fact_document_count: 0,
        updated_at: Some(Utc::now()),
        nodes: Vec::new(),
        edges: Vec::new(),
        readiness_summary: None,
    };
    overrides(&mut graph);
    graph
}

fn recent_document(overrides: impl FnOnce(&mut DocumentSummary)) -> DocumentSummary {
    let mut document = DocumentSummary {
        id: Uuid::now_v7(),
        workspace_id: Some(Uuid::now_v7()),
        library_id: Some(Uuid::now_v7()),
        file_name: "document.pdf".to_string(),
        file_type: "application/pdf".to_string(),
        file_size: 1024,
        uploaded_at: Utc::now(),
        status: DocumentStatus::Failed,
        readiness: DocumentReadiness::Failed,
        stage_label: None,
        progress_percent: None,
        cost_usd: None,
        failure_message: None,
        can_retry: false,
        prepared_segment_count: None,
        technical_fact_count: None,
        source_format: None,
    };
    overrides(&mut document);
    document
}

fn warning(kind: &str, severity: &str) -> OpsLibraryWarning {
    OpsLibraryWarning {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        warning_kind: kind.to_string(),
        severity: severity.to_string(),
        created_at: Utc::now(),
        resolved_at: None,
    }
}

#[test]
fn dashboard_attention_routes_document_lifecycle_signals_to_filtered_documents() {
    let state = ops_state(|state| state.failed_document_count = 3);
    let graph = graph_surface(|_| {});
    let recent = vec![recent_document(|document| document.can_retry = true)];

    let attention = build_attention_items_bounded(&state, &[], &graph, &recent);

    assert_eq!(
        attention
            .iter()
            .find(|item| item.code == "failed_documents")
            .map(|item| item.route_path.as_str()),
        Some("/documents?status=failed"),
    );
    assert_eq!(
        attention
            .iter()
            .find(|item| item.code == "retryable_document")
            .map(|item| item.route_path.as_str()),
        Some("/documents?status=failed"),
    );
}

#[test]
fn dashboard_attention_routes_graph_gap_to_graph_surface() {
    let state = ops_state(|_| {});
    let graph = graph_surface(|graph| {
        graph.status = GraphStatus::Partial;
        graph.graph_sparse_document_count = 4;
    });

    let attention = build_attention_items_bounded(&state, &[], &graph, &[]);

    assert_eq!(
        attention
            .iter()
            .find(|item| item.code == "graph_coverage_gap")
            .map(|item| item.route_path.as_str()),
        Some("/graph"),
    );
}

#[test]
fn dashboard_attention_warning_routes_match_landing_surfaces() {
    assert_eq!(
        map_attention_item(&warning("stale_vectors", "warning")).route_path,
        "/documents?status=processing",
    );
    assert_eq!(map_attention_item(&warning("stale_relations", "warning")).route_path, "/graph");
    assert_eq!(map_attention_item(&warning("failed_rebuilds", "error")).route_path, "/graph");
    assert_eq!(
        map_attention_item(&warning("bundle_assembly_failures", "error")).route_path,
        "/graph",
    );
}
