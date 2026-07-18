use std::collections::HashSet;

use uuid::Uuid;

use super::{RuntimeChunkScoreKind, RuntimeMatchedChunk};

pub(super) fn extend_setup_preflight_chunks_from_structured_context(
    preflight_answer_chunks: &mut Vec<RuntimeMatchedChunk>,
    structured_context_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
) {
    if structured_context_chunks.is_empty() {
        return;
    }
    let mut seen_chunk_ids =
        preflight_answer_chunks.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut context_chunks = structured_context_chunks
        .iter()
        .filter(|chunk| {
            scoped_document_ids.is_none_or(|document_ids| document_ids.contains(&chunk.document_id))
        })
        .filter(|chunk| seen_chunk_ids.insert(chunk.chunk_id))
        .cloned()
        .collect::<Vec<_>>();
    if context_chunks.is_empty()
        && preflight_answer_chunks.is_empty()
        && scoped_document_ids.is_some()
    {
        context_chunks = setup_lane_fallback_chunks(structured_context_chunks, &mut seen_chunk_ids);
    }
    sort_preflight_context_chunks(&mut context_chunks);
    preflight_answer_chunks.extend(context_chunks);
}

pub(super) fn extend_setup_preflight_chunks_from_setup_lanes(
    preflight_answer_chunks: &mut Vec<RuntimeMatchedChunk>,
    structured_context_chunks: &[RuntimeMatchedChunk],
) {
    let mut seen_chunk_ids =
        preflight_answer_chunks.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut context_chunks = structured_context_chunks
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::DocumentIdentity)
        .filter(|chunk| seen_chunk_ids.insert(chunk.chunk_id))
        .cloned()
        .collect::<Vec<_>>();
    sort_preflight_context_chunks(&mut context_chunks);
    preflight_answer_chunks.extend(context_chunks);
}

fn setup_lane_fallback_chunks(
    context_chunks: &[RuntimeMatchedChunk],
    seen_chunk_ids: &mut HashSet<Uuid>,
) -> Vec<RuntimeMatchedChunk> {
    let setup_chunks = context_chunks
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::DocumentIdentity)
        .filter(|chunk| seen_chunk_ids.insert(chunk.chunk_id))
        .cloned()
        .collect::<Vec<_>>();
    if !setup_chunks.is_empty() {
        return setup_chunks;
    }
    context_chunks.iter().filter(|chunk| seen_chunk_ids.insert(chunk.chunk_id)).cloned().collect()
}

fn sort_preflight_context_chunks(chunks: &mut [RuntimeMatchedChunk]) {
    chunks.sort_by(|left, right| {
        left.document_label
            .cmp(&right.document_label)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
}
