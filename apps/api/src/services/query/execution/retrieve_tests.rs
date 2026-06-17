use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use super::super::{
    graph_target_entity_profiles, is_table_analytics_chunk,
    merge_canonical_table_aggregation_chunks, requested_initial_table_row_count,
    source_slice_context_top_k,
};
use super::{
    DOCUMENT_IDENTITY_SCORE_FLOOR, RuntimeChunkScoreKind, apply_graph_evidence_texts_to_chunks,
    build_lexical_queries, canonical_document_revision_id, chunk_answer_source_text,
    combine_chunk_retrieval_lanes, combine_lexical_query_results,
    combine_query_ir_focus_search_results, command_dense_excerpt_for,
    document_identity_chunk_score, document_identity_focus_terms, entity_bio_chunk_score,
    graph_evidence_chunk_hits_from_rows, graph_evidence_chunk_score, graph_evidence_context_line,
    graph_evidence_source_document_ids, graph_evidence_source_document_ids_from_scored_targets,
    graph_evidence_source_document_ids_with_priority, graph_evidence_targets,
    graph_evidence_targets_for_query, graph_evidence_text_search_document_scope,
    is_answer_driving_search_chunk_row, latest_version_documents, linked_anchor_focus_queries,
    linked_anchor_hydration_target_filter, map_chunk_hit, merge_chunks, merge_entity_bio_chunks,
    merge_graph_evidence_chunks, merge_query_ir_focus_chunks, query_ir_focus_chunk_score,
    query_ir_focus_search_queries, query_ir_lexical_focus_queries,
    query_ir_promotes_graph_evidence, rank_graph_evidence_context_rows,
    retain_canonical_document_head_chunks, retain_entity_bio_candidates, retain_scoped_documents,
    truncate_bundle,
};
use crate::domains::query_ir::{
    ComparisonSpec, DocumentHint, EntityMention, EntityRole, LiteralKind, LiteralSpan, QueryAct,
    QueryIR, QueryLanguage, QueryScope, SourceSliceDirection, SourceSliceFilter, SourceSliceSpec,
};
use crate::infra::{
    arangodb::document_store::{KnowledgeChunkRow, KnowledgeDocumentRow},
    repositories::{RuntimeGraphEvidenceRow, RuntimeGraphQueryNodeRow},
};
use crate::services::knowledge::runtime_read::ActiveRuntimeGraphProjection;
use crate::services::query::{
    execution::{
        QueryGraphIndex, RetrievalBundle, RuntimeMatchedChunk, RuntimeMatchedEntity,
        RuntimeMatchedRelationship, normalized_document_target_candidates,
        should_skip_vector_search,
    },
    latest_versions::{
        compare_version_desc, extract_semver_like_version, latest_version_chunk_score,
        latest_version_context_top_k, latest_version_family_key, latest_version_scope_terms,
        query_requests_latest_versions, requested_latest_version_count,
        text_has_release_version_marker,
    },
    planner::{QueryIntentProfile, RuntimeQueryPlan, build_query_plan},
};

#[test]
fn command_dense_excerpt_preserves_multiline_shell_procedure() {
    let excerpt = command_dense_excerpt_for(
        concat!(
            "Procedure\n",
            "1. sudo su\n",
            "2. sample-transfer https://example.invalid/alpha/runner.sh -o /tmp/sample-runner.sh\n",
            "3. sample-prepare +x /tmp/sample-runner.sh\n",
            "4. /tmp/sample-runner.sh\n"
        ),
        4_000,
    )
    .expect("command-dense excerpt");

    assert!(excerpt.contains("sudo su\n"));
    assert!(excerpt.contains(
        "sample-transfer https://example.invalid/alpha/runner.sh -o /tmp/sample-runner.sh\n"
    ));
    assert!(excerpt.contains("sample-prepare +x /tmp/sample-runner.sh\n"));
    assert!(excerpt.contains("/tmp/sample-runner.sh"));
}

fn release_query_ir(count: Option<&str>, entity: Option<&str>) -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::MultiDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["version".to_string()],
        target_entities: entity
            .map(|label| {
                vec![EntityMention { label: label.to_string(), role: EntityRole::Subject }]
            })
            .unwrap_or_default(),
        literal_constraints: count
            .map(|text| {
                vec![LiteralSpan { text: text.to_string(), kind: LiteralKind::NumericCode }]
            })
            .unwrap_or_default(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

fn release_query_ir_with_source_slice_count(count: Option<u16>) -> QueryIR {
    release_query_ir_with_source_slice(SourceSliceDirection::Tail, count)
}

fn release_query_ir_with_source_slice(
    direction: SourceSliceDirection,
    count: Option<u16>,
) -> QueryIR {
    let mut ir = release_query_ir(None, None);
    ir.source_slice =
        Some(SourceSliceSpec { direction, count, filter: SourceSliceFilter::ReleaseMarker });
    ir
}

fn target_entities_query_ir(target_labels: &[&str]) -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["event".to_string()],
        target_entities: target_labels
            .iter()
            .map(|label| EntityMention { label: (*label).to_string(), role: EntityRole::Subject })
            .collect(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

fn exact_error_code_query_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["error_code".to_string()],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

fn transport_inventory_query_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["port".to_string(), "protocol".to_string(), "connection".to_string()],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

#[test]
fn document_identity_focus_terms_include_structural_prefixes() {
    let mut query_ir = target_entities_query_ir(&["integrated connector", "connection variants"]);
    query_ir.document_focus = Some(DocumentHint { hint: "Sample Subject setup".to_string() });

    let terms = document_identity_focus_terms(&["setup".to_string()], Some(&query_ir));

    assert!(terms.contains(&"Sample Subject setup".to_string()));
    assert!(terms.contains(&"integrated connector".to_string()));
    assert!(terms.contains(&"integr".to_string()));
    assert!(terms.contains(&"connec".to_string()));
}

#[test]
fn linked_anchor_focus_queries_follow_relevant_source_links() {
    let source_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("paragraph".to_string()),
        document_label: "Sample Subject overview".to_string(),
        excerpt: "See [Integrated connector catalog](/connectors) for details.".to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text:
            "See [Integrated connector catalog](/connectors) for details. Also [Support](/support)."
                .to_string(),
    };
    let queries = linked_anchor_focus_queries(
        "Which integration connectors are available?",
        None,
        &[],
        &[source_chunk],
    );

    assert_eq!(
        queries,
        vec![
            "Integrated connector catalog".to_string(),
            "integra connector catalog".to_string(),
            "integrated connect catalog".to_string(),
        ]
    );
}

#[test]
fn linked_anchor_focus_queries_add_numeric_free_prefix_variants() {
    let source_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("paragraph".to_string()),
        document_label: "Sample Subject overview".to_string(),
        excerpt: "See [15 integrated connectors](/connectors) for details.".to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: "See [15 integrated connectors](/connectors) for details.".to_string(),
    };
    let queries = linked_anchor_focus_queries(
        "Which integration connectors are available?",
        None,
        &[],
        &[source_chunk],
    );

    assert!(queries.contains(&"15 integrated connectors".to_string()));
    assert!(queries.contains(&"integrated connectors".to_string()));
    assert!(queries.contains(&"integra connectors".to_string()));
}

#[test]
fn linked_anchor_hydration_is_cross_document() {
    assert!(
        linked_anchor_hydration_target_filter().is_empty(),
        "linked markdown anchors point at related documents, so hydration must not inherit the source document scope filter"
    );
}

#[test]
fn source_profile_rows_are_not_answer_driving_search_hits() {
    let document = sample_document_row("records.jsonl", "Records");
    let row = knowledge_chunk_row(
        &document,
        0,
        Some("source_profile"),
        "[source_profile source_format=record_jsonl unit_count=42]",
    );
    let document_index = HashMap::from([(document.document_id, document)]);

    assert!(!is_answer_driving_search_chunk_row(&row));
    assert!(
        map_chunk_hit(row, 1.0, &document_index, &[]).is_some(),
        "specialized source-context paths still map source profiles explicitly"
    );
}

#[test]
fn ordinary_content_rows_remain_answer_driving_search_hits() {
    let document = sample_document_row("records.jsonl", "Records");
    let row = knowledge_chunk_row(
        &document,
        1,
        Some("source_unit"),
        "[unit_id=alpha] service-a listens on port 8080",
    );

    assert!(is_answer_driving_search_chunk_row(&row));
}

#[test]
fn table_row_answer_context_uses_semantic_row_text() {
    let chunk = KnowledgeChunkRow {
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
        chunk_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 1,
        chunk_kind: Some("table_row".to_string()),
        content_text: "| 1 |".to_string(),
        normalized_text: "Sheet: test1 | Row 1 | col_1: 1".to_string(),
        span_start: Some(0),
        span_end: Some(5),
        token_count: Some(4),
        support_block_ids: Vec::new(),
        section_path: vec!["test1".to_string()],
        heading_trail: vec!["test1".to_string()],
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: Some(1.0),

        window_text: None,

        raptor_level: None,
        occurred_at: None,
        occurred_until: None,
    };

    assert_eq!(chunk_answer_source_text(&chunk), "Sheet: test1 | Row 1 | col_1: 1");
}

#[test]
fn metadata_summary_answer_context_uses_normalized_text_when_content_is_empty() {
    let chunk = KnowledgeChunkRow {
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
        chunk_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 1,
        chunk_kind: Some("metadata_block".to_string()),
        content_text: String::new(),
        normalized_text: "Table Summary | Sheet: products | Column: Stock | Value Kind: numeric | Value Shape: label | Aggregation Priority: 3 | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 3 | Average: 20 | Min: 10 | Max: 30".to_string(),
        span_start: None,
        span_end: None,
        token_count: Some(16),
        support_block_ids: Vec::new(),
        section_path: vec!["products".to_string()],
        heading_trail: vec!["products".to_string()],
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: Some(1.0),

        window_text: None,

        raptor_level: None,
        occurred_at: None,
        occurred_until: None,
    };

    assert!(chunk_answer_source_text(&chunk).starts_with("Table Summary |"));
}

#[test]
fn source_unit_answer_context_prefers_full_record_over_window_excerpt() {
    let chunk = KnowledgeChunkRow {
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
        chunk_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 1,
        chunk_kind: Some("source_unit".to_string()),
        content_text: "[unit_ordinal=0] fields: resources.alpha.events.type=array; resources.alpha.secret.description=HMAC-SHA256 signing secret".to_string(),
        normalized_text: "[unit_ordinal=0] fields: resources.alpha.events.type=array; resources.alpha.secret.description=HMAC-SHA256 signing secret".to_string(),
        span_start: None,
        span_end: None,
        token_count: Some(16),
        support_block_ids: Vec::new(),
        section_path: vec!["resources".to_string()],
        heading_trail: vec!["resources".to_string()],
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: Some(1.0),
        window_text: Some("[unit_ordinal=0] fields: resources.alpha.events.type=array".to_string()),
        raptor_level: None,
        occurred_at: None,
        occurred_until: None,
    };

    let source_text = chunk_answer_source_text(&chunk);

    assert!(source_text.contains("events.type=array"));
    assert!(source_text.contains("HMAC-SHA256"));
}

