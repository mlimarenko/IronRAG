use anyhow::Context;
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        pricing_catalog::{PricingBillingUnit, PricingCapability, PricingResolutionStatus},
        usage_governance::{
            RuntimeStageBillingPolicy, decorate_payload_with_stage_ownership,
            runtime_stage_billing_policy, stage_native_ownership,
        },
    },
    infra::repositories::{
        self, AttemptStageAccountingRow, AttemptStageCostSummaryRow, CostLedgerRow, UsageEventRow,
    },
    services::pricing_catalog,
};

#[derive(Debug, Clone, Default)]
pub struct DocumentAccountingService;

#[derive(Debug, Clone)]
pub enum StageAccountingScope {
    StageRollup,
    ProviderCall { call_sequence_no: i32 },
}

impl StageAccountingScope {
    fn scope_label(&self) -> &'static str {
        match self {
            Self::StageRollup => "stage_rollup",
            Self::ProviderCall { .. } => "provider_call",
        }
    }

    fn call_sequence_no(&self) -> i32 {
        match self {
            Self::StageRollup => 0,
            Self::ProviderCall { call_sequence_no } => *call_sequence_no,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordStageAccountingRequest {
    pub ingestion_run_id: Uuid,
    pub stage_event_id: Uuid,
    pub stage: String,
    pub accounting_scope: StageAccountingScope,
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub capability: PricingCapability,
    pub billing_unit: PricingBillingUnit,
    pub usage_event_id: Option<Uuid>,
    pub cost_ledger_id: Option<Uuid>,
    pub pricing_catalog_entry_id: Option<Uuid>,
    pub pricing_status: PricingResolutionStatus,
    pub estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub token_usage_json: serde_json::Value,
    pub pricing_snapshot_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct StageUsageAccountingRequest {
    pub ingestion_run_id: Uuid,
    pub stage_event_id: Uuid,
    pub stage: String,
    pub accounting_scope: StageAccountingScope,
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub model_profile_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: PricingCapability,
    pub billing_unit: PricingBillingUnit,
    pub usage_kind: String,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub raw_usage_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct StageUsageAccountingResult {
    pub usage_event: UsageEventRow,
    pub cost_ledger: Option<CostLedgerRow>,
    pub stage_accounting: AttemptStageAccountingRow,
    pub attempt_summary: AttemptStageCostSummaryRow,
}

#[derive(Debug, Clone)]
pub struct StageAccountingGapRequest {
    pub ingestion_run_id: Uuid,
    pub stage_event_id: Uuid,
    pub stage: String,
    pub accounting_scope: StageAccountingScope,
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub capability: PricingCapability,
    pub billing_unit: PricingBillingUnit,
    pub pricing_status: PricingResolutionStatus,
    pub token_usage_json: serde_json::Value,
    pub pricing_snapshot_json: serde_json::Value,
}

pub async fn record_stage_accounting(
    state: &AppState,
    request: RecordStageAccountingRequest,
) -> anyhow::Result<AttemptStageAccountingRow> {
    validate_stage_accounting_request(
        &request.stage,
        &request.capability,
        &request.billing_unit,
        &request.pricing_status,
        request.estimated_cost.is_some(),
        request.cost_ledger_id.is_some(),
        request.pricing_catalog_entry_id.is_some(),
    )?;
    repositories::create_attempt_stage_accounting(
        &state.persistence.postgres,
        &repositories::NewAttemptStageAccounting {
            ingestion_run_id: request.ingestion_run_id,
            stage_event_id: request.stage_event_id,
            stage: request.stage,
            accounting_scope: request.accounting_scope.scope_label().to_string(),
            call_sequence_no: request.accounting_scope.call_sequence_no(),
            workspace_id: request.workspace_id,
            project_id: request.project_id,
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            capability: request.capability.as_ref().to_string(),
            billing_unit: request.billing_unit.as_ref().to_string(),
            usage_event_id: request.usage_event_id,
            cost_ledger_id: request.cost_ledger_id,
            pricing_catalog_entry_id: request.pricing_catalog_entry_id,
            pricing_status: request.pricing_status.as_ref().to_string(),
            estimated_cost: request.estimated_cost,
            currency: request.currency,
            token_usage_json: request.token_usage_json,
            pricing_snapshot_json: request.pricing_snapshot_json,
        },
    )
    .await
    .context("failed to persist attempt stage accounting")
}

pub async fn refresh_attempt_cost_summary(
    state: &AppState,
    ingestion_run_id: Uuid,
) -> anyhow::Result<AttemptStageCostSummaryRow> {
    repositories::refresh_attempt_stage_cost_summary(&state.persistence.postgres, ingestion_run_id)
        .await
        .context("failed to refresh attempt cost summary")
}

pub async fn record_stage_usage_and_cost(
    state: &AppState,
    request: StageUsageAccountingRequest,
) -> anyhow::Result<StageUsageAccountingResult> {
    validate_stage_usage_request(&request.stage, &request.capability, &request.billing_unit)?;
    let stage_ownership =
        stage_native_ownership(request.ingestion_run_id, request.stage_event_id, &request.stage);
    let raw_usage_json =
        decorate_payload_with_stage_ownership(request.raw_usage_json.clone(), &stage_ownership);
    let usage_event = repositories::create_usage_event(
        &state.persistence.postgres,
        &repositories::NewUsageEvent {
            workspace_id: request.workspace_id,
            project_id: request.project_id,
            provider_account_id: None,
            model_profile_id: request.model_profile_id,
            usage_kind: request.usage_kind,
            prompt_tokens: request.prompt_tokens,
            completion_tokens: request.completion_tokens,
            total_tokens: request.total_tokens,
            raw_usage_json: raw_usage_json.clone(),
        },
    )
    .await
    .context("failed to create stage usage event")?;

    let pricing_resolution = pricing_catalog::resolve_usage_cost(
        state,
        pricing_catalog::UsageCostLookupRequest {
            workspace_id: request.workspace_id,
            provider_kind: request.provider_kind.clone(),
            model_name: request.model_name.clone(),
            capability: capability_as_str(&request.capability).to_string(),
            billing_unit: billing_unit_as_str(&request.billing_unit).to_string(),
            prompt_tokens: request.prompt_tokens,
            completion_tokens: request.completion_tokens,
            total_tokens: request.total_tokens,
            at: usage_event.created_at,
        },
    )
    .await
    .context("failed to resolve stage pricing")?;

    let pricing_snapshot_json = decorate_payload_with_stage_ownership(
        pricing_resolution.pricing_snapshot_json.clone(),
        &stage_ownership,
    );

    let cost_ledger = if let Some(estimated_cost) = pricing_resolution.estimated_cost {
        Some(
            repositories::create_cost_ledger(
                &state.persistence.postgres,
                request.workspace_id,
                request.project_id,
                usage_event.id,
                &request.provider_kind,
                &request.model_name,
                estimated_cost,
                pricing_snapshot_json.clone(),
            )
            .await
            .context("failed to create stage cost ledger")?,
        )
    } else {
        None
    };

    let stage_accounting = record_stage_accounting(
        state,
        RecordStageAccountingRequest {
            ingestion_run_id: request.ingestion_run_id,
            stage_event_id: request.stage_event_id,
            stage: request.stage,
            accounting_scope: request.accounting_scope,
            workspace_id: request.workspace_id,
            project_id: request.project_id,
            provider_kind: Some(request.provider_kind),
            model_name: Some(request.model_name),
            capability: request.capability,
            billing_unit: request.billing_unit,
            usage_event_id: Some(usage_event.id),
            cost_ledger_id: cost_ledger.as_ref().map(|row| row.id),
            pricing_catalog_entry_id: pricing_resolution.entry.as_ref().map(|row| row.id),
            pricing_status: pricing_resolution.status,
            estimated_cost: cost_ledger.as_ref().map(|row| row.estimated_cost),
            currency: cost_ledger.as_ref().map(|row| row.currency.clone()),
            token_usage_json: raw_usage_json,
            pricing_snapshot_json,
        },
    )
    .await?;
    let attempt_summary = refresh_attempt_cost_summary(state, request.ingestion_run_id).await?;

    Ok(StageUsageAccountingResult { usage_event, cost_ledger, stage_accounting, attempt_summary })
}

pub async fn record_stage_accounting_gap(
    state: &AppState,
    request: StageAccountingGapRequest,
) -> anyhow::Result<(AttemptStageAccountingRow, AttemptStageCostSummaryRow)> {
    validate_stage_gap_request(
        &request.stage,
        &request.capability,
        &request.billing_unit,
        &request.pricing_status,
    )?;
    let stage_ownership =
        stage_native_ownership(request.ingestion_run_id, request.stage_event_id, &request.stage);
    let stage_accounting = record_stage_accounting(
        state,
        RecordStageAccountingRequest {
            ingestion_run_id: request.ingestion_run_id,
            stage_event_id: request.stage_event_id,
            stage: request.stage,
            accounting_scope: request.accounting_scope,
            workspace_id: request.workspace_id,
            project_id: request.project_id,
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            capability: request.capability,
            billing_unit: request.billing_unit,
            usage_event_id: None,
            cost_ledger_id: None,
            pricing_catalog_entry_id: None,
            pricing_status: request.pricing_status,
            estimated_cost: None,
            currency: None,
            token_usage_json: decorate_payload_with_stage_ownership(
                request.token_usage_json,
                &stage_ownership,
            ),
            pricing_snapshot_json: decorate_payload_with_stage_ownership(
                request.pricing_snapshot_json,
                &stage_ownership,
            ),
        },
    )
    .await?;
    let attempt_summary = refresh_attempt_cost_summary(state, request.ingestion_run_id).await?;
    Ok((stage_accounting, attempt_summary))
}

trait AsRefStr {
    fn as_ref(&self) -> &'static str;
}

fn capability_as_str(value: &PricingCapability) -> &'static str {
    value.as_ref()
}

fn billing_unit_as_str(value: &PricingBillingUnit) -> &'static str {
    value.as_ref()
}

impl AsRefStr for PricingCapability {
    fn as_ref(&self) -> &'static str {
        match self {
            Self::Indexing => "indexing",
            Self::Embedding => "embedding",
            Self::Answer => "answer",
            Self::Vision => "vision",
            Self::GraphExtract => "graph_extract",
        }
    }
}

impl AsRefStr for PricingBillingUnit {
    fn as_ref(&self) -> &'static str {
        match self {
            Self::Per1MInputTokens => "per_1m_input_tokens",
            Self::Per1MOutputTokens => "per_1m_output_tokens",
            Self::Per1MTokens => "per_1m_tokens",
            Self::FixedPerCall => "fixed_per_call",
        }
    }
}

