use uuid::Uuid;

use crate::{
    app::state::AppState,
    services::query::{
        error::QueryServiceError, vector_dimensions::validate_embedding_vector_dimensions,
    },
};

use super::{
    graph_retrieval_error::is_vector_relation_not_found,
    retrieve::validate_runtime_vector_search_context,
    types::{QueryGraphIndex, RuntimeMatchedEntity, RuntimeVectorSearchContext},
};

pub(super) async fn retrieve_entity_vector_hits(
    state: &AppState,
    library_id: Uuid,
    limit: usize,
    question_embedding: &[f32],
    vector_search_context: Option<&RuntimeVectorSearchContext>,
    graph_index: &QueryGraphIndex,
) -> anyhow::Result<Vec<RuntimeMatchedEntity>> {
    if question_embedding.is_empty() {
        return Ok(Vec::new());
    }
    let context = vector_search_context.ok_or_else(|| QueryServiceError::StateConflict {
            message: format!(
                "runtime query for library {library_id} has a graph vector without a ready exact-profile preflight; retry the query"
            ),
        })?;

    let _vector_guard =
        state.canonical_services.search.vector_plane_read_guard(state, library_id).await?;
    validate_runtime_vector_search_context(state, library_id, context).await?;
    validate_embedding_vector_dimensions(
        context.dimensions,
        question_embedding,
        "runtime entity search",
    )?;
    let raw_hits = match state
        .search_store
        .search_entity_vectors_by_similarity(
            context.dimensions,
            library_id,
            &context.embedding_profile_key,
            question_embedding,
            limit.max(1),
            None,
        )
        .await
    {
        Ok(hits) => hits,
        Err(ref error) if is_vector_relation_not_found(error) => {
            tracing::info!(
                library_id = %library_id,
                "entity vector search: empty layer, returning no graph evidence"
            );
            Vec::new()
        }
        Err(error) => {
            return Err(
                error.context("failed to search canonical entity vectors for runtime query")
            );
        }
    };

    Ok(raw_hits
        .into_iter()
        .filter_map(|hit| {
            let node = graph_index.node(hit.entity_id)?;
            (!node.node_type.eq_ignore_ascii_case("document")).then(|| RuntimeMatchedEntity {
                node_id: node.id,
                label: node.label.clone(),
                node_type: node.node_type.clone(),
                summary: node.summary.clone(),
                score: Some(hit.score as f32),
            })
        })
        .collect())
}
