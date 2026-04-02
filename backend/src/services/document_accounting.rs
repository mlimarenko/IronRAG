use anyhow::Context;
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::billing::{
        PricingBillingUnit, PricingCapability, PricingResolutionStatus, RuntimeStageBillingPolicy,
        decorate_payload_with_stage_ownership, runtime_stage_billing_policy,
        stage_native_ownership,
    },
    infra::repositories::{
        self, AttemptStageAccountingRow, AttemptStageCostSummaryRow, ai_repository,
    },
};

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
struct UsageCostResolution {
    status: PricingResolutionStatus,
    entry_id: Option<Uuid>,
    estimated_cost: Option<Decimal>,
    pricing_snapshot_json: serde_json::Value,
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
) -> anyhow::Result<()> {
    validate_stage_usage_request(&request.stage, &request.capability, &request.billing_unit)?;
    let accounting_scope_label = request.accounting_scope.scope_label().to_string();
    let call_sequence_no = request.accounting_scope.call_sequence_no();
    if let Some(existing_stage_accounting) = repositories::get_attempt_stage_accounting_by_scope(
        &state.persistence.postgres,
        request.stage_event_id,
        &accounting_scope_label,
        call_sequence_no,
    )
    .await
    .context("failed to load existing stage accounting before usage persistence")?
    {
        if let Some(existing_usage_event_id) = existing_stage_accounting.usage_event_id {
            let usage_event = repositories::get_usage_event_by_id(
                &state.persistence.postgres,
                existing_usage_event_id,
            )
            .await
            .context("failed to load existing stage usage event")?
            .context("existing stage accounting references missing usage event")?;
            let cost_ledger = match existing_stage_accounting.cost_ledger_id {
                Some(cost_ledger_id) => Some(
                    repositories::get_cost_ledger_by_id(
                        &state.persistence.postgres,
                        cost_ledger_id,
                    )
                    .await
                    .context("failed to load existing stage cost ledger")?
                    .context("existing stage accounting references missing cost ledger")?,
                ),
                None => None,
            };
            let attempt_summary =
                refresh_attempt_cost_summary(state, request.ingestion_run_id).await?;
            let _ = usage_event;
            let _ = cost_ledger;
            let _ = existing_stage_accounting;
            let _ = attempt_summary;
            return Ok(());
        }
    }
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

    let pricing_resolution = resolve_usage_cost(
        state,
        request.workspace_id,
        &request.provider_kind,
        &request.model_name,
        &request.capability,
        &request.billing_unit,
        request.prompt_tokens,
        request.completion_tokens,
        request.total_tokens,
        usage_event.created_at,
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

    let _stage_accounting = record_stage_accounting(
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
            pricing_catalog_entry_id: pricing_resolution.entry_id,
            pricing_status: pricing_resolution.status,
            estimated_cost: cost_ledger.as_ref().map(|row| row.estimated_cost),
            currency: cost_ledger.as_ref().map(|row| row.currency.clone()),
            token_usage_json: raw_usage_json,
            pricing_snapshot_json,
        },
    )
    .await?;
    let _attempt_summary = refresh_attempt_cost_summary(state, request.ingestion_run_id).await?;

    Ok(())
}

