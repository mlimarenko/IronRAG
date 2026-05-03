use std::collections::{BTreeSet, HashMap};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState, infra::arangodb::document_store::KnowledgeDocumentRow,
    shared::extraction::table_summary::is_table_summary_text,
};

use super::{RuntimeMatchedChunk, canonical_document_revision_id, map_chunk_hit, merge_chunks};

pub(crate) fn requested_initial_table_row_count(question: &str) -> Option<usize> {
    let lowered = question.to_lowercase();
    for marker in ["first"] {
        let Some(start) = lowered.find(marker) else {
            continue;
        };
        let tail = &lowered[start + marker.len()..];
        if !tail.contains("rows") {
            continue;
        }
        let count = tail
            .split(|ch: char| !ch.is_ascii_digit())
            .find_map(|token| (!token.is_empty()).then(|| token.parse::<usize>().ok()).flatten());
        if let Some(count) = count {
            return Some(count.clamp(1, 32));
        }
    }
    None
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
        chunks.extend(
            rows.into_iter()
                .filter(|chunk| chunk.chunk_kind.as_deref() == Some("table_row"))
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
