use super::{
    AppState, Context, HashMap, HashSet, KnowledgeDocumentRow, QueryIR, RuntimeMatchedChunk,
    StructuredSourceContextDiagnostics, Uuid,
    candidates::{
        chunks_by_revision, collect_source_context_candidates_with_limit,
        collect_source_profile_context_candidates, merge_graph_evidence_source_context_candidates,
        merge_source_profile_candidates, requests_expanded_source_context,
        requests_fallback_structured_source_context, requests_procedural_source_context,
        requests_structured_source_unit_inventory_context,
        seed_document_head_source_context_anchors,
        source_context_candidate_document_limit_for_request,
        source_context_neighbor_span_for_request, unique_candidate_revision_ids,
    },
    companions::{
        OptionalSourceContextRequest, PreparedSourceContextCompanions,
        SourceContextCandidateInputs, append_optional_source_context,
        append_prepared_source_context_companions, prepare_source_context_candidates,
    },
    loading::{
        load_structured_inventory_profile_source_context,
        requests_configuration_file_path_source_context, requests_path_source_context,
        requests_structured_inventory_profile_context, requests_table_row_source_context,
    },
    rendering::apply_structured_source_companions,
    selection::{
        procedural_structured_sibling_windows, source_context_focus_keywords,
        source_context_neighbor_windows,
    },
    source_slice::{OrderedSourceSliceRequest, apply_ordered_source_slice_context},
    types::{
        SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX,
        SOURCE_CONTEXT_FOCUSED_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT,
        SOURCE_CONTEXT_PATH_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT,
        SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD, SOURCE_CONTEXT_PROFILE_DOCUMENT_LIMIT,
        SOURCE_CONTEXT_TABLE_STRUCTURED_FORWARD, SourceContextCandidate, StructuredSourceCompanion,
    },
};

/// Inputs for `augment_structured_source_context`.
pub(crate) struct StructuredSourceContextAugmentRequest<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) library_id: Uuid,
    pub(crate) question: &'a str,
    pub(crate) query_ir: Option<&'a QueryIR>,
    pub(crate) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(crate) plan_keywords: &'a [String],
    pub(crate) graph_evidence_source_document_ids: &'a [Uuid],
    pub(crate) chunks: &'a mut Vec<RuntimeMatchedChunk>,
}

