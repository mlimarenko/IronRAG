use super::*;

pub(crate) fn validate_embedding_vector_dimensions(
    expected_dimensions: u64,
    vector: &[f32],
    vector_context: impl std::fmt::Display,
) -> AnyhowResult<i32> {
    if vector.is_empty() {
        return Err(anyhow!("embedding vector for {vector_context} must not be empty"));
    }
    if !vector.iter().all(|value| value.is_finite()) {
        return Err(anyhow!("embedding vector for {vector_context} contains a non-finite value"));
    }

    let actual_dimensions =
        u64::try_from(vector.len()).context("embedding vector dimension overflowed u64")?;
    EmbeddingDimensions::try_from(actual_dimensions)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("embedding vector for {vector_context} cannot be indexed"))?;
    if actual_dimensions != expected_dimensions {
        return Err(anyhow!(
            "embedding vector dimension mismatch for {vector_context}: expected {expected_dimensions} dimensions from the active library embedding binding, got {actual_dimensions}"
        ));
    }

    i32::try_from(vector.len()).context("embedding vector dimension overflowed i32")
}

/// Validate one provider batch as a single, indexable vector space. This is
/// used both when dimensions are already known and when the first ordinary
/// chunk batch establishes them.
pub(crate) fn validate_embedding_batch_dimensions(
    expected_vector_count: usize,
    reported_dimensions: usize,
    embeddings: &[Vec<f32>],
    expected_dimensions: Option<EmbeddingDimensions>,
    batch_context: impl std::fmt::Display,
) -> AnyhowResult<EmbeddingDimensions> {
    let batch_context = batch_context.to_string();
    if embeddings.len() != expected_vector_count {
        return Err(anyhow!(
            "embedding batch for {batch_context} returned {} vectors for {expected_vector_count} inputs",
            embeddings.len()
        ));
    }
    let first = embeddings
        .first()
        .ok_or_else(|| anyhow!("embedding batch for {batch_context} returned no vectors"))?;
    let dimensions = EmbeddingDimensions::try_from(
        u64::try_from(first.len()).context("embedding vector dimension overflowed u64")?,
    )
    .map_err(anyhow::Error::from)
    .with_context(|| format!("embedding batch for {batch_context} cannot be indexed"))?;
    if reported_dimensions != first.len() {
        return Err(anyhow!(
            "embedding batch for {batch_context} reported {reported_dimensions} dimensions but returned vectors with {} dimensions",
            first.len()
        ));
    }
    for (index, embedding) in embeddings.iter().enumerate() {
        validate_embedding_vector_dimensions(
            dimensions.get(),
            embedding,
            format!("{batch_context} vector {index}"),
        )?;
    }
    if let Some(expected_dimensions) = expected_dimensions
        && expected_dimensions != dimensions
    {
        return Err(anyhow!(
            "embedding batch for {batch_context} returned {} dimensions; expected {} from the exact embedding profile",
            dimensions.get(),
            expected_dimensions.get()
        ));
    }
    Ok(dimensions)
}
