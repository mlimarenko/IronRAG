use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use super::super::{
    is_table_analytics_chunk, merge_canonical_table_aggregation_chunks,
    requested_initial_table_row_count,
};
use super::{
    DOCUMENT_IDENTITY_SCORE_FLOOR, RuntimeChunkScoreKind, apply_graph_evidence_texts_to_chunks,
    canonical_document_revision_id, chunk_answer_source_text, document_identity_chunk_score,
    entity_bio_chunk_score, explicit_target_document_ids, graph_evidence_chunk_hits_from_rows,
    graph_evidence_chunk_score, graph_evidence_context_line, graph_evidence_targets,
    graph_evidence_targets_for_query, graph_target_entity_profiles, latest_version_documents,
    map_chunk_hit, merge_chunks, merge_entity_bio_chunks, merge_graph_evidence_chunks,
    merge_query_ir_focus_chunks, query_ir_focus_search_queries, query_ir_lexical_focus_queries,
    rank_graph_evidence_context_rows, retain_canonical_document_head_chunks,
    retain_entity_bio_candidates, truncate_bundle,
};
use crate::domains::query_ir::{
    DocumentHint, EntityMention, EntityRole, LiteralKind, LiteralSpan, QueryAct, QueryIR,
    QueryLanguage, QueryScope,
};
use crate::infra::{
    arangodb::document_store::{KnowledgeChunkRow, KnowledgeDocumentRow},
    repositories::{RuntimeGraphEvidenceRow, RuntimeGraphNodeRow},
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

fn release_query_ir(count: Option<&str>, entity: Option<&str>) -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::MultiDocument,
        language: QueryLanguage::Auto,
        target_types: vec!["release".to_string()],
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
        confidence: 1.0,
    }
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
        confidence: 1.0,
    }
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
fn explicit_target_document_ids_match_exact_file_name() {
    let document = sample_document_row("people-100.csv", "people-100.csv");
    let document_index = HashMap::from([(document.document_id, document.clone())]);

    let targeted = explicit_target_document_ids(
        "In people-100.csv what is Shelby Terrell's job title?",
        &document_index,
    );

    assert_eq!(targeted, BTreeSet::from([document.document_id]));
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
fn requested_initial_table_row_count_detects_english_row_ranges() {
    assert_eq!(
        requested_initial_table_row_count("Show the first 7 rows from people-100.csv."),
        Some(7)
    );
}

#[test]
fn latest_version_question_detection_uses_query_ir() {
    assert!(query_requests_latest_versions(&release_query_ir(Some("5"), None)));
    let mut ir = release_query_ir(None, None);
    ir.target_types.clear();
    assert!(!query_requests_latest_versions(&ir));
}

#[test]
fn requested_latest_version_count_defaults_and_caps() {
    assert_eq!(requested_latest_version_count(&release_query_ir(None, None)), 5);
    assert_eq!(requested_latest_version_count(&release_query_ir(Some("3"), None)), 3);
    assert_eq!(requested_latest_version_count(&release_query_ir(Some("100"), None)), 10);
    assert_eq!(requested_latest_version_count(&release_query_ir(Some("2024"), None)), 5);
}

#[test]
fn latest_version_chunk_merge_limit_preserves_requested_document_coverage() {
    assert_eq!(latest_version_context_top_k(&release_query_ir(Some("10"), None), 8), 40);
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
    assert_eq!(extract_semver_like_version("No release number"), None);
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
fn latest_version_documents_choose_dominant_release_family_for_multi_release_queries() {
    let docs = [
        sample_document_row("alpha-1.2.12.html", "Version 1.2.12 - Alpha Suite"),
        sample_document_row("alpha-1.2.11.html", "Version 1.2.11 - Alpha Suite"),
        sample_document_row("alpha-1.2.10.html", "Version 1.2.10 - Alpha Suite"),
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
            "Version 1.2.12 - Alpha Suite".to_string(),
            "Version 1.2.11 - Alpha Suite".to_string(),
            "Version 1.2.10 - Alpha Suite".to_string(),
        ]
    );
}

#[test]
fn latest_version_family_key_normalizes_only_the_version_literal() {
    assert_eq!(
        latest_version_family_key("Version 1.2.12 - Alpha Suite"),
        latest_version_family_key("Version 1.2.11 - Alpha Suite")
    );
    assert_ne!(
        latest_version_family_key("Version 1.2.12 - Alpha Suite"),
        latest_version_family_key("Version 1.2.12 - Beta Suite")
    );
}

#[test]
fn map_chunk_hit_drops_orphan_documents_without_heads() {
    // Contract update: `map_chunk_hit` no longer compares
    // `chunk.revision_id` against the canonical head — strict equality
    // dropped historical chunks for documents whose newer head revision
    // is a subset of an older complete revision (verified on stage:
    // ~80% of leader_lm chunks were silently hidden). Now the guard
    // only drops chunks whose document has NO head at all (orphan).
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
        normalized_text: "Sheet: people | Row 1 | Name: Stale".to_string(),
        span_start: None,
        span_end: None,
        token_count: Some(4),
        support_block_ids: Vec::new(),
        section_path: vec!["people".to_string()],
        heading_trail: vec!["people".to_string()],
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

    truncate_bundle(&mut bundle, 4);

    assert!(bundle.chunks.iter().any(|chunk| chunk.chunk_id == graph_evidence.chunk_id));
    assert_eq!(bundle.chunks[0].chunk_id, graph_evidence.chunk_id);
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
            score: Some(1.0),
        },
        RuntimeMatchedEntity {
            node_id,
            label: "Archive".to_string(),
            node_type: "artifact".to_string(),
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

    let targets = graph_evidence_targets_for_query(&[], &[], &plan, None, &graph_index);

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
        score: Some(0.1),
    }];

    let targets = graph_evidence_targets_for_query(&entities, &[], &plan, None, &graph_index);

    assert_eq!(targets.first(), Some(&("node".to_string(), bundle_node.id)));
    assert!(targets.contains(&("node".to_string(), query_node.id)));
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

    let targets = graph_evidence_targets_for_query(&[], &[], &plan, Some(&ir), &graph_index);

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
    );

    assert!(chunk.source_text.starts_with("Needle setting: alpha.path"));
    assert!(chunk.source_text.contains("Source chunk:"));
    assert!(chunk.excerpt.contains("alpha.path"));
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
fn graph_evidence_context_line_formats_delimited_row_fields() {
    let graph_index = graph_index_with_nodes(Vec::new());
    let row = RuntimeGraphEvidenceRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        target_kind: "node".to_string(),
        target_id: Uuid::now_v7(),
        document_id: Some(Uuid::now_v7()),
        chunk_id: Some(Uuid::now_v7()),
        source_file_name: Some("alpha-source".to_string()),
        page_ref: None,
        evidence_text: "Column Alpha: keep A | Column Beta: keep B".to_string(),
        confidence_score: Some(1.0),
        created_at: Utc::now(),
    };

    let line = graph_evidence_context_line(&row, &graph_index).expect("graph evidence line");

    assert!(line.contains("[graph-evidence source=\"alpha-source\"]"));
    assert!(line.contains("- Column Alpha: keep A"));
    assert!(line.contains("- Column Beta: keep B"));
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
        &[composite_row.clone()],
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
        document_label: "customers-100.xlsx".to_string(),
        excerpt: "customers-100".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: "customers-100".to_string(),
    };
    let summary = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "customers-100.xlsx".to_string(),
        excerpt: "City".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: "Table Summary | Sheet: customers-100 | Column: City | Value Kind: categorical | Value Shape: label | Aggregation Priority: 2 | Row Count: 100 | Non-empty Count: 100 | Distinct Count: 100 | Most Frequent Count: 1 | Most Frequent Tie Count: 100".to_string(),
    };
    let row = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "customers-100.xlsx".to_string(),
        excerpt: "Row 1".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: "Sheet: customers-100 | Row 1 | City: Acevedoville".to_string(),
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
        document_label: "customers-100.xlsx".to_string(),
        excerpt: "customers-100".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: "customers-100".to_string(),
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
        document_state: "active".to_string(),
        active_revision_id: Some(Uuid::now_v7()),
        readable_revision_id: Some(Uuid::now_v7()),
        latest_revision_no: Some(1),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        deleted_at: None,
    }
}

fn runtime_graph_node(label: &str, node_type: &str, summary: Option<&str>) -> RuntimeGraphNodeRow {
    RuntimeGraphNodeRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        canonical_key: format!("{node_type}:{}", label.to_lowercase()),
        label: label.to_string(),
        node_type: node_type.to_string(),
        aliases_json: json!([]),
        summary: summary.map(str::to_string),
        metadata_json: json!({}),
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

fn graph_index_with_nodes(nodes: Vec<RuntimeGraphNodeRow>) -> QueryGraphIndex {
    let node_positions =
        nodes.iter().enumerate().map(|(position, node)| (node.id, position)).collect();
    QueryGraphIndex::new(
        std::sync::Arc::new(ActiveRuntimeGraphProjection { nodes, edges: Vec::new() }),
        node_positions,
        Default::default(),
    )
}
