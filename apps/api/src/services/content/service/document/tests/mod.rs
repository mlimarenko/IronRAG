use super::{
    DeletedDocumentGraphCleanup, apply_document_hint_to_active_revision, build_document_list_entry,
    next_document_list_cursor,
};
use crate::{
    domains::content::{
        ContentDocument, ContentDocumentPipelineState, ContentDocumentSummary, ContentRevision,
    },
    infra::{
        knowledge_rows::{KnowledgeDocumentRow, KnowledgeRevisionRow},
        repositories::{
            RuntimeGraphEvidenceTargetRow,
            content_document_list_repository::{ContentDocumentListRow, DocumentListSortColumn},
        },
    },
};
use chrono::Utc;
use ironrag_contracts::documents::{DocumentReadiness, DocumentStatus};
use rust_decimal::Decimal;
use uuid::Uuid;

fn list_row(job_queue_state: Option<&str>, mutation_state: Option<&str>) -> ContentDocumentListRow {
    let now = Utc::now();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    ContentDocumentListRow {
        id: document_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        external_key: "docs/sample.md".to_string(),
        projection_file_name: Some("source.pdf".to_string()),
        document_state: "active".to_string(),
        created_at: now,
        deleted_at: None,
        active_revision_id: Some(revision_id),
        readable_revision_id: Some(revision_id),
        revision_title: Some("Sample document".to_string()),
        revision_mime_type: Some("text/markdown".to_string()),
        revision_byte_size: Some(128),
        revision_source_uri: None,
        revision_document_hint: None,
        revision_content_source_kind: Some("upload".to_string()),
        revision_storage_key: None,
        mutation_id: mutation_state.map(|_| Uuid::now_v7()),
        mutation_state: mutation_state.map(str::to_string),
        mutation_failure_code: None,
        mutation_requested_at: mutation_state.map(|_| now),
        job_id: job_queue_state.map(|_| Uuid::now_v7()),
        job_queue_state: job_queue_state.map(str::to_string),
        job_queued_at: job_queue_state.map(|_| now),
        job_completed_at: job_queue_state.map(|_| now),
        attempt_current_stage: None,
        attempt_started_at: None,
        attempt_finished_at: None,
        attempt_failure_code: None,
        attempt_retryable: None,
        attempt_heartbeat_at: None,
        attempt_failure_message: None,
        attempt_progress_percent: None,
        finished_at: None,
        pipeline_time_ms: None,
        cost_total: Some(Decimal::ZERO),
        cost_currency_code: Some("USD".to_string()),
        billing_rollup_dirty: false,
        billing_rollup_terminal_error_code: None,
    }
}

fn revision(
    row: &ContentDocumentListRow,
    text_state: &str,
    vector_state: &str,
    graph_state: &str,
) -> KnowledgeRevisionRow {
    let now = Utc::now();
    let revision_id =
        row.readable_revision_id.or(row.active_revision_id).unwrap_or_else(Uuid::now_v7);
    KnowledgeRevisionRow {
        revision_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        document_id: row.id,
        revision_number: 1,
        revision_state: "active".to_string(),
        revision_kind: "upload".to_string(),
        storage_ref: None,
        source_uri: None,
        document_hint: None,
        mime_type: "text/markdown".to_string(),
        checksum: "checksum".to_string(),
        title: Some("Sample document".to_string()),
        byte_size: 128,
        normalized_text: Some("sample content".to_string()),
        text_checksum: Some("text-checksum".to_string()),
        image_checksum: None,
        text_state: text_state.to_string(),
        vector_state: vector_state.to_string(),
        graph_state: graph_state.to_string(),
        text_readable_at: Some(now),
        vector_ready_at: None,
        graph_ready_at: None,
        superseded_by_revision_id: None,
        created_at: now,
    }
}

