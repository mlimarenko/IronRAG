use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::query_modes::RuntimeQueryMode;

#[derive(Debug, Clone)]
pub struct RetrievalRun {
    pub id: Uuid,
    pub project_id: Uuid,
    pub query_text: String,
    pub response_text: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatPromptState {
    Default,
    Customized,
}

impl ChatPromptState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Customized => "customized",
        }
    }
}

impl std::str::FromStr for ChatPromptState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "default" => Ok(Self::Default),
            "customized" => Ok(Self::Customized),
            other => Err(format!("unsupported chat prompt state: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatSession {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub system_prompt: String,
    pub prompt_state: ChatPromptState,
    pub preferred_mode: RuntimeQueryMode,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
