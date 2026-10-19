use std::{sync::Arc, time::Duration};

use crate::integrations::{
    provider_budget::{ProviderBudgetRegistry, ProviderIdentity},
    typesafe::{
        SystemOneBudget, SystemOneClient, SystemOneClientBuildError, SystemOneClientConfig,
        SystemOneTransportParams,
    },
};

use super::*;
use crate::domains::provider_profiles::{
    RESOLVED_REASONING_CAPABILITY_FIELD, model_reasoning_capability,
};

#[derive(Clone)]
pub struct ResolvedRuntimeBinding {
    pub binding_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_purpose: AiBindingPurpose,
    pub provider_catalog_id: Uuid,
    pub provider_kind: String,
    pub provider_base_url: Option<String>,
    pub provider_api_style: String,
    pub account_id: Uuid,
    pub api_key: Option<String>,
    pub model_catalog_id: Uuid,
    pub model_name: String,
    /// Validated output dimension of the effective embedding profile.
    ///
    /// Binding request parameters take precedence over model-catalog metadata.
    /// `None` means no dimension was declared. Runtime resolution may use only
    /// exact-profile persisted vector metadata; it never issues a hidden
    /// provider probe.
    pub effective_embedding_dimensions: Option<EmbeddingDimensions>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
}

impl Drop for ResolvedRuntimeBinding {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for ResolvedRuntimeBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedRuntimeBinding")
            .field("binding_id", &self.binding_id)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("binding_purpose", &self.binding_purpose)
            .field("provider_catalog_id", &self.provider_catalog_id)
            .field("provider_kind", &self.provider_kind)
            .field("provider_base_url", &self.provider_base_url.as_ref().map(|_| "<redacted>"))
            .field("provider_api_style", &self.provider_api_style)
            .field("account_id", &self.account_id)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("model_catalog_id", &self.model_catalog_id)
            .field("model_name", &self.model_name)
            .field("effective_embedding_dimensions", &self.effective_embedding_dimensions)
            .finish_non_exhaustive()
    }
}

/// Non-zero embedding dimension supported by the Postgres vector plane.
///
/// The private representation prevents an invalid zero or a value that cannot
/// be indexed by the selected pgvector `vector`/`halfvec` storage strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EmbeddingDimensions(std::num::NonZeroU32);

const MAX_INDEXED_EMBEDDING_DIMENSIONS: u64 = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why an embedding dimension cannot be represented by [`EmbeddingDimensions`].
pub enum EmbeddingDimensionsValidationError {
    /// Zero cannot describe a vector space.
    Zero,
    /// The dimension is greater than the largest supported pgvector storage lane.
    ExceedsStorageLimit,
}

impl std::fmt::Display for EmbeddingDimensionsValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zero => formatter.write_str("must be greater than zero"),
            Self::ExceedsStorageLimit => {
                formatter.write_str("exceeds the vector storage dimension limit")
            }
        }
    }
}

impl std::error::Error for EmbeddingDimensionsValidationError {}

impl TryFrom<u64> for EmbeddingDimensions {
    type Error = EmbeddingDimensionsValidationError;

    fn try_from(dimensions: u64) -> Result<Self, Self::Error> {
        if dimensions == 0 {
            return Err(EmbeddingDimensionsValidationError::Zero);
        }
        if dimensions > MAX_INDEXED_EMBEDDING_DIMENSIONS {
            return Err(EmbeddingDimensionsValidationError::ExceedsStorageLimit);
        }
        let dimensions = u32::try_from(dimensions)
            .map_err(|_| EmbeddingDimensionsValidationError::ExceedsStorageLimit)?;
        let dimensions = std::num::NonZeroU32::new(dimensions)
            .ok_or(EmbeddingDimensionsValidationError::Zero)?;
        Ok(Self(dimensions))
    }
}

impl EmbeddingDimensions {
    /// Returns the validated dimension as an unsigned integer suitable for
    /// persistence and provider-boundary validation.
    #[must_use]
    pub fn get(self) -> u64 {
        u64::from(self.0.get())
    }
}