fn knowledge_document(row: &ContentDocumentListRow) -> KnowledgeDocumentRow {
    let now = Utc::now();
    KnowledgeDocumentRow {
        document_id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        external_key: row.external_key.clone(),
        file_name: Some("source.pdf".to_string()),
        title: None,
        source_uri: None,
        document_hint: None,
        document_state: row.document_state.clone(),
        active_revision_id: row.active_revision_id,
        readable_revision_id: row.readable_revision_id,
        latest_revision_no: Some(1),
        parent_document_id: None,
        document_role: "primary".to_string(),
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn document_summary_with_active_revision(revision_id: Uuid) -> ContentDocumentSummary {
    let now = Utc::now();
    let document_id = Uuid::now_v7();
    ContentDocumentSummary {
        document: ContentDocument {
            id: document_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            external_key: "docs/source.pdf".to_string(),
            document_state: "active".to_string(),
            created_at: now,
        },
        file_name: Some("source.pdf".to_string()),
        head: None,
        active_revision: Some(ContentRevision {
            id: revision_id,
            document_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_number: 1,
            parent_revision_id: None,
            content_source_kind: "upload".to_string(),
            checksum: "checksum".to_string(),
            mime_type: "application/pdf".to_string(),
            byte_size: 128,
            title: Some("source.pdf".to_string()),
            language_code: None,
            source_uri: Some("upload://source.pdf".to_string()),
            document_hint: None,
            storage_key: Some("content/source.pdf".to_string()),
            created_by_principal_id: None,
            created_at: now,
        }),
        source_access: None,
        readiness: None,
        readiness_summary: None,
        prepared_revision: None,
        web_page_provenance: None,
        pipeline: ContentDocumentPipelineState { latest_mutation: None, latest_job: None },
    }
}

#[test]
fn effective_summary_revision_requires_exact_scoped_publication() {
    let service = crate::services::content::service::ContentService::new();
    let row = list_row(None, None);
    let candidate = revision(&row, "text_readable", "vector_ready", "ready");
    let mut document = knowledge_document(&row);
    let revisions = std::collections::HashMap::from([(candidate.revision_id, candidate.clone())]);
    document.readable_revision_id = None;
    assert!(
        service.resolve_effective_readiness_row(&document, Some(&candidate), &revisions).is_none()
    );
    document.readable_revision_id = Some(Uuid::now_v7());
    assert!(
        service.resolve_effective_readiness_row(&document, Some(&candidate), &revisions).is_none()
    );
    document.readable_revision_id = Some(candidate.revision_id);
    assert!(
        service.resolve_effective_readiness_row(&document, Some(&candidate), &revisions).is_some()
    );
    document.library_id = Uuid::now_v7();
    assert!(
        service.resolve_effective_readiness_row(&document, Some(&candidate), &revisions).is_none()
    );
}

#[test]
fn document_hint_patch_detail_roundtrip_uses_updated_active_revision_hint() {
    let revision_id = Uuid::now_v7();
    let mut summary = document_summary_with_active_revision(revision_id);

    apply_document_hint_to_active_revision(
        &mut summary,
        revision_id,
        Some("customer-contract.pdf".to_string()),
    );

    assert_eq!(
        summary.active_revision.and_then(|revision| revision.document_hint),
        Some("customer-contract.pdf".to_string())
    );
}

#[test]
fn list_entry_keeps_canceled_job_with_readable_revision_ready() {
    let row = list_row(Some("canceled"), None);
    let revision = revision(&row, "text_readable", "pending", "pending");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert_eq!(entry.status, DocumentStatus::Ready);
    assert_eq!(entry.readiness, DocumentReadiness::Readable);
}

#[test]
fn list_entry_rejects_mixed_currency_cost_sentinel() {
    let mut row = list_row(None, None);
    row.cost_total = None;
    row.cost_currency_code = None;
    let revision = revision(&row, "text_readable", "ready", "ready");

    let error = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect_err("mixed-currency totals must fail closed");

    assert!(error.to_string().contains("multiple currencies"));
}

#[test]
fn list_entry_keeps_metadata_and_withholds_cost_from_dirty_scope_snapshot() {
    let mut row = list_row(None, None);
    row.billing_rollup_dirty = true;
    row.cost_total = Some(Decimal::new(12345, 2));
    let revision = revision(&row, "text_readable", "ready", "ready");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("metadata remains available while the billing rollup reconciles");

    assert_eq!(entry.id, row.id);
    assert_eq!(entry.external_key, row.external_key);
    assert_eq!(entry.file_name, row.projection_file_name);
    assert_eq!(entry.status, DocumentStatus::Ready);
    assert_eq!(entry.readiness, DocumentReadiness::GraphReady);
    assert_eq!(entry.cost_total, None);
    assert_eq!(entry.cost_currency_code, None);
}

#[test]
fn list_entry_rejects_terminal_mixed_currency_even_when_dirty() {
    let mut row = list_row(None, None);
    row.billing_rollup_dirty = true;
    row.billing_rollup_terminal_error_code = Some("mixed_currency".to_string());
    let revision = revision(&row, "text_readable", "ready", "ready");

    let error = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect_err("terminal mixed-currency costs must not become reconciling metadata");

    assert!(matches!(error, super::ApiError::Conflict(_)));
}

#[test]
fn list_entry_keeps_graph_failure_readable_for_search() {
    let row = list_row(None, None);
    let revision = revision(&row, "text_readable", "ready", "failed");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert_eq!(entry.status, DocumentStatus::Ready);
    assert_eq!(entry.readiness, DocumentReadiness::Readable);
}

#[test]
fn list_entry_uses_attempt_processing_progress() {
    let mut row = list_row(Some("leased"), Some("running"));
    row.attempt_current_stage = Some("extract_graph".to_string());
    row.attempt_progress_percent = Some(76);
    let revision = revision(&row, "text_readable", "ready", "ready");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert_eq!(entry.status, DocumentStatus::Processing);
    assert_eq!(entry.progress_percent, Some(76));
}

#[test]
fn list_entry_marks_ready_documents_as_complete() {
    let row = list_row(None, None);
    let revision = revision(&row, "text_readable", "ready", "ready");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert_eq!(entry.status, DocumentStatus::Ready);
    assert_eq!(entry.progress_percent, Some(100));
}

#[test]
fn list_entry_surfaces_failed_mutation_message() {
    let mut row = list_row(None, Some("failed"));
    row.mutation_failure_code = Some("parser_failed".to_string());
    row.attempt_failure_message = Some("Parser failed on page 2".to_string());
    let revision = revision(&row, "pending", "pending", "pending");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert_eq!(entry.status, DocumentStatus::Failed);
    assert_eq!(entry.failure_code.as_deref(), Some("parser_failed"));
    assert_eq!(entry.failure_message.as_deref(), Some("Parser failed on page 2"));
}

#[test]
fn unpublished_document_has_no_display_name_or_name_cursor_value() {
    let mut row = list_row(None, None);
    row.projection_file_name = None;
    row.readable_revision_id = None;
    row.active_revision_id = None;
    row.revision_mime_type = None;
    row.revision_byte_size = None;
    row.revision_title = None;
    row.revision_content_source_kind = None;
    let stale_revision = revision(&row, "text_readable", "ready", "ready");
    let entry =
        build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&stale_revision))
            .expect("unpublished operational row must map");
    assert_eq!(entry.file_name, None);
    assert_eq!(entry.file_type, None);
    assert_eq!(entry.file_size, None);
    assert_eq!(entry.status, DocumentStatus::Empty);
    assert_eq!(entry.readiness, DocumentReadiness::Empty);
    assert!(entry.source_access.is_none());
    let cursor =
        next_document_list_cursor(&[row], &[entry], DocumentListSortColumn::ExternalKey, true)
            .expect("null name still has a continuation");
    assert_eq!(cursor.text_value, None);
    assert!(cursor.value_is_null);
}

