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
pub enum PricingResolutionStatus {
    Priced,
    Unpriced,
    UsageMissing,
    PricingMissing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StageAttributionSource {
    StageNative,
    Reconciled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStageOwnership {
    pub ingestion_run_id: Uuid,
    pub stage_event_id: Uuid,
    pub stage: String,
    pub attribution_source: StageAttributionSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStageBillingPolicy {
    Billable { capability: PricingCapability, billing_unit: PricingBillingUnit },
    NonBillable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BillingExecutionOwnerKind {
    QueryExecution,
    IngestAttempt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingProviderCall {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_id: Option<Uuid>,
    pub owning_execution_kind: BillingExecutionOwnerKind,
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
    pub owning_execution_kind: BillingExecutionOwnerKind,
    pub owning_execution_id: Uuid,
    pub total_cost: Decimal,
    pub currency_code: String,
    pub provider_call_count: i32,
    pub updated_at: DateTime<Utc>,
}

#[must_use]
pub fn runtime_stage_billing_policy(stage: &str) -> RuntimeStageBillingPolicy {
    match stage {
        "extracting_content" => RuntimeStageBillingPolicy::Billable {
            capability: PricingCapability::Vision,
            billing_unit: PricingBillingUnit::Per1MTokens,
        },
        "embedding_chunks" => RuntimeStageBillingPolicy::Billable {
            capability: PricingCapability::Embedding,
            billing_unit: PricingBillingUnit::Per1MInputTokens,
        },
        "extracting_graph" => RuntimeStageBillingPolicy::Billable {
            capability: PricingCapability::GraphExtract,
            billing_unit: PricingBillingUnit::Per1MTokens,
        },
        _ => RuntimeStageBillingPolicy::NonBillable,
    }
}

#[must_use]
pub fn stage_native_ownership(
    ingestion_run_id: Uuid,
    stage_event_id: Uuid,
    stage: &str,
) -> UsageStageOwnership {
    UsageStageOwnership {
        ingestion_run_id,
        stage_event_id,
        stage: stage.to_string(),
        attribution_source: StageAttributionSource::StageNative,
    }
}

#[must_use]
pub fn decorate_payload_with_stage_ownership(
    mut payload: serde_json::Value,
    ownership: &UsageStageOwnership,
) -> serde_json::Value {
    let ownership_json = serde_json::to_value(ownership).unwrap_or_else(|_| serde_json::json!({}));
    match payload.as_object_mut() {
        Some(object) => {
            object.insert("stage_ownership".to_string(), ownership_json);
            payload
        }
        None => serde_json::json!({
            "value": payload,
            "stage_ownership": ownership,
        }),
    }
}