pub async fn record_stage_accounting_gap(
    state: &AppState,
    request: StageAccountingGapRequest,
) -> anyhow::Result<()> {
    validate_stage_gap_request(
        &request.stage,
        &request.capability,
        &request.billing_unit,
        &request.pricing_status,
    )?;
    let stage_ownership =
        stage_native_ownership(request.ingestion_run_id, request.stage_event_id, &request.stage);
    let _stage_accounting = record_stage_accounting(
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
    let _attempt_summary = refresh_attempt_cost_summary(state, request.ingestion_run_id).await?;
    Ok(())
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
            Self::Per1MCachedInputTokens => "per_1m_cached_input_tokens",
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

async fn resolve_usage_cost(
    state: &AppState,
    workspace_id: Option<Uuid>,
    provider_kind: &str,
    model_name: &str,
    capability: &PricingCapability,
    billing_unit: &PricingBillingUnit,
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    total_tokens: Option<i32>,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<UsageCostResolution> {
    let provider_kind = provider_kind.trim().to_ascii_lowercase();
    let model_name = model_name.trim().to_string();
    let capability_key = capability_as_str(capability).to_string();
    let billing_unit_key = billing_unit_as_str(billing_unit).to_string();
    let request_input_tokens = prompt_tokens.or(total_tokens);

    let model = ai_repository::get_model_catalog_by_provider_and_name(
        &state.persistence.postgres,
        &provider_kind,
        &model_name,
    )
    .await
    .context("failed to resolve model catalog entry for usage pricing")?;

    let Some(model) = model else {
        let status = PricingResolutionStatus::PricingMissing;
        return Ok(UsageCostResolution {
            status: status.clone(),
            entry_id: None,
            estimated_cost: None,
            pricing_snapshot_json: build_usage_pricing_snapshot(
                None,
                status,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                &provider_kind,
                &model_name,
                &capability_key,
                &billing_unit_key,
                observed_at,
            ),
        });
    };

    let price = ai_repository::get_effective_price_catalog_entry(
        &state.persistence.postgres,
        model.id,
        &billing_unit_key,
        workspace_id,
        observed_at,
        "default",
        request_input_tokens,
    )
    .await
    .context("failed to resolve effective ai price catalog entry for usage pricing")?;

    let Some(price) = price else {
        let status = PricingResolutionStatus::PricingMissing;
        return Ok(UsageCostResolution {
            status: status.clone(),
            entry_id: None,
            estimated_cost: None,
            pricing_snapshot_json: build_usage_pricing_snapshot(
                None,
                status,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                &provider_kind,
                &model_name,
                &capability_key,
                &billing_unit_key,
                observed_at,
            ),
        });
    };

    let (status, estimated_cost) = estimate_usage_cost_from_price_catalog(
        &price,
        prompt_tokens,
        completion_tokens,
        total_tokens,
    );
    Ok(UsageCostResolution {
        status: status.clone(),
        entry_id: Some(price.id),
        estimated_cost,
        pricing_snapshot_json: build_usage_pricing_snapshot(
            Some(&price),
            status,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            &provider_kind,
            &model_name,
            &capability_key,
            &billing_unit_key,
            observed_at,
        ),
    })
}

fn estimate_usage_cost_from_price_catalog(
    price: &ai_repository::AiPriceCatalogRow,
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    total_tokens: Option<i32>,
) -> (PricingResolutionStatus, Option<Decimal>) {
    match price.billing_unit.as_str() {
        "per_1m_input_tokens" | "per_1m_cached_input_tokens" => {
            let Some(tokens) = prompt_tokens.or(total_tokens) else {
                return (PricingResolutionStatus::UsageMissing, None);
            };
            (PricingResolutionStatus::Priced, Some(scale_token_cost(tokens, price.unit_price)))
        }
        "per_1m_output_tokens" => {
            let Some(tokens) = completion_tokens else {
                return (PricingResolutionStatus::UsageMissing, None);
            };
            (PricingResolutionStatus::Priced, Some(scale_token_cost(tokens, price.unit_price)))
        }
        "fixed_per_call" => (PricingResolutionStatus::Priced, Some(price.unit_price)),
        _ => {
            let total_tokens = total_tokens.or_else(|| match (prompt_tokens, completion_tokens) {
                (Some(prompt), Some(completion)) => Some(prompt.saturating_add(completion)),
                (Some(prompt), None) => Some(prompt),
                (None, Some(completion)) => Some(completion),
                (None, None) => None,
            });
            let Some(tokens) = total_tokens else {
                return (PricingResolutionStatus::UsageMissing, None);
            };
            (PricingResolutionStatus::Priced, Some(scale_token_cost(tokens, price.unit_price)))
        }
    }
}

fn build_usage_pricing_snapshot(
    price: Option<&ai_repository::AiPriceCatalogRow>,
    status: PricingResolutionStatus,
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    total_tokens: Option<i32>,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
    billing_unit: &str,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> serde_json::Value {
    serde_json::json!({
        "pricing_status": status.as_ref(),
        "provider_kind": provider_kind,
        "model_name": model_name,
        "capability": capability,
        "billing_unit": billing_unit,
        "resolved_at": observed_at,
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": total_tokens,
        },
        "catalog_entry": price.map(|row| serde_json::json!({
            "id": row.id,
            "model_catalog_id": row.model_catalog_id,
            "billing_unit": row.billing_unit,
            "unit_price": row.unit_price.normalize().to_string(),
            "currency_code": row.currency_code,
            "effective_from": row.effective_from,
            "effective_to": row.effective_to,
            "catalog_scope": row.catalog_scope,
            "workspace_id": row.workspace_id,
        })),
    })
}

fn scale_token_cost(tokens: i32, price_per_million: Decimal) -> Decimal {
    if tokens <= 0 {
        return Decimal::ZERO;
    }
    (Decimal::from(tokens) * price_per_million) / Decimal::from(1_000_000u64)
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
