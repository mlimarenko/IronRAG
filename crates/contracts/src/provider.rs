//! AI provider capability and credential-policy contracts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// HTTP authorization encoding expected by a provider endpoint.
pub enum ProviderAuthScheme {
    /// Prefix the credential with the standard bearer scheme.
    Bearer,
    /// Send the configured credential as the complete authorization value.
    RawAuthorization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Request field used by a provider to limit generated tokens.
pub enum ProviderTokenLimitParameter {
    /// Use the completion-specific token-limit field.
    MaxCompletionTokens,
    /// Use the general maximum-token field.
    MaxTokens,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Strongest structured-output contract supported by an endpoint.
pub enum ProviderStructuredOutputMode {
    /// The endpoint accepts and enforces a supplied JSON Schema.
    JsonSchema,
    /// The endpoint guarantees a JSON object without schema enforcement.
    JsonObject,
    /// JSON must be requested in the prompt and is not transport-enforced.
    PromptOnlyJsonObject,
    /// No structured-output mechanism is available.
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Protocol details needed to construct requests for a provider kind.
pub struct ProviderRuntimeProfile {
    /// Stable provider-kind identifier.
    pub kind: String,
    /// Authorization-header encoding required by the provider.
    pub auth_scheme: ProviderAuthScheme,
    /// Request field used to express an output-token limit.
    pub token_limit_parameter: ProviderTokenLimitParameter,
    /// Structured-output mechanism available to callers.
    pub structured_output: ProviderStructuredOutputMode,
    /// Provider-relative path for chat or response generation.
    pub chat_path: String,
    /// Provider-relative embedding path when embeddings are supported.
    pub embeddings_path: Option<String>,
    /// Provider-relative model-list path when discovery is supported.
    pub models_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Whether and how an operator supplies a provider base URL.
pub enum ProviderBaseUrlMode {
    /// The catalog supplies a fixed URL that cannot be replaced.
    Fixed,
    /// A URL must be supplied with the credential configuration.
    Required,
    /// An operator override is optional.
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Network operation used to validate provider credentials.
pub enum ProviderCredentialValidationMode {
    /// Perform a minimal generation request.
    ChatRoundTrip,
    /// Request the provider's model catalog.
    ModelList,
    /// Store the credential without a remote validation request.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Input and validation requirements for provider credentials.
pub struct ProviderCredentialPolicy {
    /// Whether saving the provider requires an API key.
    pub api_key_required: bool,
    /// Whether saving the provider requires an explicit base URL.
    pub base_url_required: bool,
    /// Catalog policy governing base-URL input.
    pub base_url_mode: ProviderBaseUrlMode,
    /// Remote check performed before accepting the credential.
    pub validation_mode: ProviderCredentialValidationMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Security and normalization policy for provider endpoint URLs.
pub struct ProviderBaseUrlPolicy {
    /// Whether an operator may replace the catalog default.
    pub allow_override: bool,
    /// Whether non-TLS endpoint URLs must be rejected.
    pub require_https: bool,
    /// Whether resolved private-network destinations are permitted.
    pub allow_private_network: bool,
    #[serde(default)]
    /// Known endpoint suffixes removed when normalizing a supplied base URL.
    pub trim_suffixes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Credential context required to discover provider models.
pub enum ProviderModelDiscoveryMode {
    /// Discovery uses a provider-wide catalog without stored credentials.
    Shared,
    /// Discovery requires the configured provider credential.
    Credential,
    /// The provider exposes no supported discovery operation.
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Model-discovery endpoint for one provider capability family.
pub struct ProviderModelDiscoveryPath {
    /// Capability family returned by this endpoint.
    pub capability_kind: String,
    /// Provider-relative path used for discovery.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Supported strategy and endpoints for enumerating provider models.
pub struct ProviderModelDiscovery {
    /// Credential context required for discovery.
    pub mode: ProviderModelDiscoveryMode,
    /// Discovery endpoints grouped by capability family.
    pub paths: Vec<ProviderModelDiscoveryPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Catalog confidence that a provider exposes a capability.
pub enum ProviderCapabilityState {
    /// The capability is explicitly supported.
    Supported,
    /// The capability is explicitly unavailable.
    Unsupported,
    /// The catalog cannot make a reliable determination.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Capability matrix declared for a provider kind.
pub struct ProviderCapabilities {
    /// Text-generation availability.
    pub chat: ProviderCapabilityState,
    /// Vector-embedding availability.
    pub embeddings: ProviderCapabilityState,
    /// Image-input availability.
    pub vision: ProviderCapabilityState,
    /// Incremental response-streaming availability.
    pub streaming: ProviderCapabilityState,
    /// Structured tool-calling availability.
    pub tools: ProviderCapabilityState,
    /// Remote model-discovery availability.
    pub model_discovery: ProviderCapabilityState,
}