#[test]
fn non_table_chunk_answer_context_preserves_raw_content_text() {
    let chunk = KnowledgeChunkRow {
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
        chunk_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("heading".to_string()),
        content_text: "test1".to_string(),
        normalized_text: "test1".to_string(),
        span_start: Some(0),
        span_end: Some(5),
        token_count: Some(1),
        support_block_ids: Vec::new(),
        section_path: vec!["test1".to_string()],
        heading_trail: vec!["test1".to_string()],
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: Some(1.0),

        window_text: None,

        raptor_level: None,
        occurred_at: None,
        occurred_until: None,
    };

    assert_eq!(chunk_answer_source_text(&chunk), "test1");
}

#[test]
fn document_target_candidates_include_extensionless_stem() {
    let document = sample_document_row("sample-heavy-1.xls", "sample-heavy-1.xls");

    let candidates = normalized_document_target_candidates(
        [
            document.file_name.as_deref(),
            document.title.as_deref(),
            Some(document.external_key.as_str()),
        ]
        .into_iter()
        .flatten(),
    );

    assert!(candidates.contains(&"sample-heavy-1.xls".to_string()));
    assert!(candidates.contains(&"sample-heavy-1".to_string()));
}

#[test]
fn requested_initial_table_row_count_uses_typed_source_slice() {
    let mut ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["table_row".to_string()],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: Some(SourceSliceSpec {
            direction: SourceSliceDirection::Head,
            count: Some(7),
            filter: SourceSliceFilter::None,
        }),
        retrieval_query: None,
        confidence: 1.0,
    };

    assert_eq!(requested_initial_table_row_count(Some(&ir)), Some(7));

    ir.target_types = vec!["document".to_string()];
    assert_eq!(requested_initial_table_row_count(Some(&ir)), None);
}

#[test]
fn latest_version_question_detection_uses_query_ir() {
    assert!(query_requests_latest_versions(&release_query_ir(Some("5"), None)));
    assert!(query_requests_latest_versions(&release_query_ir_with_source_slice_count(Some(5))));
    let mut release_target_ir = release_query_ir_with_source_slice_count(Some(10));
    release_target_ir.target_types = vec!["release".to_string()];
    assert!(query_requests_latest_versions(&release_target_ir));
    assert!(!query_requests_latest_versions(&release_query_ir_with_source_slice(
        SourceSliceDirection::Head,
        Some(5)
    )));
    assert!(!query_requests_latest_versions(&release_query_ir_with_source_slice(
        SourceSliceDirection::All,
        Some(5)
    )));
    let mut exact_version_ir = release_query_ir(None, None);
    exact_version_ir.literal_constraints =
        vec![LiteralSpan { text: "1.2.3".to_string(), kind: LiteralKind::Version }];
    assert!(!query_requests_latest_versions(&exact_version_ir));
    exact_version_ir.source_slice = Some(SourceSliceSpec {
        direction: SourceSliceDirection::Tail,
        count: Some(3),
        filter: SourceSliceFilter::ReleaseMarker,
    });
    assert!(query_requests_latest_versions(&exact_version_ir));
    let mut ir = release_query_ir(None, None);
    ir.target_types.clear();
    assert!(!query_requests_latest_versions(&ir));
}

#[test]
fn requested_latest_version_count_defaults_and_caps() {
    assert_eq!(requested_latest_version_count(&release_query_ir(None, None)), 10);
    assert_eq!(requested_latest_version_count(&release_query_ir(Some("3"), None)), 3);
    assert_eq!(requested_latest_version_count(&release_query_ir(Some("100"), None)), 10);
    assert_eq!(requested_latest_version_count(&release_query_ir(Some("2024"), None)), 10);
    assert_eq!(
        requested_latest_version_count(&release_query_ir_with_source_slice_count(Some(10))),
        10
    );
    assert_eq!(
        requested_latest_version_count(&release_query_ir_with_source_slice_count(Some(42))),
        10
    );
    assert_eq!(
        requested_latest_version_count(&release_query_ir_with_source_slice_count(Some(0))),
        1
    );
    assert_eq!(requested_latest_version_count(&release_query_ir_with_source_slice_count(None)), 10);

    let mut source_slice_ir = release_query_ir(Some("3"), None);
    source_slice_ir.source_slice = Some(SourceSliceSpec {
        direction: SourceSliceDirection::Tail,
        count: Some(8),
        filter: SourceSliceFilter::ReleaseMarker,
    });
    assert_eq!(requested_latest_version_count(&source_slice_ir), 8);
}

#[test]
fn latest_version_scope_terms_keep_digit_bearing_subject_tokens() {
    let mut ir = release_query_ir(Some("10"), Some("Alpha2 Pay"));
    ir.document_focus = Some(DocumentHint { hint: "Release notes for Suite4".to_string() });
    ir.literal_constraints
        .push(LiteralSpan { text: "2024".to_string(), kind: LiteralKind::NumericCode });

    let terms = latest_version_scope_terms(&ir);

    assert!(terms.contains(&"alpha2".to_string()));
    assert!(terms.contains(&"suite4".to_string()));
    assert!(!terms.contains(&"10".to_string()));
    assert!(!terms.contains(&"2024".to_string()));
}

#[test]
fn latest_version_chunk_merge_limit_preserves_requested_document_coverage() {
    assert_eq!(latest_version_context_top_k(&release_query_ir(Some("10"), None), 8), 40);
    assert_eq!(
        latest_version_context_top_k(&release_query_ir_with_source_slice_count(Some(10)), 8),
        40
    );
    assert_eq!(
        latest_version_context_top_k(
            &release_query_ir_with_source_slice(SourceSliceDirection::Head, Some(10)),
            8
        ),
        8
    );
    assert_eq!(latest_version_context_top_k(&release_query_ir(Some("3"), None), 20), 20);
}

#[test]
fn latest_version_chunk_score_keeps_first_chunk_for_each_version_before_second_chunks() {
    let newest_second = latest_version_chunk_score(DOCUMENT_IDENTITY_SCORE_FLOOR, 5, 0, 1);
    let oldest_first = latest_version_chunk_score(DOCUMENT_IDENTITY_SCORE_FLOOR, 5, 4, 0);

    assert!(oldest_first > newest_second);
}

#[test]
fn extract_semver_like_version_reads_title_versions() {
    assert_eq!(extract_semver_like_version("Version 9.8.765 - Product"), Some(vec![9, 8, 765]));
    assert_eq!(extract_semver_like_version("Version 5.302 - Product"), Some(vec![5, 302]));
    assert_eq!(extract_semver_like_version("No release number"), None);
    assert_eq!(extract_semver_like_version("Screenshot 2026.05.10"), None);
    assert_eq!(extract_semver_like_version("Screenshot 2026.05"), None);
}

#[test]
fn compare_version_desc_orders_newer_versions_first() {
    assert_eq!(compare_version_desc(&[9, 8, 765], &[9, 8, 764]), std::cmp::Ordering::Less);
    assert_eq!(compare_version_desc(&[9, 8, 762], &[9, 8, 763]), std::cmp::Ordering::Greater);
}

