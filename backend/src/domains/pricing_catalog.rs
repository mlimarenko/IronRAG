use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingCapability {
    Indexing,
    Embedding,
    Answer,
    Vision,
    GraphExtract,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingBillingUnit {
    Per1MInputTokens,
    Per1MOutputTokens,
    Per1MTokens,
    FixedPerCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingCatalogStatus {
    Active,
    Superseded,
    Inactive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingSourceKind {
    Manual,
    Seeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingResolutionStatus {
    Priced,
    Unpriced,
    UsageMissing,
    PricingMissing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricingCatalogEntry {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: PricingCapability,
    pub billing_unit: PricingBillingUnit,
    pub input_price: Option<Decimal>,
    pub output_price: Option<Decimal>,
    pub currency: String,
    pub status: PricingCatalogStatus,
    pub source_kind: PricingSourceKind,
    pub note: Option<String>,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingResolution {
    pub status: PricingResolutionStatus,
    pub entry_id: Option<Uuid>,
    pub pricing_snapshot_json: serde_json::Value,
}
