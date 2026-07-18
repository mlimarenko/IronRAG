use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Context;
use sha2::{Digest, Sha256};

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    integrations::llm::EmbeddingRequest,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        query::{
            error::QueryServiceError,
            provider_billing::{QueryProviderCallReservation, QueryProviderExecutionContext},
            vector_dimensions::{
                validate_active_embedding_profile_key, validate_embedding_vector_dimensions,
            },
        },
    },
};

use super::types::RuntimeVectorSearchContext;

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
    update_framed(&mut hasher, binding.embedding_execution_profile_key().as_bytes());
    hasher.finalize().into()
}

/// Result of embedding a query question after canonical provider accounting.
#[derive(Debug, Clone)]
pub(crate) struct QuestionEmbeddingResult {
    pub(crate) embedding: Vec<f32>,
    pub(crate) embedding_profile_key: String,
}

fn missing_embedding_binding_error() -> QueryServiceError {
    QueryServiceError::BindingNotConfigured {
        message: "active embed_chunk binding is not configured for this library".to_string(),
    }
}

fn validate_cached_embedding_dimensions(
    cache_key: EmbeddingCacheKey,
    expected_dimensions: u64,
    embedding: &[f32],
    vector_context: impl std::fmt::Display,
) -> anyhow::Result<()> {
    if let Err(error) =
        validate_embedding_vector_dimensions(expected_dimensions, embedding, vector_context)
    {
        if let Ok(mut cache) = EMBEDDING_CACHE.lock() {
            cache.remove(&cache_key);
        }
        return Err(error);
    }
    Ok(())
}

