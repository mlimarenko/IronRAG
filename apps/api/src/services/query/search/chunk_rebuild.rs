use super::{
    persistence::{load_vector_write_source_fence, resolve_active_embedding_binding},
    rebuild::{
        discard_abandoned_vector_rebuilds, discard_staged_rebuild_on_error,
        discard_staged_vector_rebuild_if_prepared, ensure_embedding_profile_stable_after_rebuild,
        ensure_rebuilt_chunk_embedding_inventory, finish_deferred_manifest_rebuild,
        mark_revisions_vector_ready, vector_rebuild_staging_profile_key,
    },
    *,
};

/// Inputs for [`SearchService::rebuild_chunk_embeddings_with_dimension_state`].
pub(super) struct ChunkEmbeddingRebuildInput<'a> {
    pub state: &'a AppState,
    pub library_id: Uuid,
    pub dimension_state: EmbeddingDimensionState,
    pub embedding_binding: &'a ResolvedRuntimeBinding,
    pub active_embedding_profile_key: &'a str,
    pub staging_embedding_model_key: &'a str,
}

impl SearchService {
    pub async fn rebuild_chunk_embeddings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> std::result::Result<usize, QueryServiceError> {
        let _rebuild_guard = self.vector_rebuild_guard(state, library_id).await?;
        discard_abandoned_vector_rebuilds(state, library_id).await?;
        let source_truth_version = load_vector_write_source_fence(state, library_id).await?;
        if state
            .document_store
            .count_active_chunks_by_library(library_id)
            .await
            .context("failed to count active chunks before chunk-vector rebuild")?
            == 0
        {
            self.purge_empty_vector_plane_under_write_guard(
                state,
                library_id,
                source_truth_version,
            )
            .await?;
            return Ok(0);
        }
        let embedding_binding = resolve_active_embedding_binding(state, library_id).await?;
        let dimension_state =
            library_vector_index_dimension_state_for_binding(state, library_id, &embedding_binding)
                .await?;
        let active_embedding_profile_key = embedding_binding.embedding_execution_profile_key();
        let staging_embedding_model_key = vector_rebuild_staging_profile_key(
            &active_embedding_profile_key,
            Uuid::now_v7(),
            source_truth_version,
        );
        let mut prepared_dimensions = None;
        let staged_result = async {
            let (rebuild, target_dimensions) = self
                .rebuild_chunk_embeddings_with_dimension_state(
                    ChunkEmbeddingRebuildInput {
                        state,
                        library_id,
                        dimension_state,
                        embedding_binding: &embedding_binding,
                        active_embedding_profile_key: &active_embedding_profile_key,
                        staging_embedding_model_key: &staging_embedding_model_key,
                    },
                    &mut prepared_dimensions,
                )
                .await?;
            ensure_embedding_profile_stable_after_rebuild(
                state,
                library_id,
                &active_embedding_profile_key,
            )
            .await?;
            ensure_rebuilt_chunk_embedding_inventory(
                state,
                library_id,
                &staging_embedding_model_key,
                rebuild.rebuilt,
            )
            .await?;
            let expected_chunk_count =
                u64::try_from(rebuild.rebuilt).context("rebuilt chunk count overflowed u64")?;
            Ok::<_, QueryServiceError>((rebuild, expected_chunk_count, target_dimensions))
        }
        .await;
        let (rebuild, expected_chunk_count, target_dimensions) = discard_staged_rebuild_on_error(
            state,
            library_id,
            prepared_dimensions,
            &staging_embedding_model_key,
            staged_result,
        )
        .await?;
        if let Err(error) = self
            .promote_staged_vector_rebuild_under_write_guard(
                state,
                StagedVectorRebuildPromotion {
                    library_id,
                    dimensions: target_dimensions.get(),
                    active_embedding_profile_key: &active_embedding_profile_key,
                    staging_embedding_model_key: &staging_embedding_model_key,
                    expected_source_truth_version: source_truth_version,
                    chunk_count: Some(expected_chunk_count),
                    entity_count: None,
                },
            )
            .await
        {
            return Err(discard_staged_vector_rebuild_if_prepared(
                state,
                library_id,
                prepared_dimensions,
                &staging_embedding_model_key,
                error,
            )
            .await);
        }
        invalidate_library_vector_index_dimensions(library_id);
        ensure_rebuilt_chunk_embedding_inventory(
            state,
            library_id,
            &active_embedding_profile_key,
            rebuild.rebuilt,
        )
        .await?;
        mark_revisions_vector_ready(state, &rebuild.touched_revision_ids)
            .await
            .context("failed to mark rebuilt revisions as vector-ready")?;
        invalidate_library_embedding_profile_inventory(library_id);
        Ok(rebuild.rebuilt)
    }

