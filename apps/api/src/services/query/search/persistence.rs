use super::*;

impl SearchService {
    pub async fn resolve_embedding_model_catalog_id(
        &self,
        state: &AppState,
        provider_kind: &str,
        model_name: &str,
    ) -> std::result::Result<Uuid, QueryServiceError> {
        resolve_embedding_model_catalog_id(state, provider_kind, model_name)
            .await
            .map_err(Into::into)
    }

    pub async fn persist_chunk_embeddings(
        &self,
        state: &AppState,
        writes: &[ChunkEmbeddingWrite],
    ) -> std::result::Result<usize, QueryServiceError> {
        let mut written = 0usize;
        for write in writes {
            let chunk = load_knowledge_chunk(state, write.chunk_id).await?;
            let _vector_guard = self.vector_plane_read_guard(state, chunk.library_id).await?;
            // Capture before resolving the binding. Every source or effective
            // AI-profile mutation advances this value; the storage transaction
            // compares it while holding the same lock as manifest+vector I/O.
            let source_truth_version =
                load_vector_write_source_fence(state, chunk.library_id).await?;
            let binding = resolve_active_embedding_binding(state, chunk.library_id).await?;
            let active_profile_key = binding.embedding_execution_profile_key();
            let expected_dimensions =
                library_vector_index_dimensions_for_binding(state, chunk.library_id, &binding)
                    .await?;
            validate_embedding_profile_write(
                chunk.library_id,
                &write.embedding_profile_key,
                &active_profile_key,
            )?;
            let freshness_generation =
                resolve_chunk_vector_generation(state, &chunk).await.with_context(|| {
                    format!("failed to resolve vector generation for chunk {}", write.chunk_id)
                })?;
            let vector = write.embedding_vector.clone();
            let row = KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: chunk.workspace_id,
                library_id: chunk.library_id,
                chunk_id: chunk.chunk_id,
                revision_id: chunk.revision_id,
                embedding_model_key: write.embedding_profile_key.clone(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: validate_embedding_vector_dimensions(
                    expected_dimensions,
                    &vector,
                    format!("chunk {}", write.chunk_id),
                )
                .with_context(|| {
                    format!("failed to resolve chunk embedding dimensions for {}", write.chunk_id)
                })?,
                vector,
                freshness_generation,
                created_at: Utc::now(),
                occurred_at: chunk.occurred_at,
                occurred_until: chunk.occurred_until,
            };
            state
                .search_store
                .upsert_chunk_vectors_bulk_fenced(
                    std::slice::from_ref(&row),
                    &VectorWriteFence {
                        expected_source_truth_version: source_truth_version,
                        embedding_profile_key: active_profile_key,
                        ingest_attempt: None,
                        advance_source_truth_version: true,
                    },
                )
                .await
                .with_context(|| {
                    format!("failed to atomically persist chunk vector for {}", write.chunk_id)
                })?;
            invalidate_library_vector_index_dimensions(chunk.library_id);
            written += 1;
        }
        Ok(written)
    }

    pub async fn persist_graph_node_embeddings(
        &self,
        state: &AppState,
        writes: &[GraphNodeEmbeddingWrite],
    ) -> std::result::Result<usize, QueryServiceError> {
        let mut written = 0usize;
        for write in writes {
            let entity = state
                .graph_store
                .get_entity_by_id(write.node_id)
                .await
                .with_context(|| {
                    format!("failed to load knowledge entity {}", write.node_id)
                })?
                .ok_or_else(|| {
                    anyhow!(
                        "graph node {} is not a normalized knowledge entity; relation or projection node vectors are not supported by the search store",
                        write.node_id
                    )
            })?;
            let _vector_guard = self.vector_plane_read_guard(state, entity.library_id).await?;
            let source_truth_version =
                load_vector_write_source_fence(state, entity.library_id).await?;
            let binding = resolve_active_embedding_binding(state, entity.library_id).await?;
            let active_profile_key = binding.embedding_execution_profile_key();
            let expected_dimensions =
                library_vector_index_dimensions_for_binding(state, entity.library_id, &binding)
                    .await?;
            validate_embedding_profile_write(
                entity.library_id,
                &write.embedding_profile_key,
                &active_profile_key,
            )?;
            let vector = write.embedding_vector.clone();
            let row = KnowledgeEntityVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: entity.workspace_id,
                library_id: entity.library_id,
                entity_id: entity.entity_id,
                embedding_model_key: write.embedding_profile_key.clone(),
                vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
                dimensions: validate_embedding_vector_dimensions(
                    expected_dimensions,
                    &vector,
                    format!("entity {}", write.node_id),
                )
                .with_context(|| {
                    format!("failed to resolve entity embedding dimensions for {}", write.node_id)
                })?,
                vector,
                freshness_generation: entity.freshness_generation,
                created_at: Utc::now(),
            };
            state
                .search_store
                .upsert_entity_vectors_bulk_fenced(
                    std::slice::from_ref(&row),
                    &VectorWriteFence {
                        expected_source_truth_version: source_truth_version,
                        embedding_profile_key: active_profile_key,
                        ingest_attempt: None,
                        advance_source_truth_version: true,
                    },
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to atomically persist normalized entity vector for {}",
                        write.node_id
                    )
                })?;
            invalidate_library_vector_index_dimensions(entity.library_id);
            written += 1;
        }
        Ok(written)
    }
}

