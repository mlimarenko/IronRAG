use anyhow::{Context, Result, anyhow};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{
        self, ai_repository, catalog_repository, graph_repository,
        search_repository::{self, SearchChunkEmbeddingRow, SearchGraphNodeEmbeddingRow},
    },
    integrations::llm::EmbeddingBatchRequest,
    services::runtime_ingestion::resolve_effective_provider_profile,
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

#[derive(Clone, Default)]
pub struct SearchService;

impl SearchService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn select_active_chunk_embedding<'a>(
        &self,
        rows: &'a [SearchChunkEmbeddingRow],
    ) -> Option<&'a SearchChunkEmbeddingRow> {
        rows.iter()
            .filter(|row| row.active)
            .max_by_key(|row| row.embedded_at)
            .or_else(|| rows.iter().max_by_key(|row| row.embedded_at))
    }

    #[must_use]
    pub fn select_active_chunk_embedding_model_catalog_id(
        &self,
        rows: &[SearchChunkEmbeddingRow],
    ) -> Option<Uuid> {
        self.select_active_chunk_embedding(rows).map(|row| row.model_catalog_id)
    }

    #[must_use]
    pub fn select_active_graph_node_embedding<'a>(
        &self,
        rows: &'a [SearchGraphNodeEmbeddingRow],
    ) -> Option<&'a SearchGraphNodeEmbeddingRow> {
        rows.iter()
            .filter(|row| row.active)
            .max_by_key(|row| row.embedded_at)
            .or_else(|| rows.iter().max_by_key(|row| row.embedded_at))
    }

    #[must_use]
    pub fn select_active_graph_node_embedding_model_catalog_id(
        &self,
        rows: &[SearchGraphNodeEmbeddingRow],
    ) -> Option<Uuid> {
        self.select_active_graph_node_embedding(rows).map(|row| row.model_catalog_id)
    }

    pub async fn persist_chunk_embeddings(
        &self,
        state: &AppState,
        writes: &[ChunkEmbeddingWrite],
    ) -> Result<usize> {
        let mut written = 0usize;
        for write in writes {
            search_repository::upsert_chunk_embedding(
                &state.persistence.postgres,
                write.chunk_id,
                write.model_catalog_id,
                Some(&write.embedding_vector),
                write.active,
            )
            .await
            .with_context(|| format!("failed to persist chunk embedding for {}", write.chunk_id))?;
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
            search_repository::upsert_graph_node_embedding(
                &state.persistence.postgres,
                write.node_id,
                write.model_catalog_id,
                Some(&write.embedding_vector),
                write.active,
            )
            .await
            .with_context(|| {
                format!("failed to persist graph node embedding for {}", write.node_id)
            })?;
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
        let rows = search_repository::list_chunk_embeddings_by_chunk(
            &state.persistence.postgres,
            chunk_id,
        )
        .await
        .with_context(|| format!("failed to load chunk embeddings for {}", chunk_id))?;
        for row in rows {
            let should_be_active = row.model_catalog_id == model_catalog_id;
            search_repository::set_chunk_embedding_active(
                &state.persistence.postgres,
                chunk_id,
                row.model_catalog_id,
                should_be_active,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to update active chunk embedding {} for chunk {}",
                    row.model_catalog_id, chunk_id
                )
            })?;
        }
        Ok(())
    }

    pub async fn activate_graph_node_embedding_index(
        &self,
        state: &AppState,
        node_id: Uuid,
        model_catalog_id: Uuid,
    ) -> Result<()> {
        let rows = search_repository::list_graph_node_embeddings_by_node(
            &state.persistence.postgres,
            node_id,
        )
        .await
        .with_context(|| format!("failed to load graph node embeddings for {}", node_id))?;
        for row in rows {
            let should_be_active = row.model_catalog_id == model_catalog_id;
            search_repository::set_graph_node_embedding_active(
                &state.persistence.postgres,
                node_id,
                row.model_catalog_id,
                should_be_active,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to update active graph node embedding {} for node {}",
                    row.model_catalog_id, node_id
                )
            })?;
        }
        Ok(())
    }

    pub async fn rebuild_chunk_embeddings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<usize> {
        let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
        let model_catalog_id = resolve_embedding_model_catalog_id(
            state,
            provider_profile.embedding.provider_kind.as_str(),
            &provider_profile.embedding.model_name,
        )
        .await?;
        let chunks =
            repositories::list_chunks_by_project(&state.persistence.postgres, library_id, i64::MAX)
                .await
                .context("failed to load chunks for chunk embedding rebuild")?;
        if chunks.is_empty() {
            return Ok(0);
        }

        let mut rebuilt = 0usize;
        for chunk_batch in chunks.chunks(64) {
            let batch_response = state
                .llm_gateway
                .embed_many(EmbeddingBatchRequest {
                    provider_kind: provider_profile.embedding.provider_kind.as_str().to_string(),
                    model_name: provider_profile.embedding.model_name.clone(),
                    inputs: chunk_batch.iter().map(|chunk| chunk.content.clone()).collect(),
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
                search_repository::upsert_chunk_embedding(
                    &state.persistence.postgres,
                    chunk.id,
                    model_catalog_id,
                    Some(embedding.as_slice()),
                    true,
                )
                .await
                .with_context(|| {
                    format!("failed to persist rebuilt chunk embedding for {}", chunk.id)
                })?;
                self.activate_chunk_embedding_index(state, chunk.id, model_catalog_id).await?;
                rebuilt += 1;
            }
        }

        Ok(rebuilt)
    }

    pub async fn rebuild_graph_node_embeddings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<usize> {
        let library =
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                .await
                .context("failed to load library for graph node embedding rebuild")?
                .ok_or_else(|| anyhow!("library {library_id} not found"))?;
        let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
        let model_catalog_id = resolve_embedding_model_catalog_id(
            state,
            provider_profile.embedding.provider_kind.as_str(),
            &provider_profile.embedding.model_name,
        )
        .await?;
        let projections = graph_repository::list_graph_projections_by_library(
            &state.persistence.postgres,
            library.workspace_id,
            library_id,
        )
        .await
        .context("failed to load graph projections for node embedding rebuild")?;
        let graph_service = crate::services::graph_service::GraphService::new();
        let Some(active_projection) =
            graph_service.select_active_projection(&projections).or_else(|| projections.first())
        else {
            return Ok(0);
        };

        let nodes = graph_repository::list_graph_nodes_by_projection(
            &state.persistence.postgres,
            active_projection.id,
        )
        .await
        .context("failed to load graph nodes for node embedding rebuild")?;
        let nodes =
            nodes.into_iter().filter(|node| node.node_kind != "document").collect::<Vec<_>>();
        if nodes.is_empty() {
            return Ok(0);
        }

        let mut rebuilt = 0usize;
        for node_batch in nodes.chunks(64) {
            let batch_response = state
                .llm_gateway
                .embed_many(EmbeddingBatchRequest {
                    provider_kind: provider_profile.embedding.provider_kind.as_str().to_string(),
                    model_name: provider_profile.embedding.model_name.clone(),
                    inputs: node_batch.iter().map(build_graph_node_embedding_input).collect(),
                })
                .await
                .context("failed to rebuild graph node embeddings")?;
            if batch_response.embeddings.len() != node_batch.len() {
                return Err(anyhow!(
                    "embedding batch returned {} vectors for {} graph nodes",
                    batch_response.embeddings.len(),
                    node_batch.len()
                ));
            }

            for (node, embedding) in node_batch.iter().zip(batch_response.embeddings.iter()) {
                search_repository::upsert_graph_node_embedding(
                    &state.persistence.postgres,
                    node.id,
                    model_catalog_id,
                    Some(embedding.as_slice()),
                    true,
                )
                .await
                .with_context(|| {
                    format!("failed to persist rebuilt graph node embedding for {}", node.id)
                })?;
                self.activate_graph_node_embedding_index(state, node.id, model_catalog_id).await?;
                rebuilt += 1;
            }
        }

        Ok(rebuilt)
    }
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

fn build_graph_node_embedding_input(node: &graph_repository::GraphNodeRow) -> String {
    format!(
        "node_kind: {}\ndisplay_label: {}\nsummary: {}",
        node.node_kind,
        node.display_label,
        node.summary.clone().unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn active_chunk_embedding_selection_prefers_active_latest_row() {
        let old = SearchChunkEmbeddingRow {
            chunk_id: Uuid::now_v7(),
            model_catalog_id: Uuid::now_v7(),
            embedding_vector: None,
            embedded_at: Utc::now() - Duration::minutes(10),
            active: true,
        };
        let new = SearchChunkEmbeddingRow { embedded_at: Utc::now(), ..old.clone() };

        let chunk_rows = [old.clone(), new.clone()];
        let selected = SearchService::new()
            .select_active_chunk_embedding(&chunk_rows)
            .expect("active chunk embedding");
        assert_eq!(selected.embedded_at, new.embedded_at);
        assert_eq!(
            SearchService::new().select_active_chunk_embedding_model_catalog_id(&[old, new]),
            Some(selected.model_catalog_id)
        );
    }

    #[test]
    fn active_graph_node_embedding_selection_falls_back_to_latest_row() {
        let old = SearchGraphNodeEmbeddingRow {
            node_id: Uuid::now_v7(),
            model_catalog_id: Uuid::now_v7(),
            embedding_vector: None,
            embedded_at: Utc::now() - Duration::minutes(10),
            active: false,
        };
        let new = SearchGraphNodeEmbeddingRow { embedded_at: Utc::now(), ..old.clone() };

        let graph_rows = [old.clone(), new.clone()];
        let selected = SearchService::new()
            .select_active_graph_node_embedding(&graph_rows)
            .expect("graph node embedding");
        assert_eq!(selected.embedded_at, new.embedded_at);
        assert_eq!(
            SearchService::new().select_active_graph_node_embedding_model_catalog_id(&[old, new]),
            Some(selected.model_catalog_id)
        );
    }
}