impl AsRefStr for PricingResolutionStatus {
    fn as_ref(&self) -> &'static str {
        match self {
            Self::Priced => "priced",
            Self::Unpriced => "unpriced",
            Self::UsageMissing => "usage_missing",
            Self::PricingMissing => "pricing_missing",
        }
    }
}

fn validate_stage_usage_request(
    stage: &str,
    capability: &PricingCapability,
    billing_unit: &PricingBillingUnit,
) -> anyhow::Result<()> {
    match runtime_stage_billing_policy(stage) {
        RuntimeStageBillingPolicy::Billable {
            capability: expected_capability,
            billing_unit: expected_billing_unit,
        } => {
            if capability != &expected_capability || billing_unit != &expected_billing_unit {
                anyhow::bail!(
                    "stage accounting ownership mismatch: stage {} expects capability {} and billing unit {}, got {} / {}",
                    stage,
                    capability_as_str(&expected_capability),
                    billing_unit_as_str(&expected_billing_unit),
                    capability_as_str(capability),
                    billing_unit_as_str(billing_unit),
                );
            }
            Ok(())
        }
        RuntimeStageBillingPolicy::NonBillable => {
            anyhow::bail!("stage {} is non-billable and cannot own usage-based accounting", stage)
        }
    }
}

fn validate_stage_gap_request(
    stage: &str,
    capability: &PricingCapability,
    billing_unit: &PricingBillingUnit,
    pricing_status: &PricingResolutionStatus,
) -> anyhow::Result<()> {
    match runtime_stage_billing_policy(stage) {
        RuntimeStageBillingPolicy::Billable {
            capability: expected_capability,
            billing_unit: expected_billing_unit,
        } => {
            if capability != &expected_capability || billing_unit != &expected_billing_unit {
                anyhow::bail!(
                    "stage accounting ownership mismatch: stage {} expects capability {} and billing unit {}, got {} / {}",
                    stage,
                    capability_as_str(&expected_capability),
                    billing_unit_as_str(&expected_billing_unit),
                    capability_as_str(capability),
                    billing_unit_as_str(billing_unit),
                );
            }
            Ok(())
        }
        RuntimeStageBillingPolicy::NonBillable => {
            if matches!(pricing_status, PricingResolutionStatus::Priced) {
                anyhow::bail!(
                    "stage {} is non-billable and cannot persist priced accounting",
                    stage
                );
            }
            Ok(())
        }
    }
}

