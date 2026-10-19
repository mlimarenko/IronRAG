use super::{
    AppState, Context, HashMap, HashSet, KnowledgeChunkRow, KnowledgeDocumentRow, QueryIR,
    RuntimeMatchedChunk, SourceFocusLimits, Uuid,
    candidates::{
        push_unique_source_context_anchor, requests_fallback_structured_search_source_context,
    },
    is_source_profile_chunk_row,
    loading::{
        FallbackStructuredSearchInput, SourceContextLoadScope, code_pattern_candidate_document_ids,
        code_pattern_literal_terms, load_code_pattern_source_context,
        load_fallback_structured_search_source_context, load_transport_pattern_source_context,
        preserve_primary_context_on_code_pattern_failure,
        preserve_primary_context_on_transport_pattern_failure,
        requests_code_pattern_source_context, requests_transport_source_context,
        source_profile_matches_focus, transport_pattern_candidate_document_ids,
    },
    rendering::{
        current_source_profile_revision_ids, map_companion_chunk,
        structured_inventory_source_profile_score,
    },
    score_value,
    selection::{
        select_path_source_rows, select_procedural_structured_sibling_rows,
        select_table_structured_sibling_rows, setup_path_source_score_bonus,
        source_context_best_neighbor_score,
    },
    source_slice::requests_library_source_profile_context,
    types::{
        FOCUSED_DECLARATION_CHUNK_KIND, PreparedSourceContextCandidate,
        SOURCE_CONTEXT_DOCUMENT_LIMIT, SOURCE_CONTEXT_FOCUSED_DECLARATION_LIMIT_PER_DOCUMENT,
        SOURCE_CONTEXT_FOCUSED_MATCH_LIMIT_PER_DOCUMENT, SOURCE_CONTEXT_FOCUSED_MATCH_SCORE_BONUS,
        SOURCE_CONTEXT_LIBRARY_PROFILE_BONUS, SOURCE_CONTEXT_PATH_MATCH_LIMIT_PER_DOCUMENT,
        SOURCE_CONTEXT_PATH_MATCH_SCORE_BONUS,
        SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_LIMIT_PER_DOCUMENT,
        SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_SCORE_BONUS, SOURCE_CONTEXT_SELECTED_PROFILE_BONUS,
        SOURCE_CONTEXT_TABLE_STRUCTURED_LIMIT_PER_DOCUMENT, ScoredSourceContextRow,
        SourceContextAnchor, SourceContextCandidate, SourceContextNeighborSpan,
        StructuredSourceCompanion, StructuredSourceCompanionKind,
    },
};

/// Shared source-context scope threaded through the companion appenders.
pub(super) struct SourceContextScope<'a> {
    pub(super) state: &'a AppState,
    pub(super) library_id: Uuid,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) focus_keywords: &'a [String],
    pub(super) plan_keywords: &'a [String],
}

/// Inputs for `prepare_source_context_candidates`.
pub(super) struct SourceContextCandidateInputs<'a> {
    pub(super) candidates: &'a [SourceContextCandidate],
    pub(super) focused_rows_by_revision: &'a HashMap<Uuid, Vec<KnowledgeChunkRow>>,
    pub(super) path_rows_by_revision: &'a HashMap<Uuid, Vec<KnowledgeChunkRow>>,
    pub(super) path_head_rows_by_revision: &'a HashMap<Uuid, Vec<KnowledgeChunkRow>>,
    pub(super) focus_keywords: &'a [String],
    pub(super) path_context_requested: bool,
    pub(super) configuration_path_context: bool,
    pub(super) existing_chunk_ids: &'a HashSet<Uuid>,
}

