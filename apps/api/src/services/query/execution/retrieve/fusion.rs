use super::{
    DOCUMENT_EVIDENCE_CONTEXT_RESERVATION_LIMIT, HashSet,
    QUERY_IR_FOCUS_SOURCE_CONTEXT_RESERVATION_LIMIT, QueryAct, QueryIR, QueryScope,
    QueryTargetKind, RetrievalMergeLane, RuntimeChunkScoreKind, RuntimeMatchedChunk, Uuid,
    VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
    best_versioned_update_procedure_runbook_anchor_chunk,
    ensure_versioned_update_procedure_runbook_chunk_retained, fuse_rrf_chunks,
};

pub(crate) fn merge_chunks(
    left: Vec<RuntimeMatchedChunk>,
    right: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks(left, right, top_k, RetrievalMergeLane::Default)
}

pub(crate) fn merge_entity_bio_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    entity_bio_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks(chunks, entity_bio_chunks, top_k, RetrievalMergeLane::EntityBio)
}

pub(crate) fn merge_graph_evidence_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    graph_evidence_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks(chunks, graph_evidence_chunks, top_k, RetrievalMergeLane::GraphEvidence)
}

#[cfg(test)]
pub(crate) fn merge_query_ir_focus_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    query_ir_focus_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    merge_query_ir_focus_chunks_for_query(chunks, query_ir_focus_chunks, top_k, None)
}

pub(super) fn merge_query_ir_focus_chunks_for_query(
    chunks: Vec<RuntimeMatchedChunk>,
    query_ir_focus_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    query_ir: Option<&QueryIR>,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks_with_query(
        chunks,
        query_ir_focus_chunks,
        top_k,
        RetrievalMergeLane::QueryIrFocus,
        query_ir,
    )
}

pub(super) fn merge_versioned_update_procedure_chunks_for_query(
    chunks: Vec<RuntimeMatchedChunk>,
    procedure_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    query_ir: Option<&QueryIR>,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks_with_query(
        chunks,
        procedure_chunks,
        top_k,
        RetrievalMergeLane::VersionedUpdateProcedure,
        query_ir,
    )
}

pub(crate) fn query_ir_promotes_graph_evidence(query_ir: &QueryIR) -> bool {
    if query_ir.has_exact_technical_literal()
        || query_ir.requests_source_slice_context()
        || query_ir.is_follow_up()
    {
        return false;
    }
    if matches!(query_ir.scope, QueryScope::MultiDocument | QueryScope::CrossLibrary)
        || query_ir.comparison.is_some()
    {
        return true;
    }
    if matches!(query_ir.act, QueryAct::RetrieveValue | QueryAct::Enumerate) {
        return true;
    }

    let graph_target_min_entities = query_ir
        .target_types
        .iter()
        .filter_map(|target| graph_evidence_target_type_min_entities(*target))
        .min();
    graph_target_min_entities
        .is_some_and(|min_entities| query_ir.target_entities.len() >= min_entities)
}

pub(super) const fn graph_evidence_target_type_min_entities(
    target: QueryTargetKind,
) -> Option<usize> {
    match target {
        QueryTargetKind::Artifact => Some(1),
        QueryTargetKind::Relationship
        | QueryTargetKind::Entity
        | QueryTargetKind::Event
        | QueryTargetKind::Route
        | QueryTargetKind::Transition => Some(2),
        _ => None,
    }
}

/// Reciprocal Rank Fusion: merges two ranked lists into a single ranking.
/// Each document's score is `1/(k + rank_in_list)` summed across both lists.
/// This normalizes across different scoring scales (BM25 vs cosine similarity).
pub(super) fn rrf_merge_chunks(
    vector_hits: Vec<RuntimeMatchedChunk>,
    lexical_hits: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    right_lane: RetrievalMergeLane,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks_with_query(vector_hits, lexical_hits, top_k, right_lane, None)
}

pub(super) fn rrf_merge_chunks_with_query(
    vector_hits: Vec<RuntimeMatchedChunk>,
    lexical_hits: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    right_lane: RetrievalMergeLane,
    query_ir: Option<&QueryIR>,
) -> Vec<RuntimeMatchedChunk> {
    let values = fuse_rrf_chunks(vector_hits, lexical_hits, right_lane);
    truncate_merged_chunks_for_lane(values, top_k, right_lane, query_ir)
}