pub(crate) async fn augment_structured_source_context(
    request: StructuredSourceContextAugmentRequest<'_>,
) -> anyhow::Result<StructuredSourceContextDiagnostics> {
    let StructuredSourceContextAugmentRequest {
        state,
        library_id,
        question,
        query_ir,
        document_index,
        plan_keywords,
        graph_evidence_source_document_ids,
        chunks,
    } = request;
    let span_started = std::time::Instant::now();
    let companions = Vec::<StructuredSourceCompanion>::new();
    let fallback_structured_context_requested =
        requests_fallback_structured_source_context(query_ir, chunks);
    let structured_source_unit_inventory_context_requested =
        query_ir.is_some_and(requests_structured_source_unit_inventory_context);
    let fallback_structured_candidate_expansion_requested = fallback_structured_context_requested;
    let table_row_context_requested = query_ir.is_some_and(requests_table_row_source_context);
    let promote_source_profiles =
        query_ir.is_some_and(requests_structured_inventory_profile_context);
    let requests_expanded_source_context = query_ir.is_some_and(requests_expanded_source_context);
    let source_profile_candidate_limit = if requests_expanded_source_context
        || structured_source_unit_inventory_context_requested
        || promote_source_profiles
        || query_ir.is_some_and(|ir| ir.source_slice.is_some())
    {
        SOURCE_CONTEXT_PROFILE_DOCUMENT_LIMIT
    } else {
        0
    };
    let candidate_document_limit = source_context_candidate_document_limit_for_request(
        fallback_structured_candidate_expansion_requested,
        table_row_context_requested,
    );
    let mut candidates = collect_source_context_candidates_with_limit(
        chunks,
        candidate_document_limit,
        source_profile_candidate_limit,
    );
    let focus_keywords = source_context_focus_keywords(question, query_ir, plan_keywords);
    let inventory_profile_companions = if promote_source_profiles {
        load_structured_inventory_profile_source_context(
            state,
            library_id,
            document_index,
            &focus_keywords,
            chunks,
            &companions,
        )
        .await?
    } else {
        Vec::new()
    };
    if source_profile_candidate_limit > 0 && !inventory_profile_companions.is_empty() {
        let profile_chunks = inventory_profile_companions
            .iter()
            .map(|companion| companion.chunk.clone())
            .collect::<Vec<_>>();
        let profile_candidates = collect_source_profile_context_candidates(
            &profile_chunks,
            &candidates,
            source_profile_candidate_limit,
        );
        merge_source_profile_candidates(
            &mut candidates,
            profile_candidates,
            source_profile_candidate_limit,
        );
    }
    if requests_expanded_source_context {
        candidates = merge_graph_evidence_source_context_candidates(
            candidates,
            graph_evidence_source_document_ids,
            document_index,
            chunks,
        );
        seed_document_head_source_context_anchors(&mut candidates);
    }
    // T2: source-slice loader now honours `temporal_constraints` via the
    // storage substring filter on `occurred_at=ISO` headers, so we no longer
    // skip the slice path when bounds are present. Tail-N inside a
    // bounded window now returns the chronological tail within the
    // window instead of the unconditional tail of the file.
    let (slice_temporal_start, slice_temporal_end) =
        query_ir.map_or((None, None), |ir| ir.resolved_temporal_bounds());
    if let Some(slice) = query_ir.and_then(|ir| ir.source_slice.as_ref())
        && let Some(diagnostics) = apply_ordered_source_slice_context(OrderedSourceSliceRequest {
            state,
            document_index,
            plan_keywords,
            chunks: &mut *chunks,
            candidates: &candidates,
            slice,
            temporal_start: slice_temporal_start,
            temporal_end: slice_temporal_end,
        })
        .await?
    {
        return Ok(StructuredSourceContextDiagnostics {
            eligible_document_count: candidates.len(),
            ..diagnostics
        });
    }
    let result = complete_structured_source_context(
        state,
        StructuredSourceContextCompletion {
            library_id,
            query_ir,
            document_index,
            plan_keywords,
            focus_keywords: &focus_keywords,
            fallback_structured_context_requested,
            structured_source_unit_inventory_context_requested,
            table_row_context_requested,
            promote_source_profiles,
            fallback_structured_candidate_expansion_requested,
            inventory_profile_companions,
            slice_temporal_start,
            slice_temporal_end,
            candidates,
            chunks,
            companions,
        },
    )
    .await;
    crate::services::query::turn_spans::record_span(
        "context.source_context",
        "db",
        span_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        None,
        None,
    );
    result
}

pub(super) struct StructuredSourceContextCompletion<'a> {
    library_id: Uuid,
    query_ir: Option<&'a QueryIR>,
    document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &'a [String],
    focus_keywords: &'a [String],
    fallback_structured_context_requested: bool,
    structured_source_unit_inventory_context_requested: bool,
    table_row_context_requested: bool,
    promote_source_profiles: bool,
    fallback_structured_candidate_expansion_requested: bool,
    inventory_profile_companions: Vec<StructuredSourceCompanion>,
    slice_temporal_start: Option<chrono::DateTime<chrono::Utc>>,
    slice_temporal_end: Option<chrono::DateTime<chrono::Utc>>,
    candidates: Vec<SourceContextCandidate>,
    chunks: &'a mut Vec<RuntimeMatchedChunk>,
    companions: Vec<StructuredSourceCompanion>,
}