/// Inputs for `append_prepared_source_context_companions`.
pub(super) struct PreparedSourceContextCompanions<'a> {
    pub(super) state: &'a AppState,
    pub(super) library_id: Uuid,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) plan_keywords: &'a [String],
    pub(super) focus_keywords: &'a [String],
    pub(super) procedural_context_requested: bool,
    pub(super) table_row_context_requested: bool,
    pub(super) promote_source_profiles: bool,
    pub(super) neighbor_span: SourceContextNeighborSpan,
    pub(super) prepared_candidates: Vec<PreparedSourceContextCandidate>,
    pub(super) procedural_rows_by_revision: &'a HashMap<Uuid, Vec<KnowledgeChunkRow>>,
    pub(super) neighbor_rows_by_revision: &'a HashMap<Uuid, Vec<KnowledgeChunkRow>>,
}

/// Fields of one optional source-context stage failure.
pub(super) struct OptionalSourceContextFailure<'a> {
    pub(super) error: Option<&'a anyhow::Error>,
    pub(super) has_primary_evidence: bool,
    pub(super) library_id: Uuid,
    pub(super) primary_chunk_count: usize,
    pub(super) candidate_document_count: usize,
    pub(super) stage: &'static str,
    pub(super) message: &'static str,
}

/// Inputs for `append_fallback_structured_source_context`.
pub(super) struct FallbackStructuredSourceContextInput {
    pub(super) expansion_requested: bool,
    pub(super) slice_temporal_start: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) slice_temporal_end: Option<chrono::DateTime<chrono::Utc>>,
}

pub(super) fn prepare_source_context_candidates(
    input: SourceContextCandidateInputs<'_>,
) -> Vec<PreparedSourceContextCandidate> {
    let SourceContextCandidateInputs {
        candidates,
        focused_rows_by_revision,
        path_rows_by_revision,
        path_head_rows_by_revision,
        focus_keywords,
        path_context_requested,
        configuration_path_context,
        existing_chunk_ids,
    } = input;
    candidates
        .iter()
        .map(|candidate| {
            let mut neighbor_anchors = candidate.anchors.clone();
            let focused_rows = scored_focused_source_rows(
                candidate,
                focused_rows_by_revision,
                focus_keywords,
                &mut neighbor_anchors,
                existing_chunk_ids,
            );
            let path_rows = scored_path_source_rows(
                candidate,
                path_rows_by_revision,
                path_head_rows_by_revision,
                path_context_requested,
                configuration_path_context,
                &mut neighbor_anchors,
            );
            PreparedSourceContextCandidate {
                candidate: candidate.clone(),
                neighbor_anchors,
                focused_rows,
                path_rows,
            }
        })
        .collect()
}

pub(super) fn scored_focused_source_rows(
    candidate: &SourceContextCandidate,
    rows_by_revision: &HashMap<Uuid, Vec<KnowledgeChunkRow>>,
    focus_keywords: &[String],
    neighbor_anchors: &mut Vec<SourceContextAnchor>,
    existing_chunk_ids: &HashSet<Uuid>,
) -> Vec<ScoredSourceContextRow> {
    let rows = rows_by_revision.get(&candidate.revision_id).map_or(&[][..], Vec::as_slice);
    let anchor_indexes =
        candidate.anchors.iter().map(|anchor| anchor.chunk_index).collect::<Vec<_>>();
    super::super::source_focus_selection::select_query_focused_source_rows_with_declaration_reach(
        rows,
        focus_keywords,
        false,
        &anchor_indexes,
        SourceFocusLimits {
            limit: SOURCE_CONTEXT_FOCUSED_MATCH_LIMIT_PER_DOCUMENT,
            additional_limit: SOURCE_CONTEXT_FOCUSED_DECLARATION_LIMIT_PER_DOCUMENT,
        },
        existing_chunk_ids,
    )
    .into_iter()
    .enumerate()
    .map(|(rank, selected)| {
        let score =
            candidate.best_score + SOURCE_CONTEXT_FOCUSED_MATCH_SCORE_BONUS - rank as f32 * 0.01;
        push_unique_source_context_anchor(
            neighbor_anchors,
            SourceContextAnchor {
                chunk_index: selected.row.chunk_index,
                score,
                first_rank: usize::MAX.saturating_sub(rank),
            },
        );
        ScoredSourceContextRow {
            row: selected.row,
            score,
            additional_declaration: selected.additional_declaration,
        }
    })
    .collect()
}

