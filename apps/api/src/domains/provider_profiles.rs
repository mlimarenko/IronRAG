use serde::{Deserialize, Serialize};

pub const OPENAI_COMPATIBLE_RUNTIME_KIND: &str = "openai_compatible";

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub runtime: ProviderRuntimeProfile,
    pub credentials: ProviderCredentialPolicy,
    pub base_url: ProviderBaseUrlPolicy,
    pub model_discovery: ProviderModelDiscovery,
    pub capabilities: ProviderCapabilities,
    #[serde(default)]
    pub ui_hints: serde_json::Value,
}

use crate::domains::ai::AiBindingPurpose;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProviderModelSelection {
    pub provider_kind: String,
    pub model_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EffectiveProviderProfile {
    pub indexing: ProviderModelSelection,
    pub embedding: ProviderModelSelection,
    pub query_retrieve: ProviderModelSelection,
    pub query_compile: ProviderModelSelection,
    pub answer: ProviderModelSelection,
    /// Optional: vision binding is only exercised for multimodal
    /// ingest paths (PDFs with embedded images, screenshots). Text-only
    /// libraries and local-Ollama setups without a vision-capable
    /// model must stay operational.
    pub vision: Option<ProviderModelSelection>,
}

impl EffectiveProviderProfile {
    #[must_use]
    pub const fn selection_for_binding_purpose(
        &self,
        binding_purpose: AiBindingPurpose,
    ) -> Option<&ProviderModelSelection> {
        match binding_purpose {
            AiBindingPurpose::ExtractText | AiBindingPurpose::ExtractGraph => Some(&self.indexing),
            AiBindingPurpose::EmbedChunk => Some(&self.embedding),
            AiBindingPurpose::QueryRetrieve => Some(&self.query_retrieve),
            AiBindingPurpose::QueryCompile => Some(&self.query_compile),
            AiBindingPurpose::QueryAnswer | AiBindingPurpose::Agent => Some(&self.answer),
            AiBindingPurpose::Vision => self.vision.as_ref(),
        }
    }
}
