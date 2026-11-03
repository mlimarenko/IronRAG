use super::*;
use super::{
    management::list_documents_matches_status_filter,
    read::{canonical_read_window, effective_read_start_offset},
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
fn full_read_window_preserves_exact_canonical_whitespace_and_unicode_offsets() {
    let canonical = "\n  Условие: подтверждение обязательно.\n";
    let window = canonical_read_window(canonical, &McpReadMode::Full, 5, 100);
    assert_eq!(window.content, canonical);
    assert_eq!(window.start, 0);
    assert_eq!(window.end, canonical.chars().count());
    assert_eq!(window.total, canonical.chars().count());
    assert!(!window.has_more);
}

#[test]
fn bounded_read_window_addresses_only_the_selected_canonical_characters() {
    let canonical = "\n  Условие: подтверждение обязательно.\n";
    let window = canonical_read_window(canonical, &McpReadMode::Full, 3, 7);
    assert_eq!(window.content, "Условие");
    assert_eq!((window.start, window.end, window.total), (3, 10, canonical.chars().count()));
    assert!(window.has_more);
    let empty = canonical_read_window("", &McpReadMode::Full, 0, 100);
    assert!(empty.content.is_empty());
    assert_eq!((empty.start, empty.end, empty.total), (0, 0, 0));
    assert!(!empty.has_more);
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