pub(super) fn scored_path_source_rows(
    candidate: &SourceContextCandidate,
    rows_by_revision: &HashMap<Uuid, Vec<KnowledgeChunkRow>>,
    head_rows_by_revision: &HashMap<Uuid, Vec<KnowledgeChunkRow>>,
    path_context_requested: bool,
    configuration_path_context: bool,
    neighbor_anchors: &mut Vec<SourceContextAnchor>,
) -> Vec<ScoredSourceContextRow> {
    if !path_context_requested {
        return Vec::new();
    }
    let mut rows = rows_by_revision.get(&candidate.revision_id).cloned().unwrap_or_default();
    if configuration_path_context {
        rows.extend(
            head_rows_by_revision.get(&candidate.revision_id).into_iter().flatten().cloned(),
        );
    }
    select_path_source_rows(
        &rows,
        neighbor_anchors,
        SOURCE_CONTEXT_PATH_MATCH_LIMIT_PER_DOCUMENT,
        configuration_path_context,
    )
    .into_iter()
    .enumerate()
    .map(|(rank, row)| {
        let setup_bonus =
            if configuration_path_context { setup_path_source_score_bonus(&row) } else { 0.0 };
        let score = candidate.best_score + SOURCE_CONTEXT_PATH_MATCH_SCORE_BONUS + setup_bonus
            - rank as f32 * 0.01;
        push_unique_source_context_anchor(
            neighbor_anchors,
            SourceContextAnchor {
                chunk_index: row.chunk_index,
                score,
                first_rank: usize::MAX.saturating_sub(rank),
            },
        );
        ScoredSourceContextRow { row, score, additional_declaration: false }
    })
    .collect()
}

pub(super) async fn append_prepared_source_context_companions(
    input: PreparedSourceContextCompanions<'_>,
    companions: &mut Vec<StructuredSourceCompanion>,
) -> anyhow::Result<()> {
    let PreparedSourceContextCompanions {
        state,
        library_id,
        document_index,
        plan_keywords,
        focus_keywords,
        procedural_context_requested,
        table_row_context_requested,
        promote_source_profiles,
        neighbor_span,
        prepared_candidates,
        procedural_rows_by_revision,
        neighbor_rows_by_revision,
    } = input;
    let scope =
        SourceContextScope { state, library_id, document_index, focus_keywords, plan_keywords };
    for prepared in prepared_candidates {
        append_scored_source_rows(
            prepared.focused_rows.clone(),
            document_index,
            focus_keywords,
            companions,
        );
        append_scored_source_rows(
            prepared.path_rows.clone(),
            document_index,
            focus_keywords,
            companions,
        );
        append_procedural_source_rows(
            &prepared,
            procedural_context_requested,
            table_row_context_requested,
            procedural_rows_by_revision,
            &scope,
            companions,
        );
        let continue_with_neighbors = append_profile_source_row(
            &scope,
            &prepared.candidate,
            promote_source_profiles,
            companions,
        )
        .await?;
        if continue_with_neighbors {
            append_neighbor_source_rows(
                &prepared,
                neighbor_span,
                neighbor_rows_by_revision,
                document_index,
                plan_keywords,
                companions,
            );
        }
    }
    Ok(())
}

pub(super) fn append_scored_source_rows(
    rows: Vec<ScoredSourceContextRow>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_keywords: &[String],
    companions: &mut Vec<StructuredSourceCompanion>,
) {
    companions.extend(rows.into_iter().filter_map(|scored| {
        map_companion_chunk(scored.row, scored.score, document_index, focus_keywords).map(
            |mut chunk| {
                if scored.additional_declaration {
                    chunk.chunk_kind = Some(FOCUSED_DECLARATION_CHUNK_KIND.to_string());
                }
                StructuredSourceCompanion {
                    chunk,
                    kind: StructuredSourceCompanionKind::FocusedMatch,
                }
            },
        )
    }));
}

