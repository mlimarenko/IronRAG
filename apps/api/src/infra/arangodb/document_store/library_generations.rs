use anyhow::Context;
use uuid::Uuid;

use super::{
    ArangoDocumentStore, KnowledgeLibraryGenerationRow, decode::decode_many_results,
    decode::decode_optional_single_result, decode::decode_single_result,
};
use crate::infra::arangodb::collections::KNOWLEDGE_LIBRARY_GENERATION_COLLECTION;

impl ArangoDocumentStore {
    pub async fn upsert_library_generation(
        &self,
        row: &KnowledgeLibraryGenerationRow,
    ) -> anyhow::Result<KnowledgeLibraryGenerationRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    generation_id: @generation_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    active_text_generation: @active_text_generation,
                    active_vector_generation: @active_vector_generation,
                    active_graph_generation: @active_graph_generation,
                    degraded_state: @degraded_state,
                    updated_at: @updated_at
                 }
                 UPDATE {
                    active_text_generation: MAX([OLD.active_text_generation, @active_text_generation]),
                    active_vector_generation: MAX([OLD.active_vector_generation, @active_vector_generation]),
                    active_graph_generation: MAX([OLD.active_graph_generation, @active_graph_generation]),
                    degraded_state: @degraded_state,
                    updated_at: @updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_LIBRARY_GENERATION_COLLECTION,
                    "key": row.key,
                    "generation_id": row.generation_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "active_text_generation": row.active_text_generation,
                    "active_vector_generation": row.active_vector_generation,
                    "active_graph_generation": row.active_graph_generation,
                    "degraded_state": row.degraded_state,
                    "updated_at": row.updated_at,
                }),
            )
            .await
            .context("failed to upsert knowledge library generation")?;
        decode_single_result(cursor)
    }

    pub async fn list_library_generations(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeLibraryGenerationRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR generation IN @@collection
                 FILTER generation.library_id == @library_id
                 SORT generation.updated_at DESC, generation.generation_id DESC
                 RETURN generation",
                serde_json::json!({
                    "@collection": KNOWLEDGE_LIBRARY_GENERATION_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge library generations")?;
        decode_many_results(cursor)
    }

    pub async fn get_library_generation(
        &self,
        generation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeLibraryGenerationRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR generation IN @@collection
                 FILTER generation.generation_id == @generation_id
                 LIMIT 1
                 RETURN generation",
                serde_json::json!({
                    "@collection": KNOWLEDGE_LIBRARY_GENERATION_COLLECTION,
                    "generation_id": generation_id,
                }),
            )
            .await
            .context("failed to get knowledge library generation")?;
        decode_optional_single_result(cursor)
    }
}
