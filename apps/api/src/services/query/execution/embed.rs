use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Context;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{ai::AiBindingPurpose, provider_profiles::EffectiveProviderProfile},
    integrations::llm::EmbeddingRequest,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        query::vector_dimensions::{
            library_vector_index_dimensions, validate_embedding_vector_dimensions,
        },
    },
};

const EMBEDDING_CACHE_MAX_ENTRIES: usize = 1000;

/// Full-width content identity for an in-process question-embedding cache
/// entry. A 32-byte SHA-256 of the canonical identity tuple (trimmed question
/// text + every embedding-scoping binding dimension) keyed with full-width
/// `Eq`, so two distinct questions / bindings can never alias.
///
/// The previous design keyed by a `u64` from `DefaultHasher` and silently
/// returned on collision — a 64-bit collision would hand back the *wrong*
/// embedding for a different question (silent wrong retrieval), and
/// `DefaultHasher` is not stable across releases. SHA-256 width plus equality
/// on the full digest removes the collision risk; storage is keyed by the
/// digest itself, never a truncated hash.
type EmbeddingCacheKey = [u8; 32];

static EMBEDDING_CACHE: std::sync::LazyLock<Mutex<HashMap<EmbeddingCacheKey, Vec<f32>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn update_framed(hasher: &mut Sha256, bytes: &[u8]) {
    // Length-prefix every field so concatenation is unambiguous: adjacent
    // fields cannot be re-partitioned into a colliding identity tuple.
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

pub(super) fn embedding_cache_key(
    question: &str,
    binding: &ResolvedRuntimeBinding,
) -> EmbeddingCacheKey {
    let mut hasher = Sha256::new();
    update_framed(&mut hasher, question.as_bytes());
    update_framed(&mut hasher, binding.provider_catalog_id.as_bytes());
    update_framed(&mut hasher, binding.provider_kind.as_bytes());
    update_framed(&mut hasher, binding.provider_base_url.as_deref().unwrap_or("").as_bytes());
    update_framed(&mut hasher, binding.credential_id.as_bytes());
    update_framed(&mut hasher, binding.model_catalog_id.as_bytes());
    update_framed(&mut hasher, binding.model_name.as_bytes());
    // `serde_json::Value` has no stable `Hash`; serialize to its canonical
    // string form (BTreeMap-ordered object keys) for a deterministic identity.
    let extra_parameters = serde_json::to_string(&binding.extra_parameters_json)
        .unwrap_or_else(|_| binding.extra_parameters_json.to_string());
    update_framed(&mut hasher, extra_parameters.as_bytes());
    hasher.finalize().into()
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
    let span_started = std::time::Instant::now();
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
        let expected_dimensions = library_vector_index_dimensions(state, library_id).await?;
        validate_embedding_vector_dimensions(
            expected_dimensions,
            &cached_embedding,
            format!("cached runtime query {}", embedding_binding.model_name),
        )?;
        crate::services::query::turn_spans::record_span(
            "embed.question.cache_hit",
            "llm",
            span_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            None,
            None,
        );
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
        let expected_dimensions = library_vector_index_dimensions(state, library_id).await?;
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

    crate::services::query::turn_spans::record_span(
        "embed.question",
        "llm",
        span_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        None,
        None,
    );
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

    #[test]
    fn query_embedding_cache_key_is_full_width_sha256() {
        // The key is a 32-byte digest, equality on the full width — not a 64-bit
        // hash that could silently alias two distinct questions.
        let key = embedding_cache_key("query text", &binding());
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn query_embedding_cache_key_distinguishes_distinct_questions() {
        let base = binding();
        let key_a = embedding_cache_key("how do I configure the gateway", &base);
        let key_b = embedding_cache_key("how do I configure the database", &base);
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn query_embedding_cache_distinguishes_distinct_questions_in_storage() {
        // Engineer the old collision scenario: two distinct questions must map to
        // distinct cache entries so a `.get()` can never return another
        // question's embedding.
        let base = binding();
        let mut cache: std::collections::HashMap<EmbeddingCacheKey, Vec<f32>> =
            std::collections::HashMap::new();
        let key_a = embedding_cache_key("question alpha", &base);
        let key_b = embedding_cache_key("question beta", &base);
        cache.insert(key_a, vec![1.0, 0.0]);
        cache.insert(key_b, vec![0.0, 1.0]);
        assert_eq!(cache.get(&key_a), Some(&vec![1.0, 0.0]));
        assert_eq!(cache.get(&key_b), Some(&vec![0.0, 1.0]));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn query_embedding_cache_field_boundaries_are_unambiguous() {
        // Length-prefix framing means moving a character across a field boundary
        // changes the key: the question "ab" + provider_kind "c" must not collide
        // with question "a" + provider_kind "bc".
        let mut left = binding();
        left.provider_kind = "c".to_string();
        let mut right = binding();
        right.provider_kind = "bc".to_string();
        assert_ne!(embedding_cache_key("ab", &left), embedding_cache_key("a", &right),);
    }

    #[test]
    fn query_embedding_cache_hit_for_identical_identity_tuple() {
        let base = binding();
        assert_eq!(
            embedding_cache_key("identical question", &base),
            embedding_cache_key("identical question", &base),
        );
    }
}