#[test]
fn list_entry_keeps_terminal_failure_progress_within_display_range() {
    let mut row = list_row(Some("failed"), None);
    row.attempt_progress_percent = Some(145);
    let revision = revision(&row, "pending", "pending", "pending");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert_eq!(entry.status, DocumentStatus::Failed);
    assert_eq!(entry.progress_percent, Some(99));
}

#[test]
fn list_entry_marks_unreadable_canceled_document_as_canceled() {
    let row = list_row(Some("canceled"), Some("canceled"));
    let revision = revision(&row, "pending", "pending", "pending");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert_eq!(entry.status, DocumentStatus::Canceled);
    assert_eq!(entry.readiness, DocumentReadiness::Failed);
    assert_eq!(entry.progress_percent, Some(0));
}

#[test]
fn list_entry_marks_completed_unreadable_job_as_failed() {
    let row = list_row(Some("completed"), None);
    let revision = revision(&row, "pending", "pending", "pending");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert_eq!(entry.status, DocumentStatus::Failed);
}

#[test]
fn list_entry_marks_document_with_no_mutation_no_job_and_no_readable_revision_as_empty() {
    let row = list_row(None, None);
    let revision = revision(&row, "pending", "pending", "pending");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert_eq!(entry.status, DocumentStatus::Empty);
}

#[test]
fn list_entry_marks_paused_job_with_running_mutation_as_processing() {
    let row = list_row(Some("paused"), Some("running"));
    let revision = revision(&row, "pending", "pending", "pending");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert_eq!(entry.status, DocumentStatus::Processing);
}

#[test]
fn list_entry_marks_paused_job_without_active_mutation_as_queued() {
    let row = list_row(Some("paused"), None);
    let revision = revision(&row, "pending", "pending", "pending");

    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert_eq!(entry.status, DocumentStatus::Queued);
}