    pub(super) async fn rebuild_chunk_embeddings_with_dimension_state(
        &self,
        input: ChunkEmbeddingRebuildInput<'_>,
        prepared_dimensions: &mut Option<u64>,
    ) -> std::result::Result<(ChunkEmbeddingRebuildOutcome, EmbeddingDimensions), QueryServiceError>
    {
        let ChunkEmbeddingRebuildInput {
            state,
            library_id,
            dimension_state,
            embedding_binding,
            active_embedding_profile_key,
            staging_embedding_model_key,
        } = input;
        let mut dimensions = match dimension_state {
            EmbeddingDimensionState::Known(dimensions) => Some(dimensions),
            EmbeddingDimensionState::Unobserved => None,
        };
        if let Some(dimensions) = dimensions {
            state
                .search_store
                .prepare_chunk_vector_rebuild_lane(
                    library_id,
                    dimensions.get(),
                    staging_embedding_model_key,
                )
                .await
                .context("failed to prepare chunk vector manifest lane for rebuild")?;
            *prepared_dimensions = Some(dimensions.get());
        }
        let rebuild_result = async {
            let mut revision_number_cache = HashMap::<Uuid, i64>::new();
            let mut touched_revision_ids = BTreeSet::new();
            let mut rebuilt = 0usize;
            let mut after = None;

            loop {
                let chunks = state
                    .document_store
                    .list_active_chunks_by_library_page(
                        library_id,
                        after,
                        CHUNK_REBUILD_FETCH_PAGE_SIZE,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to stream knowledge chunks for library {library_id} rebuild"
                        )
                    })?;
                let Some(next_after) = chunk_rebuild_page_cursor(&chunks) else {
                    if rebuilt == 0 {
                        return Err(QueryServiceError::StateConflict {
                            message: format!(
                                "active chunk inventory changed while rebuilding library {library_id}; retry the rebuild"
                            ),
                        });
                    }
                    break;
                };
                let page_is_complete = chunks.len() < CHUNK_REBUILD_FETCH_PAGE_SIZE;
                let mut page_offset = 0usize;
                if dimensions.is_none() {
                    let first_batch_len = chunks.len().min(CHUNK_EMBEDDING_BATCH_SIZE);
                    let first_batch = &chunks[..first_batch_len];
                    let freshness = resolve_chunk_freshness_generations(
                        state,
                        first_batch,
                        &mut revision_number_cache,
                    )
                    .await?;
                    let (learned_dimensions, rows) = embed_rebuild_chunk_batch_rows(
                        state,
                        first_batch,
                        &freshness,
                        embedding_binding,
                        staging_embedding_model_key,
                        None,
                    )
                    .await?;
                    // The provider call above is the first ordinary rebuild
                    // batch. Fence the binding before its learned dimension is
                    // allowed to create durable staging metadata.
                    ensure_active_embedding_profile_key(
                        state,
                        library_id,
                        active_embedding_profile_key,
                    )
                    .await?;
                    state
                        .search_store
                        .prepare_chunk_vector_rebuild_lane(
                            library_id,
                            learned_dimensions.get(),
                            staging_embedding_model_key,
                        )
                        .await
                        .context(
                            "failed to prepare learned chunk vector manifest lane for rebuild",
                        )?;
                    *prepared_dimensions = Some(learned_dimensions.get());
                    state
                        .search_store
                        .upsert_chunk_vectors_bulk_deferred_manifest(&rows)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to persist {} first-batch rebuilt chunk vectors",
                                rows.len()
                            )
                        })?;
                    rebuilt = rebuilt.checked_add(rows.len()).ok_or_else(|| {
                        anyhow!("rebuilt chunk count overflowed for library {library_id}")
                    })?;
                    touched_revision_ids
                        .extend(first_batch.iter().map(|chunk| chunk.revision_id));
                    dimensions = Some(learned_dimensions);
                    page_offset = first_batch_len;
                }

                if page_offset < chunks.len() {
                    let page_dimensions = dimensions.ok_or_else(|| {
                        QueryServiceError::StateConflict {
                            message: format!(
                                "library {library_id} has remaining chunks but no established vector dimension"
                            ),
                        }
                    })?;
                    let (page_rebuilt, page_touched) = rebuild_chunk_embedding_page(
                        state,
                        &chunks[page_offset..],
                        page_dimensions,
                        embedding_binding,
                        staging_embedding_model_key,
                        &mut revision_number_cache,
                    )
                    .await?;
                    rebuilt = rebuilt.checked_add(page_rebuilt).ok_or_else(|| {
                        anyhow!("rebuilt chunk count overflowed for library {library_id}")
                    })?;
                    touched_revision_ids.extend(page_touched);
                }
                if page_is_complete {
                    break;
                }
                after = Some(next_after);
            }
            let dimensions = dimensions.ok_or_else(|| QueryServiceError::StateConflict {
                message: format!(
                    "library {library_id} rebuild finished without observing a chunk vector dimension"
                ),
            })?;
            Ok((rebuilt, touched_revision_ids, dimensions))
        }
        .await;
        let Some(prepared_dimensions) = *prepared_dimensions else {
            return rebuild_result.map(|(rebuilt, touched_revision_ids, dimensions)| {
                (ChunkEmbeddingRebuildOutcome { rebuilt, touched_revision_ids }, dimensions)
            });
        };
        let reconciliation_result = state
            .search_store
            .reconcile_chunk_vector_manifest_count(
                library_id,
                prepared_dimensions,
                staging_embedding_model_key,
            )
            .await
            .context("failed to reconcile chunk vector manifest after rebuild")
            .map_err(QueryServiceError::from);
        let (rebuilt, touched_revision_ids, dimensions) =
            finish_deferred_manifest_rebuild(rebuild_result, reconciliation_result)?;
        Ok((ChunkEmbeddingRebuildOutcome { rebuilt, touched_revision_ids }, dimensions))
    }
}

