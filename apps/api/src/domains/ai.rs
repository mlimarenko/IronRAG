use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroize as _;

pub use ironrag_contracts::ai::AiBindingPurpose;

use crate::domains::provider_profiles::{
    ProviderBaseUrlPolicy, ProviderCapabilities, ProviderCredentialPolicy, ProviderModelDiscovery,
    ProviderProfile, ProviderRuntimeProfile,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiScopeKind {
    Instance,
    Workspace,
    Library,
}

impl AiScopeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Instance => "instance",
            Self::Workspace => "workspace",
            Self::Library => "library",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProviderCatalogEntry {
    pub id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub default_base_url: Option<String>,
    pub capability_flags_json: serde_json::Value,
    pub api_key_required: bool,
    pub base_url_required: bool,
    pub credential_policy: ProviderCredentialPolicy,
    pub base_url_policy: ProviderBaseUrlPolicy,
    pub model_discovery: ProviderModelDiscovery,
    pub capabilities: ProviderCapabilities,
    pub runtime: ProviderRuntimeProfile,
    pub ui_hints: serde_json::Value,
    pub profile: ProviderProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ModelCatalogEntry {
    pub id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub capability_kind: String,
    pub modality_kind: String,
    pub lifecycle_state: String,
    pub metadata_json: serde_json::Value,
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailabilityState {
    Available,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ResolvedModelCatalogEntry {
    pub model: ModelCatalogEntry,
    pub availability_state: ModelAvailabilityState,
    pub available_account_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AiBinding {
    pub id: Uuid,
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub binding_purpose: AiBindingPurpose,
    pub account_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
    pub binding_state: String,
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AiAccount {
    pub id: Uuid,
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub label: String,
    #[serde(skip)]
    #[schema(ignore)]
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub credential_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Drop for AiAccount {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for AiAccount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AiAccount")
            .field("id", &self.id)
            .field("scope_kind", &self.scope_kind)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("provider_catalog_id", &self.provider_catalog_id)
            .field("label", &self.label)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .field("credential_state", &self.credential_state)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BindingValidation {
    pub id: Uuid,
    pub binding_id: Uuid,
    pub validation_state: String,
    pub checked_at: DateTime<Utc>,
    pub failure_code: Option<String>,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::{AiAccount, AiBindingPurpose, AiScopeKind};

    #[test]
    fn removed_binding_purposes_do_not_parse() {
        for removed in ["query_retrieve", "rerank", "vision", "utility"] {
            assert!(removed.parse::<AiBindingPurpose>().is_err());
            assert!(serde_json::from_str::<AiBindingPurpose>(&format!(r#""{removed}""#)).is_err());
        }
    }

    #[test]
    fn ai_account_serialization_never_exposes_api_key() {
        let account = AiAccount {
            id: Uuid::now_v7(),
            scope_kind: AiScopeKind::Instance,
            workspace_id: None,
            library_id: None,
            provider_catalog_id: Uuid::now_v7(),
            label: "synthetic".to_string(),
            api_key: Some("serialization-regression-secret".to_string()),
            base_url: None,
            credential_state: "active".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let serialized = serde_json::to_value(account).expect("account should serialize");

        assert!(serialized.get("api_key").is_none());
        assert!(!serialized.to_string().contains("serialization-regression-secret"));
        let round_trip: AiAccount =
            serde_json::from_value(serialized).expect("safe account form should deserialize");
        assert!(round_trip.api_key.is_none());
    }
}
