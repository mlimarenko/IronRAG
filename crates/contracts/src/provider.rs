use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCredentialValidationMode {
    ChatRoundTrip,
    ModelList,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCredentialPolicy {
    pub api_key_required: bool,
    pub base_url_required: bool,
    pub base_url_mode: ProviderBaseUrlMode,
    pub validation_mode: ProviderCredentialValidationMode,
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
