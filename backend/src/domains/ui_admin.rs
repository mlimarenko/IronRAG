use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
pub struct AdminTabCounts {
    pub api_tokens: usize,
    pub members: usize,
    pub library_access: usize,
    pub settings: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminTabAvailability {
    pub api_tokens: bool,
    pub members: bool,
    pub library_access: bool,
    pub settings: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminOverviewModel {
    pub active_tab: String,
    pub workspace_name: String,
    pub counts: AdminTabCounts,
    pub availability: AdminTabAvailability,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiTokenRowModel {
    pub id: String,
    pub label: String,
    pub masked_token: String,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub can_revoke: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateApiTokenResultModel {
    pub row: ApiTokenRowModel,
    pub plaintext_token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminMemberModel {
    pub id: String,
    pub display_name: String,
    pub email: String,
    pub role_label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryAccessRowModel {
    pub id: String,
    pub library_name: String,
    pub principal_label: String,
    pub access_level: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminSettingItemModel {
    pub id: String,
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminPricingCatalogEntryModel {
    pub id: String,
    pub workspace_id: Option<String>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub input_price: Option<String>,
    pub output_price: Option<String>,
    pub currency: String,
    pub status: String,
    pub source_kind: String,
    pub note: Option<String>,
    pub effective_from: String,
    pub effective_to: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminSupportedProviderModel {
    pub provider_kind: String,
    pub supported_capabilities: Vec<String>,
    pub default_models: BTreeMap<String, String>,
    pub available_models: BTreeMap<String, Vec<String>>,
    pub is_configured: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminProviderProfileModel {
    pub library_id: String,
    pub library_name: String,
    pub indexing_provider_kind: String,
    pub indexing_model_name: String,
    pub embedding_provider_kind: String,
    pub embedding_model_name: String,
    pub answer_provider_kind: String,
    pub answer_model_name: String,
    pub vision_provider_kind: String,
    pub vision_model_name: String,
    pub last_validated_at: Option<String>,
    pub last_validation_status: Option<String>,
    pub last_validation_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminProviderValidationCheckModel {
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub status: String,
    pub checked_at: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminProviderValidationModel {
    pub status: Option<String>,
    pub checked_at: Option<String>,
    pub error: Option<String>,
    pub checks: Vec<AdminProviderValidationCheckModel>,
}
