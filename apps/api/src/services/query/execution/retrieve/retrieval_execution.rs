use super::{
    BTreeSet, ChunkRetrievalLaneOutcome, DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT,
    FOCUS_BROADEN_MIN_CHUNKS, FinalChunkShapingRequest, HashSet, InitialCompanionRequest,
    LatestVersionDocument, MAX_CHUNKS_PER_DOCUMENT, MergedCompanionChunks,
    PostMergeCompanionRequest, PrimaryRetrievalRequest, QUERY_IR_FOCUS_CHUNK_CAP,
    RetrievalExecutionRequest, RuntimeChunkScoreKind, RuntimeMatchedChunk, Uuid,
    apply_final_chunk_shaping, build_lexical_queries, diversify_chunks_by_document,
    execute_primary_retrieval, load_initial_companion_chunks, load_post_merge_companion_chunks,
    merge_chunks, merge_initial_companion_chunks, query_ir_focus_chunk_score,
    query_ir_lexical_focus_queries, question_asks_table_aggregation, record_stage_elapsed,
    requested_initial_table_row_count, reuse_or_execute_primary, should_broaden_focus, timed_stage,
};

pub(super) async fn retrieve_document_chunks_with_targets(
    request: RetrievalExecutionRequest<'_>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let RetrievalExecutionRequest {
        state,
        library_id,
        question,
        targeted_document_ids,
        runtime_plan: plan,
        limit,
        question_embedding,
        vector_search_context,
        document_index,
        query_ir,
        allow_broaden,
        text_search_config,
        reusable_primary,
    } = request;
    let initial_table_row_count = requested_initial_table_row_count(query_ir);
    let targeted_table_aggregation =
        question_asks_table_aggregation(question, query_ir) && !targeted_document_ids.is_empty();
    let query_ir_focus_queries = query_ir.map(query_ir_lexical_focus_queries).unwrap_or_default();
    let lexical_queries = build_lexical_queries(question, plan, &query_ir_focus_queries, query_ir);
    let lexical_limit = limit.saturating_mul(2).max(24);
    let plan_keywords = &plan.keywords;
    let targeted_document_ids_ref = &targeted_document_ids;
    // Inferred focus is the only path that may broaden. Hydrate its primary
    // results without the provisional target filter once, then retain a
    // scoped view for the first pass. A broaden replan can therefore reuse
    // the same lexical/vector searches instead of paying for them twice.
    let broad_primary_scope = BTreeSet::new();
    let primary_hydration_document_ids_ref =
        if allow_broaden { &broad_primary_scope } else { targeted_document_ids_ref };
    // Resolved temporal bounds — applied as a hard filter on every
    // chunk-touching search lane. None when QueryIR has no temporal
    // constraints or none parsed as RFC3339.
    let (temporal_start, temporal_end) =
        query_ir.map_or((None, None), |ir| ir.resolved_temporal_bounds());

    let (lane_outcome, primary_reused) = timed_stage(
        "retrieve.primary",
        reuse_or_execute_primary(reusable_primary, || {
            execute_primary_retrieval(PrimaryRetrievalRequest {
                state,
                library_id,
                limit,
                question_embedding,
                vector_search_context,
                document_index,
                plan_keywords,
                hydration_document_ids: primary_hydration_document_ids_ref,
                lexical_queries,
                lexical_limit,
                text_search_config,
                temporal_start,
                temporal_end,
            })
        }),
    )
    .await?;
    if primary_reused {
        log_primary_retrieval_reuse(&lane_outcome);
    }
    log_chunk_fanout(&lane_outcome);
    let reusable_primary = lane_outcome.clone();
    let mut chunks = merge_chunks(
        lane_outcome.vector_hits,
        lane_outcome.lexical_hits,
        limit.max(initial_table_row_count.unwrap_or(0)),
    );
    if allow_broaden && !targeted_document_ids.is_empty() {
        chunks.retain(|chunk| targeted_document_ids.contains(&chunk.document_id));
    }
    let primary_evidence_available = !chunks.is_empty();
    let initial_companions = Box::pin(timed_stage(
        "retrieve.initial_companions",
        load_initial_companion_chunks(InitialCompanionRequest {
            state,
            library_id,
            question,
            targeted_document_ids: &targeted_document_ids,
            plan_keywords,
            document_index,
            query_ir,
            query_ir_focus_queries: &query_ir_focus_queries,
            temporal_start,
            temporal_end,
            primary_evidence_available,
        }),
    ))
    .await?;
    let merge_initial_started = std::time::Instant::now();
    let MergedCompanionChunks {
        chunks,
        retrieval_plan,
        latest_version_document_ids,
        protected_document_ids,
    } = merge_initial_companion_chunks(
        chunks,
        initial_companions,
        &targeted_document_ids,
        query_ir,
        limit,
        initial_table_row_count,
    )?;
    record_stage_elapsed("retrieve.merge_initial", merge_initial_started);

    let MergedCompanionChunks {
        mut chunks,
        latest_version_document_ids,
        protected_document_ids,
        ..
    } = timed_stage(
        "retrieve.post_merge_companions",
        load_post_merge_companion_chunks(
            MergedCompanionChunks {
                chunks,
                retrieval_plan,
                latest_version_document_ids,
                protected_document_ids,
            },
            PostMergeCompanionRequest {
                state,
                library_id,
                question,
                document_index,
                plan_keywords,
                query_ir,
                temporal_start,
                temporal_end,
                limit,
                primary_evidence_available,
            },
        ),
    )
    .await?;
    let max_chunks_per_document = if targeted_document_ids.is_empty() {
        MAX_CHUNKS_PER_DOCUMENT
    } else {
        MAX_CHUNKS_PER_DOCUMENT.max(DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT)
    };
    let diversify_retain_started = std::time::Instant::now();
    chunks = diversify_chunks_by_document(chunks, max_chunks_per_document, &protected_document_ids);
    retain_scoped_documents(
        &mut chunks,
        &targeted_document_ids,
        &latest_version_document_ids,
        &protected_document_ids,
    );
    record_stage_elapsed("retrieve.diversify_retain", diversify_retain_started);
    chunks = timed_stage(
        "retrieve.final_shaping",
        apply_final_chunk_shaping(
            chunks,
            FinalChunkShapingRequest {
                state,
                targeted_document_ids: &targeted_document_ids,
                document_index,
                plan_keywords,
                query_ir,
                temporal_start,
                temporal_end,
                limit,
                initial_table_row_count,
                targeted_table_aggregation,
            },
        ),
    )
    .await?;

    // P3 coverage fallback. A compiler-inferred single-document focus pin
    // can hard-lock onto a thin same-titled stub and exclude the real
    // procedure docs. When the narrowed retrieval comes back at/below the
    // coverage floor, drop the pin once and re-broaden to the whole library
    // so the answer step has material to work with. `allow_broaden` is false
    // on the recursive call; the cached lexical/vector primary result is
    // reused and only the structurally changed companion plan runs again.
    if should_broaden_focus(allow_broaden, chunks.len()) {
        log_focus_broaden_fallback(library_id, targeted_document_ids.len(), chunks.len());
        return Box::pin(retrieve_document_chunks_with_targets(RetrievalExecutionRequest {
            state,
            library_id,
            question,
            targeted_document_ids: BTreeSet::new(),
            runtime_plan: plan,
            limit,
            question_embedding,
            vector_search_context,
            document_index,
            query_ir,
            allow_broaden: false,
            text_search_config,
            reusable_primary: Some(reusable_primary),
        }))
        .await;
    }

    Ok(chunks)
}