pub(super) fn append_procedural_source_rows(
    prepared: &PreparedSourceContextCandidate,
    requested: bool,
    table_requested: bool,
    rows_by_revision: &HashMap<Uuid, Vec<KnowledgeChunkRow>>,
    scope: &SourceContextScope<'_>,
    companions: &mut Vec<StructuredSourceCompanion>,
) {
    let document_index = scope.document_index;
    let plan_keywords = scope.plan_keywords;
    if !requested {
        return;
    }
    let rows = rows_by_revision.get(&prepared.candidate.revision_id).map_or(&[][..], Vec::as_slice);
    let selected = if table_requested {
        select_table_structured_sibling_rows(
            rows,
            &prepared.neighbor_anchors,
            SOURCE_CONTEXT_TABLE_STRUCTURED_LIMIT_PER_DOCUMENT,
        )
    } else {
        select_procedural_structured_sibling_rows(
            rows,
            &prepared.neighbor_anchors,
            SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_LIMIT_PER_DOCUMENT,
        )
    };
    companions.extend(selected.into_iter().enumerate().filter_map(|(rank, row)| {
        let score = prepared.candidate.best_score
            + SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_SCORE_BONUS
            - rank as f32 * 0.01;
        map_companion_chunk(row, score, document_index, plan_keywords).map(|chunk| {
            StructuredSourceCompanion {
                chunk,
                kind: StructuredSourceCompanionKind::ProceduralStructuredSibling,
            }
        })
    }));
}

pub(super) async fn append_profile_source_row(
    scope: &SourceContextScope<'_>,
    candidate: &SourceContextCandidate,
    promote: bool,
    companions: &mut Vec<StructuredSourceCompanion>,
) -> anyhow::Result<bool> {
    let state = scope.state;
    let focus_keywords = scope.focus_keywords;
    let document_index = scope.document_index;
    let plan_keywords = scope.plan_keywords;
    let rows = state
        .document_store
        .list_chunks_by_revision_range(candidate.revision_id, 0, 0)
        .await
        .with_context(|| {
            format!("failed to load source profile chunk for revision {}", candidate.revision_id)
        })?;
    let Some(row) = rows.into_iter().find(is_source_profile_chunk_row) else {
        return Ok(true);
    };
    if promote && !source_profile_matches_focus(&row, focus_keywords) {
        return Ok(false);
    }
    let score = if promote {
        structured_inventory_source_profile_score(candidate.best_score, 0)
    } else {
        candidate.best_score + SOURCE_CONTEXT_SELECTED_PROFILE_BONUS
    };
    if let Some(chunk) = map_companion_chunk(row, score, document_index, plan_keywords) {
        companions.push(StructuredSourceCompanion {
            chunk,
            kind: StructuredSourceCompanionKind::SourceProfile,
        });
    }
    Ok(true)
}

pub(super) fn append_neighbor_source_rows(
    prepared: &PreparedSourceContextCandidate,
    neighbor_span: SourceContextNeighborSpan,
    rows_by_revision: &HashMap<Uuid, Vec<KnowledgeChunkRow>>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    companions: &mut Vec<StructuredSourceCompanion>,
) {
    let rows = rows_by_revision.get(&prepared.candidate.revision_id).map_or(&[][..], Vec::as_slice);
    companions.extend(rows.iter().filter_map(|row| {
        if is_source_profile_chunk_row(row) {
            return None;
        }
        let score = source_context_best_neighbor_score(
            &prepared.neighbor_anchors,
            row.chunk_index,
            neighbor_span,
        )?;
        map_companion_chunk(row.clone(), score, document_index, plan_keywords).map(|chunk| {
            StructuredSourceCompanion { chunk, kind: StructuredSourceCompanionKind::Neighbor }
        })
    }));
}

