use uuid::Uuid;

use super::{
    RevisionTableGraphContext, build_revision_table_graph_context, test_graph_input_hashes_by_chunk,
};
use crate::{
    infra::knowledge_rows::{KnowledgeChunkRow, KnowledgeStructuredBlockRow},
    shared::extraction::table_graph::build_graph_table_row_text,
};

fn make_chunk(normalized_text: &str) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        chunk_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("table_row".to_string()),
        content_text: String::new(),
        normalized_text: normalized_text.to_string(),
        span_start: None,
        span_end: None,
        token_count: None,
        support_block_ids: Vec::new(),
        section_path: Vec::new(),
        heading_trail: Vec::new(),
        literal_digest: None,
        chunk_state: "ready".to_string(),
        text_generation: None,
        vector_generation: None,
        quality_score: None,

        window_text: None,

        occurred_at: None,
        occurred_until: None,
    }
}

fn make_text_chunk(normalized_text: &str) -> KnowledgeChunkRow {
    let mut chunk = make_chunk(normalized_text);
    chunk.chunk_kind = Some("paragraph".to_string());
    chunk
}

fn make_kind_chunk(chunk_kind: &str, chunk_index: i32) -> KnowledgeChunkRow {
    let mut chunk = make_text_chunk(&format!("record ordinal={chunk_index} field=value"));
    chunk.chunk_kind = Some(chunk_kind.to_string());
    chunk.chunk_index = chunk_index;
    chunk
}

