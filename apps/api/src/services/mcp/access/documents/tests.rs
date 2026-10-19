use super::*;
use super::{
    management::list_documents_matches_status_filter,
    read::{effective_read_start_offset, merge_visual_description_into_content},
    search::{chunk_search_query_variants, search_document_hit_order},
};
use crate::{
    domains::content::{DocumentReadinessSummary, RuntimeDocumentActivityStatus},
    mcp_types::{McpDocumentHit, McpReadMode, McpReadabilityState},
    shared::versioning::dotted_version_key,
};
use chrono::Utc;
use ironrag_contracts::documents::DocumentReadiness;
use uuid::Uuid;

#[test]
fn image_visual_description_appends_to_existing_text_once() {
    let merged = merge_visual_description_into_content(
        Some("Visible text from OCR"),
        Some("A restaurant sign with menu items."),
    );

    assert!(merged.contains("Visible text from OCR"));
    assert!(merged.contains("## Source Image Description"));
    assert!(merged.contains("A restaurant sign with menu items."));
}

#[test]
fn image_visual_description_is_not_duplicated_when_already_present() {
    let merged = merge_visual_description_into_content(
        Some("Visible text\n\n## Source Image Description\nA restaurant sign with menu items."),
        Some("A restaurant sign with menu items."),
    );

    assert_eq!(
        merged,
        "Visible text\n\n## Source Image Description\nA restaurant sign with menu items."
    );
}

#[test]
fn readability_state_treats_graph_ready_as_readable() {
    assert_eq!(
        readability_state_from_kind(DocumentReadiness::GraphReady),
        McpReadabilityState::Readable
    );
    assert_eq!(
        readability_state_from_kind(DocumentReadiness::GraphSparse),
        McpReadabilityState::Readable
    );
    assert_eq!(
        readability_state_from_kind(DocumentReadiness::Readable),
        McpReadabilityState::Readable
    );
}

#[test]
fn list_documents_readable_filter_includes_graph_ready_and_sparse() {
    let graph_ready = DocumentReadinessSummary {
        document_id: Uuid::nil(),
        active_revision_id: None,
        readiness_kind: DocumentReadiness::GraphReady,
        activity_status: RuntimeDocumentActivityStatus::Ready,
        stalled_reason: None,
        preparation_state: "ready".to_string(),
        graph_coverage_kind: "graph_ready".to_string(),
        typed_fact_coverage: None,
        last_mutation_id: None,
        last_job_stage: None,
        updated_at: Utc::now(),
    };
    let graph_sparse = DocumentReadinessSummary {
        document_id: Uuid::nil(),
        active_revision_id: None,
        readiness_kind: DocumentReadiness::GraphSparse,
        activity_status: RuntimeDocumentActivityStatus::Ready,
        stalled_reason: None,
        preparation_state: "ready".to_string(),
        graph_coverage_kind: "graph_sparse".to_string(),
        typed_fact_coverage: None,
        last_mutation_id: None,
        last_job_stage: None,
        updated_at: Utc::now(),
    };
    let readable = DocumentReadinessSummary {
        document_id: Uuid::nil(),
        active_revision_id: None,
        readiness_kind: DocumentReadiness::Readable,
        activity_status: RuntimeDocumentActivityStatus::Ready,
        stalled_reason: None,
        preparation_state: "ready".to_string(),
        graph_coverage_kind: "none".to_string(),
        typed_fact_coverage: None,
        last_mutation_id: None,
        last_job_stage: None,
        updated_at: Utc::now(),
    };

    assert!(list_documents_matches_status_filter(Some(&graph_ready), Some("readable")));
    assert!(list_documents_matches_status_filter(Some(&graph_sparse), Some("readable")));
    assert!(list_documents_matches_status_filter(Some(&readable), Some("readable")));
    assert!(!list_documents_matches_status_filter(Some(&graph_ready), Some("failed")));
    assert!(!list_documents_matches_status_filter(None, Some("readable")));
}

#[test]
fn search_document_hits_rank_readable_before_failed_even_with_lower_score() {
    let mut hits = [
        McpDocumentHit {
            document_id: Uuid::from_u128(2),
            library_id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            document_title: "failed".to_string(),
            latest_revision_id: None,
            score: 1000.0,
            excerpt: None,
            excerpt_start_offset: None,
            excerpt_end_offset: None,
            suggested_start_offset: None,
            readability_state: McpReadabilityState::Failed,
            readiness_kind: "failed".to_string(),
            graph_coverage_kind: "failed".to_string(),
            status_reason: None,
            chunk_references: Vec::new(),
            technical_fact_references: Vec::new(),
            entity_references: Vec::new(),
            relation_references: Vec::new(),
            evidence_references: Vec::new(),
        },
        McpDocumentHit {
            document_id: Uuid::from_u128(1),
            library_id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            document_title: "readable".to_string(),
            latest_revision_id: None,
            score: 10.0,
            excerpt: None,
            excerpt_start_offset: None,
            excerpt_end_offset: None,
            suggested_start_offset: None,
            readability_state: McpReadabilityState::Readable,
            readiness_kind: "graph_ready".to_string(),
            graph_coverage_kind: "graph_ready".to_string(),
            status_reason: None,
            chunk_references: Vec::new(),
            technical_fact_references: Vec::new(),
            entity_references: Vec::new(),
            relation_references: Vec::new(),
            evidence_references: Vec::new(),
        },
    ];

    hits.sort_by(search_document_hit_order);

    assert_eq!(hits[0].document_title, "readable");
    assert_eq!(hits[1].document_title, "failed");
}

#[test]
fn full_read_starts_at_zero_when_document_fits_window() {
    assert_eq!(effective_read_start_offset(&McpReadMode::Full, 900, 1000, 1200), 0);
}

#[test]
fn full_read_honors_offset_when_document_exceeds_window() {
    assert_eq!(effective_read_start_offset(&McpReadMode::Full, 900, 3000, 1200), 900);
}

#[test]
fn excerpt_read_honors_offset_even_when_document_fits_window() {
    assert_eq!(effective_read_start_offset(&McpReadMode::Excerpt, 900, 1000, 1200), 900);
}

#[test]
fn chunk_search_query_variants_preserve_only_the_normalized_query() {
    assert_eq!(
        chunk_search_query_variants("  exact service releases  "),
        vec!["exact service releases"]
    );
    assert!(chunk_search_query_variants("   ").is_empty());
}

#[test]
fn search_hit_order_uses_dotted_version_key() {
    assert_eq!(dotted_version_key("Alpha Suite Version 2.10.3 Notes"), Some([2, 10, 3, 0]));
}
