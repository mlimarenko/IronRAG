use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{ai::AiBindingPurpose, provider_profiles::EffectiveProviderProfile},
    integrations::llm::EmbeddingRequest,
};

const EMBEDDING_CACHE_MAX_ENTRIES: usize = 1000;

static EMBEDDING_CACHE: std::sync::LazyLock<Mutex<HashMap<u64, Vec<f32>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn embedding_cache_key(question: &str, model: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    question.hash(&mut hasher);
    model.hash(&mut hasher);
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
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("active embedding binding is not configured for this library")
        })?;

    let trimmed_input = question.trim().to_string();
    let cache_key = embedding_cache_key(&trimmed_input, &embedding_binding.model_name);

    if let Ok(cache) = EMBEDDING_CACHE.lock() {
        if let Some(cached_embedding) = cache.get(&cache_key) {
            return Ok(QuestionEmbeddingResult {
                embedding: cached_embedding.clone(),
                provider_kind: embedding_binding.provider_kind,
                model_name: embedding_binding.model_name,
                usage_json: serde_json::json!({"cached": true}),
            });
        }
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
