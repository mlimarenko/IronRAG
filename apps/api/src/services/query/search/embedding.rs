use super::{
    failures::{
        fail_embed_chunks_after_cleanup, fail_embed_chunks_by_typed_policy,
        fail_embed_chunks_cancelled, fail_embed_chunks_if_cancelled,
    },
    persistence::resolve_active_embedding_binding,
    reuse::{
        ResolveReusedRevisionChunkIdsInput, chunk_ids_missing_vectors, embed_coverage_is_complete,
        resolve_reused_revision_chunk_ids,
    },
    *,
};

struct RevisionChunkEmbeddingBatchWrite<'a> {
    state: &'a AppState,
    library_id: Uuid,
    chunks: &'a [&'a KnowledgeChunkRow],
    batch: &'a [usize],
    batch_response: &'a EmbeddingBatchResponse,
    embedding_profile_key: &'a str,
    freshness_generation: i64,
    expected_dimensions: Option<EmbeddingDimensions>,
    expected_source_truth_version: i64,
    ingest_attempt: IngestVectorWriteFence,
    cleanup_owned_vector_ids: &'a mut BTreeSet<Uuid>,
    cancellation_token: &'a CancellationToken,
}

pub(super) fn take_dimension_learning_batch(
    observed_dimensions: Option<EmbeddingDimensions>,
    batches: &mut Vec<ChunkEmbeddingBatch>,
) -> Option<ChunkEmbeddingBatch> {
    if observed_dimensions.is_some() || batches.is_empty() {
        return None;
    }
    Some(batches.remove(0))
}

struct RevisionChunkEmbeddingRun<'a> {
    service: &'a SearchService,
    state: &'a AppState,
    library_id: Uuid,
    revision_id: Uuid,
    attempt_id: Uuid,
    source_truth_version: i64,
    cancellation_token: &'a CancellationToken,
    binding: &'a ResolvedRuntimeBinding,
    embedding_profile_key: &'a str,
    freshness_generation: i64,
    ingest_attempt: IngestVectorWriteFence,
    total_chunk_count: usize,
    reused_chunk_count: usize,
}

impl RevisionChunkEmbeddingRun<'_> {
    async fn resolve_request_result(
        &self,
        cleanup_owned_vector_ids: &BTreeSet<Uuid>,
        result: AnyhowResult<(Vec<usize>, EmbeddingBatchResponse)>,
    ) -> std::result::Result<(Vec<usize>, EmbeddingBatchResponse), QueryServiceError> {
        match result {
            Ok(result) => Ok(result),
            Err(error) if anyhow_is_cancelled(&error) => {
                fail_embed_chunks_cancelled(self.revision_id)
            }
            Err(error) => {
                fail_embed_chunks_by_typed_policy(
                    self.state,
                    self.revision_id,
                    self.source_truth_version,
                    self.ingest_attempt,
                    cleanup_owned_vector_ids,
                    error,
                )
                .await
            }
        }
    }

    async fn persist_batch(
        &self,
        chunks: &[&KnowledgeChunkRow],
        batch: &[usize],
        response: &EmbeddingBatchResponse,
        expected_dimensions: Option<EmbeddingDimensions>,
        cleanup_owned_vector_ids: &mut BTreeSet<Uuid>,
    ) -> std::result::Result<(usize, EmbeddingDimensions), QueryServiceError> {
        let result = self
            .service
            .persist_revision_chunk_embedding_batch(RevisionChunkEmbeddingBatchWrite {
                state: self.state,
                library_id: self.library_id,
                chunks,
                batch,
                batch_response: response,
                embedding_profile_key: self.embedding_profile_key,
                freshness_generation: self.freshness_generation,
                expected_dimensions,
                expected_source_truth_version: self.source_truth_version,
                ingest_attempt: self.ingest_attempt,
                cleanup_owned_vector_ids: &mut *cleanup_owned_vector_ids,
                cancellation_token: self.cancellation_token,
            })
            .await;
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                fail_embed_chunks_by_typed_policy(
                    self.state,
                    self.revision_id,
                    self.source_truth_version,
                    self.ingest_attempt,
                    cleanup_owned_vector_ids,
                    error,
                )
                .await
            }
        }
    }

    async fn cleanup_error<T>(
        &self,
        cleanup_owned_vector_ids: &BTreeSet<Uuid>,
        result: AnyhowResult<T>,
    ) -> std::result::Result<T, QueryServiceError> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                fail_embed_chunks_after_cleanup(
                    self.state,
                    self.revision_id,
                    self.source_truth_version,
                    self.ingest_attempt,
                    cleanup_owned_vector_ids,
                    error,
                )
                .await
            }
        }
    }

    async fn sync_progress(&self, chunks_embedded: usize) {
        sync_embed_chunk_stage_progress(
            self.state,
            self.attempt_id,
            chunks_embedded.saturating_add(self.reused_chunk_count),
            self.total_chunk_count,
        )
        .await;
    }
}

