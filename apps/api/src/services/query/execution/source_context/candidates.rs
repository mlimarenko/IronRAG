use super::{
    HashMap, HashSet, KnowledgeChunkRow, KnowledgeDocumentRow, QueryAct, QueryIR, QueryScope,
    RuntimeMatchedChunk, Uuid, current_document_revision_id, is_source_profile_runtime_chunk,
    loading::{
        query_ir_requests_focused_configuration_source_context, requests_error_code_source_context,
        requests_structured_inventory_profile_context, requests_table_row_source_context,
        requests_transport_source_context,
    },
    question_asks_table_aggregation, score_value,
    selection::runtime_chunk_has_explicit_structured_literal_surface,
    types::{
        SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT, SOURCE_CONTEXT_DEFAULT_NEIGHBOR_BACKWARD,
        SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD, SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX,
        SOURCE_CONTEXT_DOCUMENT_LIMIT, SOURCE_CONTEXT_FALLBACK_DOCUMENT_LIMIT,
        SOURCE_CONTEXT_GRAPH_EVIDENCE_BONUS, SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
        SourceContextAnchor, SourceContextCandidate, SourceContextNeighborSpan,
    },
};

#[cfg(test)]
pub(super) fn collect_source_context_candidates(
    chunks: &[RuntimeMatchedChunk],
) -> Vec<SourceContextCandidate> {
    collect_source_context_candidates_with_limit(chunks, SOURCE_CONTEXT_DOCUMENT_LIMIT, 0)
}

pub(super) fn collect_source_context_candidates_with_limit(
    chunks: &[RuntimeMatchedChunk],
    document_limit: usize,
    source_profile_document_limit: usize,
) -> Vec<SourceContextCandidate> {
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut candidates = HashMap::<Uuid, SourceContextCandidate>::new();
    let mut anchor_ranks = HashMap::<Uuid, HashMap<i32, SourceContextAnchor>>::new();
    for (rank, chunk) in chunks.iter().enumerate() {
        if is_source_profile_runtime_chunk(chunk) {
            continue;
        }
        let score = score_value(chunk.score);
        let entry = candidates.entry(chunk.document_id).or_insert_with(|| {
            ordered_document_ids.push(chunk.document_id);
            SourceContextCandidate {
                document_id: chunk.document_id,
                revision_id: chunk.revision_id,
                first_rank: rank,
                best_score: f32::MIN,
                anchors: Vec::new(),
            }
        });
        if score > entry.best_score {
            entry.best_score = score;
            entry.revision_id = chunk.revision_id;
        }
        anchor_ranks
            .entry(chunk.document_id)
            .or_default()
            .entry(chunk.chunk_index)
            .and_modify(|existing| {
                if source_context_anchor_is_better(score, rank, existing) {
                    *existing = SourceContextAnchor {
                        chunk_index: chunk.chunk_index,
                        score,
                        first_rank: rank,
                    };
                }
            })
            .or_insert(SourceContextAnchor {
                chunk_index: chunk.chunk_index,
                score,
                first_rank: rank,
            });
    }

    let mut selected = ordered_document_ids
        .into_iter()
        .filter_map(|document_id| {
            let mut candidate = candidates.remove(&document_id)?;
            let mut anchors = anchor_ranks.remove(&document_id)?.into_values().collect::<Vec<_>>();
            anchors.sort_by(|left, right| {
                right
                    .score
                    .total_cmp(&left.score)
                    .then_with(|| left.first_rank.cmp(&right.first_rank))
                    .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            });
            anchors.truncate(SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT);
            candidate.anchors = anchors;
            (!candidate.anchors.is_empty()).then_some(candidate)
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        left.first_rank
            .cmp(&right.first_rank)
            .then_with(|| left.document_id.cmp(&right.document_id))
    });
    selected.truncate(document_limit);
    let source_profile_candidates =
        collect_source_profile_context_candidates(chunks, &selected, source_profile_document_limit);
    merge_source_profile_candidates(
        &mut selected,
        source_profile_candidates,
        source_profile_document_limit,
    );
    selected
}

