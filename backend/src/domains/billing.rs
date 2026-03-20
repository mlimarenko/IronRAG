use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingProviderCall {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_id: Option<Uuid>,
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_catalog_id: Uuid,
    pub call_kind: String,
    pub call_state: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingUsage {
    pub id: Uuid,
    pub provider_call_id: Uuid,
    pub usage_kind: String,
    pub billing_unit: String,
    pub quantity: Decimal,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingCharge {
    pub id: Uuid,
    pub usage_id: Uuid,
    pub price_catalog_id: Uuid,
    pub currency_code: String,
    pub unit_price: Decimal,
    pub total_price: Decimal,
    pub priced_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingExecutionCost {
    pub id: Uuid,
    pub owning_execution_kind: String,
    pub owning_execution_id: Uuid,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub provider_call_count: i32,
    pub updated_at: DateTime<Utc>,
}