async fn sync_embed_chunk_stage_progress(
    state: &AppState,
    attempt_id: Uuid,
    completed_units: usize,
    total_units: usize,
) {
    if total_units == 0 {
        return;
    }
    let command = RecordStageUnitProgressCommand {
        attempt_id,
        stage_name: INGEST_STAGE_EMBED_CHUNK.to_string(),
        completed_units: u32::try_from(completed_units).unwrap_or(u32::MAX),
        total_units: u32::try_from(total_units).unwrap_or(u32::MAX),
        details_json: serde_json::json!({}),
    };
    if let Err(error) =
        state.application_services.ingest.record_stage_unit_progress(state, command).await
    {
        tracing::warn!(
            attempt_id = %attempt_id,
            ?error,
            "failed to sync embed_chunk stage progress"
        );
    }
}

async fn request_revision_chunk_embedding_batch(
    state: &AppState,
    revision_id: Uuid,
    chunks: &[&KnowledgeChunkRow],
    batch: Vec<usize>,
    binding: &ResolvedRuntimeBinding,
    cancellation_token: &CancellationToken,
) -> AnyhowResult<(Vec<usize>, EmbeddingBatchResponse)> {
    ensure_not_cancelled(cancellation_token)?;
    let inputs = batch.iter().map(|index| chunks[*index].normalized_text.clone()).collect();
    let first_offset = batch.first().copied().unwrap_or_default();
    let request = EmbeddingBatchRequest {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        inputs,
        api_key_override: binding.api_key.clone(),
        base_url_override: binding.provider_base_url.clone(),
        extra_parameters_json: binding.extra_parameters_json.clone(),
    };
    let response = tokio::select! {
        _ = cancellation_token.cancelled() => {
            return Err(anyhow::Error::new(StageError::Cancelled));
        }
        result = state.llm_gateway.embed_many(request) => result.with_context(|| {
            format!(
                "failed to embed chunk batch for revision {revision_id} starting at offset {first_offset}"
            )
        })?,
    };
    ensure_not_cancelled(cancellation_token)?;
    Ok((batch, response))
}

pub(super) fn revision_chunk_embedding_batches(chunk_count: usize) -> Vec<ChunkEmbeddingBatch> {
    (0..chunk_count)
        .step_by(CHUNK_EMBEDDING_BATCH_SIZE)
        .map(|start| {
            let end = start.saturating_add(CHUNK_EMBEDDING_BATCH_SIZE).min(chunk_count);
            (start..end).collect()
        })
        .collect()
}

async fn learn_revision_embedding_dimensions(
    run: &RevisionChunkEmbeddingRun<'_>,
    chunks: &[&KnowledgeChunkRow],
    batches: &mut Vec<ChunkEmbeddingBatch>,
    observed_dimensions: Option<EmbeddingDimensions>,
    usage: &mut EmbeddingUsageTotals,
    cleanup_owned_vector_ids: &mut BTreeSet<Uuid>,
) -> std::result::Result<(EmbeddingDimensions, usize), QueryServiceError> {
    if let Some(dimensions) = observed_dimensions {
        return Ok((dimensions, 0));
    }
    let first_batch = take_dimension_learning_batch(observed_dimensions, batches).ok_or_else(|| {
        QueryServiceError::StateConflict {
            message: format!(
                "library {} has chunks but no real embedding batch was available to establish its vector dimension",
                run.library_id
            ),
        }
    })?;
    let request = request_revision_chunk_embedding_batch(
        run.state,
        run.revision_id,
        chunks,
        first_batch,
        run.binding,
        run.cancellation_token,
    )
    .await;
    let (first_batch, response) =
        run.resolve_request_result(cleanup_owned_vector_ids, request).await?;
    let (persisted, learned_dimensions) =
        run.persist_batch(chunks, &first_batch, &response, None, cleanup_owned_vector_ids).await?;
    usage.record(&response.usage_json);
    run.sync_progress(persisted).await;
    fail_embed_chunks_if_cancelled(run.revision_id, run.cancellation_token)?;
    Ok((learned_dimensions, persisted))
}

