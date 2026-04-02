use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AiBindingPurpose {
    ExtractText,
    ExtractGraph,
    EmbedChunk,
    QueryRetrieve,
    QueryAnswer,
    Vision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCatalogEntry {
    pub id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalogEntry {
    pub id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub capability_kind: String,
    pub modality_kind: String,
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceCatalogEntry {
    pub id: Uuid,
    pub model_catalog_id: Uuid,
    pub billing_unit: String,
    pub price_variant_key: String,
    pub request_input_tokens_min: Option<i32>,
    pub request_input_tokens_max: Option<i32>,
    pub unit_price: Decimal,
    pub currency_code: String,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub catalog_scope: String,
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryModelBinding {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_purpose: AiBindingPurpose,
    pub provider_credential_id: Uuid,
    pub model_preset_id: Uuid,
    pub binding_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCredential {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub api_key: String,
    pub credential_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPreset {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub model_catalog_id: Uuid,
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingValidation {
    pub id: Uuid,
    pub binding_id: Uuid,
    pub validation_state: String,
    pub checked_at: DateTime<Utc>,
    pub failure_code: Option<String>,
    pub message: Option<String>,
}