#[test]
fn latest_version_documents_select_newest_distinct_versions() {
    let docs = [
        sample_document_row("release-9.8.762.html", "Version 9.8.762"),
        sample_document_row("release-9.8.765.html", "Version 9.8.765"),
        sample_document_row("release-9.8.763.html", "Version 9.8.763"),
        sample_document_row("guide.html", "Setup Guide"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 3, &[]);
    let versions = selected.into_iter().map(|document| document.version).collect::<Vec<_>>();

    assert_eq!(versions, vec![vec![9, 8, 765], vec![9, 8, 763], vec![9, 8, 762]]);
}

#[test]
fn latest_version_documents_require_release_marker_and_respect_scope_terms() {
    let docs = [
        sample_document_row("alpha-release-9.8.765.html", "Alpha Version 9.8.765"),
        sample_document_row("beta-release-9.9.999.html", "Beta Version 9.9.999"),
        sample_document_row("oauth-2.0-guide.html", "OAuth 2.0 Guide"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(
        &index,
        5,
        &latest_version_scope_terms(&release_query_ir(None, Some("Alpha"))),
    );
    let titles = selected.into_iter().map(|document| document.title).collect::<Vec<_>>();

    assert_eq!(titles, vec!["Alpha Version 9.8.765".to_string()]);
    assert!(!text_has_release_version_marker("OAuth Guide"));
}

#[test]
fn latest_version_documents_fall_back_when_instruction_words_are_not_scope() {
    let docs = [
        sample_document_row("release-9.8.765.html", "Version 9.8.765"),
        sample_document_row("release-9.9.999.html", "Version 9.9.999"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(
        &index,
        1,
        &latest_version_scope_terms(&release_query_ir(None, None)),
    );

    assert_eq!(selected[0].version, vec![9, 9, 999]);
}

#[test]
fn latest_version_documents_do_not_collapse_same_version_across_titles() {
    let docs = [
        sample_document_row("alpha-release-9.8.765.html", "Alpha Version 9.8.765"),
        sample_document_row("beta-release-9.8.765.html", "Beta Version 9.8.765"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 5, &[]);

    assert_eq!(selected.len(), 2);
}

#[test]
fn latest_version_documents_deduplicate_before_family_selection() {
    let docs = [
        sample_document_row("alpha-1.2.12-a.html", "Version 1.2.12 - Sample Subject"),
        sample_document_row("alpha-1.2.12-b.html", "Version 1.2.12 - Sample Subject"),
        sample_document_row("alpha-1.2.11.html", "Version 1.2.11 - Sample Subject"),
        sample_document_row("beta-9.9.999.html", "Version 9.9.999 - Beta Suite"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 3, &[]);

    assert_eq!(selected.len(), 3);
    assert_eq!(
        selected.into_iter().map(|document| document.title).collect::<Vec<_>>(),
        vec![
            "Version 9.9.999 - Beta Suite".to_string(),
            "Version 1.2.12 - Sample Subject".to_string(),
            "Version 1.2.11 - Sample Subject".to_string(),
        ]
    );
}

#[test]
fn latest_version_documents_choose_dominant_release_family_for_multi_release_queries() {
    let docs = [
        sample_document_row("alpha-1.2.12.html", "Version 1.2.12 - Sample Subject"),
        sample_document_row("alpha-1.2.11.html", "Version 1.2.11 - Sample Subject"),
        sample_document_row("alpha-1.2.10.html", "Version 1.2.10 - Sample Subject"),
        sample_document_row("beta-9.9.999.html", "Version 9.9.999 - Beta Suite"),
    ];
    let index = docs
        .into_iter()
        .map(|document| (document.document_id, document))
        .collect::<HashMap<_, _>>();

    let selected = latest_version_documents(&index, 3, &[]);
    let titles = selected.into_iter().map(|document| document.title).collect::<Vec<_>>();

    assert_eq!(
        titles,
        vec![
            "Version 1.2.12 - Sample Subject".to_string(),
            "Version 1.2.11 - Sample Subject".to_string(),
            "Version 1.2.10 - Sample Subject".to_string(),
        ]
    );
}

#[test]
fn latest_version_family_key_normalizes_only_the_version_literal() {
    assert_eq!(
        latest_version_family_key("Version 1.2.12 - Sample Subject"),
        latest_version_family_key("Version 1.2.11 - Sample Subject")
    );
    assert_ne!(
        latest_version_family_key("Version 1.2.12 - Sample Subject"),
        latest_version_family_key("Version 1.2.12 - Beta Suite")
    );
}

#[test]
fn map_chunk_hit_drops_orphan_documents_without_heads() {
    // Contract update: `map_chunk_hit` no longer compares
    // `chunk.revision_id` against the canonical head — strict equality
    // dropped historical chunks for documents whose newer head revision
    // is a subset of an older complete revision. Now the guard only
    // drops chunks whose document has NO head at all (orphan).
    // This test exercises the orphan branch — both heads null.
    let mut document = sample_document_row("orphan-doc.csv", "orphan-doc.csv");
    document.active_revision_id = None;
    document.readable_revision_id = None;
    let stale_revision_id = Uuid::now_v7();
    let document_index = HashMap::from([(document.document_id, document.clone())]);
    let chunk = KnowledgeChunkRow {
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
        chunk_id: Uuid::now_v7(),
        workspace_id: document.workspace_id,
        library_id: document.library_id,
        document_id: document.document_id,
        revision_id: stale_revision_id,
        chunk_index: 0,
        chunk_kind: Some("table_row".to_string()),
        content_text: "stale".to_string(),
        normalized_text: "Sheet: records | Row 1 | Name: Stale".to_string(),
        span_start: None,
        span_end: None,
        token_count: Some(4),
        support_block_ids: Vec::new(),
        section_path: vec!["records".to_string()],
        heading_trail: vec!["records".to_string()],
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: Some(1.0),

        window_text: None,

        raptor_level: None,
        occurred_at: None,
        occurred_until: None,
    };

    assert!(map_chunk_hit(chunk, 1.0, &document_index, &[]).is_none());
}

#[test]
fn map_chunk_hit_drops_orphan_raptor_chunks_without_heads() {
    // See `map_chunk_hit_drops_orphan_documents_without_heads` for the
    // contract update. Raptor (level > 0) chunks now follow the same
    // orphan-only guard as base chunks: they are dropped only when the
    // owning document has no head pointer at all, never on simple
    // revision-id mismatch.
    let mut document = sample_document_row("summary-source.md", "summary-source.md");
    document.active_revision_id = None;
    document.readable_revision_id = None;
    let stale_revision_id = Uuid::now_v7();
    let document_index = HashMap::from([(document.document_id, document.clone())]);
    let chunk = KnowledgeChunkRow {
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
        chunk_id: Uuid::now_v7(),
        workspace_id: document.workspace_id,
        library_id: document.library_id,
        document_id: document.document_id,
        revision_id: stale_revision_id,
        chunk_index: 0,
        chunk_kind: Some("summary".to_string()),
        content_text: "stale summary".to_string(),
        normalized_text: "stale summary".to_string(),
        span_start: None,
        span_end: None,
        token_count: Some(2),
        support_block_ids: Vec::new(),
        section_path: Vec::new(),
        heading_trail: Vec::new(),
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: Some(1.0),
        window_text: None,
        raptor_level: Some(1),
        occurred_at: None,
        occurred_until: None,
    };

    assert!(map_chunk_hit(chunk, 1.0, &document_index, &[]).is_none());
}

#[test]
fn map_chunk_hit_preserves_structured_literals_from_window_and_content() {
    let document = sample_document_row("sample-guide.md", "Sample Guide");
    let document_index = HashMap::from([(document.document_id, document.clone())]);
    let mut chunk = knowledge_chunk_row(
        &document,
        3,
        Some("paragraph"),
        "[Main]\nalphaMode = strict\nconfiguration path: /etc/sample/service.conf",
    );
    chunk.window_text = Some(
        "Run the reload command after editing the configuration:\n\
         samplectl reload alpha-plugin --config /etc/sample/service.conf"
            .to_string(),
    );

    let mapped = map_chunk_hit(chunk, 1.0, &document_index, &[]).unwrap();

    assert!(mapped.source_text.contains("[Main]"));
    assert!(mapped.source_text.contains("alphaMode = strict"));
    assert!(mapped.source_text.contains("samplectl reload alpha-plugin"));
    assert!(mapped.excerpt.contains("[Main]"));
    assert!(mapped.excerpt.contains("alphaMode = strict"));
    assert!(mapped.excerpt.contains("samplectl reload alpha-plugin"));
}

#[test]
fn retain_canonical_document_head_chunks_drops_orphan_documents_only() {
    // Contract update mirrors `map_chunk_hit` relaxation (2026-05-03
    // stage incident: 41% of chunks dropped by strict-equality gate).
    // The function now drops chunks only when their document has no
    // canonical head — strict revision-id mismatch is no longer a drop
    // signal because partial incremental re-ingest leaves valid older
    // chunks under non-head revisions. Downstream dedup handles
    // cross-revision duplicates.
    let document = sample_document_row("records.jsonl", "records.jsonl");
    let canonical_revision_id = canonical_document_revision_id(&document).unwrap();
    let stale_revision_id = Uuid::now_v7();
    let document_index = HashMap::from([(document.document_id, document.clone())]);
    let orphan_document_id = Uuid::now_v7();
    let mut chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: stale_revision_id,
            chunk_index: 4,
            chunk_kind: Some("paragraph".to_string()),
            document_id: document.document_id,
            document_label: "records.jsonl".to_string(),
            excerpt: "older revision".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: "older revision".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: canonical_revision_id,
            chunk_index: 4,
            chunk_kind: Some("paragraph".to_string()),
            document_id: document.document_id,
            document_label: "records.jsonl".to_string(),
            excerpt: "current".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.9),
            source_text: "current".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: orphan_document_id,
            document_label: "orphan.txt".to_string(),
            excerpt: "orphan".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.5),
            source_text: "orphan".to_string(),
        },
    ];

    // Orphan dropped, both document-with-head chunks kept.
    assert_eq!(retain_canonical_document_head_chunks(&mut chunks, &document_index), 1);
    assert_eq!(chunks.len(), 2);
    assert!(chunks.iter().all(|c| c.document_id == document.document_id));
}

fn runtime_chunk(label: &str, score: f32) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: label.to_string(),
        excerpt: label.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: label.to_string(),
    }
}

fn runtime_chunk_for_document(document_id: Uuid, label: &str, score: f32) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk { document_id, ..runtime_chunk(label, score) }
}

fn latest_version_identity_chunk(
    document_id: Uuid,
    requested_count: usize,
    document_rank: usize,
    chunk_rank: usize,
) -> RuntimeMatchedChunk {
    let score = latest_version_chunk_score(
        DOCUMENT_IDENTITY_SCORE_FLOOR,
        requested_count,
        document_rank,
        chunk_rank,
    );
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: chunk_rank as i32,
        chunk_kind: Some("paragraph".to_string()),
        document_id,
        document_label: format!("Release {document_rank}"),
        excerpt: format!("release {document_rank} chunk {chunk_rank}"),
        score_kind: RuntimeChunkScoreKind::LatestVersion,
        score: Some(score),
        source_text: format!("release {document_rank} chunk {chunk_rank}"),
    }
}

#[test]
fn latest_version_document_scope_drops_non_release_tail() {
    let release_a = Uuid::now_v7();
    let release_b = Uuid::now_v7();
    let generic = Uuid::now_v7();
    let mut chunks = vec![
        runtime_chunk_for_document(release_a, "Version 9.8.765 Product Notes", 10.0),
        runtime_chunk_for_document(release_b, "Version 9.8.764 Product Notes", 9.0),
        runtime_chunk_for_document(generic, "Operator Workflow Guide", 8.0),
    ];

    retain_scoped_documents(
        &mut chunks,
        &BTreeSet::new(),
        &BTreeSet::from([release_a, release_b]),
        &BTreeSet::new(),
    );

    assert_eq!(chunks.len(), 2);
    assert!(chunks.iter().all(|chunk| chunk.document_id != generic));
}

#[test]
fn latest_version_document_scope_falls_back_when_no_release_documents() {
    let generic = Uuid::now_v7();
    let mut chunks = vec![runtime_chunk_for_document(generic, "Operator Workflow Guide", 8.0)];

    retain_scoped_documents(&mut chunks, &BTreeSet::new(), &BTreeSet::new(), &BTreeSet::new());

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].document_id, generic);
}

#[test]
fn latest_version_document_scope_unions_with_targeted_documents() {
    let release = Uuid::now_v7();
    let targeted = Uuid::now_v7();
    let generic = Uuid::now_v7();
    let mut chunks = vec![
        runtime_chunk_for_document(release, "Version 9.8.765 Product Notes", 10.0),
        runtime_chunk_for_document(targeted, "Explicitly Targeted Guide", 9.0),
        runtime_chunk_for_document(generic, "Operator Workflow Guide", 8.0),
    ];

    retain_scoped_documents(
        &mut chunks,
        &BTreeSet::from([targeted]),
        &BTreeSet::from([release]),
        &BTreeSet::new(),
    );

    let retained = chunks.iter().map(|chunk| chunk.document_id).collect::<BTreeSet<_>>();
    assert_eq!(retained, BTreeSet::from([release, targeted]));
}

#[test]
fn merge_chunks_preserves_identity_scale_scores() {
    let ordinary = runtime_chunk("ordinary", 10.0);
    let identity = runtime_chunk("identity", DOCUMENT_IDENTITY_SCORE_FLOOR);

    let merged = merge_chunks(vec![ordinary], vec![identity.clone()], 8);

    assert_eq!(merged[0].chunk_id, identity.chunk_id);
    assert_eq!(merged[0].score, Some(DOCUMENT_IDENTITY_SCORE_FLOOR));
}

#[test]
fn entity_bio_chunks_use_explicit_merge_lane_priority() {
    let ordinary = runtime_chunk("ordinary", 10_000.0);
    let entity_bio = runtime_chunk("entity bio", entity_bio_chunk_score(0));

    let merged = merge_entity_bio_chunks(vec![ordinary], vec![entity_bio.clone()], 8);

    assert_eq!(merged[0].chunk_id, entity_bio.chunk_id);
    assert_eq!(merged[0].score, Some(entity_bio_chunk_score(0)));
    assert!(entity_bio_chunk_score(0) < DOCUMENT_IDENTITY_SCORE_FLOOR);
}

#[test]
fn graph_evidence_chunks_use_explicit_merge_lane_priority() {
    let ordinary = runtime_chunk("ordinary", 10_000.0);
    let graph_evidence = runtime_chunk("graph evidence", graph_evidence_chunk_score(0));

    let merged = merge_graph_evidence_chunks(vec![ordinary], vec![graph_evidence.clone()], 8);

    assert_eq!(merged[0].chunk_id, graph_evidence.chunk_id);
    assert_eq!(merged[0].score, Some(graph_evidence_chunk_score(0)));
}