pub(super) async fn embed_question(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    question: &str,
    vector_search_context: &RuntimeVectorSearchContext,
) -> anyhow::Result<QuestionEmbeddingResult> {
    let library_id = execution_context.library_id;
    let span_started = std::time::Instant::now();
    let embedding_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
        .ok_or_else(missing_embedding_binding_error)?;

    let trimmed_input = question.trim().to_string();
    let embedding_profile_key = embedding_binding.embedding_execution_profile_key();
    validate_active_embedding_profile_key(
        library_id,
        &vector_search_context.embedding_profile_key,
        &embedding_profile_key,
    )?;
    let cache_key = embedding_cache_key(&trimmed_input, &embedding_binding);

    let cached_embedding =
        EMBEDDING_CACHE.lock().ok().and_then(|cache| cache.get(&cache_key).cloned());
    if let Some(cached_embedding) = cached_embedding {
        let _vector_guard =
            state.canonical_services.search.vector_plane_read_guard(state, library_id).await?;
        validate_cached_embedding_dimensions(
            cache_key,
            vector_search_context.dimensions,
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
        return Ok(QuestionEmbeddingResult { embedding: cached_embedding, embedding_profile_key });
    }

    let mut provider_call = QueryProviderCallReservation::reserve(
        state,
        execution_context,
        &embedding_binding,
        AiBindingPurpose::EmbedChunk,
        "query_embedding",
    )
    .await
    .context("failed to reserve query embedding provider call")?;
    let response = match state
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
    {
        Ok(response) => response,
        Err(error) => {
            if let Err(billing_error) = provider_call.fail().await {
                tracing::error!(
                    provider_call_id = %provider_call.provider_call_id(),
                    %billing_error,
                    "failed to terminalize query embedding provider-call reservation"
                );
            }
            return Err(error).context("failed to embed runtime query");
        }
    };
    provider_call
        .complete(&response.usage_json)
        .await
        .context("failed to persist query embedding provider usage")?;
    {
        let _vector_guard =
            state.canonical_services.search.vector_plane_read_guard(state, library_id).await?;
        validate_embedding_vector_dimensions(
            vector_search_context.dimensions,
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
    Ok(QuestionEmbeddingResult { embedding: response.embedding, embedding_profile_key })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::services::ai_catalog_service::EmbeddingDimensions;

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
            account_id: Uuid::from_u128(5),
            api_key: Some("test-key".to_string()),
            model_catalog_id: Uuid::from_u128(6),
            model_name: "embed-small".to_string(),
            effective_embedding_dimensions: Some(
                EmbeddingDimensions::try_from(256).expect("valid test dimensions"),
            ),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: json!({"dimensions": 256}),
        }
    }

    #[test]
    fn missing_embedding_binding_is_typed() {
        assert!(matches!(
            missing_embedding_binding_error(),
            QueryServiceError::BindingNotConfigured { .. }
        ));
    }

    #[test]
    fn query_embedding_cache_key_tracks_semantic_vector_space_identity() {
        let base = binding();
        let base_key = embedding_cache_key("query text", &base);

        let mut reseeded_provider = base.clone();
        reseeded_provider.provider_catalog_id = Uuid::from_u128(7);
        assert_eq!(base_key, embedding_cache_key("query text", &reseeded_provider));

        let mut rotated_credential = base.clone();
        rotated_credential.account_id = Uuid::from_u128(8);
        rotated_credential.api_key = Some("rotated-test-key".to_string());
        assert_eq!(base_key, embedding_cache_key("query text", &rotated_credential));

        let mut reseeded_model = base.clone();
        reseeded_model.model_catalog_id = Uuid::from_u128(9);
        assert_eq!(base_key, embedding_cache_key("query text", &reseeded_model));

        let mut changed_provider_kind = base.clone();
        changed_provider_kind.provider_kind = "provider-beta".to_string();
        assert_ne!(base_key, embedding_cache_key("query text", &changed_provider_kind));

        let mut changed_base_url = base.clone();
        changed_base_url.provider_base_url = Some("https://beta.example/v1".to_string());
        assert_ne!(base_key, embedding_cache_key("query text", &changed_base_url));

        let mut changed_model_name = base.clone();
        changed_model_name.model_name = "embed-large".to_string();
        assert_ne!(base_key, embedding_cache_key("query text", &changed_model_name));

        let mut changed_parameters = base;
        changed_parameters.extra_parameters_json = json!({"dimensions": 512});
        changed_parameters.effective_embedding_dimensions =
            Some(EmbeddingDimensions::try_from(512).expect("valid test dimensions"));
        assert_ne!(base_key, embedding_cache_key("query text", &changed_parameters));
    }

    #[test]
    fn stale_cached_embedding_is_rejected_and_evicted_by_inventory_dimensions() {
        let binding = binding();
        let cache_key = embedding_cache_key("cache-dimension-fence", &binding);
        EMBEDDING_CACHE.lock().expect("embedding cache lock").insert(cache_key, vec![0.0, 1.0]);

        let error =
            validate_cached_embedding_dimensions(cache_key, 3, &[0.0, 1.0], "cached query test")
                .expect_err("stale cached dimensions must fail closed");

        assert!(error.to_string().contains("expected 3 dimensions"));
        assert!(
            !EMBEDDING_CACHE.lock().expect("embedding cache lock").contains_key(&cache_key),
            "a rejected cached vector must not poison later requests",
        );
    }

    #[test]
    fn embedding_execution_profile_key_ignores_record_and_credential_identity() {
        let base = binding();
        let base_key = base.embedding_execution_profile_key();

        let mut rotated = base.clone();
        rotated.binding_id = Uuid::from_u128(10);
        rotated.workspace_id = Uuid::from_u128(11);
        rotated.library_id = Uuid::from_u128(12);
        rotated.provider_catalog_id = Uuid::from_u128(13);
        rotated.account_id = Uuid::from_u128(14);
        rotated.api_key = Some("rotated-test-key".to_string());
        rotated.model_catalog_id = Uuid::from_u128(15);

        assert_eq!(base_key, rotated.embedding_execution_profile_key());
    }

    #[test]
    fn embedding_execution_profile_key_tracks_every_embedding_execution_dimension() {
        let base = binding();
        let base_key = base.embedding_execution_profile_key();

        let mut changed_provider_kind = base.clone();
        changed_provider_kind.provider_kind = "provider-beta".to_string();
        assert_ne!(base_key, changed_provider_kind.embedding_execution_profile_key());

        let mut changed_api_style = base.clone();
        changed_api_style.provider_api_style = "provider_native".to_string();
        assert_ne!(base_key, changed_api_style.embedding_execution_profile_key());

        let mut changed_base_url = base.clone();
        changed_base_url.provider_base_url = Some("https://beta.example/v1".to_string());
        assert_ne!(base_key, changed_base_url.embedding_execution_profile_key());

        let mut changed_model_name = base.clone();
        changed_model_name.model_name = "embed-large".to_string();
        assert_ne!(base_key, changed_model_name.embedding_execution_profile_key());

        let mut changed_parameters = base;
        changed_parameters.extra_parameters_json = json!({"dimensions": 512});
        changed_parameters.effective_embedding_dimensions =
            Some(EmbeddingDimensions::try_from(512).expect("valid test dimensions"));
        assert_ne!(base_key, changed_parameters.embedding_execution_profile_key());
    }

    #[test]
    fn catalog_only_embedding_dimension_changes_the_execution_profile_key() {
        let mut base = binding();
        base.extra_parameters_json = json!({"encoding_format": "float"});
        let base_key = base.embedding_execution_profile_key();

        let mut changed_catalog_dimension = base.clone();
        changed_catalog_dimension.effective_embedding_dimensions =
            Some(EmbeddingDimensions::try_from(512).expect("valid test dimensions"));

        assert_eq!(base.extra_parameters_json, changed_catalog_dimension.extra_parameters_json);
        assert_ne!(base_key, changed_catalog_dimension.embedding_execution_profile_key());
    }

    #[test]
    fn embedding_execution_profile_key_normalizes_the_resolved_endpoint() {
        let base = binding();
        let mut equivalent = base.clone();
        equivalent.provider_base_url = Some("  https://ALPHA.EXAMPLE:443/v1/  ".to_string());

        assert_eq!(
            base.embedding_execution_profile_key(),
            equivalent.embedding_execution_profile_key()
        );
    }

    #[test]
    fn embedding_execution_profile_key_tracks_the_embedding_runtime_endpoint() {
        let mut base = binding();
        base.extra_parameters_json = json!({
            "dimensions": 256,
            "_providerProfile": {
                "runtime": {"kind": "openai_compatible", "embeddingsPath": "/embeddings"},
                "baseUrl": {"allowPrivateNetwork": false}
            }
        });
        let base_key = base.embedding_execution_profile_key();

        let mut changed_path = base.clone();
        changed_path.extra_parameters_json["_providerProfile"]["runtime"]["embeddingsPath"] =
            json!("/v2/embeddings");
        assert_ne!(base_key, changed_path.embedding_execution_profile_key());

        let mut changed_routing = base;
        changed_routing.extra_parameters_json["_providerProfile"]["baseUrl"]["allowPrivateNetwork"] =
            json!(true);
        assert_ne!(base_key, changed_routing.embedding_execution_profile_key());
    }

    #[test]
    fn embedding_execution_profile_key_ignores_non_embedding_provider_metadata() {
        let mut base = binding();
        base.extra_parameters_json = json!({
            "dimensions": 256,
            "_providerProfile": {
                "runtime": {"kind": "openai_compatible", "embeddingsPath": "/embeddings"},
                "baseUrl": {"allowPrivateNetwork": false},
                "credentials": {"apiKeyRequired": true},
                "requestPolicy": {"sampling": "forward"}
            },
            "_providerRequestPolicy": {"sampling": "forward"}
        });
        let mut changed = base.clone();
        changed.extra_parameters_json["_providerProfile"]["credentials"] =
            json!({"apiKeyRequired": false});
        changed.extra_parameters_json["_providerProfile"]["requestPolicy"] =
            json!({"sampling": "omit"});
        changed.extra_parameters_json["_providerRequestPolicy"] = json!({"sampling": "omit"});
        changed.extra_parameters_json["model"] = json!("ignored-local-override");
        changed.extra_parameters_json["input"] = json!("ignored-local-override");

        assert_eq!(
            base.embedding_execution_profile_key(),
            changed.embedding_execution_profile_key()
        );
    }

    #[test]
    fn embedding_execution_profile_key_canonicalizes_json_object_order() {
        let mut left = binding();
        left.extra_parameters_json = serde_json::from_str(
            r#"{"dimensions":256,"request":{"encoding":"float","truncate":false}}"#,
        )
        .unwrap();
        let mut right = binding();
        right.extra_parameters_json = serde_json::from_str(
            r#"{"request":{"truncate":false,"encoding":"float"},"dimensions":256}"#,
        )
        .unwrap();

        assert_eq!(left.embedding_execution_profile_key(), right.embedding_execution_profile_key());
    }

    #[test]
    fn embedding_execution_profile_key_does_not_depend_on_secret_rotation() {
        let base = binding();
        let mut rotated = base.clone();
        rotated.api_key = Some("rotated-test-key".to_string());

        assert_eq!(
            base.embedding_execution_profile_key(),
            rotated.embedding_execution_profile_key()
        );
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
