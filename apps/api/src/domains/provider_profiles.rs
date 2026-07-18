use serde::{Deserialize, Serialize};

pub(crate) const OPENAI_COMPATIBLE_RUNTIME_KIND: &str = "openai_compatible";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthScheme {
    Bearer,
    RawAuthorization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTokenLimitParameter {
    MaxCompletionTokens,
    MaxTokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStructuredOutputMode {
    JsonSchema,
    JsonObject,
    PromptOnlyJsonObject,
    Unsupported,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSamplingPolicy {
    #[default]
    Forward,
    Omit,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderToolChoicePolicy {
    #[default]
    RequiredCapable,
    AutoOnly,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderRequestPolicy {
    #[serde(default)]
    pub sampling: ProviderSamplingPolicy,
    #[serde(default)]
    pub tool_choice: ProviderToolChoicePolicy,
    #[serde(default)]
    pub default_tool_max_output_tokens: Option<i32>,
}

impl ProviderRequestPolicy {
    #[must_use]
    pub const fn is_valid(self) -> bool {
        match self.default_tool_max_output_tokens {
            Some(value) => value > 0,
            None => true,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsagePolicy {
    /// Select token accounting only from formal usage-payload fields.
    #[default]
    AutoDetect,
    /// Cached input is reported as a subset of total request input.
    CachedSubsetOfInput,
    /// Cache-read and cache-write counters are disjoint from ordinary input.
    DisjointCacheCounters,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRuntimeProfile {
    pub kind: String,
    pub auth_scheme: ProviderAuthScheme,
    pub token_limit_parameter: ProviderTokenLimitParameter,
    pub structured_output: ProviderStructuredOutputMode,
    pub chat_path: String,
    pub embeddings_path: Option<String>,
    pub models_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderBaseUrlMode {
    Fixed,
    Required,
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCredentialPolicy {
    pub api_key_required: bool,
    pub base_url_required: bool,
    pub base_url_mode: ProviderBaseUrlMode,
    pub validation_mode: ProviderCredentialValidationMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCredentialValidationMode {
    ChatRoundTrip,
    ModelList,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderBaseUrlPolicy {
    pub allow_override: bool,
    pub require_https: bool,
    pub allow_private_network: bool,
    #[serde(default)]
    pub trim_suffixes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderModelDiscoveryMode {
    Shared,
    Credential,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelDiscoveryPath {
    pub capability_kind: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelDiscovery {
    pub mode: ProviderModelDiscoveryMode,
    pub paths: Vec<ProviderModelDiscoveryPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCapabilityState {
    Supported,
    Unsupported,
    Unknown,
}

impl ProviderCapabilityState {
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub chat: ProviderCapabilityState,
    pub embeddings: ProviderCapabilityState,
    pub vision: ProviderCapabilityState,
    pub streaming: ProviderCapabilityState,
    pub tools: ProviderCapabilityState,
    pub model_discovery: ProviderCapabilityState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub runtime: ProviderRuntimeProfile,
    pub credentials: ProviderCredentialPolicy,
    pub base_url: ProviderBaseUrlPolicy,
    pub model_discovery: ProviderModelDiscovery,
    pub capabilities: ProviderCapabilities,
    #[serde(default)]
    pub request_policy: ProviderRequestPolicy,
    #[serde(default)]
    pub usage_policy: ProviderUsagePolicy,
    #[serde(default)]
    pub ui_hints: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProviderModelSelection {
    pub provider_kind: String,
    pub model_name: String,
}
