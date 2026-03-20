#![allow(dead_code)]

use chrono::Utc;
use rust_decimal::Decimal;
use uuid::Uuid;

use rustrag_backend::domains::{
    runtime_graph::{
        RuntimeGraphDiagnosticsSnapshot, RuntimeGraphProjectionHealth, RuntimeGraphWriteFailureKind,
    },
    runtime_ingestion::{
        RuntimeCollectionResidualReason, RuntimeCollectionTerminalOutcome,
        RuntimeCollectionTerminalState, RuntimeProviderFailureClass, RuntimeProviderFailureDetail,
    },
    ui_documents::{
        DocumentCollectionWarning, DocumentsWorkspaceDiagnosticChip, DocumentsWorkspaceModel,
        DocumentsWorkspaceNotice, DocumentsWorkspacePrimarySummary,
    },
};

#[must_use]
pub fn sample_terminal_outcome() -> RuntimeCollectionTerminalOutcome {
    RuntimeCollectionTerminalOutcome {
        terminal_state: RuntimeCollectionTerminalState::FailedWithResidualWork,
        residual_reason: Some(RuntimeCollectionResidualReason::ProjectionContention),
        queued_count: 0,
        processing_count: 0,
        pending_graph_count: 2,
        failed_document_count: 3,
        live_total_estimated_cost: Some(Decimal::new(125, 2)),
        settled_total_estimated_cost: Some(Decimal::new(980, 2)),
        missing_total_estimated_cost: Some(Decimal::new(10, 2)),
        currency: Some("USD".to_string()),
        settled_at: None,
        last_transition_at: Utc::now(),
    }
}

#[must_use]
pub fn sample_graph_diagnostics_snapshot() -> RuntimeGraphDiagnosticsSnapshot {
    RuntimeGraphDiagnosticsSnapshot {
        library_id: Uuid::nil(),
        snapshot_at: Utc::now(),
        projection_health: RuntimeGraphProjectionHealth::RetryingContention,
        active_projection_count: 1,
        retrying_projection_count: 1,
        failed_projection_count: 0,
        pending_node_write_count: 8,
        pending_edge_write_count: 21,
        last_projection_failure_kind: Some(RuntimeGraphWriteFailureKind::ProjectionContention),
        last_projection_failure_at: Some(Utc::now()),
        is_runtime_readable: true,
    }
}

#[must_use]
pub fn sample_provider_failure_detail() -> RuntimeProviderFailureDetail {
    RuntimeProviderFailureDetail {
        failure_class: RuntimeProviderFailureClass::UpstreamTimeout,
        provider_kind: Some("openai".to_string()),
        model_name: Some("gpt-5.4-mini".to_string()),
        request_shape_key: Some("graph_extract_chunked_v2".to_string()),
        request_size_bytes: Some(32_000),
        chunk_count: Some(6),
        upstream_status: Some("504".to_string()),
        elapsed_ms: Some(45_000),
        retry_decision: Some("retry_once".to_string()),
        usage_visible: true,
    }
}

#[must_use]
pub fn sample_documents_workspace() -> DocumentsWorkspaceModel {
    DocumentsWorkspaceModel {
        primary_summary: DocumentsWorkspacePrimarySummary {
            progress_label: "193 / 280".to_string(),
            spend_label: "$3.50 settled".to_string(),
            backlog_label: "76 remaining".to_string(),
            terminal_state: "live_in_flight".to_string(),
        },
        secondary_diagnostics: vec![DocumentsWorkspaceDiagnosticChip {
            kind: "graph_health".to_string(),
            label: "Graph".to_string(),
            value: "retrying".to_string(),
        }],
        degraded_notices: vec![DocumentsWorkspaceNotice {
            kind: "projection_contention".to_string(),
            title: "Projection contention".to_string(),
            message: "Projection retries are active.".to_string(),
        }],
        informational_notices: vec![DocumentsWorkspaceNotice {
            kind: "backlog".to_string(),
            title: "Backlog".to_string(),
            message: "Healthy queue remains active.".to_string(),
        }],
        table_document_count: 280,
        active_filter_count: 1,
        highlighted_status: Some("failed".to_string()),
    }
}

#[must_use]
pub fn sample_collection_warning() -> DocumentCollectionWarning {
    DocumentCollectionWarning {
        warning_kind: "projection_contention".to_string(),
        warning_scope: "collection".to_string(),
        warning_message: "Projection retries are active.".to_string(),
        is_degraded: true,
    }
}

#[test]
fn support_builders_produce_non_empty_residual_truth() {
    let terminal = sample_terminal_outcome();
    let diagnostics = sample_graph_diagnostics_snapshot();

    assert_eq!(terminal.terminal_state, RuntimeCollectionTerminalState::FailedWithResidualWork);
    assert_eq!(diagnostics.projection_health, RuntimeGraphProjectionHealth::RetryingContention);
}
