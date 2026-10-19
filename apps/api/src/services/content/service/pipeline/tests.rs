use uuid::Uuid;

use super::{GraphExtractionChunkPolicy, build_graph_chunk_content};
use crate::infra::knowledge_rows::KnowledgeChunkRow;

fn make_chunk(chunk_kind: &str, normalized_text: &str) -> KnowledgeChunkRow {
    KnowledgeChunkRow {
        chunk_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some(chunk_kind.to_string()),
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

#[test]
fn build_graph_chunk_content_skips_table_summary_metadata_chunks() {
    let chunk = make_chunk(
        "metadata_block",
        "Table Summary | Sheet: products | Column: Stock | Value Kind: numeric | Row Count: 100 | Average: 545.71",
    );

    assert_eq!(
        build_graph_chunk_content(&chunk, None, false, &GraphExtractionChunkPolicy::standard()),
        None
    );
}

#[test]
fn build_graph_chunk_content_skips_heading_chunks_for_row_only_table_revisions() {
    let chunk = make_chunk("heading", "test1");

    assert_eq!(
        build_graph_chunk_content(&chunk, None, true, &GraphExtractionChunkPolicy::standard()),
        None
    );
}

#[test]
fn build_graph_chunk_content_skips_low_confidence_text_without_stored_score() {
    let chunk = make_chunk(
        "paragraph",
        concat!(
            "summary topic alpha beta gamma ",
            "abCD4efGH hiJKlmNO pQrST uvWXyZab ",
            "cdEFGh3Ij klMNOprs tuVWxyZq mnOPqRst",
        ),
    );

    assert_eq!(
        build_graph_chunk_content(&chunk, None, false, &GraphExtractionChunkPolicy::standard()),
        None
    );
}

#[test]
fn build_graph_chunk_content_skips_dense_mixed_case_noise() {
    let chunk = make_chunk(
        "paragraph",
        concat!(
            "abCDEfgH ijKLMnOp qRStuVWx yzABcDef gHIjKLmn ",
            "opQRS7tu vwXYZabC deFGhIJk lmNOPqRs tuVWxyZa ",
            "bcDEFgHi jkLMNopQ rsTUVwxy zaBCDefG",
        ),
    );

    assert_eq!(
        build_graph_chunk_content(&chunk, None, false, &GraphExtractionChunkPolicy::standard()),
        None
    );
}

#[test]
fn build_graph_chunk_content_never_feeds_source_profile_to_graph() {
    // The source_profile line is pure machine bookkeeping (counters,
    // top_keys, format token) and carries no entities, so it is dropped for
    // graph extraction regardless of policy or quality score.
    let chunk = make_chunk(
        "source_profile",
        "[source_profile source_format=record_jsonl unit_count=20 top_keys=carrier:1]",
    );
    assert_eq!(
        build_graph_chunk_content(&chunk, None, false, &GraphExtractionChunkPolicy::standard()),
        None
    );
    let record_stream =
        GraphExtractionChunkPolicy::record_stream(std::collections::BTreeSet::new());
    assert_eq!(build_graph_chunk_content(&chunk, None, false, &record_stream), None);

    let mut low_quality = chunk;
    low_quality.quality_score = Some(0.0);
    assert_eq!(build_graph_chunk_content(&low_quality, None, false, &record_stream), None);
}

#[test]
fn build_graph_chunk_content_projects_record_stream_source_units_to_values_only() {
    // An admitted source_unit feeds a values-only projection: the header,
    // dotted-path keys, and timestamp values are stripped; real values stay.
    let chunk = make_chunk(
        "source_unit",
        "[unit_ordinal=1 occurred_at=2026-04-22T19:05:00+00:00] fields: \
carrier=ZephyrFreight-77; shippedAt=2026-04-22T19:05:00+00:00",
    );

    // The values-only projection is keyed off the source_unit kind, which is
    // only ever emitted by the record renderer, so it applies whenever the
    // chunk is a source_unit (the standard policy admits every source_unit).
    let standard = GraphExtractionChunkPolicy::standard();
    assert_eq!(
        build_graph_chunk_content(&chunk, None, false, &standard),
        Some("ZephyrFreight-77".to_string())
    );

    // Record-stream, not selected → skipped.
    let unselected = GraphExtractionChunkPolicy::record_stream(std::collections::BTreeSet::new());
    assert_eq!(build_graph_chunk_content(&chunk, None, false, &unselected), None);

    // Record-stream, selected → values-only projection (no scaffolding).
    let mut selected_ids = std::collections::BTreeSet::new();
    selected_ids.insert(chunk.chunk_id);
    let selected = GraphExtractionChunkPolicy::record_stream(selected_ids);
    assert_eq!(
        build_graph_chunk_content(&chunk, None, false, &selected),
        Some("ZephyrFreight-77".to_string())
    );
}
