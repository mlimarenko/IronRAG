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
    pub purpose_gaps: Vec<crate::domains::provider_profiles::ProviderPurposeGap>,
    pub profile: ProviderProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ModelCatalogEntry {
    pub id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub lifecycle_state: String,
    pub metadata_json: serde_json::Value,
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub pricing: ModelPricing,
}

/// One model a provider reports as reachable with a given account, read from
/// the fields the provider itself publishes on its model-listing endpoint. A
/// router's catalogue changes weekly, so the seeded catalog cannot hold it and
/// this is the only way an operator can see what their plan actually exposes.
///
/// Every field below is copied from a declared provider field or left absent —
/// nothing here is inferred from the model's name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredProviderModel {
    pub model_name: String,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    /// What the provider says the model accepts (`text`, `image`, ...). Empty
    /// means the provider did not say, which is not the same as text-only.
    pub input_modalities: Vec<String>,
    /// Present only when the provider published rates. Absent means unknown,
    /// and a caller that needs a price must say so rather than record zero.
    pub pricing: Option<ModelPricing>,
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

/// Vendor rates carried by every model row, in the model's currency per one
/// million tokens. Zero is an honest price (local models, embedding output);
/// an absent price is unrepresentable, which is the point: the separate
/// price-catalog entity rotted apart from the models actually in use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelPricing {
    pub currency_code: String,
    pub input_price_per_1m: Decimal,
    pub cached_input_price_per_1m: Decimal,
    pub cache_write_input_price_per_1m: Decimal,
    pub output_price_per_1m: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long_context: Option<ModelLongContextPricing>,
}

/// Rates applied when a request's input exceeds the threshold. All fields are
/// present together or the tier does not exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelLongContextPricing {
    pub threshold_tokens: i32,
    pub input_price_per_1m: Decimal,
    pub cached_input_price_per_1m: Decimal,
    pub output_price_per_1m: Decimal,
}

impl ModelPricing {
    /// # Errors
    /// Returns a message naming the first invalid field: negative rate,
    /// non-positive threshold, or an unsupported currency code shape.
    pub fn validate(&self) -> Result<(), String> {
        let currency = self.currency_code.trim();
        if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_uppercase()) {
            return Err("currencyCode must be a three-letter uppercase ISO code".to_string());
        }
        for (name, value) in [
            ("inputPricePer1m", self.input_price_per_1m),
            ("cachedInputPricePer1m", self.cached_input_price_per_1m),
            ("cacheWriteInputPricePer1m", self.cache_write_input_price_per_1m),
            ("outputPricePer1m", self.output_price_per_1m),
        ] {
            if value.is_sign_negative() {
                return Err(format!("{name} must not be negative"));
            }
        }
        if let Some(tier) = &self.long_context {
            if tier.threshold_tokens <= 0 {
                return Err("longContext.thresholdTokens must be positive".to_string());
            }
            for (name, value) in [
                ("longContext.inputPricePer1m", tier.input_price_per_1m),
                ("longContext.cachedInputPricePer1m", tier.cached_input_price_per_1m),
                ("longContext.outputPricePer1m", tier.output_price_per_1m),
            ] {
                if value.is_sign_negative() {
                    return Err(format!("{name} must not be negative"));
                }
            }
        }
        Ok(())
    }

    /// Rate for one billing unit, honoring the long-context tier when the
    /// request's input size crosses the threshold. Unknown units price at
    /// zero rather than failing billing persistence.
    #[must_use]
    pub fn unit_price(&self, billing_unit: &str, request_input_tokens: Option<i32>) -> Decimal {
        let long_context = self.long_context.as_ref().filter(|tier| {
            request_input_tokens.is_some_and(|tokens| tokens > tier.threshold_tokens)
        });
        match billing_unit {
            "per_1m_input_tokens" => {
                long_context.map_or(self.input_price_per_1m, |tier| tier.input_price_per_1m)
            }
            "per_1m_cached_input_tokens" => long_context
                .map_or(self.cached_input_price_per_1m, |tier| tier.cached_input_price_per_1m),
            "per_1m_cache_write_input_tokens" => self.cache_write_input_price_per_1m,
            "per_1m_output_tokens" => {
                long_context.map_or(self.output_price_per_1m, |tier| tier.output_price_per_1m)
            }
            _ => Decimal::ZERO,
        }
    }
}

#[macro_export]
macro_rules! ai_binding_type {
    ($(#[$attribute:meta])* $visibility:vis struct $name:ident) => {
        $(#[$attribute])*
        $visibility struct $name {
            pub id: uuid::Uuid,
            pub scope_kind: $crate::domains::ai::AiScopeKind,
            pub workspace_id: Option<uuid::Uuid>,
            pub library_id: Option<uuid::Uuid>,
            pub binding_purpose: $crate::domains::ai::AiBindingPurpose,
            pub account_id: uuid::Uuid,
            pub provider_catalog_id: uuid::Uuid,
            pub model_catalog_id: uuid::Uuid,
            pub system_prompt: Option<String>,
            pub temperature: Option<f64>,
            pub top_p: Option<f64>,
            pub max_output_tokens_override: Option<i32>,
            pub extra_parameters_json: serde_json::Value,
            pub binding_state: String,
        }
    };
}

crate::ai_binding_type! {
    #[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
    pub struct AiBinding
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
