use crate::infra::knowledge_plane::VectorSimilaritySearchInput;

use uuid::Uuid;

use crate::{
    app::state::AppState,
    services::query::{
        error::QueryServiceError, vector_dimensions::validate_embedding_vector_dimensions,
    },
};

use super::{
    retrieve::validate_runtime_vector_search_context,
    types::{QueryGraphIndex, RuntimeMatchedEntity, RuntimeVectorSearchContext},
};

#[derive(Debug, thiserror::Error)]
#[error("graph entity vector store unavailable")]
pub(super) struct EntityVectorStoreUnavailable;

fn entity_vector_fetch_error(error: anyhow::Error) -> anyhow::Error {
    let operational = error.downcast_ref::<QueryServiceError>().is_none()
        && error.chain().any(|cause| {
            cause.downcast_ref::<sqlx::Error>().is_some_and(|error| match error {
                sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
                sqlx::Error::Database(database) => {
                    database.code().is_some_and(|code| code.starts_with("08") || code == "57P03")
                }
                _ => false,
            })
        });
    if operational {
        EntityVectorStoreUnavailable.into()
    } else {
        error.context("failed to search normalized entity vectors for runtime query")
    }
}

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
        state.application_services.search.vector_plane_read_guard(state, library_id).await?;
    validate_runtime_vector_search_context(state, library_id, context).await?;
    validate_embedding_vector_dimensions(
        context.dimensions,
        question_embedding,
        "runtime entity search",
    )?;
    let raw_hits = state
        .search_store
        .search_entity_vectors_by_similarity(
            VectorSimilaritySearchInput {
                dim: context.dimensions,
                library_id,
                embedding_model_key: &context.embedding_profile_key,
                query_vector: question_embedding,
                n_probe: None,
            },
            limit.max(1),
        )
        .await
        .map_err(entity_vector_fetch_error)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_vector_fetch_classifies_only_operational_failures() {
        assert!(
            entity_vector_fetch_error(anyhow::Error::new(sqlx::Error::PoolTimedOut))
                .downcast_ref::<EntityVectorStoreUnavailable>()
                .is_some()
        );
        assert!(
            entity_vector_fetch_error(anyhow::Error::new(QueryServiceError::StateConflict {
                message: "identity changed".into(),
            }))
            .downcast_ref::<EntityVectorStoreUnavailable>()
            .is_none()
        );
    }
}