impl ResolvedRuntimeBinding {
    /// Stable, secret-free identity of the effective request profile that
    /// produces embedding vectors.
    ///
    /// Scope and binding-row identifiers are deliberately excluded: an
    /// inherited instance/workspace binding and its library resolution must
    /// address the same vector lane. Conversely, every value that can select
    /// a different embedding execution path is included. Credential/account
    /// and catalog-row UUIDs are deliberately excluded: rotating or reseeding
    /// those records does not change a vector space when endpoint, runtime,
    /// model identity, and semantic request parameters are unchanged. Binding
    /// purpose is routing metadata and is not part of the provider request.
    #[must_use]
    pub fn embedding_execution_profile_key(&self) -> String {
        const PROFILE_DOMAIN: &[u8] = b"ironrag.embedding-execution-profile.v1";
        const KEY_PREFIX: &str = "embedding-profile:v1:";

        let mut hasher = Sha256::new();
        update_embedding_profile_field(&mut hasher, b"domain", PROFILE_DOMAIN);
        update_embedding_profile_field(
            &mut hasher,
            b"provider_kind",
            self.provider_kind.as_bytes(),
        );
        let normalized_base_url =
            normalized_embedding_profile_base_url(self.provider_base_url.as_deref());
        update_embedding_profile_optional_field(
            &mut hasher,
            b"provider_base_url",
            normalized_base_url.as_deref().map(str::as_bytes),
        );
        update_embedding_profile_field(
            &mut hasher,
            b"provider_api_style",
            self.provider_api_style.as_bytes(),
        );
        update_embedding_profile_optional_field(
            &mut hasher,
            b"provider_runtime_kind",
            embedding_profile_runtime_string(&self.extra_parameters_json, "kind")
                .as_deref()
                .map(str::as_bytes),
        );
        update_embedding_profile_optional_field(
            &mut hasher,
            b"provider_embeddings_path",
            embedding_profile_runtime_string(&self.extra_parameters_json, "embeddingsPath")
                .as_deref()
                .map(str::as_bytes),
        );
        update_embedding_profile_optional_field(
            &mut hasher,
            b"provider_private_network_routing",
            embedding_profile_private_network_routing(&self.extra_parameters_json)
                .as_deref()
                .map(str::as_bytes),
        );
        update_embedding_profile_field(&mut hasher, b"model_name", self.model_name.as_bytes());
        update_embedding_profile_json_field(
            &mut hasher,
            b"embedding_request_parameters",
            &self.effective_embedding_request_parameters(),
        );
        format!("{KEY_PREFIX}{}", hex::encode(hasher.finalize()))
    }

    /// Normalized semantic request profile used only for vector-space identity.
    ///
    /// Catalog dimensions are inserted into this projection without mutating
    /// `extra_parameters_json`, so they fence stored vectors but are not sent to
    /// providers that do not accept an explicit dimensions request parameter.
    /// Explicit binding dimensions are reinserted from the validated typed
    /// field, keeping hashing and dimension resolution on one source of truth.
    fn effective_embedding_request_parameters(&self) -> serde_json::Value {
        let mut parameters = embedding_request_parameters(&self.extra_parameters_json);
        let Some(parameters) = parameters.as_object_mut() else {
            return serde_json::json!({});
        };
        parameters.remove("dimensions");
        if let Some(dimensions) = self.effective_embedding_dimensions {
            parameters.insert(
                "dimensions".to_string(),
                serde_json::Value::Number(serde_json::Number::from(dimensions.get())),
            );
        }
        serde_json::Value::Object(std::mem::take(parameters))
    }

    #[must_use]
    pub fn chat_request_seed(&self) -> ChatRequestSeed {
        ChatRequestSeed {
            provider_kind: self.provider_kind.clone(),
            model_name: self.model_name.clone(),
            api_key_override: self.api_key.clone(),
            base_url_override: self.provider_base_url.clone(),
            system_prompt: self.system_prompt.clone(),
            temperature: self.temperature,
            top_p: self.top_p,
            max_output_tokens_override: self.max_output_tokens_override,
            extra_parameters_json: self.extra_parameters_json.clone(),
        }
    }

    /// Ordered relevance levels a SystemOne `Score` question must carry for
    /// this binding, read from the same `_providerProfile` source the gateway
    /// resolves the runtime kind from. `None` when the profile declares no
    /// valid rubric: the decision route then fails closed instead of scoring
    /// against an invented scale.
    #[must_use]
    pub fn decision_score_criteria(&self) -> Option<Vec<String>> {
        let levels = self
            .extra_parameters_json
            .get("_providerProfile")?
            .get("runtime")?
            .get("scoreCriteria")?
            .as_array()?
            .iter()
            .map(|level| {
                level.as_str().map(str::trim).filter(|level| !level.is_empty()).map(str::to_string)
            })
            .collect::<Option<Vec<_>>>()?;
        (crate::integrations::typesafe::MIN_SCORE_LEVELS
            ..=crate::integrations::typesafe::MAX_SCORE_LEVELS)
            .contains(&levels.len())
            .then_some(levels)
    }