pub(super) async fn complete_structured_source_context(
    state: &AppState,
    request: StructuredSourceContextCompletion<'_>,
) -> anyhow::Result<StructuredSourceContextDiagnostics> {
    let StructuredSourceContextCompletion {
        library_id,
        query_ir,
        document_index,
        plan_keywords,
        focus_keywords,
        fallback_structured_context_requested,
        structured_source_unit_inventory_context_requested,
        table_row_context_requested,
        promote_source_profiles,
        fallback_structured_candidate_expansion_requested,
        inventory_profile_companions,
        slice_temporal_start,
        slice_temporal_end,
        candidates,
        chunks,
        mut companions,
    } = request;
    let candidate_revision_ids = unique_candidate_revision_ids(&candidates);
    let focused_rows_by_revision = chunks_by_revision(
        state
            .document_store
            .list_chunks_by_revisions_matching_terms(
                &candidate_revision_ids,
                focus_keywords,
                SOURCE_CONTEXT_FOCUSED_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT,
            )
            .await
            .context(
                "failed to load query-focused source context chunks for candidate revisions",
            )?,
    );
    let path_context_requested = query_ir.is_some_and(requests_path_source_context);
    let configuration_path_context =
        query_ir.is_some_and(requests_configuration_file_path_source_context);
    let path_term = "/".to_string();
    let path_rows_by_revision = if path_context_requested {
        chunks_by_revision(
            state
                .document_store
                .list_chunks_by_revisions_matching_terms(
                    &candidate_revision_ids,
                    std::slice::from_ref(&path_term),
                    SOURCE_CONTEXT_PATH_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT,
                )
                .await
                .context(
                    "failed to load path-bearing source context chunks for candidate revisions",
                )?,
        )
    } else {
        HashMap::new()
    };
    let path_head_rows_by_revision = if path_context_requested && configuration_path_context {
        let windows = candidate_revision_ids
            .iter()
            .map(|revision_id| {
                (
                    *revision_id,
                    SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX,
                    SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD,
                )
            })
            .collect::<Vec<_>>();
        chunks_by_revision(
            state.document_store.list_chunks_by_revisions_windows(&windows).await.context(
                "failed to load setup path source context chunks for candidate revisions",
            )?,
        )
    } else {
        HashMap::new()
    };

    let existing_chunk_ids = chunks.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let prepared_candidates = prepare_source_context_candidates(SourceContextCandidateInputs {
        candidates: &candidates,
        focused_rows_by_revision: &focused_rows_by_revision,
        path_rows_by_revision: &path_rows_by_revision,
        path_head_rows_by_revision: &path_head_rows_by_revision,
        focus_keywords,
        path_context_requested,
        configuration_path_context,
        existing_chunk_ids: &existing_chunk_ids,
    });

    let procedural_context_requested = query_ir.is_some_and(requests_procedural_source_context)
        || fallback_structured_context_requested
        || structured_source_unit_inventory_context_requested
        || table_row_context_requested;
    let procedural_windows = if procedural_context_requested {
        let structured_forward = if table_row_context_requested {
            SOURCE_CONTEXT_TABLE_STRUCTURED_FORWARD
        } else {
            SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD
        };
        prepared_candidates
            .iter()
            .flat_map(|prepared| {
                procedural_structured_sibling_windows(
                    &prepared.neighbor_anchors,
                    structured_forward,
                )
                .into_iter()
                .map(|(min_index, max_index)| {
                    (prepared.candidate.revision_id, min_index, max_index)
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let procedural_rows_by_revision = if procedural_context_requested {
        chunks_by_revision(
            state
                .document_store
                .list_chunks_by_revisions_windows(&procedural_windows)
                .await
                .context(
                    "failed to load procedural structured source siblings for candidate revisions",
                )?,
        )
    } else {
        HashMap::new()
    };

    let neighbor_span = source_context_neighbor_span_for_request(
        query_ir,
        fallback_structured_context_requested || structured_source_unit_inventory_context_requested,
    );
    let neighbor_windows = prepared_candidates
        .iter()
        .flat_map(|prepared| {
            source_context_neighbor_windows(&prepared.neighbor_anchors, neighbor_span)
                .into_iter()
                .map(|(min_index, max_index)| {
                    (prepared.candidate.revision_id, min_index, max_index)
                })
        })
        .collect::<Vec<_>>();
    let neighbor_rows_by_revision = chunks_by_revision(
        state
            .document_store
            .list_chunks_by_revisions_windows(&neighbor_windows)
            .await
            .context("failed to load structured source neighbor chunks for candidate revisions")?,
    );

    append_prepared_source_context_companions(
        PreparedSourceContextCompanions {
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
            procedural_rows_by_revision: &procedural_rows_by_revision,
            neighbor_rows_by_revision: &neighbor_rows_by_revision,
        },
        &mut companions,
    )
    .await?;

    append_optional_source_context(
        state,
        library_id,
        OptionalSourceContextRequest {
            query_ir,
            document_index,
            plan_keywords,
            focus_keywords,
            fallback_structured_candidate_expansion_requested,
            promote_source_profiles,
            inventory_profile_companions,
            slice_temporal_start,
            slice_temporal_end,
        },
        chunks,
        &mut companions,
    )
    .await?;

    let mut diagnostics = apply_structured_source_companions(chunks, companions);
    diagnostics.eligible_document_count = candidates.len();
    Ok(diagnostics)
}