#[test]
fn graph_evidence_promotes_for_structural_relationship_ir() {
    let mut query_ir = target_entities_query_ir(&["Alpha Node", "Beta Endpoint"]);
    query_ir.target_types = vec!["relationship".to_string()];

    assert!(query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_promotes_for_relation_like_event_ir() {
    let query_ir = target_entities_query_ir(&["Beacon Node", "Harbor Delta"]);

    assert!(query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_promotes_for_artifact_selection_ir() {
    let mut query_ir = target_entities_query_ir(&["Router Hub"]);
    query_ir.target_types = vec!["artifact".to_string()];

    assert!(query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_does_not_promote_for_single_document_concept_ir() {
    let mut query_ir = target_entities_query_ir(&["Transition Hooks"]);
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec!["concept".to_string()];

    assert!(!query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_promotes_for_fact_value_ir() {
    let mut query_ir = target_entities_query_ir(&["Transition Hooks"]);
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.target_types = vec!["concept".to_string()];

    assert!(query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_promotes_for_fact_inventory_ir() {
    let mut query_ir = target_entities_query_ir(&["Transition Hooks"]);
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec!["concept".to_string()];

    assert!(query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn graph_evidence_does_not_promote_for_exact_technical_ir() {
    let mut query_ir = target_entities_query_ir(&["Alpha Node", "Beta Endpoint"]);
    query_ir.target_types = vec!["relationship".to_string()];
    query_ir.literal_constraints =
        vec![LiteralSpan { text: "/v1/alpha".to_string(), kind: LiteralKind::Path }];

    assert!(!query_ir_promotes_graph_evidence(&query_ir));
}

#[test]
fn truncate_bundle_preserves_runtime_evidence_lanes() {
    let mut high_scored_noise = (0..8)
        .map(|index| runtime_chunk(&format!("noise-{index}"), 10_000.0 - index as f32))
        .collect::<Vec<_>>();
    let mut graph_evidence = runtime_chunk("rare graph evidence", graph_evidence_chunk_score(0));
    graph_evidence.score_kind = RuntimeChunkScoreKind::GraphEvidence;
    high_scored_noise.push(graph_evidence.clone());
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: high_scored_noise,
    };

    truncate_bundle(&mut bundle, 4, None, &std::collections::HashSet::new());

    assert!(bundle.chunks.iter().any(|chunk| chunk.chunk_id == graph_evidence.chunk_id));
    assert_eq!(bundle.chunks[0].chunk_id, graph_evidence.chunk_id);
}

#[test]
fn truncate_bundle_orders_same_lane_by_score_before_insertion_order() {
    let mut chunks = (0..8)
        .map(|index| {
            let mut chunk = runtime_chunk(
                &format!("identity-noise-{index}"),
                DOCUMENT_IDENTITY_SCORE_FLOOR + index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut focused = runtime_chunk("focused setup parameter table", DOCUMENT_IDENTITY_SCORE_FLOOR);
    focused.score = Some(DOCUMENT_IDENTITY_SCORE_FLOOR + 1_000.0);
    focused.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
    let focused_id = focused.chunk_id;
    chunks.push(focused);
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(&mut bundle, 4, None, &std::collections::HashSet::new());

    assert_eq!(bundle.chunks[0].chunk_id, focused_id);
}

#[test]
fn truncate_bundle_demotes_attached_context_documents_below_peers() {
    let attached_doc = Uuid::now_v7();
    let primary_doc = Uuid::now_v7();
    // The attached-context chunk has a far HIGHER raw score than the primary
    // chunk, so absent demotion it would win the single context slot — exactly
    // the OCR-screenshot-floods-the-answer failure this guards against.
    let attached = runtime_chunk_for_document(attached_doc, "screenshot", 10_000.0);
    let primary = runtime_chunk_for_document(primary_doc, "procedure", 1.0);
    let attached_id = attached.chunk_id;
    let primary_id = primary.chunk_id;

    // Baseline (no demotion): the high-scored attached-context chunk wins.
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![attached.clone(), primary.clone()],
    };
    truncate_bundle(&mut bundle, 1, None, &std::collections::HashSet::new());
    assert_eq!(bundle.chunks.len(), 1);
    assert_eq!(bundle.chunks[0].chunk_id, attached_id);

    // With the attached-context document demoted, the lower-scored primary
    // chunk takes the slot instead.
    let mut demoted = std::collections::HashSet::new();
    demoted.insert(attached_doc);
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![attached, primary],
    };
    truncate_bundle(&mut bundle, 1, None, &demoted);
    assert_eq!(bundle.chunks.len(), 1);
    assert_eq!(bundle.chunks[0].chunk_id, primary_id);
}

#[test]
fn truncate_bundle_reserves_source_context_for_exact_technical_queries() {
    let mut chunks = (0..8)
        .map(|index| {
            let mut chunk = runtime_chunk(
                &format!("identity-{index}"),
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut source_context = runtime_chunk("owner = 100,200\n100 = first\n200 = second", 1.0);
    source_context.score_kind = RuntimeChunkScoreKind::SourceContext;
    source_context.score = Some(DOCUMENT_IDENTITY_SCORE_FLOOR + 1.0);
    let source_context_id = source_context.chunk_id;
    chunks.push(source_context);
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        4,
        Some(&exact_error_code_query_ir()),
        &std::collections::HashSet::new(),
    );

    assert_eq!(bundle.chunks.len(), 4);
    assert!(bundle.chunks.iter().any(|chunk| chunk.chunk_id == source_context_id));
}

#[test]
fn truncate_bundle_caps_error_code_source_context_reservation() {
    let mut graph_evidence = runtime_chunk("high confidence graph evidence", 1.0);
    graph_evidence.score_kind = RuntimeChunkScoreKind::GraphEvidence;
    graph_evidence.score = Some(graph_evidence_chunk_score(0));
    let graph_evidence_id = graph_evidence.chunk_id;

    let mut relevance = runtime_chunk("ordinary relevance", 10_000.0);
    relevance.score_kind = RuntimeChunkScoreKind::Relevance;

    let mut chunks = vec![graph_evidence, relevance];
    for index in 0..4 {
        let mut source_context =
            runtime_chunk(&format!("owner{index} = 100,200\n100 = first\n200 = second"), 1.0);
        source_context.score_kind = RuntimeChunkScoreKind::SourceContext;
        source_context.score = Some(DOCUMENT_IDENTITY_SCORE_FLOOR + 50.0 - index as f32);
        chunks.push(source_context);
    }
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        4,
        Some(&exact_error_code_query_ir()),
        &std::collections::HashSet::new(),
    );

    let source_context_count = bundle
        .chunks
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
        .count();
    assert_eq!(source_context_count, 2);
    assert!(bundle.chunks.iter().any(|chunk| chunk.chunk_id == graph_evidence_id));
}

fn configure_how_focus_query_ir(focus: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["procedure".to_string()],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: Some(crate::domains::query_ir::DocumentHint { hint: focus.to_string() }),
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.95,
    }
}

#[test]
fn truncate_bundle_reserves_setup_focus_anchor_for_configure_how() {
    // High-scored filler chunks from the focused document would fill the whole
    // top_k by score, pushing the low-scored install anchor out. The reserved
    // slot must keep the anchor (package command + config path) in context.
    let mut chunks = (0..8)
        .map(|index| runtime_chunk("Sample Subject admin guide", 10_000.0 - index as f32))
        .collect::<Vec<_>>();
    let mut anchor = runtime_chunk("Sample Subject admin guide", 1.0);
    anchor.source_text =
        "Module configuration\nsample-runner --install sample-link\nSettings in the file /opt/alpha/connector/connector.conf".to_string();
    let anchor_id = anchor.chunk_id;
    chunks.push(anchor);
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        4,
        Some(&configure_how_focus_query_ir("Sample Subject")),
        &std::collections::HashSet::new(),
    );

    assert_eq!(bundle.chunks.len(), 4);
    assert!(
        bundle.chunks.iter().any(|chunk| chunk.chunk_id == anchor_id),
        "focused-document setup anchor must survive truncation"
    );
}

#[test]
fn truncate_bundle_reserves_versioned_update_procedure_body() {
    let mut chunks = (0..8)
        .map(|index| {
            let mut chunk = runtime_chunk(
                &format!("identity-noise-{index}"),
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut focused_noise = runtime_chunk("Sample Target overview", 9_000.0);
    focused_noise.score_kind = RuntimeChunkScoreKind::FocusedDocument;
    chunks.push(focused_noise);
    let mut procedure = runtime_chunk("Sample Target update guide", 1.0);
    procedure.score_kind = RuntimeChunkScoreKind::FocusedDocument;
    procedure.document_label = "Sample Target update guide".to_string();
    procedure.source_text = "Procedure\n\
        1. Remove /etc/alpha/sources.list.\n\
        2. Install alpha-upgrade package.\n\
        3. Run ./upgrade_alpha.sh."
        .to_string();
    let procedure_id = procedure.chunk_id;
    chunks.push(procedure);
    let mut query_ir = target_entities_query_ir(&["Sample Target"]);
    query_ir.act = QueryAct::ConfigureHow;
    query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
    query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(&mut bundle, 4, Some(&query_ir), &std::collections::HashSet::new());

    assert!(
        bundle.chunks.iter().any(|chunk| chunk.chunk_id == procedure_id),
        "versioned update body chunk must survive final context truncation"
    );
}

#[test]
fn truncate_bundle_prefers_exact_versioned_update_runbook_over_short_title_anchors() {
    let mut chunks = (0..8)
        .map(|index| {
            let mut chunk = runtime_chunk(
                &format!("identity-noise-{index}"),
                DOCUMENT_IDENTITY_SCORE_FLOOR + 500.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();

    for (label, score) in
        [("Sample Target update note", 10_000.0), ("Sample Target update checklist", 9_999.0)]
    {
        let mut short_anchor = runtime_chunk(label, score);
        short_anchor.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        short_anchor.source_text = format!(
            "{label}\n\
             1. Check the current Sample Target version.\n\
             2. Read the release note.\n\
             3. Run sample-runner --refresh."
        );
        chunks.push(short_anchor);
    }

    let mut runbook = runtime_chunk("Sample Target update runbook", 1.0);
    runbook.score_kind = RuntimeChunkScoreKind::FocusedDocument;
    runbook.source_text = "Sample Target update runbook\n\
         1. Run sample-runner --refresh.\n\
         2. Run sample-runner --apply.\n\
         3. Run sudo sample-configure alpha-rest.\n\
         4. Run sudo service alpha-rest restart."
        .to_string();
    let runbook_id = runbook.chunk_id;
    chunks.push(runbook);

    let mut query_ir = target_entities_query_ir(&["Sample Target"]);
    query_ir.act = QueryAct::ConfigureHow;
    query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
    query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(&mut bundle, 4, Some(&query_ir), &std::collections::HashSet::new());

    assert!(
        bundle.chunks.iter().any(|chunk| chunk.chunk_id == runbook_id),
        "exact-target command runbook must outrank shorter exact-title anchors: {:#?}",
        bundle
            .chunks
            .iter()
            .map(|chunk| (&chunk.document_label, &chunk.source_text))
            .collect::<Vec<_>>()
    );
}

#[test]
fn truncate_bundle_does_not_reserve_anchor_without_document_focus() {
    // Same chunks, but the query has no document_focus → no reservation, the
    // low-scored anchor is truncated like any other chunk.
    let mut chunks = (0..8)
        .map(|index| runtime_chunk("Sample Subject admin guide", 10_000.0 - index as f32))
        .collect::<Vec<_>>();
    let mut anchor = runtime_chunk("Sample Subject admin guide", 1.0);
    anchor.source_text =
        "sample-runner --install sample-link\nSettings in the file /opt/alpha/connector/connector.conf"
            .to_string();
    let anchor_id = anchor.chunk_id;
    chunks.push(anchor);
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    let mut query_ir = configure_how_focus_query_ir("Sample Subject");
    query_ir.document_focus = None;
    truncate_bundle(&mut bundle, 4, Some(&query_ir), &std::collections::HashSet::new());

    assert_eq!(bundle.chunks.len(), 4);
    assert!(!bundle.chunks.iter().any(|chunk| chunk.chunk_id == anchor_id));
}

#[test]
fn truncate_bundle_reserves_source_context_for_transport_inventory_queries() {
    let mut chunks = (0..12)
        .map(|index| {
            let mut chunk = runtime_chunk(
                &format!("identity-{index}"),
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    let mut source_context_ids = Vec::new();
    for index in 0..8 {
        let mut source_context = runtime_chunk(
            &format!("serviceUrl{index} = http://localhost:8080\nservicePort{index} = 8080"),
            1.0,
        );
        source_context.score_kind = RuntimeChunkScoreKind::SourceContext;
        source_context.score = Some(DOCUMENT_IDENTITY_SCORE_FLOOR + 8.0 - index as f32);
        source_context_ids.push(source_context.chunk_id);
        chunks.push(source_context);
    }
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        10,
        Some(&transport_inventory_query_ir()),
        &std::collections::HashSet::new(),
    );

    let source_context_count = bundle
        .chunks
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
        .count();
    assert_eq!(source_context_count, 8);
    assert!(
        source_context_ids.iter().all(|id| bundle.chunks.iter().any(|chunk| chunk.chunk_id == *id))
    );
}

#[test]
fn truncate_bundle_expands_entity_budget_for_inventory_queries() {
    let entities = (0..12)
        .map(|index| RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: format!("Package {index:02}"),
            node_type: "artifact".to_string(),
            summary: None,
            score: Some(1.0),
        })
        .collect::<Vec<_>>();
    let chunks =
        (0..12).map(|index| runtime_chunk(&format!("chunk-{index:02}"), 1.0)).collect::<Vec<_>>();
    let mut bundle = RetrievalBundle { entities, relationships: Vec::new(), chunks };
    let query_ir = release_query_ir(None, Some("package-*"));

    truncate_bundle(&mut bundle, 4, Some(&query_ir), &std::collections::HashSet::new());

    assert_eq!(bundle.entities.len(), 12);
    assert_eq!(bundle.chunks.len(), 4);
}

#[test]
fn truncate_bundle_keeps_requested_latest_version_document_coverage() {
    let requested_count = 10;
    let document_ids = (0..requested_count).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
    let mut chunks = Vec::new();
    for chunk_rank in 0..2 {
        for (document_rank, document_id) in document_ids.iter().copied().enumerate() {
            chunks.push(latest_version_identity_chunk(
                document_id,
                requested_count,
                document_rank,
                chunk_rank,
            ));
        }
    }
    let mut query_ir = release_query_ir_with_source_slice_count(Some(requested_count as u16));
    query_ir.scope = QueryScope::SingleDocument;
    let effective_top_k = source_slice_context_top_k(&query_ir, 8);
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        effective_top_k,
        Some(&query_ir),
        &std::collections::HashSet::new(),
    );

    let retained_documents =
        bundle.chunks.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>();
    assert_eq!(effective_top_k, 11);
    assert_eq!(retained_documents.len(), requested_count);
}

#[test]
fn truncate_bundle_reserves_latest_version_chunks_before_stale_document_identity_hits() {
    let requested_count = 5;
    let mut chunks = (0..8)
        .map(|index| {
            let mut chunk =
                runtime_chunk(&format!("latest releases stale context {index}"), 1_000_000.0);
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunk
        })
        .collect::<Vec<_>>();
    chunks.extend((0..requested_count).map(|document_rank| {
        latest_version_identity_chunk(Uuid::now_v7(), requested_count, document_rank, 0)
    }));
    let mut query_ir = release_query_ir_with_source_slice_count(Some(requested_count as u16));
    query_ir.scope = QueryScope::MultiDocument;
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(
        &mut bundle,
        requested_count,
        Some(&query_ir),
        &std::collections::HashSet::new(),
    );

    assert_eq!(bundle.chunks.len(), requested_count);
    assert_eq!(
        bundle
            .chunks
            .iter()
            .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::LatestVersion)
            .count(),
        requested_count
    );
}

#[test]
fn truncate_bundle_preserves_multi_document_relevant_coverage_for_compare_queries() {
    let rust_document = Uuid::now_v7();
    let llm_document = Uuid::now_v7();
    let rust_chunk = |score: f32| RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: rust_document,
        document_label: "rust_programming_language_wikipedia".to_string(),
        excerpt: "Rust language runtime context".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: "Rust language context".to_string(),
    };
    let llm_chunk = |score: f32| RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: llm_document,
        document_label: "large_language_model_wikipedia".to_string(),
        excerpt: "LLM context".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: "Large Language Model context".to_string(),
    };

    let mut query_ir = target_entities_query_ir(&["Rust", "Large Language Model"]);
    query_ir.act = QueryAct::Compare;
    query_ir.scope = QueryScope::MultiDocument;
    query_ir.comparison = Some(ComparisonSpec {
        a: Some("Rust".to_string()),
        b: Some("Large Language Model".to_string()),
        dimension: "features".to_string(),
    });

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            rust_chunk(10.0),
            rust_chunk(9.9),
            rust_chunk(9.8),
            rust_chunk(9.7),
            rust_chunk(9.6),
            rust_chunk(9.5),
            llm_chunk(8.0),
            llm_chunk(7.9),
        ],
    };

    truncate_bundle(&mut bundle, 6, Some(&query_ir), &std::collections::HashSet::new());

    assert_eq!(bundle.chunks.len(), 6);
    assert!(bundle.chunks.iter().any(|chunk| chunk.document_id == rust_document));
    assert!(bundle.chunks.iter().any(|chunk| chunk.document_id == llm_document));
}

#[test]
fn truncate_bundle_compare_fallback_keeps_second_document_when_terms_are_not_present() {
    let first_document = Uuid::now_v7();
    let second_document = Uuid::now_v7();
    let chunk = |document_id: Uuid, label: &str, score: f32| RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: label.to_string(),
        excerpt: "neutral context".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text: "neutral context".to_string(),
    };

    let mut query_ir = target_entities_query_ir(&["Alpha", "Beta"]);
    query_ir.act = QueryAct::Compare;
    query_ir.scope = QueryScope::MultiDocument;
    query_ir.comparison = Some(ComparisonSpec {
        a: Some("Alpha".to_string()),
        b: Some("Beta".to_string()),
        dimension: "difference".to_string(),
    });

    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            chunk(first_document, "first.md", 10.0),
            chunk(first_document, "first.md", 9.9),
            chunk(first_document, "first.md", 9.8),
            chunk(second_document, "second.md", 8.0),
        ],
    };

    truncate_bundle(&mut bundle, 2, Some(&query_ir), &std::collections::HashSet::new());

    assert_eq!(bundle.chunks.len(), 2);
    assert!(bundle.chunks.iter().any(|chunk| chunk.document_id == first_document));
    assert!(bundle.chunks.iter().any(|chunk| chunk.document_id == second_document));
}

#[test]
fn graph_evidence_merge_preserves_prior_query_ir_focus_score_kind() {
    let ordinary = runtime_chunk("ordinary", 0.02);
    let graph_evidence = runtime_chunk("graph evidence", graph_evidence_chunk_score(0));
    let exact_focus = runtime_chunk("exact focus", 100.0);

    let merged = merge_query_ir_focus_chunks(vec![ordinary], vec![exact_focus.clone()], 8);
    let merged = merge_graph_evidence_chunks(merged, vec![graph_evidence], 8);

    assert_eq!(merged[0].chunk_id, exact_focus.chunk_id);
    assert_eq!(merged[0].score_kind, RuntimeChunkScoreKind::QueryIrFocus);
    assert_eq!(merged[0].score, Some(100.0));
}

#[test]
fn entity_bio_lane_does_not_override_document_identity_priority() {
    let identity = runtime_chunk("identity", DOCUMENT_IDENTITY_SCORE_FLOOR);
    let entity_bio = runtime_chunk("entity bio", entity_bio_chunk_score(0));

    let merged = merge_entity_bio_chunks(vec![identity.clone()], vec![entity_bio], 8);

    assert_eq!(merged[0].chunk_id, identity.chunk_id);
    assert_eq!(merged[0].score, Some(DOCUMENT_IDENTITY_SCORE_FLOOR));
}

#[test]
fn rrf_merge_preserves_latest_version_score_kind() {
    let mut latest = runtime_chunk("latest", DOCUMENT_IDENTITY_SCORE_FLOOR);
    latest.score_kind = RuntimeChunkScoreKind::LatestVersion;
    let ordinary = runtime_chunk("ordinary", 0.02);

    let merged = merge_chunks(vec![ordinary], vec![latest.clone()], 8);

    assert_eq!(merged[0].chunk_id, latest.chunk_id);
    assert_eq!(merged[0].score_kind, RuntimeChunkScoreKind::LatestVersion);
    assert_eq!(merged[0].score, Some(DOCUMENT_IDENTITY_SCORE_FLOOR));
}

#[test]
fn entity_bio_scores_keep_fanout_order_inside_lane() {
    assert!(entity_bio_chunk_score(0) > entity_bio_chunk_score(1));
    assert!(entity_bio_chunk_score(23) > 0.9);
}

#[test]
fn graph_evidence_scores_keep_fanout_order_inside_entity_bio_lane() {
    assert!(graph_evidence_chunk_score(0) > graph_evidence_chunk_score(1));
    assert!(graph_evidence_chunk_score(0) > entity_bio_chunk_score(0));
    assert!(graph_evidence_chunk_score(23) < DOCUMENT_IDENTITY_SCORE_FLOOR);
}

#[test]
fn graph_evidence_targets_preserve_graph_order_and_dedupe() {
    let node_id = Uuid::now_v7();
    let edge_id = Uuid::now_v7();
    let entities = vec![
        RuntimeMatchedEntity {
            node_id,
            label: "Archive".to_string(),
            node_type: "artifact".to_string(),
            summary: None,
            score: Some(1.0),
        },
        RuntimeMatchedEntity {
            node_id,
            label: "Archive".to_string(),
            node_type: "artifact".to_string(),
            summary: None,
            score: Some(0.5),
        },
    ];
    let relationships = vec![RuntimeMatchedRelationship {
        edge_id,
        relation_type: "mentions".to_string(),
        from_node_id: node_id,
        from_label: "Guide".to_string(),
        to_node_id: node_id,
        to_label: "Archive".to_string(),
        summary: None,
        support_count: 1,
        score: Some(0.8),
    }];

    let targets = graph_evidence_targets(&entities, &relationships);

    assert_eq!(targets, vec![("node".to_string(), node_id), ("edge".to_string(), edge_id)]);
}

#[test]
fn graph_evidence_targets_for_query_include_lexical_node_outside_visible_bundle() {
    let noise = runtime_graph_node("Common Topic", "concept", None);
    let needle = runtime_graph_node(
        "Needle Endpoint",
        "artifact",
        Some("Contains the rare configuration endpoint"),
    );
    let graph_index = graph_index_with_nodes(vec![noise, needle.clone()]);
    let plan = RuntimeQueryPlan {
        keywords: vec!["needle".to_string(), "endpoint".to_string()],
        entity_keywords: vec!["needle".to_string(), "endpoint".to_string()],
        ..build_query_plan("Which endpoint does the needle setup use?", None, Some(8), None)
    };

    let targets = graph_evidence_targets_for_query(&[], &[], &plan, None, &[], &graph_index);

    assert!(targets.contains(&("node".to_string(), needle.id)));
}

#[test]
fn graph_evidence_targets_for_query_keep_retrieved_bundle_targets_first() {
    let bundle_node = runtime_graph_node("Selected Bundle Target", "artifact", None);
    let query_node = runtime_graph_node(
        "Needle Endpoint",
        "artifact",
        Some("Contains the rare configuration endpoint"),
    );
    let graph_index = graph_index_with_nodes(vec![bundle_node.clone(), query_node.clone()]);
    let plan = RuntimeQueryPlan {
        keywords: vec!["needle".to_string(), "endpoint".to_string()],
        entity_keywords: vec!["needle".to_string(), "endpoint".to_string()],
        ..build_query_plan("Which endpoint does the needle setup use?", None, Some(8), None)
    };
    let entities = vec![RuntimeMatchedEntity {
        node_id: bundle_node.id,
        label: bundle_node.label.clone(),
        node_type: bundle_node.node_type.clone(),
        summary: None,
        score: Some(0.1),
    }];

    let targets = graph_evidence_targets_for_query(&entities, &[], &plan, None, &[], &graph_index);

    assert_eq!(targets.first(), Some(&("node".to_string(), bundle_node.id)));
    assert!(targets.contains(&("node".to_string(), query_node.id)));
}

#[test]
fn graph_evidence_targets_for_query_return_full_bundle_without_supplemental_targets() {
    let bundle_nodes = (0..48)
        .map(|index| runtime_graph_node(&format!("Bundle target {index:02}"), "artifact", None))
        .collect::<Vec<_>>();
    let query_node = runtime_graph_node(
        "Needle Endpoint",
        "artifact",
        Some("Contains the rare configuration endpoint"),
    );
    let mut graph_nodes = bundle_nodes.clone();
    graph_nodes.push(query_node.clone());
    let graph_index = graph_index_with_nodes(graph_nodes);
    let plan = RuntimeQueryPlan {
        keywords: vec!["needle".to_string(), "endpoint".to_string()],
        entity_keywords: vec!["needle".to_string(), "endpoint".to_string()],
        ..build_query_plan("Which endpoint does the needle setup use?", None, Some(8), None)
    };
    let entities = bundle_nodes
        .iter()
        .map(|node| RuntimeMatchedEntity {
            node_id: node.id,
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            summary: None,
            score: Some(0.1),
        })
        .collect::<Vec<_>>();

    let targets = graph_evidence_targets_for_query(&entities, &[], &plan, None, &[], &graph_index);

    assert_eq!(targets.len(), 48);
    assert!(!targets.contains(&("node".to_string(), query_node.id)));
}

#[test]
fn graph_evidence_targets_for_query_prioritize_explicit_targets_under_cap_pressure() {
    let bundle_nodes = (0..48)
        .map(|index| runtime_graph_node(&format!("Bundle target {index:02}"), "artifact", None))
        .collect::<Vec<_>>();
    let query_node = runtime_graph_node(
        "Needle Endpoint",
        "artifact",
        Some("Contains the rare configuration endpoint"),
    );
    let mut graph_nodes = bundle_nodes.clone();
    graph_nodes.push(query_node.clone());
    let graph_index = graph_index_with_nodes(graph_nodes);
    let plan = RuntimeQueryPlan {
        keywords: vec!["needle".to_string(), "endpoint".to_string()],
        entity_keywords: vec!["needle".to_string(), "endpoint".to_string()],
        ..build_query_plan("Which endpoint does the needle setup use?", None, Some(8), None)
    };
    let entities = bundle_nodes
        .iter()
        .map(|node| RuntimeMatchedEntity {
            node_id: node.id,
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            summary: None,
            score: Some(0.1),
        })
        .collect::<Vec<_>>();
    let ir = target_entities_query_ir(&["Needle"]);
    let target_entity_profiles = graph_target_entity_profiles(Some(&ir), &graph_index);

    let targets = graph_evidence_targets_for_query(
        &entities,
        &[],
        &plan,
        Some(&ir),
        &target_entity_profiles,
        &graph_index,
    );

    assert_eq!(targets.len(), 48);
    assert_eq!(targets.first(), Some(&("node".to_string(), query_node.id)));
    assert!(targets.contains(&("node".to_string(), bundle_nodes[0].id)));
}

#[test]
fn graph_evidence_targets_for_query_can_use_document_nodes_as_source_anchors() {
    let bundle_nodes = (0..48)
        .map(|index| runtime_graph_node(&format!("Bundle target {index:02}"), "artifact", None))
        .collect::<Vec<_>>();
    let target_document =
        runtime_graph_node("Acmealpha setup guide", "document", Some("Focused configuration"));
    let mut graph_nodes = bundle_nodes.clone();
    graph_nodes.push(target_document.clone());
    let graph_index = graph_index_with_nodes(graph_nodes);
    let plan = build_query_plan("show the acmew configuration steps", None, Some(8), None);
    let entities = bundle_nodes
        .iter()
        .map(|node| RuntimeMatchedEntity {
            node_id: node.id,
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            summary: None,
            score: Some(0.1),
        })
        .collect::<Vec<_>>();
    let ir = target_entities_query_ir(&["Acmew"]);
    let target_entity_profiles = graph_target_entity_profiles(Some(&ir), &graph_index);

    let targets = graph_evidence_targets_for_query(
        &entities,
        &[],
        &plan,
        Some(&ir),
        &target_entity_profiles,
        &graph_index,
    );

    assert_eq!(targets.first(), Some(&("node".to_string(), target_document.id)));
}

#[test]
fn graph_evidence_targets_for_query_keep_multi_anchor_node_under_target_cap_pressure() {
    let composite = runtime_graph_node("Beacon crossed Harbor Delta", "event", None);
    let mut nodes = (0..80)
        .map(|index| {
            runtime_graph_node(&format!("Harbor Delta reference {index:02}"), "artifact", None)
        })
        .collect::<Vec<_>>();
    nodes.push(composite.clone());
    let graph_index = graph_index_with_nodes(nodes);
    let plan = build_query_plan("find Beacon near Harbor Delta", None, Some(8), None);
    let ir = target_entities_query_ir(&["Beacon", "Harbor Delta"]);

    let target_entity_profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    let targets = graph_evidence_targets_for_query(
        &[],
        &[],
        &plan,
        Some(&ir),
        &target_entity_profiles,
        &graph_index,
    );

    assert!(
        targets.contains(&("node".to_string(), composite.id)),
        "multi-anchor graph node must survive graph evidence target cap pressure"
    );
}

#[test]
fn graph_evidence_text_replaces_weak_support_chunk_text() {
    let mut chunk = runtime_chunk("document title without the requested literal", 1.0);
    let chunk_id = chunk.chunk_id;
    let mut evidence_texts_by_chunk = HashMap::new();
    evidence_texts_by_chunk.insert(
        chunk_id,
        vec!["Needle setting: alpha.path = /srv/alpha and port = 9407".to_string()],
    );

    apply_graph_evidence_texts_to_chunks(
        std::slice::from_mut(&mut chunk),
        &evidence_texts_by_chunk,
        &["alpha".to_string(), "9407".to_string()],
        &["alpha".to_string(), "9407".to_string()],
    );

    assert!(chunk.source_text.starts_with("Needle setting: alpha.path"));
    assert!(chunk.source_text.contains("Source chunk:"));
    assert!(chunk.excerpt.contains("alpha.path"));
}

#[test]
fn graph_evidence_text_overlay_focuses_long_mixed_rows() {
    let mut chunk = runtime_chunk("generic support chunk", 1.0);
    let chunk_id = chunk.chunk_id;
    let filler = (0..80)
        .map(|index| format!("Generic section {index}: profile forms and archive notes."))
        .collect::<Vec<_>>()
        .join(" | ");
    let mut evidence_texts_by_chunk = HashMap::new();
    evidence_texts_by_chunk.insert(
        chunk_id,
        vec![format!(
            "{filler} | Sample connectors: Option One, Option Two, custom route | More unrelated material."
        )],
    );

    apply_graph_evidence_texts_to_chunks(
        std::slice::from_mut(&mut chunk),
        &evidence_texts_by_chunk,
        &["sample".to_string(), "connectors".to_string()],
        &["sample".to_string(), "connectors".to_string(), "route".to_string()],
    );

    assert!(chunk.source_text.contains("Sample connectors"));
    assert!(chunk.source_text.contains("custom route"));
    assert!(!chunk.source_text.contains("Generic section 0"));
}

#[test]
fn graph_evidence_chunk_hits_use_ranked_text_rows_as_chunk_candidates() {
    let target_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let mut row = runtime_graph_evidence_row(
        target_id,
        "Needle setting: alpha.path = /srv/alpha and port = 9407",
    );
    row.chunk_id = Some(chunk_id);

    let (hits, evidence_texts_by_chunk) = graph_evidence_chunk_hits_from_rows(&[row]);

    assert_eq!(hits, vec![(chunk_id, graph_evidence_chunk_score(0))]);
    assert_eq!(
        evidence_texts_by_chunk.get(&chunk_id).and_then(|texts| texts.first()),
        Some(&"Needle setting: alpha.path = /srv/alpha and port = 9407".to_string())
    );
}

#[test]
fn graph_evidence_source_document_ids_preserve_rank_order_without_duplicates() {
    let target_id = Uuid::now_v7();
    let first_document_id = Uuid::now_v7();
    let second_document_id = Uuid::now_v7();
    let mut first = runtime_graph_evidence_row(target_id, "Alpha setup overview");
    first.document_id = Some(first_document_id);
    first.chunk_id = None;
    let mut duplicate = runtime_graph_evidence_row(target_id, "Alpha setup detail");
    duplicate.document_id = Some(first_document_id);
    duplicate.chunk_id = Some(Uuid::now_v7());
    let mut second = runtime_graph_evidence_row(target_id, "Beta setup overview");
    second.document_id = Some(second_document_id);
    second.chunk_id = None;
    let mut orphan = runtime_graph_evidence_row(target_id, "Detached summary");
    orphan.document_id = None;

    let document_ids = graph_evidence_source_document_ids(&[first, duplicate, orphan, second]);

    assert_eq!(document_ids, vec![first_document_id, second_document_id]);
}

#[test]
fn graph_evidence_source_document_ids_with_priority_prepend_target_documents() {
    let target_id = Uuid::now_v7();
    let ranked_first_document_id = Uuid::now_v7();
    let target_document_id = Uuid::now_v7();
    let mut ranked_first = runtime_graph_evidence_row(target_id, "Broad setup overview");
    ranked_first.document_id = Some(ranked_first_document_id);
    let mut ranked_target = runtime_graph_evidence_row(target_id, "Focused setup detail");
    ranked_target.document_id = Some(target_document_id);

    let document_ids = graph_evidence_source_document_ids_with_priority(
        &[target_document_id],
        &[ranked_first, ranked_target],
    );

    assert_eq!(document_ids, vec![target_document_id, ranked_first_document_id]);
}

#[test]
fn graph_evidence_text_search_document_scope_prefers_explicit_targets() {
    let explicit_a = Uuid::now_v7();
    let explicit_b = Uuid::now_v7();
    let derived_a = Uuid::now_v7();
    let derived_b = Uuid::now_v7();
    let explicit = BTreeSet::from([explicit_a, explicit_b]);

    let scoped =
        graph_evidence_text_search_document_scope(&explicit, &[derived_a, derived_b, derived_a]);

    assert_eq!(scoped, vec![explicit_a, explicit_b]);
}

#[test]
fn graph_evidence_text_search_document_scope_dedupes_derived_targets() {
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();

    let scoped =
        graph_evidence_text_search_document_scope(&BTreeSet::new(), &[first, second, first]);

    assert_eq!(scoped, vec![first, second]);
}

#[test]
fn graph_evidence_context_line_formats_delimited_row_fields() {
    let graph_index = graph_index_with_nodes(Vec::new());
    let document_id = Uuid::now_v7();
    let row = RuntimeGraphEvidenceRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        target_kind: "node".to_string(),
        target_id: Uuid::now_v7(),
        document_id: Some(document_id),
        chunk_id: Some(Uuid::now_v7()),
        source_file_name: Some("alpha-source".to_string()),
        page_ref: None,
        evidence_text: "Column Alpha: keep A | Column Beta: keep B".to_string(),
        confidence_score: Some(1.0),
        created_at: Utc::now(),
    };
    let source_labels =
        HashMap::from([(document_id, "https://example.test/docs/alpha".to_string())]);

    let line = graph_evidence_context_line(&row, &graph_index, &source_labels, &[])
        .expect("graph evidence line");

    assert!(line.contains("[graph-evidence source=\"https://example.test/docs/alpha\"]"));
    assert!(!line.contains("alpha-source"));
    assert!(line.contains("- Column Alpha: keep A"));
    assert!(line.contains("- Column Beta: keep B"));
}

#[test]
fn graph_evidence_context_line_focuses_long_mixed_rows() {
    let target = runtime_graph_node("Sample Subject", "artifact", None);
    let graph_index = graph_index_with_nodes(vec![target.clone()]);
    let filler = (0..80)
        .map(|index| format!("Generic section {index}: record forms and archive notes."))
        .collect::<Vec<_>>()
        .join(" | ");
    let row = runtime_graph_evidence_row(
        target.id,
        &format!(
            "{filler} | Sample connectors: Option One, Option Two, custom route | More unrelated material after the focused sentence."
        ),
    );

    let line = graph_evidence_context_line(
        &row,
        &graph_index,
        &HashMap::new(),
        &["sample".to_string(), "connectors".to_string(), "route".to_string()],
    )
    .expect("graph evidence line");

    assert!(line.contains("Sample connectors"));
    assert!(line.contains("custom route"));
    assert!(!line.contains("Generic section 0"));
}

#[test]
fn graph_evidence_source_document_ids_from_scored_targets_promote_structural_target_document() {
    let broad_target = runtime_graph_node("General configuration", "concept", None);
    let focused_target = runtime_graph_node("Needle gateway", "artifact", None);
    let graph_index = graph_index_with_nodes(vec![broad_target.clone(), focused_target.clone()]);
    let target_entity_profiles =
        graph_target_entity_profiles(Some(&target_entities_query_ir(&["Needle"])), &graph_index);
    let broad_document_id = Uuid::now_v7();
    let focused_document_id = Uuid::now_v7();
    let mut broad = runtime_graph_evidence_row(
        broad_target.id,
        "Configuration appendix | file: /srv/common.ini | option: enabled",
    );
    broad.document_id = Some(broad_document_id);
    broad.source_file_name = Some("general configuration".to_string());
    let mut focused = runtime_graph_evidence_row(
        focused_target.id,
        "Gateway setup | file: /srv/needle.ini | option: enabled",
    );
    focused.document_id = Some(focused_document_id);
    focused.source_file_name = Some("needle gateway setup".to_string());

    let document_ids = graph_evidence_source_document_ids_from_scored_targets(
        &[broad, focused],
        "Where is the Needle gateway configuration file?",
        &["Needle gateway configuration".to_string()],
        &graph_index,
        &target_entity_profiles,
    );

    assert_eq!(document_ids.first(), Some(&focused_document_id));
}

#[test]
fn graph_evidence_source_document_ids_from_scored_targets_skip_zero_signal_documents() {
    let target = runtime_graph_node("Archive index", "concept", None);
    let graph_index = graph_index_with_nodes(vec![target.clone()]);
    let mut row = runtime_graph_evidence_row(target.id, "Archive index | owner: operations");
    row.document_id = Some(Uuid::now_v7());

    let document_ids = graph_evidence_source_document_ids_from_scored_targets(
        &[row],
        "Where is the Needle gateway configuration file?",
        &["Needle gateway configuration".to_string()],
        &graph_index,
        &[],
    );

    assert!(document_ids.is_empty());
}

#[test]
fn graph_evidence_context_ranking_prefers_specific_rare_row_over_repeated_generic_rows() {
    let specific = runtime_graph_node("Alpha mismatch status", "condition", None);
    let generic = runtime_graph_node("Checkout behavior", "field", None);
    let graph_index = graph_index_with_nodes(vec![specific.clone(), generic.clone()]);
    let generic_body =
        "Row 12 | Status: Unknown status | Sale behavior: hold item | Return behavior: hold item";
    let generic_a = runtime_graph_evidence_row(generic.id, generic_body);
    let generic_b = runtime_graph_evidence_row(generic.id, generic_body);
    let exact = runtime_graph_evidence_row(
        specific.id,
        "Row 7 | Status: Alpha mismatch status | Sale behavior: hold item | Return behavior: release item",
    );

    let ranked = rank_graph_evidence_context_rows(
        &[generic_a, generic_b, exact.clone()],
        &[],
        "What are sale and return behavior for Alpha mismatch status?",
        &["Alpha mismatch status".to_string()],
        &graph_index,
        &[],
        4,
    );

    assert_eq!(ranked.first().map(|row| row.id), Some(exact.id));
    assert_eq!(ranked.len(), 2);
}

#[test]
fn graph_evidence_context_ranking_keeps_source_metadata_below_evidence_body() {
    let specific = runtime_graph_node("Beta release channel", "condition", None);
    let generic = runtime_graph_node("Release notes", "document", None);
    let graph_index = graph_index_with_nodes(vec![specific.clone(), generic.clone()]);
    let mut source_only = runtime_graph_evidence_row(
        generic.id,
        "Row 2 | Status: general note | Action: inspect archive",
    );
    source_only.source_file_name = Some("Beta release channel index".to_string());
    let exact = runtime_graph_evidence_row(
        specific.id,
        "Row 4 | Status: Beta release channel | Action: enable canary rollout",
    );

    let ranked = rank_graph_evidence_context_rows(
        &[source_only, exact.clone()],
        &[],
        "Which action belongs to Beta release channel?",
        &["Beta release channel".to_string()],
        &graph_index,
        &[],
        4,
    );

    assert_eq!(ranked.first().map(|row| row.id), Some(exact.id));
}

#[test]
fn graph_evidence_context_ranking_keeps_multi_anchor_target_row_under_body_dedupe_pressure() {
    let composite = runtime_graph_node("Beacon crossed Harbor Delta", "event", None);
    let generic = runtime_graph_node("Harbor Delta", "location", None);
    let graph_index = graph_index_with_nodes(vec![generic.clone(), composite.clone()]);
    let shared_evidence = "event: Beacon crossed Harbor Delta after the calibration window closed";
    let generic_row = runtime_graph_evidence_row(generic.id, shared_evidence);
    let composite_row = runtime_graph_evidence_row(composite.id, shared_evidence);
    let ir = target_entities_query_ir(&["Beacon", "Harbor Delta"]);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);

    let ranked = rank_graph_evidence_context_rows(
        &[generic_row],
        std::slice::from_ref(&composite_row),
        "Which event involved Beacon and Harbor Delta?",
        &["Beacon Harbor Delta".to_string()],
        &graph_index,
        &profiles,
        1,
    );

    assert_eq!(ranked.first().map(|row| row.id), Some(composite_row.id));
}

#[test]
fn query_ir_focus_ignores_weak_document_focus_when_typed_focus_exists() {
    let mut ir = release_query_ir(None, Some("Needle Server"));
    ir.document_focus = Some(DocumentHint { hint: "Neighbor Document".to_string() });

    let focus_queries = query_ir_lexical_focus_queries(&ir);
    let search_queries = query_ir_focus_search_queries(
        "broad question mentioning Neighbor Document",
        &focus_queries,
    );

    assert!(focus_queries.contains(&"Needle Server".to_string()));
    assert!(!focus_queries.contains(&"Neighbor Document".to_string()));
    assert_eq!(search_queries, vec!["Needle Server".to_string()]);
}

#[test]
fn configure_focus_queries_do_not_inject_low_idf_configuration_markers() {
    let mut ir = release_query_ir(None, None);
    ir.act = QueryAct::ConfigureHow;
    ir.scope = QueryScope::SingleDocument;
    ir.document_focus = Some(DocumentHint { hint: "Payment Gateway".to_string() });
    ir.target_types = vec!["procedure".to_string()];
    ir.target_entities =
        vec![EntityMention { label: "settlement connector".to_string(), role: EntityRole::Object }];

    let focus_queries = query_ir_lexical_focus_queries(&ir);

    assert!(focus_queries.contains(&"settlement connector".to_string()));
    assert!(focus_queries.contains(&"Payment Gateway".to_string()));
    assert!(!focus_queries.contains(&"http".to_string()));
    assert!(!focus_queries.contains(&"settlement connector http".to_string()));
    assert!(!focus_queries.contains(&"settlement connector .conf".to_string()));
}

#[test]
fn lexical_queries_strip_leading_question_markers() {
    let plan = build_query_plan("Q16. Which ports should a terminal use?", None, None, None);
    let queries =
        build_lexical_queries("Q16. Which ports should a terminal use?", &plan, &[], None);

    assert_eq!(queries.first().map(String::as_str), Some("Which ports should a terminal use?"));
    assert!(!queries.iter().any(|query| query.split_whitespace().any(|term| term == "Q16.")));
    assert!(!plan.keywords.contains(&"q16".to_string()));
}

#[test]
fn entity_bio_filter_keeps_canonical_graph_evidence_without_label_substring() {
    let evidence = runtime_chunk("configuration facts only", entity_bio_chunk_score(0));
    let lexical_false_positive = runtime_chunk("forest inventory", entity_bio_chunk_score(1));
    let retained = retain_entity_bio_candidates(
        vec![evidence.clone(), lexical_false_positive],
        &HashSet::from([evidence.chunk_id]),
        &["foster".to_string()],
    );

    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].chunk_id, evidence.chunk_id);
}

#[test]
fn entity_bio_filter_still_rejects_lexical_false_positive_without_label() {
    let lexical_false_positive = runtime_chunk("forest inventory", entity_bio_chunk_score(0));
    let retained = retain_entity_bio_candidates(
        vec![lexical_false_positive],
        &HashSet::new(),
        &["foster".to_string()],
    );

    assert!(retained.is_empty());
}

#[test]
fn document_identity_scores_stay_above_identity_floor_and_preserve_order() {
    let first = document_identity_chunk_score(0, 0);
    let second = document_identity_chunk_score(0, 1);
    let next_document = document_identity_chunk_score(1, 0);

    assert!(first >= DOCUMENT_IDENTITY_SCORE_FLOOR);
    assert!(first > second);
    assert!(second > next_document);
}

#[test]
fn merge_chunks_normalizes_ordinary_scores() {
    let first = runtime_chunk("first", 10_000.0);
    let second = runtime_chunk("second", 9_000.0);

    let merged = merge_chunks(vec![first], vec![second], 8);

    assert!(merged.iter().all(|chunk| chunk.score.is_some_and(|score| score < 1.0)));
}

#[test]
fn merge_canonical_table_aggregation_chunks_prefers_table_analytics() {
    let document_id = Uuid::now_v7();
    let heading = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "records-b.xlsx".to_string(),
        excerpt: "records-b".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: "records-b".to_string(),
    };
    let summary = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "records-b.xlsx".to_string(),
        excerpt: "City".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: "Table Summary | Sheet: records-b | Column: City | Value Kind: categorical | Value Shape: label | Aggregation Priority: 2 | Row Count: 100 | Non-empty Count: 100 | Distinct Count: 100 | Most Frequent Count: 1 | Most Frequent Tie Count: 100".to_string(),
    };
    let row = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "records-b.xlsx".to_string(),
        excerpt: "Row 1".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: "Sheet: records-b | Row 1 | City: Acevedoville".to_string(),
    };

    let merged = merge_canonical_table_aggregation_chunks(
        vec![heading],
        vec![summary.clone()],
        vec![row.clone()],
        8,
    );

    assert_eq!(merged.len(), 2);
    assert!(merged.iter().all(is_table_analytics_chunk));
    let merged_ids = merged.into_iter().map(|chunk| chunk.chunk_id).collect::<BTreeSet<_>>();
    assert_eq!(merged_ids, BTreeSet::from([summary.chunk_id, row.chunk_id]));
}

#[test]
fn merge_canonical_table_aggregation_chunks_keeps_existing_when_no_direct_analytics_exist() {
    let heading = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: "records-b.xlsx".to_string(),
        excerpt: "records-b".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: "records-b".to_string(),
    };

    let merged =
        merge_canonical_table_aggregation_chunks(vec![heading.clone()], Vec::new(), Vec::new(), 8);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].chunk_id, heading.chunk_id);
}

