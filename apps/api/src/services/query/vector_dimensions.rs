use anyhow::{Context, Result as AnyhowResult, anyhow};

use crate::app::state::AppState;

#[must_use]
pub(crate) fn expected_embedding_dimensions(state: &AppState) -> u64 {
    state.settings.arangodb_vector_dimensions
}

pub(crate) fn validate_embedding_vector_dimensions(
    state: &AppState,
    vector: &[f32],
    vector_context: impl std::fmt::Display,
) -> AnyhowResult<i32> {
    if vector.is_empty() {
        return Err(anyhow!("embedding vector for {vector_context} must not be empty"));
    }

    let expected_dimensions = expected_embedding_dimensions(state);
    let actual_dimensions =
        u64::try_from(vector.len()).context("embedding vector dimension overflowed u64")?;
    if actual_dimensions != expected_dimensions {
        return Err(anyhow!(
            "embedding vector dimension mismatch for {vector_context}: expected {expected_dimensions} dimensions from arangodb_vector_dimensions, got {actual_dimensions}"
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
            validate_embedding_vector_dimensions_for_expected(3, &[0.0, 1.0, 2.0], "test vector")
                .unwrap()
        );
    }

    #[test]
    fn rejects_unexpected_embedding_dimensions() {
        let error =
            validate_embedding_vector_dimensions_for_expected(3, &[0.0, 1.0], "test vector")
                .unwrap_err()
                .to_string();
        assert!(error.contains("expected 3 dimensions"));
        assert!(error.contains("got 2"));
    }

    fn validate_embedding_vector_dimensions_for_expected(
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
                "embedding vector dimension mismatch for {vector_context}: expected {expected_dimensions} dimensions from arangodb_vector_dimensions, got {actual_dimensions}"
            ));
        }
        i32::try_from(vector.len()).context("embedding vector dimension overflowed i32")
    }
}
