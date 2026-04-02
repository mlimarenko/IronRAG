use std::collections::BTreeSet;

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    infra::arangodb::{
        collections::KNOWLEDGE_CHUNK_COLLECTION,
        document_store::KnowledgeChunkRow,
        graph_store::KnowledgeEntityRow,
        search_store::{
            KnowledgeChunkSearchRow, KnowledgeChunkVectorRow, KnowledgeEntitySearchRow,
            KnowledgeEntityVectorRow, KnowledgeRelationSearchRow, KnowledgeTechnicalFactSearchRow,
        },
    },
    infra::repositories::ai_repository,
    integrations::llm::EmbeddingBatchRequest,
    services::knowledge_service::RefreshKnowledgeLibraryGenerationCommand,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ChunkEmbeddingWrite {
    pub chunk_id: Uuid,
    pub model_catalog_id: Uuid,
    pub embedding_vector: Vec<f32>,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphNodeEmbeddingWrite {
    pub node_id: Uuid,
    pub model_catalog_id: Uuid,
    pub embedding_vector: Vec<f32>,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct QueryEvidenceSearchResult {
    pub chunk_hits: Vec<KnowledgeChunkSearchRow>,
    pub technical_fact_hits: Vec<KnowledgeTechnicalFactSearchRow>,
    pub entity_hits: Vec<KnowledgeEntitySearchRow>,
    pub relation_hits: Vec<KnowledgeRelationSearchRow>,
    pub exact_literal_bias: bool,
}

#[derive(Clone, Default)]
pub struct SearchService;

impl SearchService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn is_exact_literal_technical_query(&self, query: &str) -> bool {
        exact_literal_technical_query(query)
    }

    pub async fn search_query_evidence(
        &self,
        state: &AppState,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> Result<QueryEvidenceSearchResult> {
        let normalized_limit = limit.max(1);
        let exact_literal_bias = self.is_exact_literal_technical_query(query);
        let fact_limit = if exact_literal_bias {
            normalized_limit.saturating_mul(2).max(6)
        } else {
            normalized_limit
        };
        let chunk_hits = state
            .arango_search_store
            .search_chunks(library_id, query, normalized_limit)
            .await
            .context("failed to search canonical knowledge chunks")?;
        let technical_fact_hits = state
            .arango_search_store
            .search_technical_facts(library_id, query, fact_limit)
            .await
            .context("failed to search canonical technical facts")?;
        let entity_hits = state
            .arango_search_store
            .search_entities(library_id, query, normalized_limit)
            .await
            .context("failed to search canonical entities")?;
        let relation_hits = state
            .arango_search_store
            .search_relations(library_id, query, normalized_limit)
            .await
            .context("failed to search canonical relations")?;
        Ok(QueryEvidenceSearchResult {
            chunk_hits,
            technical_fact_hits,
            entity_hits,
            relation_hits,
            exact_literal_bias,
        })
    }

    #[must_use]
    pub fn select_current_chunk_vector<'a>(
        &self,
        rows: &'a [KnowledgeChunkVectorRow],
    ) -> Option<&'a KnowledgeChunkVectorRow> {
        rows.iter().max_by(|left, right| {
            left.freshness_generation
                .cmp(&right.freshness_generation)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.vector_id.cmp(&right.vector_id))
        })
    }

    #[must_use]
    pub fn select_current_entity_vector<'a>(
        &self,
        rows: &'a [KnowledgeEntityVectorRow],
    ) -> Option<&'a KnowledgeEntityVectorRow> {
        rows.iter().max_by(|left, right| {
            left.freshness_generation
                .cmp(&right.freshness_generation)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.vector_id.cmp(&right.vector_id))
        })
    }

    pub async fn resolve_embedding_model_catalog_id(
        &self,
        state: &AppState,
        provider_kind: &str,
        model_name: &str,
    ) -> Result<Uuid> {
        resolve_embedding_model_catalog_id(state, provider_kind, model_name).await
    }

    pub async fn persist_chunk_embeddings(
        &self,
        state: &AppState,
        writes: &[ChunkEmbeddingWrite],
    ) -> Result<usize> {
        let mut written = 0usize;
        for write in writes {
            let chunk = load_knowledge_chunk(state, write.chunk_id).await?;
            let freshness_generation =
                resolve_chunk_vector_generation(state, &chunk).await.with_context(|| {
                    format!("failed to resolve vector generation for chunk {}", write.chunk_id)
                })?;
            let vector = write.embedding_vector.clone();
            let row = KnowledgeChunkVectorRow {
                key: build_chunk_vector_key(
                    write.chunk_id,
                    write.model_catalog_id,
                    freshness_generation,
                ),
                arango_id: None,
                arango_rev: None,
                vector_id: Uuid::now_v7(),
                workspace_id: chunk.workspace_id,
                library_id: chunk.library_id,
                chunk_id: chunk.chunk_id,
                revision_id: chunk.revision_id,
                embedding_model_key: write.model_catalog_id.to_string(),
                vector_kind: "chunk_embedding".to_string(),
                dimensions: embedding_dimensions(&vector).with_context(|| {
                    format!("failed to resolve chunk embedding dimensions for {}", write.chunk_id)
                })?,
                vector,
                freshness_generation,
                created_at: Utc::now(),
            };
            let _ =
                state.arango_search_store.upsert_chunk_vector(&row).await.with_context(|| {
                    format!("failed to persist chunk vector for {}", write.chunk_id)
                })?;
            if write.active {
                self.activate_chunk_embedding_index(state, write.chunk_id, write.model_catalog_id)
                    .await?;
            }
            written += 1;
        }
        Ok(written)
    }

    pub async fn persist_graph_node_embeddings(
        &self,
        state: &AppState,
        writes: &[GraphNodeEmbeddingWrite],
    ) -> Result<usize> {
        let mut written = 0usize;
        for write in writes {
            let entity = state
                .arango_graph_store
                .get_entity_by_id(write.node_id)
                .await
                .with_context(|| {
                    format!("failed to load knowledge entity {}", write.node_id)
                })?
                .ok_or_else(|| {
                    anyhow!(
                        "graph node {} is not a canonical knowledge entity; relation or projection node vectors are not supported by the Arango search store",
                        write.node_id
                    )
                })?;
            let vector = write.embedding_vector.clone();
            let row = KnowledgeEntityVectorRow {
                key: build_entity_vector_key(
                    entity.entity_id,
                    write.model_catalog_id,
                    entity.freshness_generation,
                ),
                arango_id: None,
                arango_rev: None,
                vector_id: Uuid::now_v7(),
                workspace_id: entity.workspace_id,
                library_id: entity.library_id,
                entity_id: entity.entity_id,
                embedding_model_key: write.model_catalog_id.to_string(),
                vector_kind: "entity_embedding".to_string(),
                dimensions: embedding_dimensions(&vector).with_context(|| {
                    format!("failed to resolve entity embedding dimensions for {}", write.node_id)
                })?,
                vector,
                freshness_generation: entity.freshness_generation,
                created_at: Utc::now(),
            };
            let _ =
                state.arango_search_store.upsert_entity_vector(&row).await.with_context(|| {
                    format!("failed to persist canonical entity vector for {}", write.node_id)
                })?;
            if write.active {
                self.activate_graph_node_embedding_index(
                    state,
                    write.node_id,
                    write.model_catalog_id,
                )
                .await?;
            }
            written += 1;
        }
        Ok(written)
    }

    pub async fn activate_chunk_embedding_index(
        &self,
        state: &AppState,
        chunk_id: Uuid,
        model_catalog_id: Uuid,
    ) -> Result<()> {
        let embedding_model_key = model_catalog_id.to_string();
        let rows = state
            .arango_search_store
            .list_chunk_vectors_by_chunk(chunk_id)
            .await
            .with_context(|| format!("failed to load chunk vectors for {}", chunk_id))?;
        let has_model = rows.iter().any(|row| row.embedding_model_key == embedding_model_key);
        if !has_model {
            return Err(anyhow!(
                "chunk {} has no canonical vector for model {}",
                chunk_id,
                model_catalog_id
            ));
        }
        Ok(())
    }

    pub async fn activate_graph_node_embedding_index(
        &self,
        state: &AppState,
        node_id: Uuid,
        model_catalog_id: Uuid,
    ) -> Result<()> {
        let embedding_model_key = model_catalog_id.to_string();
        let rows = state
            .arango_search_store
            .list_entity_vectors_by_entity(node_id)
            .await
            .with_context(|| format!("failed to load entity vectors for {}", node_id))?;
        let has_model = rows.iter().any(|row| row.embedding_model_key == embedding_model_key);
        if !has_model {
            return Err(anyhow!(
                "entity {} has no canonical vector for model {}",
                node_id,
                model_catalog_id
            ));
        }
        Ok(())
    }

    pub async fn rebuild_chunk_embeddings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<usize> {
        let embedding_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
            .await?
            .ok_or_else(|| {
                anyhow!("active embedding binding is not configured for library {}", library_id)
            })?;
        let model_catalog_id = embedding_binding.model_catalog_id;
        let chunks = list_knowledge_chunks_by_library(state, library_id)
            .await
            .context("failed to load knowledge chunks for chunk embedding rebuild")?;
        if chunks.is_empty() {
            return Ok(0);
        }

        let mut touched_revision_ids = BTreeSet::new();
        let mut max_vector_generation = None::<i64>;
        let mut rebuilt = 0usize;
        for chunk_batch in chunks.chunks(64) {
            let batch_response = state
                .llm_gateway
                .embed_many(EmbeddingBatchRequest {
                    provider_kind: embedding_binding.provider_kind.clone(),
                    model_name: embedding_binding.model_name.clone(),
                    inputs: chunk_batch.iter().map(|chunk| chunk.content_text.clone()).collect(),
                    api_key_override: Some(embedding_binding.api_key.clone()),
                    base_url_override: embedding_binding.provider_base_url.clone(),
                })
                .await
                .context("failed to rebuild chunk embeddings")?;
            if batch_response.embeddings.len() != chunk_batch.len() {
                return Err(anyhow!(
                    "embedding batch returned {} vectors for {} chunks",
                    batch_response.embeddings.len(),
                    chunk_batch.len()
                ));
            }

            for (chunk, embedding) in chunk_batch.iter().zip(batch_response.embeddings.iter()) {
                let freshness_generation =
                    resolve_chunk_vector_generation(state, chunk).await.with_context(|| {
                        format!("failed to resolve chunk vector generation for {}", chunk.chunk_id)
                    })?;
                let row = KnowledgeChunkVectorRow {
                    key: build_chunk_vector_key(
                        chunk.chunk_id,
                        model_catalog_id,
                        freshness_generation,
                    ),
                    arango_id: None,
                    arango_rev: None,
                    vector_id: Uuid::now_v7(),
                    workspace_id: chunk.workspace_id,
                    library_id: chunk.library_id,
                    chunk_id: chunk.chunk_id,
                    revision_id: chunk.revision_id,
                    embedding_model_key: model_catalog_id.to_string(),
                    vector_kind: "chunk_embedding".to_string(),
                    dimensions: embedding_dimensions(embedding.as_slice()).with_context(|| {
                        format!(
                            "failed to resolve rebuilt chunk vector dimensions for {}",
                            chunk.chunk_id
                        )
                    })?,
                    vector: embedding.clone(),
                    freshness_generation,
                    created_at: Utc::now(),
                };
                let _ = state.arango_search_store.upsert_chunk_vector(&row).await.with_context(
                    || format!("failed to persist rebuilt chunk vector for {}", chunk.chunk_id),
                )?;
                self.activate_chunk_embedding_index(state, chunk.chunk_id, model_catalog_id)
                    .await?;
                touched_revision_ids.insert(chunk.revision_id);
                max_vector_generation = Some(
                    max_vector_generation
                        .map_or(freshness_generation, |current| current.max(freshness_generation)),
                );
                rebuilt += 1;
            }
        }

        mark_revisions_vector_ready(state, &touched_revision_ids)
            .await
            .context("failed to mark rebuilt revisions as vector-ready")?;
        if let Some(vector_generation) = max_vector_generation {
            refresh_library_vector_generation_if_present(
                state,
                library_id,
                chunks[0].workspace_id,
                vector_generation,
            )
            .await
            .context("failed to refresh library vector generation after chunk rebuild")?;
        }

        Ok(rebuilt)
    }

    pub async fn rebuild_graph_node_embeddings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<usize> {
        let embedding_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
            .await?
            .ok_or_else(|| {
                anyhow!("active embedding binding is not configured for library {}", library_id)
            })?;
        let model_catalog_id = embedding_binding.model_catalog_id;
        state
            .arango_search_store
            .delete_entity_vectors_by_library(library_id)
            .await
            .context("failed to clear stale entity vectors before rebuild")?;
        let entities = state
            .arango_graph_store
            .list_entities_by_library(library_id)
            .await
            .context("failed to load knowledge entities for canonical vector rebuild")?;
        if entities.is_empty() {
            return Ok(0);
        }

        let mut max_vector_generation = None::<i64>;
        let mut rebuilt = 0usize;
        for entity_batch in entities.chunks(64) {
            let batch_response = state
                .llm_gateway
                .embed_many(EmbeddingBatchRequest {
                    provider_kind: embedding_binding.provider_kind.clone(),
                    model_name: embedding_binding.model_name.clone(),
                    inputs: entity_batch.iter().map(build_entity_embedding_input).collect(),
                    api_key_override: Some(embedding_binding.api_key.clone()),
                    base_url_override: embedding_binding.provider_base_url.clone(),
                })
                .await
                .context("failed to rebuild entity vectors")?;
            if batch_response.embeddings.len() != entity_batch.len() {
                return Err(anyhow!(
                    "embedding batch returned {} vectors for {} entities",
                    batch_response.embeddings.len(),
                    entity_batch.len()
                ));
            }

            for (entity, embedding) in entity_batch.iter().zip(batch_response.embeddings.iter()) {
                let row = KnowledgeEntityVectorRow {
                    key: build_entity_vector_key(
                        entity.entity_id,
                        model_catalog_id,
                        entity.freshness_generation,
                    ),
                    arango_id: None,
                    arango_rev: None,
                    vector_id: Uuid::now_v7(),
                    workspace_id: entity.workspace_id,
                    library_id: entity.library_id,
                    entity_id: entity.entity_id,
                    embedding_model_key: model_catalog_id.to_string(),
                    vector_kind: "entity_embedding".to_string(),
                    dimensions: embedding_dimensions(embedding.as_slice()).with_context(|| {
                        format!(
                            "failed to resolve rebuilt entity vector dimensions for {}",
                            entity.entity_id
                        )
                    })?,
                    vector: embedding.clone(),
                    freshness_generation: entity.freshness_generation,
                    created_at: Utc::now(),
                };
                let _ = state.arango_search_store.upsert_entity_vector(&row).await.with_context(
                    || format!("failed to persist rebuilt entity vector for {}", entity.entity_id),
                )?;
                self.activate_graph_node_embedding_index(state, entity.entity_id, model_catalog_id)
                    .await?;
                max_vector_generation =
                    Some(max_vector_generation.map_or(entity.freshness_generation, |current| {
                        current.max(entity.freshness_generation)
                    }));
                rebuilt += 1;
            }
        }

        if let Some(vector_generation) = max_vector_generation {
            refresh_library_vector_generation_if_present(
                state,
                library_id,
                entities[0].workspace_id,
                vector_generation,
            )
            .await
            .context("failed to refresh library vector generation after entity rebuild")?;
        }

        Ok(rebuilt)
    }
}

