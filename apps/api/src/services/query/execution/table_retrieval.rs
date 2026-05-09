use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query_ir::{QueryIR, SourceSliceDirection},
    infra::arangodb::document_store::{KnowledgeChunkRow, KnowledgeDocumentRow},
    shared::extraction::table_summary::is_table_summary_text,
};

use super::{RuntimeMatchedChunk, canonical_document_revision_id, map_chunk_hit, merge_chunks};

pub(crate) fn requested_initial_table_row_count(ir: Option<&QueryIR>) -> Option<usize> {
    let ir = ir?;
    let slice = ir.source_slice.as_ref()?;
    if slice.direction != SourceSliceDirection::Head {
        return None;
    }
    let targets_table_rows = ir.target_types.iter().any(|tag| tag == "table_row");
    targets_table_rows.then(|| usize::from(slice.count.unwrap_or(12)).clamp(1, 32))
}

pub(crate) async fn load_initial_table_rows_for_documents(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    row_count: usize,
    keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    load_table_rows_for_documents(state, document_index, targeted_document_ids, row_count, keywords)
        .await
}

pub(crate) async fn load_table_rows_for_documents(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    limit_per_document: usize,
    keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if targeted_document_ids.is_empty() || limit_per_document == 0 {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    for document_id in targeted_document_ids {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let rows = state
            .arango_document_store
            .list_chunks_by_revision(revision_id)
            .await
            .with_context(|| format!("failed to load table rows for document {document_id}"))?;
        let synthetic_base_score = 0.5_f32;
        let normalized_keywords =
            keywords.iter().map(|keyword| keyword.to_lowercase()).collect::<Vec<_>>();
        let mut ranked_rows = rows
            .into_iter()
            .filter(|chunk| chunk.chunk_kind.as_deref() == Some("table_row"))
            .enumerate()
            .map(|(ordinal, chunk)| {
                let match_count =
                    table_row_keyword_match_count(&chunk, &normalized_keywords) as f32;
                let score = synthetic_base_score + match_count * TABLE_ROW_KEYWORD_SCORE_BOOST
                    - ordinal as f32 * 0.0001;
                (score, match_count, ordinal, chunk)
            })
            .collect::<Vec<_>>();
        ranked_rows.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(Ordering::Equal)
                .then_with(|| right.1.partial_cmp(&left.1).unwrap_or(Ordering::Equal))
                .then_with(|| left.2.cmp(&right.2))
        });
        chunks.extend(
            ranked_rows.into_iter().take(limit_per_document).filter_map(|(score, _, _, row)| {
                map_chunk_hit(row, score, document_index, keywords)
            }),
        );
    }

    Ok(chunks)
}

const TABLE_ROW_KEYWORD_SCORE_BOOST: f32 = 0.08;

fn table_row_keyword_match_count(
    chunk: &KnowledgeChunkRow,
    normalized_keywords: &[String],
) -> usize {
    if normalized_keywords.is_empty() {
        return 0;
    }
    let haystack = format!("{} {}", chunk.normalized_text, chunk.content_text).to_lowercase();
    normalized_keywords
        .iter()
        .map(|keyword| {
            let keyword = keyword.trim().to_lowercase();
            if keyword.is_empty() { 0 } else { haystack.match_indices(&keyword).count() }
        })
        .sum()
}

pub(crate) async fn load_table_summary_chunks_for_documents(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    limit_per_document: usize,
    keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if targeted_document_ids.is_empty() || limit_per_document == 0 {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    for document_id in targeted_document_ids {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let revision_chunks =
            state.arango_document_store.list_chunks_by_revision(revision_id).await.with_context(
                || format!("failed to load table summaries for document {document_id}"),
            )?;
        let synthetic_base_score = 0.01_f32;
        chunks.extend(
            revision_chunks
                .into_iter()
                .filter(|chunk| {
                    chunk.chunk_kind.as_deref() == Some("metadata_block")
                        && is_table_summary_text(&chunk.normalized_text)
                })
                .take(limit_per_document)
                .enumerate()
                .filter_map(|(ordinal, chunk)| {
                    map_chunk_hit(
                        chunk,
                        synthetic_base_score - ordinal as f32 * 0.0001,
                        document_index,
                        keywords,
                    )
                }),
        );
    }

    Ok(chunks)
}

pub(crate) fn is_table_analytics_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    let text = chunk.source_text.trim();
    is_table_summary_text(text) || (text.starts_with("Sheet: ") && text.contains(" | Row "))
}

pub(crate) fn merge_canonical_table_aggregation_chunks(
    existing_chunks: Vec<RuntimeMatchedChunk>,
    direct_summary_chunks: Vec<RuntimeMatchedChunk>,
    direct_row_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    if direct_summary_chunks.is_empty() && direct_row_chunks.is_empty() {
        return existing_chunks;
    }

    let direct_chunks = merge_chunks(direct_summary_chunks, direct_row_chunks, top_k);
    let mut merged = merge_chunks(direct_chunks, existing_chunks, top_k);
    if merged.iter().any(is_table_analytics_chunk) {
        merged.retain(is_table_analytics_chunk);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::arangodb::document_store::KnowledgeChunkRow;

    fn table_row(
        chunk_id: Uuid,
        chunk_index: usize,
        normalized_text: &str,
        content_text: &str,
        kind: &str,
    ) -> KnowledgeChunkRow {
        KnowledgeChunkRow {
            key: chunk_id.to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: chunk_index as i32,
            chunk_kind: Some(kind.to_string()),
            content_text: content_text.to_string(),
            normalized_text: normalized_text.to_string(),
            span_start: None,
            span_end: None,
            token_count: None,
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

    #[test]
    fn table_row_keyword_match_count_prefers_matching_rows_for_exact_value_queries() {
        let keywords = vec!["route_map".to_string(), "threshold".to_string()];
        let matching_row = table_row(
            Uuid::now_v7(),
            20,
            "system route_map inventory route_map = enabled",
            "unused",
            "table_row",
        );
        let distant_matching_row = table_row(
            Uuid::now_v7(),
            0,
            "other unrelated table_row",
            "route_map threshold 42",
            "table_row",
        );
        let non_matching_row = table_row(
            Uuid::now_v7(),
            2,
            "other unrelated table_row",
            "no technical tokens",
            "table_row",
        );

        let matching = table_row_keyword_match_count(&matching_row, &keywords);
        let distant_matching = table_row_keyword_match_count(&distant_matching_row, &keywords);
        let non_matching = table_row_keyword_match_count(&non_matching_row, &keywords);

        assert_eq!(matching, 2);
        assert_eq!(distant_matching, 2);
        assert_eq!(non_matching, 0);
    }

    #[test]
    fn table_row_keyword_match_count_is_case_insensitive() {
        let keywords = vec!["Route_Map".to_string()];
        let row = table_row(Uuid::now_v7(), 0, "Route_Map Inventory", "", "table_row");

        assert_eq!(table_row_keyword_match_count(&row, &keywords), 1);
    }
}
