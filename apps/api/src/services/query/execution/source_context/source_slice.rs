use super::{
    AppState, Context, HashMap, HashSet, KnowledgeChunkRow, KnowledgeDocumentRow,
    KnowledgeStructuredBlockRow, QueryIR, QueryScope, RuntimeMatchedChunk,
    SOURCE_SLICE_DEFAULT_COUNT, SOURCE_SLICE_MAX_COUNT, SourceSliceDirection, SourceSliceFilter,
    SourceSliceSpec, Uuid, current_document_revision_id, excerpt_for, focused_excerpt_for,
    focused_record_unit_excerpt, is_record_stream_source_profile_row,
    rendering::{
        map_companion_chunk, source_context_companion_text_should_replace,
        source_context_companion_visible_text,
    },
    repair_technical_layout_noise, score_value, source_local_evidence_line_score,
    types::{
        SOURCE_CONTEXT_EXCERPT_CHARS, SOURCE_CONTEXT_SLICE_BONUS,
        SOURCE_CONTEXT_SLICE_PROFILE_BONUS, SOURCE_UNIT_CHUNK_KIND, SourceContextCandidate,
        StructuredSourceContextDiagnostics,
    },
};

/// Inputs for `apply_ordered_source_slice_context`.
pub(super) struct OrderedSourceSliceRequest<'a> {
    pub(super) state: &'a AppState,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) plan_keywords: &'a [String],
    pub(super) chunks: &'a mut Vec<RuntimeMatchedChunk>,
    pub(super) candidates: &'a [SourceContextCandidate],
    pub(super) slice: &'a SourceSliceSpec,
    pub(super) temporal_start: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) temporal_end: Option<chrono::DateTime<chrono::Utc>>,
}

pub(super) async fn apply_ordered_source_slice_context(
    request: OrderedSourceSliceRequest<'_>,
) -> anyhow::Result<Option<StructuredSourceContextDiagnostics>> {
    let OrderedSourceSliceRequest {
        state,
        document_index,
        plan_keywords,
        chunks,
        candidates,
        slice,
        temporal_start,
        temporal_end,
    } = request;
    let Some((candidate, profile_row)) =
        top_ranked_record_stream_candidate_profile(state, chunks, candidates).await?
    else {
        return Ok(None);
    };
    let count = source_slice_count(slice);
    let release_marker_required = matches!(slice.filter, SourceSliceFilter::ReleaseMarker);
    let unit_blocks = match slice.direction {
        SourceSliceDirection::Head | SourceSliceDirection::All => state
            .document_store
            .list_head_source_unit_blocks_by_revision(
                candidate.revision_id,
                count,
                temporal_start,
                temporal_end,
                release_marker_required,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load ordered source-unit head slice for revision {}",
                    candidate.revision_id
                )
            })?,
        SourceSliceDirection::Tail => state
            .document_store
            .list_tail_source_unit_blocks_by_revision(
                candidate.revision_id,
                count,
                temporal_start,
                temporal_end,
                release_marker_required,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load ordered source-unit tail slice for revision {}",
                    candidate.revision_id
                )
            })?,
    };
    let source_unit_support_chunks_by_block =
        source_unit_support_chunks_by_block(state, candidate.revision_id, &unit_blocks).await?;

    let mut selected = Vec::with_capacity(count.saturating_add(1));
    let profile_score = candidate.best_score + SOURCE_CONTEXT_SLICE_PROFILE_BONUS;
    if let Some(profile) =
        map_companion_chunk(profile_row, profile_score, document_index, plan_keywords)
    {
        selected.push(profile);
    }
    let slice_score = candidate.best_score + SOURCE_CONTEXT_SLICE_BONUS;
    for block in unit_blocks.into_iter().take(count) {
        let support_chunk = source_unit_support_chunks_by_block.get(&block.block_id);
        if let Some(unit) =
            map_source_unit_block(block, support_chunk, slice_score, document_index, plan_keywords)
        {
            selected.push(unit);
        }
    }

    if selected.len() <= 1 {
        return Ok(None);
    }
    selected.sort_by(|left, right| {
        score_value(right.score)
            .total_cmp(&score_value(left.score))
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    let source_slice_count = selected.len().saturating_sub(1);
    *chunks = selected;
    Ok(Some(StructuredSourceContextDiagnostics {
        eligible_document_count: 1,
        source_profile_count: 1,
        neighbor_count: 0,
        focused_match_count: 0,
        procedural_structured_sibling_count: 0,
        library_profile_count: 0,
        source_slice_count,
    }))
}

pub(super) async fn source_unit_support_chunks_by_block(
    state: &AppState,
    revision_id: Uuid,
    unit_blocks: &[KnowledgeStructuredBlockRow],
) -> anyhow::Result<HashMap<Uuid, KnowledgeChunkRow>> {
    if unit_blocks.is_empty() {
        return Ok(HashMap::new());
    }
    let unit_block_ids = unit_blocks.iter().map(|block| block.block_id).collect::<HashSet<_>>();
    let support_refs = state
        .document_store
        .list_chunk_support_references_by_revision(revision_id)
        .await
        .with_context(|| {
            format!(
                "failed to load chunk support references for source-unit revision {revision_id}"
            )
        })?;
    let mut chunk_ids_by_block = HashMap::<Uuid, Uuid>::new();
    for chunk in support_refs {
        for block_id in chunk.support_block_ids {
            if unit_block_ids.contains(&block_id) {
                chunk_ids_by_block.entry(block_id).or_insert(chunk.chunk_id);
            }
        }
    }
    if chunk_ids_by_block.is_empty() {
        return Ok(HashMap::new());
    }

    let chunk_ids = chunk_ids_by_block.values().copied().collect::<Vec<_>>();
    let support_chunks = state
        .document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .with_context(|| {
            format!("failed to load source-unit support chunks for revision {revision_id}")
        })?
        .into_iter()
        .map(|chunk| (chunk.chunk_id, chunk))
        .collect::<HashMap<_, _>>();

    Ok(chunk_ids_by_block
        .into_iter()
        .filter_map(|(block_id, chunk_id)| {
            support_chunks.get(&chunk_id).cloned().map(|chunk| (block_id, chunk))
        })
        .collect())
}

async fn top_ranked_record_stream_candidate_profile(
    state: &AppState,
    ranked_chunks: &[RuntimeMatchedChunk],
    candidates: &[SourceContextCandidate],
) -> anyhow::Result<Option<(SourceContextCandidate, KnowledgeChunkRow)>> {
    let Some(candidate) = top_ranked_source_context_candidate(ranked_chunks, candidates) else {
        return Ok(None);
    };
    let profile_rows = state
        .document_store
        .list_chunks_by_revision_range(candidate.revision_id, 0, 0)
        .await
        .with_context(|| {
            format!(
                "failed to load ordered source profile chunk for revision {}",
                candidate.revision_id
            )
        })?;
    Ok(profile_rows
        .into_iter()
        .find(is_record_stream_source_profile_row)
        .map(|profile| (candidate.clone(), profile)))
}

pub(super) fn top_ranked_source_context_candidate<'a>(
    ranked_chunks: &[RuntimeMatchedChunk],
    candidates: &'a [SourceContextCandidate],
) -> Option<&'a SourceContextCandidate> {
    let top_ranked_document_id = ranked_chunks.first()?.document_id;
    candidates.iter().find(|candidate| candidate.document_id == top_ranked_document_id)
}