pub(super) struct OptionalSourceContextRequest<'a> {
    pub(super) query_ir: Option<&'a QueryIR>,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) plan_keywords: &'a [String],
    pub(super) focus_keywords: &'a [String],
    pub(super) fallback_structured_candidate_expansion_requested: bool,
    pub(super) promote_source_profiles: bool,
    pub(super) inventory_profile_companions: Vec<StructuredSourceCompanion>,
    pub(super) slice_temporal_start: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) slice_temporal_end: Option<chrono::DateTime<chrono::Utc>>,
}

pub(super) async fn append_optional_source_context(
    state: &AppState,
    library_id: Uuid,
    request: OptionalSourceContextRequest<'_>,
    chunks: &mut [RuntimeMatchedChunk],
    companions: &mut Vec<StructuredSourceCompanion>,
) -> anyhow::Result<()> {
    let OptionalSourceContextRequest {
        query_ir,
        document_index,
        plan_keywords,
        focus_keywords,
        fallback_structured_candidate_expansion_requested,
        promote_source_profiles,
        inventory_profile_companions,
        slice_temporal_start,
        slice_temporal_end,
    } = request;
    let scope =
        SourceContextScope { state, library_id, document_index, focus_keywords, plan_keywords };
    append_code_pattern_source_context(&scope, query_ir, chunks, companions).await?;
    append_transport_pattern_source_context(&scope, query_ir, chunks, companions).await?;
    if promote_source_profiles {
        companions.extend(inventory_profile_companions);
    }

    append_fallback_structured_source_context(
        &scope,
        query_ir,
        FallbackStructuredSourceContextInput {
            expansion_requested: fallback_structured_candidate_expansion_requested,
            slice_temporal_start,
            slice_temporal_end,
        },
        chunks,
        companions,
    )
    .await?;
    append_library_source_profile_context(&scope, query_ir, chunks, companions).await
}

pub(super) async fn append_code_pattern_source_context(
    scope: &SourceContextScope<'_>,
    query_ir: Option<&QueryIR>,
    chunks: &mut [RuntimeMatchedChunk],
    companions: &mut Vec<StructuredSourceCompanion>,
) -> anyhow::Result<()> {
    let state = scope.state;
    let library_id = scope.library_id;
    let document_index = scope.document_index;
    let focus_keywords = scope.focus_keywords;
    let Some(query_ir) = query_ir.filter(|ir| requests_code_pattern_source_context(ir)) else {
        return Ok(());
    };
    let candidate_document_ids = code_pattern_candidate_document_ids(chunks);
    let has_primary_evidence = !chunks.is_empty();
    let result = load_code_pattern_source_context(
        SourceContextLoadScope { state, library_id, document_index, focus_keywords },
        &code_pattern_literal_terms(query_ir),
        &candidate_document_ids,
        chunks,
        companions,
    )
    .await;
    log_optional_source_context_failure(OptionalSourceContextFailure {
        error: result.as_ref().err(),
        has_primary_evidence,
        library_id,
        primary_chunk_count: chunks.len(),
        candidate_document_count: candidate_document_ids.len(),
        stage: "retrieval.structured_source_context.code_pattern",
        message: "optional code-pattern source context failed; preserving primary evidence",
    });
    companions
        .extend(preserve_primary_context_on_code_pattern_failure(result, has_primary_evidence)?);
    Ok(())
}