pub(super) async fn resolve_active_embedding_binding(
    state: &AppState,
    library_id: Uuid,
) -> std::result::Result<ResolvedRuntimeBinding, QueryServiceError> {
    state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| QueryServiceError::BindingNotConfigured {
            message: format!(
                "active embed_chunk binding is not configured for library {library_id}"
            ),
        })
}

pub(super) async fn load_vector_write_source_fence(
    state: &AppState,
    library_id: Uuid,
) -> std::result::Result<i64, QueryServiceError> {
    // Capture without holding a transaction across provider I/O. Ordinary
    // revision writes use this as a monotonic lower bound and re-resolve the
    // active profile before persistence; staged rebuilds compare and advance
    // it in their short promotion/purge transaction.
    catalog_repository::get_library_source_truth_version(&state.persistence.postgres, library_id)
        .await
        .with_context(|| {
            format!(
                "failed to capture source/profile fence before vector work for library {library_id}"
            )
        })
        .map_err(QueryServiceError::from)
}

pub(super) fn validate_embedding_profile_write(
    library_id: Uuid,
    supplied_profile_key: &str,
    active_profile_key: &str,
) -> std::result::Result<(), QueryServiceError> {
    if supplied_profile_key == active_profile_key {
        return Ok(());
    }
    Err(QueryServiceError::StateConflict {
        message: format!(
            "refusing to persist a vector for library {library_id}: supplied embedding execution-profile provenance does not match the active binding"
        ),
    })
}

async fn resolve_embedding_model_catalog_id(
    state: &AppState,
    provider_kind: &str,
    model_name: &str,
) -> AnyhowResult<Uuid> {
    let provider = ai_repository::list_provider_catalog(&state.persistence.postgres)
        .await
        .context("failed to list provider catalog while resolving embedding model")?
        .into_iter()
        .find(|row| row.provider_kind == provider_kind)
        .ok_or_else(|| anyhow!("provider catalog entry {provider_kind} not found"))?;
    ai_repository::list_model_catalog(&state.persistence.postgres, Some(provider.id))
        .await
        .context("failed to list model catalog while resolving embedding model")?
        .into_iter()
        .find(|row| row.model_name == model_name)
        .map(|row| row.id)
        .ok_or_else(|| anyhow!("model catalog entry {provider_kind}/{model_name} not found"))
}

async fn load_knowledge_chunk(state: &AppState, chunk_id: Uuid) -> AnyhowResult<KnowledgeChunkRow> {
    state
        .document_store
        .list_chunks_by_ids(&[chunk_id])
        .await
        .with_context(|| format!("failed to load knowledge chunk {}", chunk_id))?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("knowledge chunk {} not found", chunk_id))
}

async fn resolve_chunk_vector_generation(
    state: &AppState,
    chunk: &KnowledgeChunkRow,
) -> AnyhowResult<i64> {
    if let Some(generation) = chunk.vector_generation.or(chunk.text_generation) {
        return Ok(generation);
    }

    let revision = state
        .document_store
        .get_revision(chunk.revision_id)
        .await
        .with_context(|| {
            format!(
                "failed to load revision {} while resolving chunk generation",
                chunk.revision_id
            )
        })?
        .ok_or_else(|| anyhow!("knowledge revision {} not found", chunk.revision_id))?;
    Ok(revision.revision_number)
}