pub(super) fn chunk_rebuild_page_cursor(chunks: &[KnowledgeChunkRow]) -> Option<(i32, Uuid)> {
    chunks.last().map(|chunk| (chunk.chunk_index, chunk.chunk_id))
}

async fn rebuild_chunk_embedding_page(
    state: &AppState,
    chunks: &[KnowledgeChunkRow],
    expected_dimensions: EmbeddingDimensions,
    embedding_binding: &ResolvedRuntimeBinding,
    embedding_model_key: &str,
    revision_number_cache: &mut HashMap<Uuid, i64>,
) -> std::result::Result<(usize, BTreeSet<Uuid>), QueryServiceError> {
    let freshness_per_chunk =
        resolve_chunk_freshness_generations(state, chunks, revision_number_cache).await?;

    let parallelism = state.settings.ingestion_embedding_parallelism.max(1);
    let batch_results = stream::iter(
        chunks
            .chunks(CHUNK_EMBEDDING_BATCH_SIZE)
            .zip(freshness_per_chunk.chunks(CHUNK_EMBEDDING_BATCH_SIZE))
            .map(|(chunk_batch, freshness_batch)| {
                let binding = embedding_binding.clone();
                async move {
                    let (_, rows) = embed_rebuild_chunk_batch_rows(
                        state,
                        chunk_batch,
                        freshness_batch,
                        &binding,
                        embedding_model_key,
                        Some(expected_dimensions),
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(rows)
                }
            }),
    )
    .buffer_unordered(parallelism)
    .collect::<Vec<_>>()
    .await;

    let mut rows = Vec::with_capacity(chunks.len());
    for batch_result in batch_results {
        rows.extend(batch_result?);
    }
    state
        .search_store
        .upsert_chunk_vectors_bulk_deferred_manifest(rows.as_slice())
        .await
        .with_context(|| format!("failed to bulk-persist {} rebuilt chunk vectors", rows.len()))?;
    let touched_revision_ids = chunks.iter().map(|chunk| chunk.revision_id).collect();
    Ok((rows.len(), touched_revision_ids))
}

async fn resolve_chunk_freshness_generations(
    state: &AppState,
    chunks: &[KnowledgeChunkRow],
    revision_number_cache: &mut HashMap<Uuid, i64>,
) -> AnyhowResult<Vec<i64>> {
    let mut freshness = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if let Some(generation) = chunk.vector_generation.or(chunk.text_generation) {
            freshness.push(generation);
            continue;
        }
        let revision_number = match revision_number_cache.get(&chunk.revision_id).copied() {
            Some(revision_number) => revision_number,
            None => {
                let revision = state
                    .document_store
                    .get_revision(chunk.revision_id)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to load revision {} for chunk generation",
                            chunk.revision_id
                        )
                    })?
                    .ok_or_else(|| anyhow!("knowledge revision {} not found", chunk.revision_id))?;
                revision_number_cache.insert(chunk.revision_id, revision.revision_number);
                revision.revision_number
            }
        };
        freshness.push(revision_number);
    }
    Ok(freshness)
}