/// Inputs for [`persist_revision_embedding_batches`].
struct PersistRevisionEmbeddingBatchesInput<'a> {
    run: &'a RevisionChunkEmbeddingRun<'a>,
    chunks: &'a [&'a KnowledgeChunkRow],
    batches: Vec<ChunkEmbeddingBatch>,
    expected_dimensions: EmbeddingDimensions,
    parallelism: usize,
    initial_chunks_embedded: usize,
    usage: &'a mut EmbeddingUsageTotals,
    cleanup_owned_vector_ids: &'a mut BTreeSet<Uuid>,
}

async fn persist_revision_embedding_batches(
    input: PersistRevisionEmbeddingBatchesInput<'_>,
) -> std::result::Result<usize, QueryServiceError> {
    let PersistRevisionEmbeddingBatchesInput {
        run,
        chunks,
        batches,
        expected_dimensions,
        parallelism,
        initial_chunks_embedded,
        usage,
        cleanup_owned_vector_ids,
    } = input;
    let batch_responses = stream::iter(batches.into_iter().map(|batch| {
        request_revision_chunk_embedding_batch(
            run.state,
            run.revision_id,
            chunks,
            batch,
            run.binding,
            run.cancellation_token,
        )
    }))
    .buffer_unordered(parallelism)
    .collect::<Vec<_>>()
    .await;
    let mut chunks_embedded = initial_chunks_embedded;
    for batch_result in batch_responses {
        fail_embed_chunks_if_cancelled(run.revision_id, run.cancellation_token)?;
        let (batch, response) =
            run.resolve_request_result(cleanup_owned_vector_ids, batch_result).await?;
        let (persisted, _) = run
            .persist_batch(
                chunks,
                &batch,
                &response,
                Some(expected_dimensions),
                cleanup_owned_vector_ids,
            )
            .await?;
        usage.record(&response.usage_json);
        chunks_embedded = chunks_embedded.saturating_add(persisted);
        if persisted > 0 {
            run.sync_progress(chunks_embedded).await;
            fail_embed_chunks_if_cancelled(run.revision_id, run.cancellation_token)?;
        }
    }
    Ok(chunks_embedded)
}

async fn verify_revision_embedding_coverage(
    run: &RevisionChunkEmbeddingRun<'_>,
    chunks_embedded: usize,
    cleanup_owned_vector_ids: &BTreeSet<Uuid>,
) -> std::result::Result<(), QueryServiceError> {
    fail_embed_chunks_if_cancelled(run.revision_id, run.cancellation_token)?;
    if !embed_coverage_is_complete(chunks_embedded, run.reused_chunk_count, run.total_chunk_count) {
        return run
            .cleanup_error(
                cleanup_owned_vector_ids,
                Err(anyhow!(
                    "embedding coverage mismatch for revision {}: {} chunks, {} embedded, {} reused",
                    run.revision_id,
                    run.total_chunk_count,
                    chunks_embedded,
                    run.reused_chunk_count,
                )),
            )
            .await;
    }
    let persisted_vector_count = run
        .cleanup_error(
            cleanup_owned_vector_ids,
            run.state
                .search_store
                .count_chunk_vectors_by_revision(
                    run.revision_id,
                    run.embedding_profile_key,
                    KNOWLEDGE_CHUNK_VECTOR_KIND,
                    run.freshness_generation,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to verify persisted chunk-vector coverage for revision {}",
                        run.revision_id
                    )
                }),
        )
        .await?;
    ensure_not_cancelled(run.cancellation_token)?;
    if persisted_vector_count == run.total_chunk_count {
        return Ok(());
    }
    run.cleanup_error(
        cleanup_owned_vector_ids,
        Err(anyhow!(
            "persisted chunk-vector coverage mismatch for revision {}: {} chunks, {} current vectors",
            run.revision_id,
            run.total_chunk_count,
            persisted_vector_count,
        )),
    )
    .await
}

pub(super) fn embedding_usage_token_counts(
    usage: &EmbeddingUsageTotals,
) -> (Option<i32>, Option<i32>, Option<i32>) {
    let prompt_tokens =
        usage.saw_prompt.then(|| i32::try_from(usage.prompt_tokens).unwrap_or(i32::MAX));
    let completion_tokens =
        usage.saw_completion.then(|| i32::try_from(usage.completion_tokens).unwrap_or(i32::MAX));
    let total_tokens = if usage.saw_total {
        Some(i32::try_from(usage.total_tokens).unwrap_or(i32::MAX))
    } else if usage.saw_prompt || usage.saw_completion {
        Some(
            i32::try_from(usage.prompt_tokens.saturating_add(usage.completion_tokens))
                .unwrap_or(i32::MAX),
        )
    } else {
        None
    };
    (prompt_tokens, completion_tokens, total_tokens)
}