/// Logs the reuse of a cached primary retrieval result for a focus-broaden replan.
fn log_primary_retrieval_reuse(lane_outcome: &ChunkRetrievalLaneOutcome) {
    tracing::info!(
        stage = "retrieval.primary_reused",
        vector_hits = lane_outcome.vector_hits.len(),
        lexical_hits = lane_outcome.lexical_hits.len(),
        "reused lexical/vector primary results for focus-broaden replan"
    );
}

/// Logs the vector + lexical primary fan-out outcome of this retrieval pass.
fn log_chunk_fanout(lane_outcome: &ChunkRetrievalLaneOutcome) {
    tracing::info!(
        stage = "retrieval.chunks_fanout",
        vector_elapsed_ms = lane_outcome.vector_elapsed_ms,
        vector_hits = lane_outcome.vector_hits.len(),
        lexical_elapsed_ms = lane_outcome.lexical_elapsed_ms,
        lexical_query_count = lane_outcome.lexical_query_count,
        lexical_hits = lane_outcome.lexical_hits.len(),
        degraded_lane_count = lane_outcome.degraded_lane_count,
        "vector + lexical chunk fan-out"
    );
}

/// Logs the P3 coverage fallback that drops an inferred focus pin and re-broadens.
fn log_focus_broaden_fallback(
    library_id: Uuid,
    pinned_document_count: usize,
    narrowed_chunk_count: usize,
) {
    tracing::info!(
        stage = "retrieval.focus_broaden_fallback",
        library_id = %library_id,
        pinned_document_count,
        narrowed_chunk_count,
        coverage_floor = FOCUS_BROADEN_MIN_CHUNKS,
        "inferred focus pin returned too few chunks; dropping pin and re-broadening retrieval"
    );
}