pub(super) fn collect_source_profile_context_candidates(
    chunks: &[RuntimeMatchedChunk],
    existing_candidates: &[SourceContextCandidate],
    limit: usize,
) -> Vec<SourceContextCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let mut existing_document_ids =
        existing_candidates.iter().map(|candidate| candidate.document_id).collect::<HashSet<_>>();
    let mut by_document = HashMap::<Uuid, SourceContextCandidate>::new();
    let mut ordered_document_ids = Vec::<Uuid>::new();
    for (rank, chunk) in chunks.iter().enumerate() {
        if !is_source_profile_runtime_chunk(chunk)
            || existing_document_ids.contains(&chunk.document_id)
        {
            continue;
        }
        let score = score_value(chunk.score);
        let entry = by_document.entry(chunk.document_id).or_insert_with(|| {
            ordered_document_ids.push(chunk.document_id);
            SourceContextCandidate {
                document_id: chunk.document_id,
                revision_id: chunk.revision_id,
                first_rank: rank,
                best_score: score,
                anchors: Vec::new(),
            }
        });
        if source_context_anchor_is_better(
            score,
            rank,
            &SourceContextAnchor {
                chunk_index: entry
                    .anchors
                    .first()
                    .map(|anchor| anchor.chunk_index)
                    .unwrap_or(i32::MAX),
                score: entry.best_score,
                first_rank: entry.first_rank,
            },
        ) {
            entry.revision_id = chunk.revision_id;
            entry.first_rank = rank.min(entry.first_rank);
            entry.best_score = score;
            entry.anchors = vec![SourceContextAnchor {
                chunk_index: chunk.chunk_index,
                score,
                first_rank: rank,
            }];
        } else if entry.anchors.is_empty() {
            entry.anchors.push(SourceContextAnchor {
                chunk_index: chunk.chunk_index,
                score,
                first_rank: rank,
            });
        }
    }
    let mut selected = Vec::new();
    for document_id in ordered_document_ids {
        if selected.len() >= limit {
            break;
        }
        let Some(candidate) = by_document.remove(&document_id) else {
            continue;
        };
        if candidate.anchors.is_empty() || !existing_document_ids.insert(candidate.document_id) {
            continue;
        }
        selected.push(candidate);
    }
    selected
}

pub(super) fn merge_source_profile_candidates(
    candidates: &mut Vec<SourceContextCandidate>,
    source_profile_candidates: Vec<SourceContextCandidate>,
    limit: usize,
) {
    if limit == 0 || source_profile_candidates.is_empty() {
        return;
    }
    let mut existing_document_ids =
        candidates.iter().map(|candidate| candidate.document_id).collect::<HashSet<_>>();
    for candidate in source_profile_candidates.into_iter().take(limit) {
        if existing_document_ids.insert(candidate.document_id) {
            candidates.push(candidate);
        }
    }
}

pub(super) fn merge_graph_evidence_source_context_candidates(
    candidates: Vec<SourceContextCandidate>,
    graph_evidence_source_document_ids: &[Uuid],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<SourceContextCandidate> {
    if graph_evidence_source_document_ids.is_empty() {
        return candidates;
    }

    let global_best_score = chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
    let mut by_document = candidates
        .iter()
        .cloned()
        .map(|candidate| (candidate.document_id, candidate))
        .collect::<HashMap<_, _>>();
    let mut seen_graph_document_ids = std::collections::BTreeSet::new();
    let mut promoted_document_ids = std::collections::BTreeSet::new();
    let mut merged = Vec::new();

    for (rank, document_id) in graph_evidence_source_document_ids.iter().enumerate() {
        if !seen_graph_document_ids.insert(*document_id) {
            continue;
        }
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        if !document.is_query_readable() {
            continue;
        }
        let Some(revision_id) = current_document_revision_id(document) else {
            continue;
        };
        let graph_score =
            global_best_score + SOURCE_CONTEXT_GRAPH_EVIDENCE_BONUS - rank as f32 * 0.01;
        promoted_document_ids.insert(*document_id);
        if let Some(mut candidate) = by_document.remove(document_id) {
            candidate.first_rank = rank.min(candidate.first_rank);
            candidate.revision_id = revision_id;
            candidate.best_score = candidate.best_score.max(graph_score);
            merged.push(candidate);
        } else {
            merged.push(SourceContextCandidate {
                document_id: *document_id,
                revision_id,
                first_rank: rank,
                best_score: graph_score,
                anchors: Vec::new(),
            });
        }
    }

    for candidate in candidates {
        if !promoted_document_ids.contains(&candidate.document_id) {
            merged.push(candidate);
        }
    }
    merged.truncate(SOURCE_CONTEXT_DOCUMENT_LIMIT);
    merged
}

pub(super) fn seed_document_head_source_context_anchors(candidates: &mut [SourceContextCandidate]) {
    for (rank, candidate) in candidates.iter_mut().enumerate() {
        push_unique_source_context_anchor(
            &mut candidate.anchors,
            SourceContextAnchor {
                chunk_index: SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX,
                score: candidate.best_score,
                first_rank: candidate.first_rank.min(rank),
            },
        );
    }
}

pub(super) fn unique_candidate_revision_ids(candidates: &[SourceContextCandidate]) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    candidates
        .iter()
        .filter_map(|candidate| seen.insert(candidate.revision_id).then_some(candidate.revision_id))
        .collect()
}

