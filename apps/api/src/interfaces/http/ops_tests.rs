use crate::{
    domains::ops::{
        OpsAsyncOperation, OpsAsyncOperationProgress, OpsAsyncOperationStatus,
        OpsFailureDisposition, OpsLibraryState, OpsLibraryWarning,
    },
    interfaces::http::ops::{
        AsyncOperationDetailResponse, build_attention_items_bounded, map_attention_item,
    },
};
use chrono::Utc;
use ironrag_contracts::{
    documents::{DocumentReadiness, DocumentStatus, DocumentSummary},
    graph::{GraphStatus, GraphSurface},
};
use serde_json::json;
use uuid::Uuid;

#[test]
fn async_operation_serializes_using_expected_camel_case_fields() -> Result<(), serde_json::Error> {
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
        failure_disposition: Some(OpsFailureDisposition::ContentUnsupported),
        created_at: Utc::now(),
        completed_at: Some(Utc::now()),
        result_json: Some(json!({ "blobsRestored": 0 })),
    };

    let serialized = serde_json::to_value(&operation)?;

    assert!(serialized.get("completedAt").is_some());
    assert!(serialized.get("completed_at").is_none());
    assert_eq!(serialized.get("status"), Some(&json!("ready")));
    // The connector consumes exactly this field; its wire value and casing are
    // part of the contract, so assert the JSON, not a Rust-side helper.
    assert_eq!(serialized.get("failureDisposition"), Some(&json!("content_unsupported")));
    assert!(serialized.get("failure_disposition").is_none());
    // The result payload rides on the same envelope, under the same casing.
    assert_eq!(serialized.get("resultJson"), Some(&json!({ "blobsRestored": 0 })));
    assert!(serialized.get("result_json").is_none());
    Ok(())
}

#[test]
fn get_async_operation_response_serializes_failure_disposition_as_camel_case()
-> Result<(), serde_json::Error> {
    // This is the exact body `GET /v1/ops/operations/{operationId}` returns:
    // the operation is flattened into the response, so the field the connector
    // reads lives at the top level with camelCase keys.
    let response = AsyncOperationDetailResponse {
        operation: OpsAsyncOperation {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Some(Uuid::now_v7()),
            operation_kind: "content_mutation".to_string(),
            status: OpsAsyncOperationStatus::Failed,
            surface_kind: Some("rest".to_string()),
            subject_kind: Some("content_mutation".to_string()),
            subject_id: Some(Uuid::now_v7()),
            parent_async_operation_id: None,
            failure_code: Some("upload_extraction_failed".to_string()),
            failure_disposition: Some(OpsFailureDisposition::ContentUnsupported),
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
            result_json: None,
        },
        progress: OpsAsyncOperationProgress { total: 0, completed: 0, failed: 0, in_flight: 0 },
    };

    let serialized = serde_json::to_value(&response)?;

    assert_eq!(serialized.get("failureDisposition"), Some(&json!("content_unsupported")));
    assert_eq!(serialized.get("failureCode"), Some(&json!("upload_extraction_failed")));
    Ok(())
}

#[test]
fn async_operation_without_terminal_proof_serializes_null_failure_disposition()
-> Result<(), serde_json::Error> {
    let operation = OpsAsyncOperation {
        id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: None,
        operation_kind: "delete_library".to_string(),
        status: OpsAsyncOperationStatus::Failed,
        surface_kind: Some("rest".to_string()),
        subject_kind: Some("library".to_string()),
        subject_id: Some(Uuid::now_v7()),
        parent_async_operation_id: None,
        failure_code: Some("library_deleting".to_string()),
        failure_disposition: None,
        created_at: Utc::now(),
        completed_at: Some(Utc::now()),
        result_json: None,
    };

    let serialized = serde_json::to_value(&operation)?;

    assert_eq!(serialized.get("failureDisposition"), Some(&serde_json::Value::Null));
    assert_eq!(serialized.get("failureCode"), Some(&json!("library_deleting")));
    Ok(())
}

