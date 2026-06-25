use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, Result as AnyhowResult, anyhow};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    infra::{knowledge_rows::KNOWLEDGE_CHUNK_VECTOR_KIND, repositories::ai_repository},
    integrations::llm::EmbeddingRequest,
};

/// Process-lifetime cache mapping `library_id -> active embed_chunk binding
/// vector dimension`. Resolving the dim requires a Postgres roundtrip
/// (binding + model catalog) and, in the worst case, a provider probe; both
/// are stable until vector bindings change.
fn library_dim_cache() -> &'static Mutex<HashMap<Uuid, u64>> {
    static CACHE: OnceLock<Mutex<HashMap<Uuid, u64>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn invalidate_library_vector_index_dimensions(library_id: Uuid) {
    library_dim_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&library_id);
}

pub(crate) fn invalidate_vector_index_dimension_cache() {
    library_dim_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
}

/// Resolve the active embed_chunk vector dimension for `library_id`.
///
/// Reads `metadata_json["dimensions"]` from the resolved model-catalog row
/// for the library's active `EmbedChunk` binding. When the catalog row does
/// not carry an explicit dimension, persisted vector metadata is used before
/// falling back to a one-shot embedding probe via the LLM gateway. Cached per
/// process so hot-path callers can resolve without repeated DB round-trips.
pub async fn library_vector_index_dimensions(
    state: &AppState,
    library_id: Uuid,
) -> AnyhowResult<u64> {
    let cached = {
        let cache = library_dim_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.get(&library_id).copied()
    };
    if let Some(dim) = cached {
        return Ok(dim);
    }

    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await
        .with_context(|| {
            format!("failed to resolve active embed_chunk binding for library {library_id}")
        })?
        .ok_or_else(|| {
            anyhow!("active embed_chunk binding is not configured for library {library_id}")
        })?;

    let model_row = ai_repository::get_model_catalog_by_id(
        &state.persistence.postgres,
        binding.model_catalog_id,
    )
    .await
    .with_context(|| {
        format!("failed to load model catalog row for binding {}", binding.binding_id)
    })?
    .ok_or_else(|| {
        anyhow!(
            "model catalog row {} for library {library_id} embed_chunk binding is missing",
            binding.model_catalog_id
        )
    })?;

    let catalog_dim = model_row.metadata_json.get("dimensions").and_then(serde_json::Value::as_u64);

    let dim = if let Some(dim) = catalog_dim {
        dim
    } else if let Some(dim) = select_persisted_chunk_vector_dimension(
        &state
            .search_store
            .list_chunk_vector_dimensions(
                library_id,
                &binding.model_catalog_id.to_string(),
                KNOWLEDGE_CHUNK_VECTOR_KIND,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to inspect persisted chunk vector dimensions for library {library_id}"
                )
            })?,
    ) {
        dim
    } else {
        // Fallback: probe the provider once. One probe per library per
        // process lifetime — cached below.
        let response = state
            .llm_gateway
            .embed(EmbeddingRequest {
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
                input: "vector dimension probe".to_string(),
                api_key_override: binding.api_key.clone(),
                base_url_override: binding.provider_base_url.clone(),
                extra_parameters_json: binding.extra_parameters_json.clone(),
            })
            .await
            .with_context(|| {
                format!(
                    "failed to probe vector dimensions for library {library_id} via {}/{}",
                    binding.provider_kind, binding.model_name
                )
            })?;
        u64::try_from(response.embedding.len())
            .context("probed embedding dimension overflowed u64")?
    };

    library_dim_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(library_id, dim);
    Ok(dim)
}

fn select_persisted_chunk_vector_dimension(dimensions: &[u64]) -> Option<u64> {
    let mut nonzero = dimensions.iter().copied().filter(|dim| *dim > 0);
    let selected = nonzero.next()?;
    if nonzero.next().is_some() {
        tracing::warn!(
            selected_dimension = selected,
            dimensions = ?dimensions,
            "multiple persisted chunk vector dimensions found for active embedding binding"
        );
    }
    Some(selected)
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
            "embedding vector dimension mismatch for {vector_context}: expected {expected_dimensions} dimensions from the active library embedding binding, got {actual_dimensions}"
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

    #[test]
    fn selects_first_persisted_chunk_vector_dimension() {
        assert_eq!(Some(3072), select_persisted_chunk_vector_dimension(&[3072, 1536, 768]));
    }

    #[test]
    fn ignores_zero_persisted_chunk_vector_dimensions() {
        assert_eq!(Some(1536), select_persisted_chunk_vector_dimension(&[0, 1536]));
        assert_eq!(None, select_persisted_chunk_vector_dimension(&[0]));
    }
}