pub(super) fn source_slice_count(slice: &SourceSliceSpec) -> usize {
    slice
        .count
        .map(usize::from)
        .unwrap_or(SOURCE_SLICE_DEFAULT_COUNT)
        .clamp(1, SOURCE_SLICE_MAX_COUNT)
}

pub(super) fn requests_library_source_profile_context(query_ir: &QueryIR) -> bool {
    query_ir.requests_source_coverage_context()
        && matches!(
            query_ir.scope,
            QueryScope::LibraryMeta | QueryScope::MultiDocument | QueryScope::CrossLibrary
        )
}

pub(super) fn map_source_unit_block(
    block: KnowledgeStructuredBlockRow,
    support_chunk: Option<&KnowledgeChunkRow>,
    score: f32,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_keywords: &[String],
) -> Option<RuntimeMatchedChunk> {
    let document = document_index.get(&block.document_id)?;
    let current_revision_id = current_document_revision_id(document)?;
    if block.revision_id != current_revision_id {
        return None;
    }
    let document_label = document
        .title
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| document.file_name.clone())
        .unwrap_or_else(|| document.external_key.clone());
    let unit_text = if block.text.trim().is_empty() {
        block.normalized_text.clone()
    } else {
        block.text.clone()
    };
    let source_text = source_unit_visible_text(&unit_text, support_chunk);
    let excerpt =
        focused_record_unit_excerpt(&source_text, focus_keywords, SOURCE_CONTEXT_EXCERPT_CHARS)
            .unwrap_or_else(|| {
                let focused =
                    focused_excerpt_for(&source_text, focus_keywords, SOURCE_CONTEXT_EXCERPT_CHARS);
                if focused.trim().is_empty() {
                    excerpt_for(&source_text, SOURCE_CONTEXT_EXCERPT_CHARS)
                } else {
                    focused
                }
            });
    Some(RuntimeMatchedChunk {
        chunk_id: support_chunk.map(|chunk| chunk.chunk_id).unwrap_or(block.block_id),
        document_id: block.document_id,
        revision_id: block.revision_id,
        chunk_index: block.ordinal,
        chunk_kind: Some(SOURCE_UNIT_CHUNK_KIND.to_string()),
        document_label,
        excerpt,
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
        score: Some(score),
        source_text,
    })
}

pub(super) fn source_unit_visible_text(
    unit_text: &str,
    support_chunk: Option<&KnowledgeChunkRow>,
) -> String {
    let unit_text = repair_technical_layout_noise(unit_text);
    let Some(support_chunk) = support_chunk else {
        return unit_text;
    };
    let support_text = source_context_companion_visible_text(support_chunk);
    if !source_unit_support_text_should_extend(&support_text, &unit_text) {
        return unit_text;
    }
    format!("{}\n{}", unit_text.trim_end(), support_text.trim_start())
}

pub(super) fn source_unit_support_text_should_extend(candidate: &str, current: &str) -> bool {
    let candidate = candidate.trim();
    let current = current.trim();
    if candidate.is_empty() {
        return false;
    }
    let normalized_candidate = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized_current = current.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized_candidate.is_empty()
        || normalized_candidate == normalized_current
        || normalized_current.contains(&normalized_candidate)
    {
        return false;
    }
    normalized_candidate.contains(&normalized_current)
        || source_context_companion_text_should_replace(candidate, current)
        || source_unit_support_text_has_new_salient_lines(candidate, current)
        || candidate.chars().count() > current.chars().count().saturating_add(64)
}

pub(super) fn source_unit_support_text_has_new_salient_lines(
    candidate: &str,
    current: &str,
) -> bool {
    let normalized_current =
        current.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    candidate.lines().map(str::trim).any(|line| {
        if line.is_empty() || source_local_evidence_line_score(line, &[]) == 0 {
            return false;
        }
        let normalized_line = line.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        !normalized_line.is_empty() && !normalized_current.contains(&normalized_line)
    })
}