    /// The typed provider runtime kind carried by the merged
    /// `_providerProfile`, trimmed and empty-filtered.
    ///
    /// This is the same source `UnifiedGateway::resolve_provider` dispatches on,
    /// so a caller recognises a decision provider structurally instead of
    /// guessing from `provider_kind` or the model name.
    #[must_use]
    pub fn provider_runtime_kind(&self) -> Option<&str> {
        self.extra_parameters_json
            .get("_providerProfile")?
            .get("runtime")?
            .get("kind")?
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    /// Whether the merged provider profile permits a private-network base URL.
    ///
    /// A missing or non-boolean policy fails closed: the provider endpoint is
    /// contacted only when the profile explicitly allows private routing.
    #[must_use]
    pub fn provider_allows_private_network(&self) -> bool {
        self.extra_parameters_json
            .get("_providerProfile")
            .and_then(|profile| profile.get("baseUrl"))
            .and_then(|base_url| base_url.get("allowPrivateNetwork"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    /// Builds a SystemOne decision client for this resolved binding.
    ///
    /// This is the one owner of decision-client assembly: it trims and filters
    /// the binding's base URL and API key, parses the URL, applies the
    /// binding's private-network policy, and derives the provider budget
    /// identity from the provider kind and normalized base URL. Every decision
    /// caller maps the typed build failure onto its own failure vocabulary and
    /// keeps its own timeout wrapper.
    ///
    /// # Errors
    /// Returns [`SystemOneClientBuildError`] when the binding declares no base
    /// URL or API key, when the base URL cannot be parsed, or when the shared
    /// provider transport cannot be built.
    pub fn systemone_client(
        &self,
        transport: SystemOneTransportParams,
        provider_budget: &Arc<ProviderBudgetRegistry>,
        execution_deadline: Duration,
    ) -> Result<SystemOneClient, SystemOneClientBuildError> {
        let base_url = self
            .provider_base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(SystemOneClientBuildError::MissingBaseUrl)?;
        let api_key = self
            .api_key
            .clone()
            .filter(|key| !key.trim().is_empty())
            .ok_or(SystemOneClientBuildError::MissingApiKey)?;
        let endpoint =
            reqwest::Url::parse(base_url).map_err(|_| SystemOneClientBuildError::InvalidBaseUrl)?;
        SystemOneClient::try_new(SystemOneClientConfig {
            base_url: endpoint,
            api_key: Zeroizing::new(api_key),
            request_timeout: transport.request_timeout,
            allow_private_network: self.provider_allows_private_network(),
            max_attempts: transport.max_attempts,
            initial_backoff: transport.initial_backoff,
            max_backoff: transport.max_backoff,
            execution_deadline,
            budget: Some(SystemOneBudget {
                registry: Arc::clone(provider_budget),
                identity: ProviderIdentity::new(self.provider_kind.clone(), base_url.to_string()),
            }),
        })
        .map_err(|_| SystemOneClientBuildError::Config)
    }
}

fn normalized_embedding_profile_base_url(value: Option<&str>) -> Option<String> {
    value.map(|value| {
        let trimmed = value.trim().trim_end_matches('/');
        reqwest::Url::parse(trimmed).map_or_else(
            |_| trimmed.to_string(),
            |url| url.to_string().trim_end_matches('/').to_string(),
        )
    })
}

fn embedding_profile_runtime_string(
    extra_parameters_json: &serde_json::Value,
    field: &str,
) -> Option<String> {
    extra_parameters_json
        .get("_providerProfile")?
        .get("runtime")?
        .get(field)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if field == "embeddingsPath" {
                format!("/{}", value.trim_matches('/'))
            } else {
                value.to_string()
            }
        })
}

fn embedding_profile_private_network_routing(
    extra_parameters_json: &serde_json::Value,
) -> Option<String> {
    extra_parameters_json
        .get("_providerProfile")?
        .get("baseUrl")?
        .get("allowPrivateNetwork")?
        .as_bool()
        .map(|value| value.to_string())
}

fn update_embedding_profile_field(hasher: &mut Sha256, name: &[u8], value: &[u8]) {
    hasher.update((name.len() as u64).to_le_bytes());
    hasher.update(name);
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn update_embedding_profile_optional_field(hasher: &mut Sha256, name: &[u8], value: Option<&[u8]>) {
    if let Some(value) = value {
        hasher.update([1]);
        update_embedding_profile_field(hasher, name, value);
    } else {
        hasher.update([0]);
        update_embedding_profile_field(hasher, name, &[]);
    }
}

fn update_embedding_profile_json_field(
    hasher: &mut Sha256,
    name: &[u8],
    value: &serde_json::Value,
) {
    hasher.update((name.len() as u64).to_le_bytes());
    hasher.update(name);
    update_embedding_profile_json(hasher, value);
}

fn update_embedding_profile_json(hasher: &mut Sha256, value: &serde_json::Value) {
    use serde_json::Value;

    match value {
        Value::Null => hasher.update(b"n"),
        Value::Bool(value) => hasher.update(if *value { b"b1" } else { b"b0" }),
        Value::Number(value) => {
            update_embedding_profile_field(hasher, b"number", value.to_string().as_bytes());
        }
        Value::String(value) => {
            update_embedding_profile_field(hasher, b"string", value.as_bytes());
        }
        Value::Array(values) => {
            hasher.update(b"[");
            hasher.update((values.len() as u64).to_le_bytes());
            for value in values {
                update_embedding_profile_json(hasher, value);
            }
            hasher.update(b"]");
        }
        Value::Object(values) => {
            hasher.update(b"{");
            hasher.update((values.len() as u64).to_le_bytes());
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for key in keys {
                update_embedding_profile_field(hasher, b"key", key.as_bytes());
                if let Some(value) = values.get(key) {
                    update_embedding_profile_json(hasher, value);
                }
            }
            hasher.update(b"}");
        }
    }
}
fn provider_runtime_profile_json(profile: &ProviderProfile) -> serde_json::Value {
    serde_json::json!({
        "runtime": profile.runtime,
        "baseUrl": profile.base_url,
        "credentials": profile.credentials,
        "requestPolicy": profile.request_policy,
    })
}

pub(super) fn merge_provider_runtime_profile(
    extra_parameters_json: serde_json::Value,
    profile: &ProviderProfile,
) -> serde_json::Value {
    let mut object =
        extra_parameters_json.as_object().cloned().unwrap_or_else(serde_json::Map::new);
    object.insert("_providerProfile".to_string(), provider_runtime_profile_json(profile));
    serde_json::Value::Object(object)
}

pub(super) fn merge_model_request_policy(
    extra_parameters_json: serde_json::Value,
    model_metadata_json: &serde_json::Value,
) -> serde_json::Value {
    let mut object =
        extra_parameters_json.as_object().cloned().unwrap_or_else(serde_json::Map::new);
    if !object.contains_key("_providerRequestPolicy")
        && let Some(policy) = model_metadata_json.get("requestPolicy")
    {
        object.insert("_providerRequestPolicy".to_string(), policy.clone());
    }
    serde_json::Value::Object(object)
}

/// The one canonical resolution of a binding's request context: the binding's
/// own JSON, the bound model's catalog metadata and the provider profile are
/// merged in a fixed order, then the exact pair reasoning capability is
/// projected from the model's catalog row into a reserved internal key.
///
/// The projection always overwrites that key, so a client binding can never
/// grant, override or inject reasoning authority through `_providerRequestPolicy`,
/// `_providerProfile` or the reserved key itself; the key is also
/// `_provider`-prefixed, so it is never forwarded upstream nor treated as an
/// embedding parameter.
pub(super) fn merge_binding_request_context(
    extra_parameters_json: serde_json::Value,
    model: &ModelCatalogEntry,
    provider: &ProviderProfile,
) -> serde_json::Value {
    let merged = merge_provider_runtime_profile(
        merge_model_request_policy(extra_parameters_json, &model.metadata_json),
        provider,
    );
    let mut object = merged.as_object().cloned().unwrap_or_default();
    let capability = model_reasoning_capability(&model.metadata_json).unwrap_or_default();
    object.insert(
        RESOLVED_REASONING_CAPABILITY_FIELD.to_string(),
        serde_json::json!({"modelName": model.model_name, "capability": capability}),
    );
    serde_json::Value::Object(object)
}
fn parse_embedding_dimensions(
    source: &serde_json::Value,
    field_name: &str,
) -> Result<Option<EmbeddingDimensions>, ApiError> {
    let Some(value) = source.get("dimensions") else {
        return Ok(None);
    };
    let dimensions = value.as_u64().ok_or_else(|| {
        ApiError::BadRequest(format!("{field_name}.dimensions must be a positive integer"))
    })?;
    EmbeddingDimensions::try_from(dimensions)
        .map(Some)
        .map_err(|error| ApiError::BadRequest(format!("{field_name}.dimensions {error}")))
}

pub(super) fn resolve_effective_embedding_dimensions(
    binding_purpose: AiBindingPurpose,
    binding_extra_parameters_json: &serde_json::Value,
    model_metadata_json: &serde_json::Value,
) -> Result<Option<EmbeddingDimensions>, ApiError> {
    if binding_purpose != AiBindingPurpose::EmbedChunk {
        return Ok(None);
    }
    if let Some(dimensions) =
        parse_embedding_dimensions(binding_extra_parameters_json, "extraParametersJson")?
    {
        return Ok(Some(dimensions));
    }
    parse_embedding_dimensions(model_metadata_json, "model.metadataJson")
}