fn validate_stage_accounting_request(
    stage: &str,
    capability: &PricingCapability,
    billing_unit: &PricingBillingUnit,
    pricing_status: &PricingResolutionStatus,
    has_estimated_cost: bool,
    has_cost_ledger: bool,
    has_pricing_catalog_entry: bool,
) -> anyhow::Result<()> {
    match runtime_stage_billing_policy(stage) {
        RuntimeStageBillingPolicy::Billable {
            capability: expected_capability,
            billing_unit: expected_billing_unit,
        } => {
            if capability != &expected_capability || billing_unit != &expected_billing_unit {
                anyhow::bail!(
                    "stage accounting ownership mismatch: stage {} expects capability {} and billing unit {}, got {} / {}",
                    stage,
                    capability_as_str(&expected_capability),
                    billing_unit_as_str(&expected_billing_unit),
                    capability_as_str(capability),
                    billing_unit_as_str(billing_unit),
                );
            }
            Ok(())
        }
        RuntimeStageBillingPolicy::NonBillable => {
            if matches!(pricing_status, PricingResolutionStatus::Priced)
                || has_estimated_cost
                || has_cost_ledger
                || has_pricing_catalog_entry
            {
                anyhow::bail!(
                    "stage {} is non-billable and cannot persist priced accounting artifacts",
                    stage
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_priced_non_billable_stage_accounting() {
        let error = validate_stage_accounting_request(
            "merging_graph",
            &PricingCapability::Embedding,
            &PricingBillingUnit::Per1MInputTokens,
            &PricingResolutionStatus::Priced,
            true,
            true,
            true,
        )
        .expect_err("non-billable stage should reject priced accounting");

        assert!(error.to_string().contains("non-billable"));
    }

    #[test]
    fn rejects_billable_stage_with_wrong_capability() {
        let error = validate_stage_usage_request(
            "extracting_graph",
            &PricingCapability::Embedding,
            &PricingBillingUnit::Per1MInputTokens,
        )
        .expect_err("extracting_graph should reject embedding capability");

        assert!(error.to_string().contains("ownership mismatch"));
    }

    #[test]
    fn allows_unpriced_gap_for_non_billable_stage() {
        validate_stage_gap_request(
            "merging_graph",
            &PricingCapability::Embedding,
            &PricingBillingUnit::Per1MInputTokens,
            &PricingResolutionStatus::Unpriced,
        )
        .expect("non-billable unpriced gap remains representable");
    }
}
