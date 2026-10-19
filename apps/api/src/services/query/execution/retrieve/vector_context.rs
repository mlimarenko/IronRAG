use anyhow::Context;

use super::{
    AiBindingPurpose, AppState, BTreeSet, EmbeddingProfileIndexState,
    EmbeddingProfileInventoryVersion, HashMap, KnowledgeChunkRow, KnowledgeDocumentRow,
    QueryServiceError, RuntimeMatchedChunk, RuntimeQueryMode, RuntimeQueryPlan,
    RuntimeVectorSearchContext, Uuid, ensure_active_embedding_profile_key,
    ensure_embedding_profile_inventory_version_current, ensure_library_embedding_profile_indexed,
    is_source_profile_chunk_row, load_embedding_profile_inventory_version, map_chunk_hit,
    score_desc_chunks,
};

/// Hydrate a bag of `(chunk_id, score)` hits into ranked
/// `RuntimeMatchedChunk` rows with exactly one knowledge-store read.
/// The previous `join_all(get_chunk)` pattern turned every hit into a
/// separate coordinator call — on a typical 16-hit vector + 4×20-hit
/// lexical fan-out that was ~100 sequential chunk fetches per
/// grounded_answer turn. Batch hydration collapses them into ≤5.
///
/// Score/order is preserved via an id→score map: `list_chunks_by_ids`
/// returns rows unordered, so we re-zip the scores in a hash lookup
/// instead of relying on the database's ordering.
pub(super) async fn batch_hydrate_hits(
    state: &AppState,
    hits: Vec<(Uuid, f32)>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    targeted_document_ids: &BTreeSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    // Build the score lookup and the id list in one pass. Dedupe ids
    // — a hit list can legitimately contain the same chunk across
    // vector and lexical queries before the RRF merge, and we don't
    // want to waste payload bytes on duplicate filter args.
    let mut score_by_chunk: HashMap<Uuid, f32> = HashMap::with_capacity(hits.len());
    for (chunk_id, score) in &hits {
        // Keep the best (highest) score if the same chunk appears
        // twice. Ranking downstream expects a single row per chunk.
        score_by_chunk
            .entry(*chunk_id)
            .and_modify(|existing| {
                if *score > *existing {
                    *existing = *score;
                }
            })
            .or_insert(*score);
    }
    let chunk_ids: Vec<Uuid> = score_by_chunk.keys().copied().collect();
    let chunk_rows = state
        .document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to batch-load runtime query chunks")?;
    let mut mapped: Vec<RuntimeMatchedChunk> = Vec::with_capacity(chunk_rows.len());
    for chunk in chunk_rows {
        let Some(score) = score_by_chunk.get(&chunk.chunk_id).copied() else {
            continue;
        };
        if !is_answer_driving_search_chunk_row(&chunk) {
            continue;
        }
        if !targeted_document_ids.is_empty() && !targeted_document_ids.contains(&chunk.document_id)
        {
            continue;
        }
        let Some(matched) = map_chunk_hit(chunk, score, document_index, plan_keywords) else {
            continue;
        };
        mapped.push(matched);
    }
    // Preserve score order — the merge/rerank pipeline relies on
    // the hit list coming in "best-first".
    mapped.sort_by(score_desc_chunks);
    Ok(mapped)
}

pub(super) fn is_answer_driving_search_chunk_row(chunk: &KnowledgeChunkRow) -> bool {
    !is_source_profile_chunk_row(chunk)
}

pub(crate) async fn prepare_runtime_vector_search_context(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<Option<RuntimeVectorSearchContext>> {
    let _vector_guard =
        state.application_services.search.vector_plane_read_guard(state, library_id).await?;
    let version = load_embedding_profile_inventory_version(state, library_id).await?;
    let binding = state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await
        .context("failed to resolve embed_chunk binding for runtime vector search")?
        .ok_or_else(|| QueryServiceError::StateConflict {
            message: format!(
                "active embed_chunk binding is unavailable while proving the exact vector inventory for library {library_id}; configure the binding and rebuild before querying"
            ),
        })?;

    let embedding_profile_key = binding.embedding_execution_profile_key();
    let index_state = ensure_library_embedding_profile_indexed(
        state,
        library_id,
        &embedding_profile_key,
        version,
    )
    .await?;

    let dimensions = match index_state {
        EmbeddingProfileIndexState::Empty => {
            ensure_embedding_profile_inventory_version_current(state, library_id, version).await?;
            tracing::info!(
                stage = "embed.skip",
                reason = "empty_vector_inventory",
                library_id = %library_id,
                "query embedding and ANN lookup skipped for an empty library"
            );
            return Ok(None);
        }
        EmbeddingProfileIndexState::Ready { dimensions } => dimensions,
    };

    Ok(Some(RuntimeVectorSearchContext {
        embedding_profile_key,
        dimensions,
        active_vector_generation: version.active_vector_generation,
        source_truth_version: version.source_truth_version,
    }))
}

/// Cheap request-boundary fence after provider work. The exact inventory was
/// already validated once by [`prepare_runtime_vector_search_context`]; this
/// recheck only verifies that generation/profile identity did not change
/// before either ANN lane consumes the shared preflight.
pub(crate) async fn validate_runtime_vector_search_context(
    state: &AppState,
    library_id: Uuid,
    context: &RuntimeVectorSearchContext,
) -> anyhow::Result<()> {
    ensure_embedding_profile_inventory_version_current(
        state,
        library_id,
        EmbeddingProfileInventoryVersion {
            active_vector_generation: context.active_vector_generation,
            source_truth_version: context.source_truth_version,
            has_ready_vector: true,
        },
    )
    .await?;
    ensure_active_embedding_profile_key(state, library_id, &context.embedding_profile_key).await?;
    Ok(())
}

pub(crate) fn expanded_candidate_limit(
    planned_mode: RuntimeQueryMode,
    top_k: usize,
    rerank_enabled: bool,
    rerank_candidate_limit: usize,
) -> usize {
    if matches!(planned_mode, RuntimeQueryMode::Hybrid | RuntimeQueryMode::Mix) {
        let intrinsic_limit = top_k.saturating_mul(3).clamp(top_k, 96);
        if rerank_enabled {
            return intrinsic_limit.max(rerank_candidate_limit);
        }
        return intrinsic_limit;
    }
    top_k
}

/// Always returns `false` — both vector and lexical lanes run on every
/// query. The `exact_literal_technical` planner bit still affects boosts
/// and context packing; it does not disable the semantic lane.
pub(crate) const fn should_skip_vector_search(_plan: &RuntimeQueryPlan) -> bool {
    false
}