pub(super) async fn append_transport_pattern_source_context(
    scope: &SourceContextScope<'_>,
    query_ir: Option<&QueryIR>,
    chunks: &mut [RuntimeMatchedChunk],
    companions: &mut Vec<StructuredSourceCompanion>,
) -> anyhow::Result<()> {
    let state = scope.state;
    let library_id = scope.library_id;
    let document_index = scope.document_index;
    let focus_keywords = scope.focus_keywords;
    if !query_ir.is_some_and(requests_transport_source_context) {
        return Ok(());
    }
    let candidate_document_ids = transport_pattern_candidate_document_ids(chunks);
    let has_primary_evidence = !chunks.is_empty();
    let result = load_transport_pattern_source_context(
        SourceContextLoadScope { state, library_id, document_index, focus_keywords },
        &candidate_document_ids,
        chunks,
        companions,
    )
    .await;
    log_optional_source_context_failure(OptionalSourceContextFailure {
        error: result.as_ref().err(),
        has_primary_evidence,
        library_id,
        primary_chunk_count: chunks.len(),
        candidate_document_count: candidate_document_ids.len(),
        stage: "retrieval.structured_source_context.transport_pattern",
        message: "optional transport-pattern source context failed; preserving primary evidence",
    });
    companions.extend(preserve_primary_context_on_transport_pattern_failure(
        result,
        has_primary_evidence,
    )?);
    Ok(())
}

pub(super) fn log_optional_source_context_failure(failure: OptionalSourceContextFailure<'_>) {
    let OptionalSourceContextFailure {
        error,
        has_primary_evidence,
        library_id,
        primary_chunk_count,
        candidate_document_count,
        stage,
        message,
    } = failure;
    if error.is_some() && has_primary_evidence {
        tracing::warn!(
            stage,
            %library_id,
            primary_chunk_count,
            candidate_document_count,
            failure_kind = "optional_document_store_query",
            "{message}"
        );
    }
}

pub(super) async fn append_fallback_structured_source_context(
    scope: &SourceContextScope<'_>,
    query_ir: Option<&QueryIR>,
    input: FallbackStructuredSourceContextInput,
    chunks: &mut [RuntimeMatchedChunk],
    companions: &mut Vec<StructuredSourceCompanion>,
) -> anyhow::Result<()> {
    let state = scope.state;
    let library_id = scope.library_id;
    let document_index = scope.document_index;
    let focus_keywords = scope.focus_keywords;
    let FallbackStructuredSourceContextInput {
        expansion_requested,
        slice_temporal_start,
        slice_temporal_end,
    } = input;
    if !expansion_requested
        && !query_ir.is_some_and(requests_fallback_structured_search_source_context)
    {
        return Ok(());
    }
    companions.extend(
        load_fallback_structured_search_source_context(
            SourceContextLoadScope { state, library_id, document_index, focus_keywords },
            FallbackStructuredSearchInput {
                temporal_start: slice_temporal_start,
                temporal_end: slice_temporal_end,
                include_long_focus_terms: !expansion_requested,
            },
            chunks,
            companions,
        )
        .await?,
    );
    Ok(())
}

pub(super) async fn append_library_source_profile_context(
    scope: &SourceContextScope<'_>,
    query_ir: Option<&QueryIR>,
    chunks: &[RuntimeMatchedChunk],
    companions: &mut Vec<StructuredSourceCompanion>,
) -> anyhow::Result<()> {
    let state = scope.state;
    let library_id = scope.library_id;
    let document_index = scope.document_index;
    let plan_keywords = scope.plan_keywords;
    if !query_ir.is_some_and(requests_library_source_profile_context) {
        return Ok(());
    }
    let global_best_score = chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
    let revision_ids =
        current_source_profile_revision_ids(document_index, SOURCE_CONTEXT_DOCUMENT_LIMIT * 4);
    let rows = state
        .document_store
        .list_source_profile_chunks_by_revisions(
            library_id,
            &revision_ids,
            SOURCE_CONTEXT_DOCUMENT_LIMIT,
        )
        .await
        .context("failed to load library source profile chunks for source coverage")?;
    companions.extend(rows.into_iter().enumerate().filter_map(|(rank, row)| {
        map_companion_chunk(
            row,
            global_best_score + SOURCE_CONTEXT_LIBRARY_PROFILE_BONUS - rank as f32 * 0.01,
            document_index,
            plan_keywords,
        )
        .map(|chunk| StructuredSourceCompanion {
            chunk,
            kind: StructuredSourceCompanionKind::LibrarySourceProfile,
        })
    }));
    Ok(())
}