fn make_block(
    block_id: Uuid,
    block_kind: &str,
    normalized_text: &str,
    parent_block_id: Option<Uuid>,
) -> KnowledgeStructuredBlockRow {
    KnowledgeStructuredBlockRow {
        block_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        ordinal: 0,
        block_kind: block_kind.to_string(),
        text: normalized_text.to_string(),
        normalized_text: normalized_text.to_string(),
        heading_trail: vec![],
        section_path: vec![],
        page_number: None,
        span_start: None,
        span_end: None,
        parent_block_id,
        table_coordinates_json: None,
        code_language: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn make_structured_revision(
    source_format: &str,
) -> crate::infra::knowledge_rows::KnowledgeStructuredRevisionRow {
    let revision_id = Uuid::now_v7();
    crate::infra::knowledge_rows::KnowledgeStructuredRevisionRow {
        revision_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        preparation_state: "ready".to_string(),
        normalization_profile: "normalized".to_string(),
        source_format: source_format.to_string(),
        language_code: None,
        block_count: 0,
        chunk_count: 0,
        typed_fact_count: 0,
        outline_json: serde_json::json!({}),
        prepared_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[test]
fn table_graph_profiles_attach_summary_statistics_to_row_chunks() {
    let table_id = Uuid::now_v7();
    let row_id = Uuid::now_v7();
    let graph_context = build_revision_table_graph_context(&[
        make_block(
            row_id,
            "table_row",
            "Sheet: organizations | Row 1 | Name: Ferrell LLC | Country: Papua New Guinea | Industry: Plastics | Founded: 1972 | Website: https://price.net",
            Some(table_id),
        ),
        make_block(
            Uuid::now_v7(),
            "metadata_block",
            "Table Summary | Sheet: organizations | Column: Name | Value Kind: categorical | Value Shape: label | Aggregation Priority: 2 | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 3 | Most Frequent Count: 1 | Most Frequent Tie Count: 3 | Most Frequent Values: Ferrell LLC; Meyer Group; Adams LLC",
            Some(table_id),
        ),
        make_block(
            Uuid::now_v7(),
            "metadata_block",
            "Table Summary | Sheet: organizations | Column: Country | Value Kind: categorical | Value Shape: label | Aggregation Priority: 3 | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 2 | Most Frequent Count: 2 | Most Frequent Tie Count: 1 | Most Frequent Values: Papua New Guinea",
            Some(table_id),
        ),
        make_block(
            Uuid::now_v7(),
            "metadata_block",
            "Table Summary | Sheet: organizations | Column: Founded | Value Kind: numeric | Value Shape: identifier | Aggregation Priority: 3 | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 3 | Average: 1991.67 | Min: 1972 | Max: 2012",
            Some(table_id),
        ),
    ]);

    let mut chunk = make_chunk(
        "Sheet: organizations | Row 1 | Name: Ferrell LLC | Country: Papua New Guinea | Industry: Plastics | Founded: 1972 | Website: https://price.net",
    );
    chunk.support_block_ids = vec![row_id];

    let profile = graph_context.profile_for_chunk(&chunk).expect("profile");
    let text =
        build_graph_table_row_text(&chunk.normalized_text, Some(profile)).expect("graph text");

    assert_eq!(text, "Name: Ferrell LLC | Country: Papua New Guinea");
}

#[test]
fn row_only_table_graph_mode_activates_for_table_native_revisions() {
    let table_id = Uuid::now_v7();
    let graph_context = build_revision_table_graph_context(&[
        make_block(Uuid::now_v7(), "heading", "test1", None),
        make_block(table_id, "table", "| col_1 |\n| --- |\n| test1 |", None),
        make_block(
            Uuid::now_v7(),
            "table_row",
            "Sheet: test1 | Row 1 | col_1: test1",
            Some(table_id),
        ),
        make_block(
            Uuid::now_v7(),
            "metadata_block",
            "Table Summary | Sheet: test1 | Column: col_1 | Value Kind: categorical | Value Shape: label | Aggregation Priority: 1 | Row Count: 1 | Non-empty Count: 1 | Distinct Count: 1 | Most Frequent Count: 1 | Most Frequent Tie Count: 1 | Most Frequent Values: test1",
            Some(table_id),
        ),
    ]);

    assert!(graph_context.requires_row_only_graph());
}

#[test]
fn row_only_table_graph_mode_stays_disabled_for_mixed_markdown_and_tables() {
    let table_id = Uuid::now_v7();
    let graph_context = build_revision_table_graph_context(&[
        make_block(Uuid::now_v7(), "heading", "Inventory", None),
        make_block(Uuid::now_v7(), "paragraph", "This section summarizes the inventory.", None),
        make_block(table_id, "table", "| name | stock |\n| --- | --- |\n| Widget | 7 |", None),
        make_block(
            Uuid::now_v7(),
            "table_row",
            "Sheet: inventory | Row 1 | Name: Widget | Stock: 7",
            Some(table_id),
        ),
    ]);

    assert!(!graph_context.requires_row_only_graph());
}

#[test]
fn graph_input_eligibility_uses_current_chunk_policy() {
    let clean = make_text_chunk(
        "Alpha service stores project settings and renders a concise operational summary.",
    );
    let noisy = make_text_chunk(concat!(
        "abCDEfgH ijKLMnOp qRStuVWx yzABcDef gHIjKLmn ",
        "opQRS7tu vwXYZabC deFGhIJk lmNOPqRs tuVWxyZa ",
        "bcDEFgHi jkLMNopQ rsTUVwxy zaBCDefG"
    ));

    let graph_inputs = test_graph_input_hashes_by_chunk(
        &[clean.clone(), noisy.clone()],
        &RevisionTableGraphContext::default(),
    );

    assert!(graph_inputs.contains_key(&clean.chunk_id));
    assert!(!graph_inputs.contains_key(&noisy.chunk_id));
}

#[test]
fn graph_input_hash_changes_when_table_rendering_changes() {
    let table_block_id = Uuid::now_v7();
    let row_block_id = Uuid::now_v7();
    let mut row_chunk = make_chunk(
        "Sheet: products | Row 1 | Name: Alpha Suite | Owner: Platform | Internal Note: ignore",
    );
    row_chunk.support_block_ids = vec![row_block_id];

    let plain_input = test_graph_input_hashes_by_chunk(
        &[row_chunk.clone()],
        &RevisionTableGraphContext::default(),
    );
    let table_input = test_graph_input_hashes_by_chunk(
        &[row_chunk.clone()],
        &build_revision_table_graph_context(&[
            make_block(row_block_id, "table_row", &row_chunk.normalized_text, Some(table_block_id)),
            make_block(
                Uuid::now_v7(),
                "metadata_block",
                "Table Summary | Sheet: products | Column: Name | Value Kind: categorical | Value Shape: label | Aggregation Priority: 2 | Row Count: 1 | Non-empty Count: 1 | Distinct Count: 1 | Most Frequent Count: 1 | Most Frequent Tie Count: 1 | Most Frequent Values: Alpha Suite",
                Some(table_block_id),
            ),
        ]),
    );

    assert_ne!(plain_input.get(&row_chunk.chunk_id), table_input.get(&row_chunk.chunk_id));
}

#[test]
fn record_stream_policy_keeps_retrieval_chunks_but_bounds_graph_source_units() {
    let mut chunks = vec![make_kind_chunk("source_profile", 0)];
    chunks.extend((1..=20).map(|index| make_kind_chunk("source_unit", index)));
    let structured_revision = make_structured_revision(
        crate::shared::extraction::record_jsonl::RECORD_JSONL_SOURCE_FORMAT,
    );

    let policy =
        super::build_graph_extraction_chunk_policy(Some(&structured_revision), &chunks, &[]);

    assert!(policy.is_record_stream());
    assert_eq!(policy.selected_source_unit_count(), 12);
    assert_eq!(super::record_stream_source_units_skipped(&chunks, &policy), 8);
}

#[test]
fn record_stream_policy_requires_structured_revision_format() {
    let mut chunks = vec![make_kind_chunk("source_profile", 0)];
    chunks.extend((1..=20).map(|index| make_kind_chunk("source_unit", index)));

    let policy = super::build_graph_extraction_chunk_policy(None, &chunks, &[]);

    assert!(!policy.is_record_stream());
    assert_eq!(policy.selected_source_unit_count(), 0);
}

fn hint_row(
    node_type: &str,
    sub_type: &str,
    occurrences: i64,
) -> crate::infra::repositories::runtime_graph_repository::RuntimeGraphSubTypeHintRow {
    crate::infra::repositories::runtime_graph_repository::RuntimeGraphSubTypeHintRow {
        node_type: node_type.to_string(),
        sub_type: sub_type.to_string(),
        occurrences,
    }
}

#[test]
fn rank_sub_type_hints_keeps_top_n_per_node_type_sorted_alphabetically() {
    // Rows arrive ranked `node_type asc, occurrences desc, sub_type asc`.
    let hints = super::rank_sub_type_hints(
        vec![
            hint_row("organization", "vendor", 9),
            hint_row("organization", "customer", 7),
            hint_row("organization", "partner", 5),
            hint_row("person", "engineer", 4),
        ],
        2,
    );

    assert_eq!(hints.by_node_type.len(), 2);
    let organization = &hints.by_node_type[0];
    assert_eq!(organization.node_type, "organization");
    // Top-2 by rank (vendor, customer) then re-sorted alphabetically for a
    // stable prompt/cache.
    let sub_types: Vec<&str> =
        organization.entries.iter().map(|entry| entry.sub_type.as_str()).collect();
    assert_eq!(sub_types, vec!["customer", "vendor"]);
    assert_eq!(hints.by_node_type[1].entries.len(), 1);
}
