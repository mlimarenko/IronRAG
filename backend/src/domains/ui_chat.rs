use serde::Serialize;

use crate::domains::query_intelligence::{
    ContextAssemblyMetadata, QueryPlanningMetadata, RerankMetadata,
};

#[derive(Debug, Clone, Serialize)]
pub struct ChatSessionSummaryModel {
    pub session_id: String,
    pub title: String,
    pub message_count: i64,
    pub last_message_preview: Option<String>,
    pub updated_at: String,
    pub prompt_state: String,
    pub preferred_mode: String,
    pub is_empty: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatSessionDetailModel {
    pub session_id: String,
    pub title: String,
    pub message_count: i64,
    pub last_message_preview: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub prompt_state: String,
    pub preferred_mode: String,
    pub is_empty: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatSessionSettingsModel {
    pub session_id: String,
    pub system_prompt: String,
    pub prompt_state: String,
    pub preferred_mode: String,
    pub default_prompt_available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatThreadProviderModel {
    pub provider_kind: String,
    pub model_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatThreadReferenceModel {
    pub kind: String,
    pub reference_id: String,
    pub excerpt: Option<String>,
    pub rank: usize,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatThreadMessageModel {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
    pub query_id: Option<String>,
    pub mode: Option<String>,
    pub grounding_status: Option<String>,
    pub provider: Option<ChatThreadProviderModel>,
    pub references: Vec<ChatThreadReferenceModel>,
    pub planning: Option<QueryPlanningMetadata>,
    pub rerank: Option<RerankMetadata>,
    pub context_assembly: Option<ContextAssemblyMetadata>,
    pub warning: Option<String>,
    pub warning_kind: Option<String>,
}