#[test]
fn vector_search_always_runs_regardless_of_exact_literal_flag() {
    // Canonical contract since v0.3.3: vector retrieval is always
    // exercised alongside lexical. The `exact_literal_technical` flag
    // on the intent profile influences ranking/boost, never excludes
    // the whole vector lane. Skipping the vector lane on exact-literal
    // questions causes BM25 stem collisions to promote unrelated
    // templates over the actual configuration sections.
    let mut literal_plan = RuntimeQueryPlan {
        requested_mode: crate::domains::query::RuntimeQueryMode::Document,
        planned_mode: crate::domains::query::RuntimeQueryMode::Document,
        intent_profile: QueryIntentProfile::default(),
        keywords: vec!["endpoint".to_string()],
        high_level_keywords: vec!["endpoint".to_string()],
        low_level_keywords: vec!["system".to_string()],
        entity_keywords: Vec::new(),
        concept_keywords: Vec::new(),
        top_k: 8,
        context_budget_chars: 4_000,
        hyde_recommended: false,
    };

    assert!(!should_skip_vector_search(&literal_plan));
    literal_plan.intent_profile.exact_literal_technical = true;
    assert!(!should_skip_vector_search(&literal_plan));
}

#[test]
fn chunk_retrieval_lanes_continue_when_vector_lane_fails() {
    let lexical_chunk = runtime_chunk("lexical", 0.8);

    let outcome = combine_chunk_retrieval_lanes(
        Err(anyhow::anyhow!("vector backend unavailable")),
        Ok((vec![lexical_chunk.clone()], 3, 42)),
    )
    .expect("lexical lane should keep retrieval usable");

    assert_eq!(outcome.degraded_lane_count, 1);
    assert!(outcome.vector_hits.is_empty());
    assert_eq!(outcome.lexical_hits.len(), 1);
    assert_eq!(outcome.lexical_hits[0].chunk_id, lexical_chunk.chunk_id);
    assert_eq!(outcome.lexical_query_count, 3);
    assert_eq!(outcome.lexical_elapsed_ms, 42);
}