#[test]
fn unknown_failure_disposition_is_surfaced_as_absent_in_a_successful_read() {
    // A value this binary does not know — written by a newer binary or by hand
    // — must not turn the whole operation read into a 500. The row-to-domain
    // mapping keeps the response shape and reports no terminal disposition.
    let operation = crate::services::ops::service::map_async_operation_row(
        crate::infra::repositories::ops_repository::OpsAsyncOperationRow {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Some(Uuid::now_v7()),
            operation_kind: "content_mutation".to_string(),
            surface_kind: "rest".to_string(),
            status: "failed".to_string(),
            subject_kind: "content_mutation".to_string(),
            subject_id: Some(Uuid::now_v7()),
            parent_async_operation_id: None,
            created_at: Utc::now(),
            completed_at: Some(Utc::now()),
            failure_code: Some("future_code".to_string()),
            failure_disposition: Some("future_value".to_string()),
            result_json: None,
        },
    )
    .expect("an unknown failure disposition must not fail the read");
    assert_eq!(operation.failure_disposition, None);
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
        file_name: Some("document.pdf".to_string()),
        file_type: Some("application/pdf".to_string()),
        file_size: Some(1024),
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
fn dashboard_attention_generated_items_link_to_their_recovery_actions() {
    let state = ops_state(|state| state.failed_document_count = 3);
    let graph = graph_surface(|graph| {
        graph.status = GraphStatus::Partial;
        graph.graph_sparse_document_count = 4;
    });
    let retryable_document = recent_document(|document| document.can_retry = true);
    let retryable_document_id = retryable_document.id;
    let recent = vec![retryable_document];

    let attention = build_attention_items_bounded(&state, &[], &graph, &recent);

    let expected = [
        (
            "failed_documents",
            "Failed documents need review",
            "Open the 3 failed documents, review each error, and retry processing or replace the source file."
                .to_string(),
            "/documents?status=failed".to_string(),
        ),
        (
            "graph_coverage_gap",
            "Graph coverage remains partial",
            "4 ready documents are missing graph coverage. Open them and reprocess the affected files."
                .to_string(),
            "/documents?status=graphgap".to_string(),
        ),
        (
            "retryable_document",
            "A document can be retried",
            "Open this document and retry its processing.".to_string(),
            format!("/documents?documentId={retryable_document_id}"),
        ),
    ];

    for (code, title, detail, route_path) in expected {
        let item = attention
            .iter()
            .find(|item| item.code == code)
            .unwrap_or_else(|| panic!("missing dashboard attention item {code}"));
        assert_eq!(item.title, title, "unexpected title for {code}");
        assert_eq!(item.detail, detail, "unexpected detail for {code}");
        assert_eq!(item.route_path, route_path, "unexpected route for {code}");
    }
}

#[test]
fn dashboard_attention_warning_items_link_to_their_recovery_actions() {
    let expected = [
        (
            "vector_index_gap",
            "warning",
            "Ready documents are missing their vectors",
            "Open the documents whose vectors are missing and reprocess them.",
            "/documents?status=vectorgap",
        ),
        (
            "queue_not_progressing",
            "error",
            "Ingest queue is not progressing",
            "Open the ingest queue and resume, retry, or cancel the jobs that are blocking progress.",
            "/admin/queue",
        ),
        (
            "failed_rebuilds",
            "error",
            "Recent rebuild failed",
            "Open failed documents, review the processing error, and retry or replace the affected file.",
            "/documents?status=failed",
        ),
        (
            "bundle_assembly_failures",
            "error",
            "Context bundle assembly failed",
            "Open the Assistant and retry the failed question. If it fails again, review the library AI configuration.",
            "/assistant",
        ),
        (
            "unknown_warning",
            "info",
            "Operator warning",
            "Open the document list and review the library for failed or stalled processing.",
            "/documents",
        ),
    ];

    for (kind, severity, title, detail, route_path) in expected {
        let item = map_attention_item(&warning(kind, severity));
        assert_eq!(item.code, kind, "unexpected code for {kind}");
        assert_eq!(item.title, title, "unexpected title for {kind}");
        assert_eq!(item.detail, detail, "unexpected detail for {kind}");
        assert_eq!(item.route_path, route_path, "unexpected route for {kind}");
    }
}
