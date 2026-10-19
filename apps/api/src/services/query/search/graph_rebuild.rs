use super::{
    persistence::{load_vector_write_source_fence, resolve_active_embedding_binding},
    rebuild::{
        discard_abandoned_vector_rebuilds, discard_staged_vector_rebuild_if_prepared,
        ensure_embedding_profile_stable_after_rebuild, finish_deferred_manifest_rebuild,
        vector_rebuild_staging_profile_key,
    },
    *,
};

/// Inputs for [`SearchService::rebuild_graph_node_embeddings_with_expected_dimensions`].
pub(super) struct GraphNodeEmbeddingRebuildInput<'a> {
    pub state: &'a AppState,
    pub library_id: Uuid,
    pub expected_dimensions: u64,
    pub embedding_binding: &'a ResolvedRuntimeBinding,
    pub staging_embedding_model_key: &'a str,
}

impl SearchService {
    pub async fn rebuild_graph_node_embeddings(
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
            .context("failed to count active chunks before entity-vector rebuild")?
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
        let expected_dimensions =
            library_vector_index_dimensions_for_binding(state, library_id, &embedding_binding)
                .await?;
        let active_embedding_profile_key = embedding_binding.embedding_execution_profile_key();
        let staging_embedding_model_key = vector_rebuild_staging_profile_key(
            &active_embedding_profile_key,
            Uuid::now_v7(),
            source_truth_version,
        );
        let mut prepared_dimensions = None;
        let staged_result = async {
            let rebuilt = self
                .rebuild_graph_node_embeddings_with_expected_dimensions(
                    GraphNodeEmbeddingRebuildInput {
                        state,
                        library_id,
                        expected_dimensions,
                        embedding_binding: &embedding_binding,
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
            let expected_entity_count =
                u64::try_from(rebuilt).context("rebuilt entity count overflowed u64")?;
            Ok::<_, QueryServiceError>((rebuilt, expected_entity_count))
        }
        .await;
        let (rebuilt, expected_entity_count) = match staged_result {
            Ok(result) => result,
            Err(error) => {
                return Err(discard_staged_vector_rebuild_if_prepared(
                    state,
                    library_id,
                    prepared_dimensions,
                    &staging_embedding_model_key,
                    error,
                )
                .await);
            }
        };
        if let Err(error) = self
            .promote_staged_vector_rebuild_under_write_guard(
                state,
                StagedVectorRebuildPromotion {
                    library_id,
                    dimensions: expected_dimensions,
                    active_embedding_profile_key: &active_embedding_profile_key,
                    staging_embedding_model_key: &staging_embedding_model_key,
                    expected_source_truth_version: source_truth_version,
                    chunk_count: None,
                    entity_count: Some(expected_entity_count),
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
        invalidate_library_embedding_profile_inventory(library_id);
        Ok(rebuilt)
    }

    pub(super) async fn rebuild_graph_node_embeddings_with_expected_dimensions(
        &self,
        input: GraphNodeEmbeddingRebuildInput<'_>,
        prepared_dimensions: &mut Option<u64>,
    ) -> std::result::Result<usize, QueryServiceError> {
        let GraphNodeEmbeddingRebuildInput {
            state,
            library_id,
            expected_dimensions,
            embedding_binding,
            staging_embedding_model_key,
        } = input;
        state
            .search_store
            .prepare_entity_vector_rebuild_lane(
                library_id,
                expected_dimensions,
                staging_embedding_model_key,
            )
            .await
            .context("failed to prepare entity vector manifest lane for rebuild")?;
        *prepared_dimensions = Some(expected_dimensions);
        let rebuild_result = async {
            let entities = state
                .graph_store
                .list_entities_by_library(library_id)
                .await
                .context("failed to load knowledge entities for active vector rebuild")?;
            let mut rebuilt = 0usize;
            for entity_batch in entities.chunks(64) {
                let batch_response = state
                    .llm_gateway
                    .embed_many(EmbeddingBatchRequest {
                        provider_kind: embedding_binding.provider_kind.clone(),
                        model_name: embedding_binding.model_name.clone(),
                        inputs: entity_batch.iter().map(build_entity_embedding_input).collect(),
                        api_key_override: embedding_binding.api_key.clone(),
                        base_url_override: embedding_binding.provider_base_url.clone(),
                        extra_parameters_json: embedding_binding.extra_parameters_json.clone(),
                    })
                    .await
                    .context("failed to rebuild entity vectors")?;
                let dimensions =
                    validate_embedding_batch_dimensions(
                        entity_batch.len(),
                        batch_response.dimensions,
                        &batch_response.embeddings,
                        Some(EmbeddingDimensions::try_from(expected_dimensions).map_err(
                            |error| QueryServiceError::Internal(anyhow::Error::new(error)),
                        )?),
                        "entity rebuild batch",
                    )?;

                let rows = entity_batch
                    .iter()
                    .zip(batch_response.embeddings.iter())
                    .map(|(entity, embedding)| {
                        Ok(KnowledgeEntityVectorRow {
                            vector_id: Uuid::now_v7(),
                            workspace_id: entity.workspace_id,
                            library_id: entity.library_id,
                            entity_id: entity.entity_id,
                            embedding_model_key: staging_embedding_model_key.to_string(),
                            vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                            dimensions: validate_embedding_vector_dimensions(
                                dimensions.get(),
                                embedding.as_slice(),
                                format!("rebuilt entity {}", entity.entity_id),
                            )?,
                            vector: embedding.clone(),
                            freshness_generation: entity.freshness_generation,
                            created_at: Utc::now(),
                        })
                    })
                    .collect::<AnyhowResult<Vec<_>>>()?;
                state
                    .search_store
                    .upsert_entity_vectors_bulk_deferred_manifest(rows.as_slice())
                    .await
                    .with_context(|| {
                        format!("failed to bulk-persist {} rebuilt entity vectors", rows.len())
                    })?;
                rebuilt = rebuilt.checked_add(rows.len()).ok_or_else(|| {
                    anyhow!("rebuilt entity count overflowed for library {library_id}")
                })?;
            }
            Ok(rebuilt)
        }
        .await;
        let reconciliation_result = state
            .search_store
            .reconcile_entity_vector_manifest_count(
                library_id,
                expected_dimensions,
                staging_embedding_model_key,
            )
            .await
            .context("failed to reconcile entity vector manifest after rebuild")
            .map_err(QueryServiceError::from);
        finish_deferred_manifest_rebuild(rebuild_result, reconciliation_result)
    }
}

fn build_entity_embedding_input(entity: &KnowledgeEntityRow) -> String {
    format!(
        "entity_type: {}\nprimary_label: {}\naliases: {}\nsummary: {}",
        entity.entity_type,
        entity.primary_label,
        entity.aliases.join(", "),
        entity.summary.clone().unwrap_or_default(),
    )
}