#[test]
fn chunk_retrieval_lanes_continue_when_lexical_lane_fails() {
    let vector_chunk = runtime_chunk("vector", 0.9);

    let outcome = combine_chunk_retrieval_lanes(
        Ok((vec![vector_chunk.clone()], 17)),
        Err(anyhow::anyhow!("lexical backend unavailable")),
    )
    .expect("vector lane should keep retrieval usable");

    assert_eq!(outcome.degraded_lane_count, 1);
    assert_eq!(outcome.vector_hits.len(), 1);
    assert_eq!(outcome.vector_hits[0].chunk_id, vector_chunk.chunk_id);
    assert!(outcome.lexical_hits.is_empty());
    assert_eq!(outcome.vector_elapsed_ms, 17);
}

#[test]
fn chunk_retrieval_lanes_fail_when_both_lanes_fail() {
    let result = combine_chunk_retrieval_lanes(
        Err(anyhow::anyhow!("vector backend unavailable")),
        Err(anyhow::anyhow!("lexical backend unavailable")),
    );
    let error = match result {
        Ok(_) => panic!("both failed lanes must fail retrieval"),
        Err(error) => error,
    };

    let message = format!("{error:#}");
    assert!(message.contains("all chunk retrieval lanes failed"));
    assert!(message.contains("vector backend unavailable"));
    assert!(message.contains("lexical backend unavailable"));
}