/// Inputs for [`SearchService::embed_chunks_for_revision`].
pub struct EmbedRevisionChunksInput<'a> {
    pub state: &'a AppState,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub attempt_id: Uuid,
    pub vector_write_source_truth_version: i64,
    pub cancellation_token: &'a CancellationToken,
}

impl SearchService {
    async fn persist_revision_chunk_embedding_batch(
        &self,
        write: RevisionChunkEmbeddingBatchWrite<'_>,
    ) -> AnyhowResult<(usize, EmbeddingDimensions)> {
        let RevisionChunkEmbeddingBatchWrite {
            state,
            library_id,
            chunks,
            batch,
            batch_response,
            embedding_profile_key,
            freshness_generation,
            expected_dimensions,
            expected_source_truth_version,
            ingest_attempt,
            cleanup_owned_vector_ids,
            cancellation_token,
        } = write;
        let dimensions = validate_embedding_batch_dimensions(
            batch.len(),
            batch_response.dimensions,
            &batch_response.embeddings,
            expected_dimensions,
            "revision chunk batch",
        )?;
        let _vector_guard = self.vector_plane_read_guard(state, library_id).await?;
        ensure_active_embedding_profile_key(state, library_id, embedding_profile_key).await?;
        ensure_not_cancelled(cancellation_token)?;

        let mut rows = Vec::with_capacity(batch.len());
        for (chunk_index, vector) in batch.iter().zip(batch_response.embeddings.iter()) {
            let chunk = chunks[*chunk_index];
            rows.push(KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: chunk.workspace_id,
                library_id: chunk.library_id,
                chunk_id: chunk.chunk_id,
                revision_id: chunk.revision_id,
                embedding_model_key: embedding_profile_key.to_string(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: validate_embedding_vector_dimensions(
                    dimensions.get(),
                    vector,
                    format!("chunk {}", chunk.chunk_id),
                )?,
                vector: vector.clone(),
                freshness_generation,
                created_at: Utc::now(),
                occurred_at: chunk.occurred_at,
                occurred_until: chunk.occurred_until,
            });
        }
        state
            .search_store
            .upsert_ingest_chunk_vectors_bulk_fenced(
                &rows,
                &VectorWriteFence {
                    expected_source_truth_version,
                    embedding_profile_key: embedding_profile_key.to_string(),
                    ingest_attempt: Some(ingest_attempt),
                    advance_source_truth_version: false,
                },
                state,
            )
            .await
            .context("failed to authority/profile-fenced bulk-persist chunk vectors")?;
        cleanup_owned_vector_ids.extend(rows.iter().map(|row| row.vector_id));
        // `Unobserved` is cached as healthy dimension state. Evict it as soon
        // as the first exact-profile manifest claim and its real vectors are
        // durable, so query/entity paths cannot falsely fail closed.
        invalidate_library_vector_index_dimensions(library_id);
        ensure_not_cancelled(cancellation_token)?;
        Ok((rows.len(), dimensions))
    }

