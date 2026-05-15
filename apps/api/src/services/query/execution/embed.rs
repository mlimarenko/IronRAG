use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{ai::AiBindingPurpose, provider_profiles::EffectiveProviderProfile},
    integrations::llm::EmbeddingRequest,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        query::vector_dimensions::{
            require_current_vector_index_dimensions, validate_embedding_vector_dimensions,
        },
    },
};

const EMBEDDING_CACHE_MAX_ENTRIES: usize = 1000;

static EMBEDDING_CACHE: std::sync::LazyLock<Mutex<HashMap<u64, Vec<f32>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn embedding_cache_key(question: &str, binding: &ResolvedRuntimeBinding) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    question.hash(&mut hasher);
    binding.provider_catalog_id.hash(&mut hasher);
    binding.provider_kind.hash(&mut hasher);
    binding.provider_base_url.hash(&mut hasher);
    binding.credential_id.hash(&mut hasher);
    binding.model_catalog_id.hash(&mut hasher);
    binding.model_name.hash(&mut hasher);
    binding.extra_parameters_json.hash(&mut hasher);
    hasher.finish()
}

/// Result of embedding a query question, including billing-relevant usage data.
#[derive(Debug, Clone)]
pub(crate) struct QuestionEmbeddingResult {
    pub(crate) embedding: Vec<f32>,
    pub(crate) provider_kind: String,
    pub(crate) model_name: String,
    pub(crate) usage_json: serde_json::Value,
}

pub(super) async fn embed_question(
    state: &AppState,
    library_id: Uuid,
    _provider_profile: &EffectiveProviderProfile,
    question: &str,
) -> anyhow::Result<QuestionEmbeddingResult> {
    let embedding_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryRetrieve)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("active query retrieval binding is not configured for this library")
        })?;

    let trimmed_input = question.trim().to_string();
    let cache_key = embedding_cache_key(&trimmed_input, &embedding_binding);

    let cached_embedding =
        EMBEDDING_CACHE.lock().ok().and_then(|cache| cache.get(&cache_key).cloned());
    if let Some(cached_embedding) = cached_embedding {
        let _vector_guard = state.canonical_services.search.vector_plane_read_guard(state).await?;
        let expected_dimensions = require_current_vector_index_dimensions(state).await?;
        validate_embedding_vector_dimensions(
            expected_dimensions,
            &cached_embedding,
            format!("cached runtime query {}", embedding_binding.model_name),
        )?;
        return Ok(QuestionEmbeddingResult {
            embedding: cached_embedding,
            provider_kind: embedding_binding.provider_kind,
            model_name: embedding_binding.model_name,
            usage_json: serde_json::json!({"cached": true}),
        });
    }

    let response = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: embedding_binding.provider_kind.clone(),
            model_name: embedding_binding.model_name.clone(),
            input: trimmed_input,
            api_key_override: embedding_binding.api_key.clone(),
            base_url_override: embedding_binding.provider_base_url.clone(),
            extra_parameters_json: embedding_binding.extra_parameters_json.clone(),
        })
        .await
        .context("failed to embed runtime query")?;
    {
        let _vector_guard = state.canonical_services.search.vector_plane_read_guard(state).await?;
        let expected_dimensions = require_current_vector_index_dimensions(state).await?;
        validate_embedding_vector_dimensions(
            expected_dimensions,
            &response.embedding,
            format!("runtime query {}", response.model_name),
        )?;
    }

    if let Ok(mut cache) = EMBEDDING_CACHE.lock() {
        if cache.len() >= EMBEDDING_CACHE_MAX_ENTRIES {
            // Evict an arbitrary entry when the cache is full.
            if let Some(&evict_key) = cache.keys().next() {
                cache.remove(&evict_key);
            }
        }
        cache.insert(cache_key, response.embedding.clone());
    }

    Ok(QuestionEmbeddingResult {
        embedding: response.embedding,
        provider_kind: response.provider_kind,
        model_name: response.model_name,
        usage_json: response.usage_json,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn binding() -> ResolvedRuntimeBinding {
        ResolvedRuntimeBinding {
            binding_id: Uuid::from_u128(1),
            workspace_id: Uuid::from_u128(2),
            library_id: Uuid::from_u128(3),
            binding_purpose: AiBindingPurpose::EmbedChunk,
            provider_catalog_id: Uuid::from_u128(4),
            provider_kind: "provider-alpha".to_string(),
            provider_base_url: Some("https://alpha.example/v1".to_string()),
            provider_api_style: "openai_compatible".to_string(),
            credential_id: Uuid::from_u128(5),
            api_key: Some("test-key".to_string()),
            model_catalog_id: Uuid::from_u128(6),
            model_name: "embed-small".to_string(),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: json!({"dimensions": 256}),
        }
    }

    #[test]
    fn query_embedding_cache_key_tracks_vector_source_identity() {
        let base = binding();
        let base_key = embedding_cache_key("query text", &base);

        let mut changed_provider = base.clone();
        changed_provider.provider_catalog_id = Uuid::from_u128(7);
        assert_ne!(base_key, embedding_cache_key("query text", &changed_provider));

        let mut changed_credential = base.clone();
        changed_credential.credential_id = Uuid::from_u128(8);
        assert_ne!(base_key, embedding_cache_key("query text", &changed_credential));

        let mut changed_model = base.clone();
        changed_model.model_catalog_id = Uuid::from_u128(9);
        assert_ne!(base_key, embedding_cache_key("query text", &changed_model));

        let mut changed_base_url = base.clone();
        changed_base_url.provider_base_url = Some("https://beta.example/v1".to_string());
        assert_ne!(base_key, embedding_cache_key("query text", &changed_base_url));

        let mut changed_parameters = base;
        changed_parameters.extra_parameters_json = json!({"dimensions": 512});
        assert_ne!(base_key, embedding_cache_key("query text", &changed_parameters));
    }
}