#[test]
fn lexical_query_results_keep_successful_subqueries_when_one_fails() {
    let first = runtime_chunk("first", 0.9);
    let second = runtime_chunk("second", 0.7);

    let hits = combine_lexical_query_results(
        vec![
            Ok(vec![first.clone()]),
            Err(anyhow::anyhow!("one lexical subquery failed")),
            Ok(vec![second.clone()]),
        ],
        3,
        8,
    )
    .expect("successful lexical subqueries should keep the lane usable");

    let ids = hits.into_iter().map(|chunk| chunk.chunk_id).collect::<BTreeSet<_>>();
    assert_eq!(ids, BTreeSet::from([first.chunk_id, second.chunk_id]));
}

#[test]
fn lexical_query_results_fail_when_all_subqueries_fail() {
    let result = combine_lexical_query_results(
        vec![
            Err(anyhow::anyhow!("first lexical subquery failed")),
            Err(anyhow::anyhow!("second lexical subquery failed")),
        ],
        2,
        8,
    );
    let error = match result {
        Ok(_) => panic!("all failed lexical subqueries must fail the lane"),
        Err(error) => error,
    };

    let message = format!("{error:#}");
    assert!(message.contains("all lexical chunk search queries failed"));
    assert!(message.contains("first lexical subquery failed"));
    assert!(message.contains("second lexical subquery failed"));
}