pub(super) fn truncate_merged_chunks_for_lane(
    values: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    right_lane: RetrievalMergeLane,
    query_ir: Option<&QueryIR>,
) -> Vec<RuntimeMatchedChunk> {
    if top_k == 0 || values.is_empty() {
        return Vec::new();
    }
    let reserved_latest_version_count = reserved_latest_version_merge_count(&values, top_k);
    let versioned_update_runbook_anchor = query_ir.and_then(|ir| {
        let indexed = values.iter().cloned().enumerate().collect::<Vec<_>>();
        best_versioned_update_procedure_runbook_anchor_chunk(&indexed, ir).map(|(_, chunk)| chunk)
    });
    if right_lane == RetrievalMergeLane::VersionedUpdateProcedure {
        let mut selected = truncate_merged_chunks_with_reservations(
            values,
            top_k,
            &[
                (RuntimeChunkScoreKind::LatestVersion, reserved_latest_version_count),
                (
                    RuntimeChunkScoreKind::FocusedDocument,
                    VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
                ),
            ],
        );
        if let Some(anchor) = versioned_update_runbook_anchor {
            ensure_versioned_update_procedure_runbook_chunk_retained(&mut selected, anchor, top_k);
        }
        return selected;
    }
    if right_lane != RetrievalMergeLane::QueryIrFocus {
        if reserved_latest_version_count > 0 {
            return truncate_merged_chunks_with_reservations(
                values,
                top_k,
                &[(RuntimeChunkScoreKind::LatestVersion, reserved_latest_version_count)],
            );
        }
        return values.into_iter().take(top_k).collect();
    }
    let reserved_focused_document_count = values
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument)
        .count()
        .min(top_k.saturating_sub(1))
        .min(DOCUMENT_EVIDENCE_CONTEXT_RESERVATION_LIMIT);
    let reserved_source_context_count = values
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
        .count()
        .min(top_k.saturating_sub(1).saturating_sub(reserved_focused_document_count))
        .min(QUERY_IR_FOCUS_SOURCE_CONTEXT_RESERVATION_LIMIT);
    let mut selected = truncate_merged_chunks_with_reservations(
        values,
        top_k,
        &[
            (RuntimeChunkScoreKind::LatestVersion, reserved_latest_version_count),
            (RuntimeChunkScoreKind::FocusedDocument, reserved_focused_document_count),
            (RuntimeChunkScoreKind::SourceContext, reserved_source_context_count),
        ],
    );
    if let Some(anchor) = versioned_update_runbook_anchor {
        ensure_versioned_update_procedure_runbook_chunk_retained(&mut selected, anchor, top_k);
    }
    selected
}

pub(super) fn reserved_latest_version_merge_count(
    values: &[RuntimeMatchedChunk],
    top_k: usize,
) -> usize {
    values
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::LatestVersion)
        .count()
        .min(top_k.saturating_sub(1))
}

pub(super) fn truncate_merged_chunks_with_reservations(
    values: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    reservations: &[(RuntimeChunkScoreKind, usize)],
) -> Vec<RuntimeMatchedChunk> {
    if top_k == 0 || values.is_empty() {
        return Vec::new();
    }
    let mut remaining = top_k.saturating_sub(1);
    let effective_reservations = reservations
        .iter()
        .filter_map(|(kind, requested)| {
            let count = values
                .iter()
                .filter(|chunk| chunk.score_kind == *kind)
                .count()
                .min(*requested)
                .min(remaining);
            remaining = remaining.saturating_sub(count);
            (count > 0).then_some((*kind, count))
        })
        .collect::<Vec<_>>();
    if effective_reservations.is_empty() {
        return values.into_iter().take(top_k).collect();
    }

    let mut selected = Vec::with_capacity(top_k);
    let mut selected_chunk_ids = HashSet::<Uuid>::new();
    for (kind, count) in effective_reservations {
        for chunk in values.iter().filter(|chunk| chunk.score_kind == kind).take(count) {
            selected_chunk_ids.insert(chunk.chunk_id);
            selected.push(chunk.clone());
        }
    }
    for chunk in values {
        if selected.len() >= top_k {
            break;
        }
        if selected_chunk_ids.insert(chunk.chunk_id) {
            selected.push(chunk);
        }
    }
    selected
}

pub(crate) fn score_desc_chunks(
    left: &RuntimeMatchedChunk,
    right: &RuntimeMatchedChunk,
) -> std::cmp::Ordering {
    score_value(right.score)
        .total_cmp(&score_value(left.score))
        .then_with(|| left.chunk_id.cmp(&right.chunk_id))
}

pub(crate) fn score_value(score: Option<f32>) -> f32 {
    score.unwrap_or(0.0)
}