#[test]
fn list_entry_does_not_reconstruct_missing_publication_metadata_from_knowledge() {
    let mut row = list_row(None, None);
    row.revision_mime_type = None;
    row.revision_byte_size = None;
    row.revision_content_source_kind = None;
    row.revision_source_uri = None;
    let mut effective_revision = revision(&row, "text_readable", "ready", "ready");
    effective_revision.mime_type = "text/plain".to_string();
    effective_revision.byte_size = 512;
    effective_revision.revision_kind = "web_page".to_string();
    effective_revision.source_uri = Some("https://example.invalid/reference".to_string());

    let entry =
        build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&effective_revision))
            .expect("single-currency list row should map");

    assert_eq!(entry.file_type, None);
    assert_eq!(entry.file_size, None);
    assert_eq!(entry.source_kind, None);
    assert_eq!(entry.source_uri, None);
    assert!(entry.source_access.is_none());
}

#[test]
fn graph_cleanup_deduplicates_known_targets_and_ignores_unknown_kinds() {
    let node_id = Uuid::now_v7();
    let edge_id = Uuid::now_v7();
    let cleanup = DeletedDocumentGraphCleanup::from_targets(vec![
        RuntimeGraphEvidenceTargetRow { target_kind: "node".to_string(), target_id: node_id },
        RuntimeGraphEvidenceTargetRow { target_kind: "edge".to_string(), target_id: edge_id },
        RuntimeGraphEvidenceTargetRow { target_kind: "node".to_string(), target_id: node_id },
        RuntimeGraphEvidenceTargetRow {
            target_kind: "unrecognized".to_string(),
            target_id: Uuid::now_v7(),
        },
    ]);

    assert_eq!(cleanup.node_ids, vec![node_id]);
    assert_eq!(cleanup.edge_ids, vec![edge_id]);
    assert!(cleanup.requires_graph_convergence());
    assert!(!DeletedDocumentGraphCleanup::default().requires_graph_convergence());
}

#[test]
fn document_list_cursor_preserves_sort_specific_last_row_values() {
    let mut row = list_row(None, None);
    row.external_key = "Docs/Mixed-Case.md".to_string();
    row.projection_file_name = Some("Reports/Visible-Name.PDF".to_string());
    row.revision_mime_type = None;
    row.revision_byte_size = None;
    let revision = revision(&row, "text_readable", "ready", "ready");
    let entry = build_document_list_entry(&row, Some(&knowledge_document(&row)), Some(&revision))
        .expect("single-currency list row should map");

    assert!(
        next_document_list_cursor(
            std::slice::from_ref(&row),
            std::slice::from_ref(&entry),
            DocumentListSortColumn::CreatedAt,
            false,
        )
        .is_none()
    );
    assert!(
        next_document_list_cursor(&[], &[], DocumentListSortColumn::ExternalKey, true,).is_none()
    );

    let external_key_cursor = next_document_list_cursor(
        std::slice::from_ref(&row),
        std::slice::from_ref(&entry),
        DocumentListSortColumn::ExternalKey,
        true,
    )
    .expect("a non-empty page with more rows has a cursor");
    assert_eq!(external_key_cursor.text_value.as_deref(), Some("reports/visible-name.pdf"));
    assert_eq!(external_key_cursor.number_value, None);
    assert!(!external_key_cursor.value_is_null);

    let mime_type_cursor = next_document_list_cursor(
        std::slice::from_ref(&row),
        std::slice::from_ref(&entry),
        DocumentListSortColumn::MimeType,
        true,
    )
    .expect("a non-empty page with more rows has a cursor");
    assert_eq!(mime_type_cursor.text_value, None);
    assert!(mime_type_cursor.value_is_null);

    let byte_size_cursor = next_document_list_cursor(
        std::slice::from_ref(&row),
        std::slice::from_ref(&entry),
        DocumentListSortColumn::ByteSize,
        true,
    )
    .expect("a non-empty page with more rows has a cursor");
    assert_eq!(byte_size_cursor.number_value, None);
    assert!(byte_size_cursor.value_is_null);

    let status_cursor = next_document_list_cursor(
        std::slice::from_ref(&row),
        std::slice::from_ref(&entry),
        DocumentListSortColumn::DerivedStatus,
        true,
    )
    .expect("a non-empty page with more rows has a cursor");
    assert_eq!(status_cursor.text_value.as_deref(), Some("ready"));
}