#[test]
fn query_ir_focus_search_results_keep_successful_subqueries_when_one_fails() {
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();

    let hits = combine_query_ir_focus_search_results(
        vec![
            Ok(vec![(first, 0.0)]),
            Err(anyhow::anyhow!("one focus subquery failed")),
            Ok(vec![(second, 0.7)]),
        ],
        3,
    )
    .expect("successful focus subqueries should keep the additive lane usable");

    let ids = hits.iter().map(|(chunk_id, _)| *chunk_id).collect::<BTreeSet<_>>();
    assert_eq!(ids, BTreeSet::from([first, second]));
    assert_eq!(hits[0].1, query_ir_focus_chunk_score(0));
    assert_eq!(hits[1].1, 0.7);
}

#[test]
fn query_ir_focus_search_results_fail_when_all_subqueries_fail() {
    let result = combine_query_ir_focus_search_results(
        vec![
            Err(anyhow::anyhow!("first focus subquery failed")),
            Err(anyhow::anyhow!("second focus subquery failed")),
        ],
        2,
    );
    let error = match result {
        Ok(_) => panic!("all failed focus subqueries must fail the focus lane"),
        Err(error) => error,
    };

    let message = format!("{error:#}");
    assert!(message.contains("all query-IR focus chunk searches failed"));
    assert!(message.contains("first focus subquery failed"));
    assert!(message.contains("second focus subquery failed"));
}

fn sample_document_row(file_name: &str, title: &str) -> KnowledgeDocumentRow {
    let document_id = Uuid::now_v7();
    KnowledgeDocumentRow {
        key: document_id.to_string(),
        arango_id: None,
        arango_rev: None,
        document_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        external_key: document_id.to_string(),
        file_name: Some(file_name.to_string()),
        title: Some(title.to_string()),
        source_uri: None,
        document_hint: None,
        document_state: "active".to_string(),
        active_revision_id: Some(Uuid::now_v7()),
        readable_revision_id: Some(Uuid::now_v7()),
        latest_revision_no: Some(1),
        parent_document_id: None,
        document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        deleted_at: None,
    }
}

fn knowledge_chunk_row(
    document: &KnowledgeDocumentRow,
    chunk_index: i32,
    chunk_kind: Option<&str>,
    text: &str,
) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
        chunk_id: Uuid::now_v7(),
        workspace_id: document.workspace_id,
        library_id: document.library_id,
        document_id: document.document_id,
        revision_id: document.readable_revision_id.unwrap_or_else(Uuid::now_v7),
        chunk_index,
        chunk_kind: chunk_kind.map(str::to_string),
        content_text: text.to_string(),
        normalized_text: text.to_string(),
        span_start: None,
        span_end: None,
        token_count: Some(text.split_whitespace().count() as i32),
        support_block_ids: Vec::new(),
        section_path: Vec::new(),
        heading_trail: Vec::new(),
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: Some(1),
        vector_generation: Some(1),
        quality_score: Some(1.0),
        window_text: None,
        raptor_level: None,
        occurred_at: None,
        occurred_until: None,
    }
}

fn runtime_graph_node(
    label: &str,
    node_type: &str,
    summary: Option<&str>,
) -> RuntimeGraphQueryNodeRow {
    RuntimeGraphQueryNodeRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        canonical_key: format!("{node_type}:{}", label.to_lowercase()),
        label: label.to_string(),
        node_type: node_type.to_string(),
        aliases_json: json!([]),
        summary: summary.map(str::to_string),
        support_count: 1,
        projection_version: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn runtime_graph_evidence_row(target_id: Uuid, evidence_text: &str) -> RuntimeGraphEvidenceRow {
    RuntimeGraphEvidenceRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        target_kind: "node".to_string(),
        target_id,
        document_id: Some(Uuid::now_v7()),
        chunk_id: Some(Uuid::now_v7()),
        source_file_name: Some("synthetic-source".to_string()),
        page_ref: None,
        evidence_text: evidence_text.to_string(),
        confidence_score: Some(1.0),
        created_at: Utc::now(),
    }
}

fn graph_index_with_nodes(nodes: Vec<RuntimeGraphQueryNodeRow>) -> QueryGraphIndex {
    let node_positions =
        nodes.iter().enumerate().map(|(position, node)| (node.id, position)).collect();
    QueryGraphIndex::new(
        std::sync::Arc::new(ActiveRuntimeGraphProjection { nodes, edges: Vec::new() }),
        node_positions,
        Default::default(),
    )
}
