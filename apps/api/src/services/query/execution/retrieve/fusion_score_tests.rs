use super::test_prelude::*;

#[test]
fn merge_chunks_normalizes_ordinary_scores() {
    let first = runtime_chunk("first", 10_000.0);
    let second = runtime_chunk("second", 9_000.0);

    let merged = merge_chunks(vec![first], vec![second], 8);

    assert!(merged.iter().all(|chunk| chunk.score.is_some_and(|score| score < 1.0)));
}

#[test]
fn merge_table_aggregation_chunks_prefers_table_analytics() {
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

    let merged =
        merge_table_aggregation_chunks(vec![heading], vec![summary.clone()], vec![row.clone()], 8);

    assert_eq!(merged.len(), 2);
    assert!(merged.iter().all(is_table_analytics_chunk));
    let merged_ids = merged.into_iter().map(|chunk| chunk.chunk_id).collect::<BTreeSet<_>>();
    assert_eq!(merged_ids, BTreeSet::from([summary.chunk_id, row.chunk_id]));
}

#[test]
fn merge_table_aggregation_chunks_keeps_existing_when_no_direct_analytics_exist() {
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

    let merged = merge_table_aggregation_chunks(vec![heading.clone()], Vec::new(), Vec::new(), 8);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].chunk_id, heading.chunk_id);
}