pub(super) fn chunks_by_revision(
    rows: Vec<KnowledgeChunkRow>,
) -> HashMap<Uuid, Vec<KnowledgeChunkRow>> {
    let mut grouped = HashMap::<Uuid, Vec<KnowledgeChunkRow>>::new();
    for row in rows {
        grouped.entry(row.revision_id).or_default().push(row);
    }
    grouped
}

pub(super) fn source_context_anchor_is_better(
    score: f32,
    rank: usize,
    existing: &SourceContextAnchor,
) -> bool {
    score > existing.score || (score == existing.score && rank < existing.first_rank)
}

pub(super) fn push_unique_source_context_anchor(
    anchors: &mut Vec<SourceContextAnchor>,
    anchor: SourceContextAnchor,
) {
    if let Some(existing) =
        anchors.iter_mut().find(|existing| existing.chunk_index == anchor.chunk_index)
    {
        if source_context_anchor_is_better(anchor.score, anchor.first_rank, existing) {
            *existing = anchor;
        }
    } else {
        anchors.push(anchor);
    }
}

#[cfg(test)]
pub(super) fn source_context_neighbor_span(
    query_ir: Option<&QueryIR>,
) -> SourceContextNeighborSpan {
    source_context_neighbor_span_for_request(query_ir, false)
}

pub(super) fn source_context_neighbor_span_for_request(
    query_ir: Option<&QueryIR>,
    fallback_structured_context_requested: bool,
) -> SourceContextNeighborSpan {
    let mut span = SourceContextNeighborSpan {
        backward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_BACKWARD,
        forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD,
    };
    if fallback_structured_context_requested
        || query_ir.is_some_and(requests_expanded_source_context)
    {
        span.backward = SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD;
    }
    span
}

pub(super) fn requests_expanded_source_context(query_ir: &QueryIR) -> bool {
    requests_procedural_source_context(query_ir)
        || requests_error_code_source_context(query_ir)
        || requests_transport_source_context(query_ir)
        || requests_table_row_source_context(query_ir)
}

pub(super) fn requests_procedural_source_context(query_ir: &QueryIR) -> bool {
    matches!(query_ir.scope, QueryScope::SingleDocument)
        && query_ir.source_slice.is_none()
        && (matches!(query_ir.act, QueryAct::ConfigureHow)
            || query_ir_requests_focused_configuration_source_context(query_ir))
}

pub(super) fn requests_fallback_structured_source_context(
    query_ir: Option<&QueryIR>,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    let Some(query_ir) = query_ir else {
        return false;
    };
    query_ir.confidence <= 0.3
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::ConfigureHow)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.literal_constraints.is_empty()
        && chunks
            .iter()
            .filter(|chunk| !is_source_profile_runtime_chunk(chunk))
            .any(runtime_chunk_has_explicit_structured_literal_surface)
}

pub(super) fn requests_fallback_structured_search_source_context(query_ir: &QueryIR) -> bool {
    query_ir.source_slice.is_none()
        && query_ir.literal_constraints.is_empty()
        && (requests_expanded_source_context(query_ir)
            || requests_structured_inventory_profile_context(query_ir))
}

pub(super) fn requests_structured_source_unit_inventory_context(query_ir: &QueryIR) -> bool {
    query_ir.source_slice.is_none()
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(
            query_ir.act,
            QueryAct::Compare | QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
        && !question_asks_table_aggregation("", Some(query_ir))
}

pub(super) fn source_context_candidate_document_limit(
    fallback_structured_candidate_expansion: bool,
) -> usize {
    if fallback_structured_candidate_expansion {
        SOURCE_CONTEXT_FALLBACK_DOCUMENT_LIMIT
    } else {
        SOURCE_CONTEXT_DOCUMENT_LIMIT
    }
}

pub(super) fn source_context_candidate_document_limit_for_request(
    fallback_structured_candidate_expansion: bool,
    table_row_context_requested: bool,
) -> usize {
    if table_row_context_requested {
        SOURCE_CONTEXT_FALLBACK_DOCUMENT_LIMIT
    } else {
        source_context_candidate_document_limit(fallback_structured_candidate_expansion)
    }
}