async fn embed_rebuild_chunk_batch_rows(
    state: &AppState,
    chunks: &[KnowledgeChunkRow],
    freshness: &[i64],
    embedding_binding: &ResolvedRuntimeBinding,
    embedding_model_key: &str,
    expected_dimensions: Option<EmbeddingDimensions>,
) -> AnyhowResult<(EmbeddingDimensions, Vec<KnowledgeChunkVectorRow>)> {
    anyhow::ensure!(
        chunks.len() == freshness.len(),
        "chunk rebuild freshness batch length mismatch"
    );
    let response = state
        .llm_gateway
        .embed_many(EmbeddingBatchRequest {
            provider_kind: embedding_binding.provider_kind.clone(),
            model_name: embedding_binding.model_name.clone(),
            inputs: chunks.iter().map(|chunk| chunk.normalized_text.clone()).collect(),
            api_key_override: embedding_binding.api_key.clone(),
            base_url_override: embedding_binding.provider_base_url.clone(),
            extra_parameters_json: embedding_binding.extra_parameters_json.clone(),
        })
        .await
        .with_context(|| {
            let first_chunk_id = chunks.first().map(|chunk| chunk.chunk_id);
            format!("failed to rebuild chunk embeddings for batch starting at {first_chunk_id:?}")
        })?;
    let dimensions = validate_embedding_batch_dimensions(
        chunks.len(),
        response.dimensions,
        &response.embeddings,
        expected_dimensions,
        "chunk rebuild batch",
    )?;
    let rows = chunks
        .iter()
        .zip(freshness)
        .zip(response.embeddings.iter())
        .map(|((chunk, freshness_generation), embedding)| {
            Ok(KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: chunk.workspace_id,
                library_id: chunk.library_id,
                chunk_id: chunk.chunk_id,
                revision_id: chunk.revision_id,
                embedding_model_key: embedding_model_key.to_string(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: validate_embedding_vector_dimensions(
                    dimensions.get(),
                    embedding,
                    format!("rebuilt chunk {}", chunk.chunk_id),
                )?,
                vector: embedding.clone(),
                freshness_generation: *freshness_generation,
                created_at: Utc::now(),
                occurred_at: chunk.occurred_at,
                occurred_until: chunk.occurred_until,
            })
        })
        .collect::<AnyhowResult<Vec<_>>>()?;
    Ok((dimensions, rows))
}
