use serde::{Deserialize, Serialize};

use crate::app::config::Settings;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeProviderProfileDefaults {
    pub indexing: ProviderModelSelection,
    pub embedding: ProviderModelSelection,
    pub answer: ProviderModelSelection,
    pub vision: ProviderModelSelection,
}

impl RuntimeProviderProfileDefaults {
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            indexing: ProviderModelSelection {
                provider_kind: settings
                    .runtime_default_indexing_provider
                    .parse()
                    .unwrap_or_default(),
                model_name: settings.runtime_default_indexing_model.clone(),
            },
            embedding: ProviderModelSelection {
                provider_kind: settings
                    .runtime_default_embedding_provider
                    .parse()
                    .unwrap_or_default(),
                model_name: settings.runtime_default_embedding_model.clone(),
            },
            answer: ProviderModelSelection {
                provider_kind: settings.runtime_default_answer_provider.parse().unwrap_or_default(),
                model_name: settings.runtime_default_answer_model.clone(),
            },
            vision: ProviderModelSelection {
                provider_kind: settings.runtime_default_vision_provider.parse().unwrap_or_default(),
                model_name: settings.runtime_default_vision_model.clone(),
            },
        }
    }

    #[must_use]
    pub fn effective_profile(&self) -> EffectiveProviderProfile {
        EffectiveProviderProfile {
            indexing: self.indexing.clone(),
            embedding: self.embedding.clone(),
            answer: self.answer.clone(),
            vision: self.vision.clone(),
        }
    }
}
