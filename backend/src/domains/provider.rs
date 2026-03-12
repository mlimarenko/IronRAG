use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum ProviderKind {
    OpenAi,
    DeepSeek,
    OpenAiCompatible,
}

#[derive(Debug, Clone)]
pub struct ProviderAccount {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider_kind: ProviderKind,
    pub label: String,
    pub api_base_url: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ModelProfile {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider_account_id: Uuid,
    pub profile_kind: String,
    pub model_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
