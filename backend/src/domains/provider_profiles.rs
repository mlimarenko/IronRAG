use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportedProviderKind {
    OpenAi,
    DeepSeek,
    Qwen,
}

impl SupportedProviderKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::DeepSeek => "deepseek",
            Self::Qwen => "qwen",
        }
    }
}

impl Default for SupportedProviderKind {
    fn default() -> Self {
        Self::OpenAi
    }
}

impl std::str::FromStr for SupportedProviderKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai" => Ok(Self::OpenAi),
            "deepseek" => Ok(Self::DeepSeek),
            "qwen" => Ok(Self::Qwen),
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
    pub answer: ProviderModelSelection,
    pub vision: ProviderModelSelection,
}
