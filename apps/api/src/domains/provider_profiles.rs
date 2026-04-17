use serde::{Deserialize, Serialize};

use crate::domains::{agent_runtime::RuntimeTaskKind, ai::AiBindingPurpose};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportedProviderKind {
    #[default]
    OpenAi,
    DeepSeek,
    Qwen,
    // Self-hosted OpenAI-compatible runtime. Local stacks (Ollama,
    // llama.cpp, vLLM, LM Studio, OpenWebUI) all serve the same chat
    // completions shape, so once the enum accepts the name the existing
    // `UnifiedGateway` path at integrations/llm.rs handles the transport.
    Ollama,
}

impl SupportedProviderKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::DeepSeek => "deepseek",
            Self::Qwen => "qwen",
            Self::Ollama => "ollama",
        }
    }
}

impl std::str::FromStr for SupportedProviderKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai" => Ok(Self::OpenAi),
            "deepseek" => Ok(Self::DeepSeek),
            "qwen" => Ok(Self::Qwen),
            "ollama" => Ok(Self::Ollama),
            other => Err(format!("unsupported provider kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModelSelection {
    pub provider_kind: SupportedProviderKind,
    pub model_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveProviderProfile {
    pub indexing: ProviderModelSelection,
    pub embedding: ProviderModelSelection,
    pub query_compile: ProviderModelSelection,
    pub answer: ProviderModelSelection,
    pub vision: ProviderModelSelection,
}

impl EffectiveProviderProfile {
    #[must_use]
    pub const fn selection_for_binding_purpose(
        &self,
        binding_purpose: AiBindingPurpose,
    ) -> &ProviderModelSelection {
        match binding_purpose {
            AiBindingPurpose::ExtractText | AiBindingPurpose::ExtractGraph => &self.indexing,
            AiBindingPurpose::EmbedChunk | AiBindingPurpose::QueryRetrieve => &self.embedding,
            AiBindingPurpose::QueryCompile => &self.query_compile,
            AiBindingPurpose::QueryAnswer => &self.answer,
            AiBindingPurpose::Vision => &self.vision,
        }
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn selection_for_runtime_task_kind(
        &self,
        task_kind: RuntimeTaskKind,
    ) -> Option<&ProviderModelSelection> {
        AiBindingPurpose::for_runtime_task_kind(task_kind)
            .map(|binding_purpose| self.selection_for_binding_purpose(binding_purpose))
    }
}
