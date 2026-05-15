use anyhow::{Context, Result as AnyhowResult, anyhow};

use crate::{
    app::state::AppState,
    infra::arangodb::collections::{
        KNOWLEDGE_CHUNK_VECTOR_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_INDEX,
        KNOWLEDGE_ENTITY_VECTOR_COLLECTION, KNOWLEDGE_ENTITY_VECTOR_INDEX,
    },
};

pub(crate) async fn current_vector_index_dimensions(state: &AppState) -> AnyhowResult<Option<u64>> {
    let chunk_dimensions = state
        .arango_client
        .vector_index_dimensions(
            KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
            KNOWLEDGE_CHUNK_VECTOR_INDEX,
            "vector",
        )
        .await
        .context("failed to read chunk vector index dimensions")?;
    let entity_dimensions = state
        .arango_client
        .vector_index_dimensions(
            KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
            KNOWLEDGE_ENTITY_VECTOR_INDEX,
            "vector",
        )
        .await
        .context("failed to read entity vector index dimensions")?;
    match (chunk_dimensions, entity_dimensions) {
        (Some(chunk), Some(entity)) if chunk == entity => Ok(Some(chunk)),
        (None, None) => Ok(None),
        (Some(chunk), Some(entity)) => Err(anyhow!(
            "Arango vector index dimension mismatch: chunk index has {chunk}, entity index has {entity}"
        )),
        (Some(chunk), None) => Err(anyhow!(
            "Arango vector index dimension mismatch: chunk index has {chunk}, entity index is missing"
        )),
        (None, Some(entity)) => Err(anyhow!(
            "Arango vector index dimension mismatch: chunk index is missing, entity index has {entity}"
        )),
    }
}

pub(crate) async fn require_current_vector_index_dimensions(state: &AppState) -> AnyhowResult<u64> {
    current_vector_index_dimensions(state)
        .await?
        .ok_or_else(|| anyhow!("Arango vector indexes are missing; run the vector rebuild first"))
}

pub(crate) fn validate_embedding_vector_dimensions(
    expected_dimensions: u64,
    vector: &[f32],
    vector_context: impl std::fmt::Display,
) -> AnyhowResult<i32> {
    if vector.is_empty() {
        return Err(anyhow!("embedding vector for {vector_context} must not be empty"));
    }

    let actual_dimensions =
        u64::try_from(vector.len()).context("embedding vector dimension overflowed u64")?;
    if actual_dimensions != expected_dimensions {
        return Err(anyhow!(
            "embedding vector dimension mismatch for {vector_context}: expected {expected_dimensions} dimensions from Arango vector index, got {actual_dimensions}"
        ));
    }

    i32::try_from(vector.len()).context("embedding vector dimension overflowed i32")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_expected_embedding_dimensions() {
        assert_eq!(
            i32::try_from(3usize).unwrap(),
            validate_embedding_vector_dimensions(3, &[0.0, 1.0, 2.0], "test vector").unwrap()
        );
    }

    #[test]
    fn rejects_unexpected_embedding_dimensions() {
        let error = validate_embedding_vector_dimensions(3, &[0.0, 1.0], "test vector")
            .unwrap_err()
            .to_string();
        assert!(error.contains("expected 3 dimensions"));
        assert!(error.contains("got 2"));
    }
}
