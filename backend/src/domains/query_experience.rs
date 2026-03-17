use serde::{Deserialize, Serialize};

use crate::domains::query_modes::RuntimeQueryMode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryModeDescriptor {
    pub mode: RuntimeQueryMode,
    pub label_key: String,
    pub short_description_key: String,
    pub best_for_key: String,
    pub caution_key: Option<String>,
    pub example_question_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantExperienceConfig {
    pub scope_hint_key: String,
    pub default_prompt_keys: Vec<String>,
    pub modes: Vec<QueryModeDescriptor>,
}
