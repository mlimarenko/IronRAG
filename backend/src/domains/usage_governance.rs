use uuid::Uuid;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::domains::pricing_catalog::{PricingBillingUnit, PricingCapability};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageGovernanceSummary {
    pub usage_events: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub estimated_cost: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingCoverageStatus {
    Covered,
    Partial,
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingCoverageWarning {
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingCoverageSummary {
    pub status: PricingCoverageStatus,
    pub covered_targets: usize,
    pub missing_targets: usize,
    pub warnings: Vec<PricingCoverageWarning>,
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
