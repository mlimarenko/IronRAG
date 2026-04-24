use std::collections::{BTreeSet, HashMap};

use chrono::Utc;
use uuid::Uuid;

use super::super::{
    is_table_analytics_chunk, merge_canonical_table_aggregation_chunks,
    requested_initial_table_row_count,
};
use super::{
    DOCUMENT_IDENTITY_SCORE_FLOOR, canonical_document_revision_id, chunk_answer_source_text,
    explicit_target_document_ids, latest_version_documents, map_chunk_hit, merge_chunks,
};
use crate::infra::arangodb::document_store::{KnowledgeChunkRow, KnowledgeDocumentRow};
use crate::services::query::{
    execution::{
        RuntimeMatchedChunk, normalized_document_target_candidates, should_skip_vector_search,
    },
    latest_versions::{
        compare_version_desc, extract_semver_like_version, latest_version_chunk_score,
        latest_version_context_top_k, latest_version_family_key, latest_version_scope_terms,
        question_requests_latest_versions, requested_latest_version_count,
        text_has_release_version_marker,
    },
    planner::{QueryIntentProfile, RuntimeQueryPlan},
};

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
    };

    assert_eq!(chunk_answer_source_text(&chunk), "test1");
}

#[test]
fn explicit_target_document_ids_match_exact_file_name() {
    let document = sample_document_row("people-100.csv", "people-100.csv");
    let document_index = HashMap::from([(document.document_id, document.clone())]);

    let targeted = explicit_target_document_ids(
        "В people-100.csv какая должность у Shelby Terrell?",
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
fn requested_initial_table_row_count_detects_russian_row_ranges() {
    assert_eq!(
        requested_initial_table_row_count("Покажи значения из первых 5 строк sample-heavy-1.xls."),
        Some(5)
    );
}

#[test]
fn requested_initial_table_row_count_detects_english_row_ranges() {
    assert_eq!(
        requested_initial_table_row_count("Show the first 7 rows from people-100.csv."),
        Some(7)
    );
}

#[test]
fn latest_version_question_detection_supports_russian_and_english() {
    assert!(question_requests_latest_versions("Что нового в последних 5 релизах?"));
    assert!(question_requests_latest_versions("latest 3 release notes"));
    assert!(!question_requests_latest_versions("как настроить оплату"));
}

#[test]
fn requested_latest_version_count_defaults_and_caps() {
    assert_eq!(requested_latest_version_count("последние релизы"), 5);
    assert_eq!(requested_latest_version_count("последние 3 версии"), 3);
    assert_eq!(requested_latest_version_count("latest 100 releases"), 10);
    assert_eq!(requested_latest_version_count("latest version 9.8.765"), 5);
    assert_eq!(requested_latest_version_count("latest 2024.10 release"), 5);
}

#[test]
fn latest_version_chunk_merge_limit_preserves_requested_document_coverage() {
    assert_eq!(latest_version_context_top_k("latest 10 releases", 8), 40);
    assert_eq!(latest_version_context_top_k("latest 3 releases", 20), 20);
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

    let selected =
        latest_version_documents(&index, 5, &latest_version_scope_terms("latest alpha release"));
    let titles = selected.into_iter().map(|document| document.title).collect::<Vec<_>>();

    assert_eq!(titles, vec!["Alpha Version 9.8.765".to_string()]);
    assert!(!text_has_release_version_marker("OAuth 2.0 Guide"));
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
        &latest_version_scope_terms("что нового в последних релизах, дай список изменений"),
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
fn map_chunk_hit_skips_noncanonical_revision_chunks() {
    let document = sample_document_row("people-100.csv", "people-100.csv");
    let canonical_revision_id = canonical_document_revision_id(&document).unwrap();
    let stale_revision_id = Uuid::now_v7();
    assert_ne!(canonical_revision_id, stale_revision_id);
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
    };

    assert!(map_chunk_hit(chunk, 1.0, &document_index, &[]).is_none());
}

fn runtime_chunk(label: &str, score: f32) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        document_id: Uuid::now_v7(),
        document_label: label.to_string(),
        excerpt: label.to_string(),
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
        document_id,
        document_label: "customers-100.xlsx".to_string(),
        excerpt: "customers-100".to_string(),
        score: Some(1.0),
        source_text: "customers-100".to_string(),
    };
    let summary = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        document_id,
        document_label: "customers-100.xlsx".to_string(),
        excerpt: "City".to_string(),
        score: Some(1.0),
        source_text: "Table Summary | Sheet: customers-100 | Column: City | Value Kind: categorical | Value Shape: label | Aggregation Priority: 2 | Row Count: 100 | Non-empty Count: 100 | Distinct Count: 100 | Most Frequent Count: 1 | Most Frequent Tie Count: 100".to_string(),
    };
    let row = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        document_id,
        document_label: "customers-100.xlsx".to_string(),
        excerpt: "Row 1".to_string(),
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
        document_id: Uuid::now_v7(),
        document_label: "customers-100.xlsx".to_string(),
        excerpt: "customers-100".to_string(),
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
    // the whole vector lane. Prod smoke on short configure-style
    // Russian questions showed the old skip-vector-on-exact-literal
    // path caused relevant config chunks to miss top-10 (BM25 stem
    // collision on `настро*` promoted unrelated templates over the
    // actual configuration sections).
    let mut literal_plan = RuntimeQueryPlan {
        requested_mode: crate::domains::query::RuntimeQueryMode::Document,
        planned_mode: crate::domains::query::RuntimeQueryMode::Document,
        intent_profile: QueryIntentProfile::default(),
        keywords: vec!["endpoint".to_string()],
        high_level_keywords: vec!["endpoint".to_string()],
        low_level_keywords: vec!["system".to_string()],
        entity_keywords: Vec::new(),
        concept_keywords: Vec::new(),
        expanded_keywords: Vec::new(),
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