    /// Embeds every chunk of a single revision using the library's active
    /// EmbedChunk binding, persists the vectors into per-dim vector relations,
    /// and returns per-stage usage
    /// for billing + stage-event reporting.
    ///
    /// Called inline from the ingest worker and inline-mutation pipelines
    /// so a newly readable revision gets queryable vectors before graph
    /// extraction runs. The revision's `vector_state` / library's
    /// `active_vector_generation` only flip to "ready" when this returns
    /// a matching chunks_embedded count — no silent "pretend ready"
    /// divergence between revision metadata and actual vector inventory.
    pub async fn embed_chunks_for_revision(
        &self,
        input: EmbedRevisionChunksInput<'_>,
    ) -> std::result::Result<EmbedChunksStageOutcome, QueryServiceError> {
        let EmbedRevisionChunksInput {
            state,
            library_id,
            revision_id,
            attempt_id,
            vector_write_source_truth_version,
            cancellation_token,
        } = input;
        ensure_not_cancelled(cancellation_token)?;
        let revision = state
            .document_store
            .get_revision(revision_id)
            .await
            .with_context(|| format!("failed to load revision {revision_id}"))?
            .ok_or_else(|| anyhow!("knowledge revision {revision_id} not found"))?;
        let chunks = state
            .document_store
            .list_chunks_by_revision(revision_id)
            .await
            .with_context(|| format!("failed to list chunks for revision {revision_id}"))?;
        ensure_not_cancelled(cancellation_token)?;
        if chunks.is_empty() {
            return Ok(EmbedChunksStageOutcome::default());
        }

        // The caller captures this snapshot before entering the stage so the
        // same fence can authorize success or failure publication even when a
        // provider call returns an error.
        if vector_write_source_truth_version <= 0 {
            return Err(QueryServiceError::StateConflict {
                message: "embed_chunk requires a positive source-truth fence".to_string(),
            });
        }
        let ingest_attempt = IngestVectorWriteFence { attempt_id, revision_id };
        let mut cleanup_owned_vector_ids = BTreeSet::new();
        let binding = resolve_active_embedding_binding(state, library_id).await?;
        ensure_not_cancelled(cancellation_token)?;
        let embedding_model_key = binding.embedding_execution_profile_key();
        let parallelism = state.settings.ingestion_embedding_parallelism.max(1);
        let freshness_generation = revision.revision_number;
        let (dimension_state, reused_chunk_ids) =
            resolve_reused_revision_chunk_ids(ResolveReusedRevisionChunkIdsInput {
                service: self,
                state,
                library_id,
                revision_id,
                chunks: &chunks,
                binding: &binding,
                embedding_profile_key: &embedding_model_key,
                freshness_generation,
                source_truth_version: vector_write_source_truth_version,
                ingest_attempt,
                cleanup_owned_vector_ids: &mut cleanup_owned_vector_ids,
            })
            .await?;
        ensure_not_cancelled(cancellation_token)?;
        // Resume path: only the chunks not already covered by a persisted /
        // reusable vector are embedded on this attempt. After a transient failure
        // that preserved partial vectors, the prior batches land in
        // `reused_chunk_ids` and the retry pays only for the missing remainder.
        let chunk_ids = chunks.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>();
        let missing_chunk_ids: BTreeSet<Uuid> =
            chunk_ids_missing_vectors(&chunk_ids, &reused_chunk_ids).into_iter().collect();
        let chunks_to_embed: Vec<&KnowledgeChunkRow> =
            chunks.iter().filter(|chunk| missing_chunk_ids.contains(&chunk.chunk_id)).collect();
        sync_embed_chunk_stage_progress(state, attempt_id, reused_chunk_ids.len(), chunks.len())
            .await;

        let provider_kind_owned = binding.provider_kind.clone();
        let model_name_owned = binding.model_name.clone();

        let mut batches = revision_chunk_embedding_batches(chunks_to_embed.len());
        let chunks_ref = &chunks_to_embed;
        let observed_dimensions = match dimension_state {
            EmbeddingDimensionState::Known(dimensions) => Some(dimensions),
            EmbeddingDimensionState::Unobserved => None,
        };
        let mut usage = EmbeddingUsageTotals::default();
        let run = RevisionChunkEmbeddingRun {
            service: self,
            state,
            library_id,
            revision_id,
            attempt_id,
            source_truth_version: vector_write_source_truth_version,
            cancellation_token,
            binding: &binding,
            embedding_profile_key: &embedding_model_key,
            freshness_generation,
            ingest_attempt,
            total_chunk_count: chunks.len(),
            reused_chunk_count: reused_chunk_ids.len(),
        };
        let (expected_dimensions, learned_chunk_count) = learn_revision_embedding_dimensions(
            &run,
            chunks_ref,
            &mut batches,
            observed_dimensions,
            &mut usage,
            &mut cleanup_owned_vector_ids,
        )
        .await?;
        let chunks_embedded =
            persist_revision_embedding_batches(PersistRevisionEmbeddingBatchesInput {
                run: &run,
                chunks: chunks_ref.as_slice(),
                batches,
                expected_dimensions,
                parallelism,
                initial_chunks_embedded: learned_chunk_count,
                usage: &mut usage,
                cleanup_owned_vector_ids: &mut cleanup_owned_vector_ids,
            })
            .await?;
        verify_revision_embedding_coverage(&run, chunks_embedded, &cleanup_owned_vector_ids)
            .await?;

        let (prompt_tokens, completion_tokens, total_tokens) = embedding_usage_token_counts(&usage);

        let usage_json = serde_json::json!({
            "provider_kind": provider_kind_owned,
            "model_name": model_name_owned,
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": total_tokens,
            "chunks_embedded": chunks_embedded,
            "chunks_reused": reused_chunk_ids.len(),
        });

        Ok(EmbedChunksStageOutcome {
            embedding_profile_key: Some(embedding_model_key),
            chunks_embedded,
            chunks_reused: reused_chunk_ids.len(),
            usage_json: (chunks_embedded > 0).then_some(usage_json),
            provider_kind: Some(provider_kind_owned),
            model_name: Some(model_name_owned),
            prompt_tokens,
            completion_tokens,
            total_tokens,
        })
    }
}