fn exact_literal_technical_query(query: &str) -> bool {
    let normalized = query.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    let strong_markers = [
        "http://",
        "https://",
        "wsdl",
        "endpoint",
        "method",
        "path",
        "port",
        "status code",
        "query parameter",
        "parameter",
        "url",
        "graphql",
        "rest",
        "soap",
        "/",
    ];
    let has_marker =
        strong_markers.iter().any(|marker| normalized.contains(marker)) || query.contains("?");
    let has_code_like_literal =
        query.chars().any(|ch| ch == '/' || ch == ':' || ch == '_' || ch == '-')
            || query.split_whitespace().any(|token| {
                let has_letters = token.chars().any(|ch| ch.is_ascii_alphabetic());
                let has_digits = token.chars().any(|ch| ch.is_ascii_digit());
                has_letters && has_digits
            });
    has_marker || has_code_like_literal
}

async fn resolve_embedding_model_catalog_id(
    state: &AppState,
    provider_kind: &str,
    model_name: &str,
) -> Result<Uuid> {
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

fn build_entity_embedding_input(entity: &KnowledgeEntityRow) -> String {
    format!(
        "entity_type: {}\ncanonical_label: {}\naliases: {}\nsummary: {}",
        entity.entity_type,
        entity.canonical_label,
        entity.aliases.join(", "),
        entity.summary.clone().unwrap_or_default(),
    )
}

fn build_chunk_vector_key(
    chunk_id: Uuid,
    model_catalog_id: Uuid,
    freshness_generation: i64,
) -> String {
    format!("{chunk_id}:{model_catalog_id}:{freshness_generation}")
}

fn build_entity_vector_key(
    entity_id: Uuid,
    model_catalog_id: Uuid,
    freshness_generation: i64,
) -> String {
    format!("{entity_id}:{model_catalog_id}:{freshness_generation}")
}

fn embedding_dimensions(vector: &[f32]) -> Result<i32> {
    if vector.is_empty() {
        return Err(anyhow!("embedding vector must not be empty"));
    }
    i32::try_from(vector.len()).context("embedding vector dimension overflowed i32")
}

async fn load_knowledge_chunk(state: &AppState, chunk_id: Uuid) -> Result<KnowledgeChunkRow> {
    let cursor = state
        .arango_document_store
        .client()
        .query_json(
            "FOR chunk IN @@collection
             FILTER chunk.chunk_id == @chunk_id
             LIMIT 1
             RETURN chunk",
            serde_json::json!({
                "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                "chunk_id": chunk_id,
            }),
        )
        .await
        .with_context(|| format!("failed to load knowledge chunk {}", chunk_id))?;
    decode_optional_single_result(cursor)?
        .ok_or_else(|| anyhow!("knowledge chunk {} not found", chunk_id))
}

async fn list_knowledge_chunks_by_library(
    state: &AppState,
    library_id: Uuid,
) -> Result<Vec<KnowledgeChunkRow>> {
    let cursor = state
        .arango_document_store
        .client()
        .query_json(
            "FOR chunk IN @@collection
             FILTER chunk.library_id == @library_id
             SORT chunk.revision_id ASC, chunk.chunk_index ASC, chunk.chunk_id ASC
             RETURN chunk",
            serde_json::json!({
                "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                "library_id": library_id,
            }),
        )
        .await
        .with_context(|| format!("failed to list knowledge chunks for library {}", library_id))?;
    decode_many_results(cursor)
}

async fn resolve_chunk_vector_generation(
    state: &AppState,
    chunk: &KnowledgeChunkRow,
) -> Result<i64> {
    if let Some(generation) = chunk.vector_generation.or(chunk.text_generation) {
        return Ok(generation);
    }

    let revision = state
        .arango_document_store
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

async fn mark_revisions_vector_ready(
    state: &AppState,
    revision_ids: &BTreeSet<Uuid>,
) -> Result<()> {
    for revision_id in revision_ids {
        let revision = state
            .arango_document_store
            .get_revision(*revision_id)
            .await
            .with_context(|| format!("failed to load revision {}", revision_id))?
            .ok_or_else(|| anyhow!("knowledge revision {} not found", revision_id))?;
        let updated = state
            .arango_document_store
            .update_revision_readiness(
                revision.revision_id,
                &revision.text_state,
                "ready",
                &revision.graph_state,
                revision.text_readable_at,
                Some(Utc::now()),
                revision.graph_ready_at,
                revision.superseded_by_revision_id,
            )
            .await
            .with_context(|| format!("failed to update vector readiness for {}", revision_id))?;
        if updated.is_none() {
            return Err(anyhow!(
                "knowledge revision {} disappeared during vector readiness update",
                revision_id
            ));
        }
    }
    Ok(())
}

async fn refresh_library_vector_generation_if_present(
    state: &AppState,
    library_id: Uuid,
    workspace_id: Uuid,
    vector_generation: i64,
) -> Result<()> {
    let Some(existing) = state
        .arango_document_store
        .list_library_generations(library_id)
        .await
        .with_context(|| format!("failed to list library generations for {}", library_id))?
        .into_iter()
        .next()
    else {
        return Ok(());
    };

    state
        .canonical_services
        .knowledge
        .refresh_library_generation(
            state,
            RefreshKnowledgeLibraryGenerationCommand {
                generation_id: existing.generation_id,
                workspace_id,
                library_id,
                active_text_generation: existing.active_text_generation,
                active_vector_generation: existing.active_vector_generation.max(vector_generation),
                active_graph_generation: existing.active_graph_generation,
                degraded_state: existing.degraded_state,
            },
        )
        .await
        .map_err(|error| {
            anyhow!("failed to refresh vector generation for library {}: {:?}", library_id, error)
        })?;
    Ok(())
}

fn decode_optional_single_result<T>(cursor: serde_json::Value) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?;
    let mut rows: Vec<T> =
        serde_json::from_value(result).context("failed to decode ArangoDB result rows")?;
    Ok((!rows.is_empty()).then(|| rows.remove(0)))
}

fn decode_many_results<T>(cursor: serde_json::Value) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?;
    serde_json::from_value(result).context("failed to decode ArangoDB result rows")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn current_chunk_vector_selection_prefers_latest_generation() {
        let chunk_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let old = KnowledgeChunkVectorRow {
            key: "old".to_string(),
            arango_id: None,
            arango_rev: None,
            vector_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            chunk_id,
            revision_id: Uuid::now_v7(),
            embedding_model_key: model_catalog_id.to_string(),
            vector_kind: "chunk_embedding".to_string(),
            dimensions: 3,
            vector: vec![1.0, 2.0, 3.0],
            freshness_generation: 1,
            created_at: Utc::now() - Duration::minutes(1),
        };
        let new = KnowledgeChunkVectorRow {
            key: "new".to_string(),
            freshness_generation: 2,
            created_at: Utc::now(),
            ..old.clone()
        };

        let candidates = [old, new.clone()];
        let selected =
            SearchService::new().select_current_chunk_vector(&candidates).expect("chunk vector");
        assert_eq!(selected.freshness_generation, new.freshness_generation);
    }

    #[test]
    fn current_entity_vector_selection_prefers_latest_generation() {
        let entity_id = Uuid::now_v7();
        let model_catalog_id = Uuid::now_v7();
        let old = KnowledgeEntityVectorRow {
            key: "old".to_string(),
            arango_id: None,
            arango_rev: None,
            vector_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            entity_id,
            embedding_model_key: model_catalog_id.to_string(),
            vector_kind: "entity_embedding".to_string(),
            dimensions: 3,
            vector: vec![1.0, 2.0, 3.0],
            freshness_generation: 1,
            created_at: Utc::now() - Duration::minutes(1),
        };
        let new = KnowledgeEntityVectorRow {
            key: "new".to_string(),
            freshness_generation: 2,
            created_at: Utc::now(),
            ..old.clone()
        };

        let candidates = [old, new.clone()];
        let selected =
            SearchService::new().select_current_entity_vector(&candidates).expect("entity vector");
        assert_eq!(selected.freshness_generation, new.freshness_generation);
    }

    #[test]
    fn exact_literal_query_detection_prefers_technical_markers() {
        let service = SearchService::new();
        assert!(service.is_exact_literal_technical_query("Какой endpoint у GET /v1/system/info?"));
        assert!(service.is_exact_literal_technical_query("WSDL URL inventory.wsdl"));
        assert!(!service.is_exact_literal_technical_query("О чем вообще этот документ"));
    }
}
