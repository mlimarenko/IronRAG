#![allow(dead_code)]

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use rustrag_backend::{
    domains::mcp_memory::{
        McpAuditActionKind, McpAuditStatus, McpMutationOperationKind, McpMutationReceiptStatus,
        McpReadabilityState,
    },
    infra::repositories::{
        ApiTokenRow, DocumentRow, McpAuditEventRow, McpMutationReceiptRow, ProjectRow,
        RuntimeIngestionRunRow, WorkspaceRow,
    },
};

#[must_use]
pub fn sample_mcp_token(scopes: &[&str]) -> ApiTokenRow {
    ApiTokenRow {
        id: Uuid::now_v7(),
        workspace_id: Some(Uuid::now_v7()),
        token_kind: "workspace".to_string(),
        label: "MCP workspace token".to_string(),
        token_hash: "sha256:sample".to_string(),
        token_preview: Some("rtrg_***abcd".to_string()),
        scope_json: json!(scopes),
        status: "active".to_string(),
        last_used_at: Some(Utc::now()),
        expires_at: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[must_use]
pub fn sample_mcp_workspace() -> WorkspaceRow {
    WorkspaceRow {
        id: Uuid::now_v7(),
        slug: "memory".to_string(),
        name: "Memory Workspace".to_string(),
        status: "active".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[must_use]
pub fn sample_mcp_library(workspace_id: Uuid) -> ProjectRow {
    ProjectRow {
        id: Uuid::now_v7(),
        workspace_id,
        slug: "agents".to_string(),
        name: "Agent Memory".to_string(),
        description: Some("Primary MCP memory library".to_string()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[must_use]
pub fn sample_readable_mcp_document(
    library_id: Uuid,
) -> (DocumentRow, RuntimeIngestionRunRow, String) {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let content =
        "This is a normalized memory document used for MCP read and search tests.".to_string();

    (
        DocumentRow {
            id: document_id,
            project_id: library_id,
            source_id: None,
            external_key: format!("memory-{document_id}"),
            title: Some("Readable MCP Document".to_string()),
            mime_type: Some("text/plain".to_string()),
            checksum: Some("sample-checksum".to_string()),
            current_revision_id: Some(revision_id),
            active_status: "active".to_string(),
            active_mutation_kind: None,
            active_mutation_status: None,
            deleted_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        RuntimeIngestionRunRow {
            id: Uuid::now_v7(),
            project_id: library_id,
            document_id: Some(document_id),
            revision_id: Some(revision_id),
            upload_batch_id: None,
            track_id: format!("track-{}", Uuid::now_v7()),
            file_name: "readable.txt".to_string(),
            file_type: "text".to_string(),
            mime_type: Some("text/plain".to_string()),
            file_size_bytes: Some(content.len() as i64),
            status: "ready".to_string(),
            current_stage: "completed".to_string(),
            progress_percent: Some(100),
            activity_status: "idle".to_string(),
            last_activity_at: Some(Utc::now()),
            last_heartbeat_at: Some(Utc::now()),
            provider_profile_snapshot_json: json!({}),
            latest_error_message: None,
            current_attempt_no: 1,
            attempt_kind: "upload".to_string(),
            queue_started_at: Utc::now(),
            started_at: Some(Utc::now()),
            finished_at: Some(Utc::now()),
            queue_elapsed_ms: Some(10),
            total_elapsed_ms: Some(15),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        content,
    )
}

#[must_use]
pub fn sample_unreadable_mcp_document(
    library_id: Uuid,
    readability_state: McpReadabilityState,
) -> (DocumentRow, RuntimeIngestionRunRow) {
    let document_id = Uuid::now_v7();
    let (status, error_message) = match readability_state {
        McpReadabilityState::Readable => ("ready", None),
        McpReadabilityState::Processing => ("processing", None),
        McpReadabilityState::Failed => ("failed", Some("extraction failed".to_string())),
        McpReadabilityState::Unavailable => ("ready_no_graph", None),
    };

    (
        DocumentRow {
            id: document_id,
            project_id: library_id,
            source_id: None,
            external_key: format!("memory-{document_id}"),
            title: Some("Unreadable MCP Document".to_string()),
            mime_type: Some("application/pdf".to_string()),
            checksum: None,
            current_revision_id: Some(Uuid::now_v7()),
            active_status: "active".to_string(),
            active_mutation_kind: None,
            active_mutation_status: None,
            deleted_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        RuntimeIngestionRunRow {
            id: Uuid::now_v7(),
            project_id: library_id,
            document_id: Some(document_id),
            revision_id: Some(Uuid::now_v7()),
            upload_batch_id: None,
            track_id: format!("track-{}", Uuid::now_v7()),
            file_name: "unreadable.pdf".to_string(),
            file_type: "pdf".to_string(),
            mime_type: Some("application/pdf".to_string()),
            file_size_bytes: Some(4096),
            status: status.to_string(),
            current_stage: "extracting".to_string(),
            progress_percent: Some(65),
            activity_status: "processing".to_string(),
            last_activity_at: Some(Utc::now()),
            last_heartbeat_at: Some(Utc::now()),
            provider_profile_snapshot_json: json!({}),
            latest_error_message: error_message,
            current_attempt_no: 1,
            attempt_kind: "upload".to_string(),
            queue_started_at: Utc::now(),
            started_at: Some(Utc::now()),
            finished_at: None,
            queue_elapsed_ms: Some(20),
            total_elapsed_ms: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
    )
}

#[must_use]
pub fn sample_mcp_mutation_receipt(
    token_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Option<Uuid>,
) -> McpMutationReceiptRow {
    McpMutationReceiptRow {
        id: Uuid::now_v7(),
        token_id,
        workspace_id,
        library_id,
        document_id,
        operation_kind: serde_json::to_string(&McpMutationOperationKind::Upload)
            .unwrap()
            .trim_matches('"')
            .to_string(),
        idempotency_key: "mcp-upload-001".to_string(),
        payload_identity: Some("sha256:payload".to_string()),
        runtime_tracking_id: Some("track-upload-001".to_string()),
        status: serde_json::to_string(&McpMutationReceiptStatus::Accepted)
            .unwrap()
            .trim_matches('"')
            .to_string(),
        failure_kind: None,
        accepted_at: Utc::now(),
        last_status_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[must_use]
pub fn sample_mcp_audit_event(
    token_id: Uuid,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    document_id: Option<Uuid>,
) -> McpAuditEventRow {
    McpAuditEventRow {
        id: Uuid::now_v7(),
        request_id: Uuid::now_v7().to_string(),
        token_id,
        token_kind: "workspace".to_string(),
        action_kind: serde_json::to_string(&McpAuditActionKind::SearchDocuments)
            .unwrap()
            .trim_matches('"')
            .to_string(),
        workspace_id,
        library_id,
        document_id,
        status: serde_json::to_string(&McpAuditStatus::Succeeded)
            .unwrap()
            .trim_matches('"')
            .to_string(),
        error_kind: None,
        metadata_json: json!({ "query": "memory" }),
        created_at: Utc::now(),
    }
}

#[test]
fn sample_mcp_builders_keep_scope_relationships_consistent() {
    let token = sample_mcp_token(&["documents:read"]);
    let workspace = sample_mcp_workspace();
    let library = sample_mcp_library(workspace.id);
    let (document, _, _) = sample_readable_mcp_document(library.id);
    let receipt =
        sample_mcp_mutation_receipt(token.id, workspace.id, library.id, Some(document.id));
    let audit =
        sample_mcp_audit_event(token.id, Some(workspace.id), Some(library.id), Some(document.id));

    assert_eq!(library.workspace_id, workspace.id);
    assert_eq!(document.project_id, library.id);
    assert_eq!(receipt.workspace_id, workspace.id);
    assert_eq!(audit.library_id, Some(library.id));
}