pub(crate) fn retain_scoped_documents(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    targeted_document_ids: &BTreeSet<Uuid>,
    latest_version_document_ids: &BTreeSet<Uuid>,
    protected_document_ids: &BTreeSet<Uuid>,
) {
    let mut scoped_document_ids = targeted_document_ids.clone();
    scoped_document_ids.extend(latest_version_document_ids.iter().copied());
    scoped_document_ids.extend(protected_document_ids.iter().copied());
    if scoped_document_ids.is_empty() {
        return;
    }
    chunks.retain(|chunk| scoped_document_ids.contains(&chunk.document_id));
}

pub(super) fn latest_version_scoped_document_ids(
    documents: &[LatestVersionDocument],
    semantic_chunks: &[RuntimeMatchedChunk],
) -> BTreeSet<Uuid> {
    let mut document_ids =
        documents.iter().map(|document| document.document_id).collect::<BTreeSet<_>>();
    document_ids.extend(
        semantic_chunks
            .iter()
            .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::LatestVersion)
            .map(|chunk| chunk.document_id),
    );
    document_ids
}

pub(super) fn combine_lexical_query_results(
    per_query_results: Vec<anyhow::Result<Vec<RuntimeMatchedChunk>>>,
    lexical_query_count: usize,
    lexical_limit: usize,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let mut lexical_hits: Vec<RuntimeMatchedChunk> = Vec::new();
    let mut failed_query_count = 0usize;
    let mut failures = Vec::new();
    for result in per_query_results {
        match result {
            Ok(query_hits) => {
                lexical_hits = merge_chunks(lexical_hits, query_hits, lexical_limit);
            }
            Err(error) => {
                failed_query_count += 1;
                let summary = format!("{error:#}");
                tracing::warn!(
                    stage = "retrieval.lexical_query_failed",
                    error = %summary,
                    retrieval_degraded = true,
                    failed_source = "lexical_query",
                    failed_query_count,
                    lexical_query_count,
                    "lexical chunk search query failed; continuing with other lexical queries"
                );
                failures.push(summary);
            }
        }
    }
    if lexical_query_count > 0 && failed_query_count == lexical_query_count {
        anyhow::bail!("all lexical chunk search queries failed: {}", failures.join("; "));
    }
    // Same rule one level down: surviving subqueries that matched nothing leave
    // the lane with no evidence, and a partial failure is the reason it is
    // empty. Returning that as a clean empty lane hides the degradation from
    // the lane-level evidence floor, which then cannot tell it apart from a
    // question that genuinely had no lexical match.
    if failed_query_count > 0 && lexical_hits.is_empty() {
        anyhow::bail!("lexical chunk search degraded to no evidence: {}", failures.join("; "));
    }
    Ok(lexical_hits)
}

pub(super) fn append_query_ir_focus_hits(
    query_hits: Vec<(Uuid, f32)>,
    seen: &mut HashSet<Uuid>,
    hits: &mut Vec<(Uuid, f32)>,
) {
    for (chunk_id, raw_score) in query_hits {
        if hits.len() >= QUERY_IR_FOCUS_CHUNK_CAP {
            break;
        }
        if seen.insert(chunk_id) {
            let fallback_score = query_ir_focus_chunk_score(hits.len());
            let score =
                if raw_score.is_finite() && raw_score > 0.0 { raw_score } else { fallback_score };
            hits.push((chunk_id, score));
        }
    }
}

pub(super) fn combine_query_ir_focus_search_results(
    per_query_results: Vec<anyhow::Result<Vec<(Uuid, f32)>>>,
    focus_query_count: usize,
) -> anyhow::Result<Vec<(Uuid, f32)>> {
    let mut hits = Vec::new();
    let mut seen = HashSet::new();
    let mut failed_query_count = 0usize;
    let mut failures = Vec::new();

    for result in per_query_results {
        match result {
            Ok(query_hits) => {
                append_query_ir_focus_hits(query_hits, &mut seen, &mut hits);
            }
            Err(error) => {
                failed_query_count += 1;
                let summary = format!("{error:#}");
                tracing::warn!(
                    stage = "retrieval.query_ir_focus_query_failed",
                    error = %summary,
                    retrieval_degraded = true,
                    failed_source = "query_ir_focus",
                    failed_query_count,
                    focus_query_count,
                    "query-IR focus chunk search query failed; continuing with other focus queries"
                );
                failures.push(summary);
            }
        }
        if hits.len() >= QUERY_IR_FOCUS_CHUNK_CAP {
            break;
        }
    }

    if focus_query_count > 0 && failed_query_count == focus_query_count {
        anyhow::bail!("all query-IR focus chunk searches failed: {}", failures.join("; "));
    }

    Ok(hits)
}
