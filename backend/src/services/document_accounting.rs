use anyhow::Context;
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        pricing_catalog::{PricingBillingUnit, PricingCapability, PricingResolutionStatus},
        runtime_ingestion::RuntimeAccountingTruthStatus,
        usage_governance::{
            RuntimeStageBillingPolicy, decorate_payload_with_stage_ownership,
            runtime_stage_billing_policy, stage_native_ownership,
        },
    },
    infra::repositories::{
        self, AttemptStageAccountingRow, AttemptStageCostSummaryRow, CostLedgerRow,
        RuntimeIngestionStageEventRow, UsageEventRow,
    },
    services::{pricing_catalog, runtime_ingestion},
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
pub struct ResolvedStageAccountingView {
    pub stage: String,
    pub anchor_event_id: Option<Uuid>,
    pub accounting_scope: String,
    pub pricing_status: String,
    pub estimated_cost: Option<Decimal>,
    pub settled_estimated_cost: Option<Decimal>,
    pub in_flight_estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub usage_event_id: Option<Uuid>,
    pub cost_ledger_id: Option<Uuid>,
    pub pricing_catalog_entry_id: Option<Uuid>,
    pub attribution_source: String,
}

#[derive(Debug, Clone)]
pub struct AttemptAccountingSummaryView {
    pub total_estimated_cost: Option<Decimal>,
    pub settled_estimated_cost: Option<Decimal>,
    pub in_flight_estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub priced_stage_count: i32,
    pub unpriced_stage_count: i32,
    pub in_flight_stage_count: i32,
    pub missing_stage_count: i32,
    pub accounting_status: String,
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

#[must_use]
pub fn classify_accounting_truth_status(
    priced_stage_count: i32,
    unpriced_stage_count: i32,
    in_flight_stage_count: i32,
    missing_stage_count: i32,
) -> RuntimeAccountingTruthStatus {
    if in_flight_stage_count > 0 {
        RuntimeAccountingTruthStatus::InFlightUnsettled
    } else if priced_stage_count > 0 && unpriced_stage_count == 0 && missing_stage_count == 0 {
        RuntimeAccountingTruthStatus::Priced
    } else if priced_stage_count > 0 || unpriced_stage_count > 0 || missing_stage_count > 0 {
        RuntimeAccountingTruthStatus::Partial
    } else {
        RuntimeAccountingTruthStatus::Unpriced
    }
}

#[must_use]
pub fn accounting_truth_status_key(status: &RuntimeAccountingTruthStatus) -> &'static str {
    match status {
        RuntimeAccountingTruthStatus::Priced => "priced",
        RuntimeAccountingTruthStatus::Partial => "partial",
        RuntimeAccountingTruthStatus::Unpriced => "unpriced",
        RuntimeAccountingTruthStatus::InFlightUnsettled => "in_flight_unsettled",
    }
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

async fn refresh_collection_settlement_if_possible(
    state: &AppState,
    ingestion_run_id: Uuid,
    project_id: Option<Uuid>,
) -> anyhow::Result<()> {
    let resolved_project_id = match project_id {
        Some(project_id) => Some(project_id),
        None => repositories::get_runtime_ingestion_run_by_id(
            &state.persistence.postgres,
            ingestion_run_id,
        )
        .await
        .context("failed to load ingestion run for collection settlement refresh")?
        .map(|row| row.project_id),
    };
    if let Some(project_id) = resolved_project_id {
        runtime_ingestion::refresh_library_collection_settlement_snapshots(state, project_id)
            .await
            .context("failed to refresh collection settlement snapshots")?;
        runtime_ingestion::refresh_library_warning_snapshots(state, project_id)
            .await
            .context("failed to refresh collection warning snapshots")?;
    }
    Ok(())
}

pub async fn record_stage_usage_and_cost(
    state: &AppState,
    request: StageUsageAccountingRequest,
) -> anyhow::Result<StageUsageAccountingResult> {
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
            refresh_collection_settlement_if_possible(
                state,
                request.ingestion_run_id,
                request.project_id,
            )
            .await?;
            return Ok(StageUsageAccountingResult {
                usage_event,
                cost_ledger,
                stage_accounting: existing_stage_accounting,
                attempt_summary,
            });
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
    refresh_collection_settlement_if_possible(state, request.ingestion_run_id, request.project_id)
        .await?;

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
    refresh_collection_settlement_if_possible(state, request.ingestion_run_id, request.project_id)
        .await?;
    Ok((stage_accounting, attempt_summary))
}

#[must_use]
pub fn resolve_attempt_stage_accounting(
    attempt_stage_events: &[RuntimeIngestionStageEventRow],
    stage_accounting: &[AttemptStageAccountingRow],
) -> Vec<ResolvedStageAccountingView> {
    let attempt_stage_event_ids = attempt_stage_events
        .iter()
        .map(|event| event.id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut billable_stage_names = attempt_stage_events
        .iter()
        .filter_map(|event| match runtime_stage_billing_policy(&event.stage) {
            RuntimeStageBillingPolicy::Billable { .. } => Some(event.stage.clone()),
            RuntimeStageBillingPolicy::NonBillable => None,
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    billable_stage_names.sort();

    billable_stage_names
        .into_iter()
        .map(|stage_name| {
            let stage_rows = stage_accounting
                .iter()
                .filter(|row| {
                    row.stage == stage_name && attempt_stage_event_ids.contains(&row.stage_event_id)
                })
                .collect::<Vec<_>>();
            let anchor_event = attempt_stage_events
                .iter()
                .rev()
                .find(|event| {
                    event.stage == stage_name
                        && matches!(event.status.as_str(), "completed" | "failed" | "skipped")
                })
                .or_else(|| {
                    attempt_stage_events.iter().rev().find(|event| event.stage == stage_name)
                });
            let anchor_event_status = anchor_event.map(|event| event.status.as_str());
            let anchor_event_id = anchor_event.map(|event| event.id);
            let stage_rollup =
                stage_rows.iter().rev().find(|row| row.accounting_scope == "stage_rollup");
            let provider_calls = stage_rows
                .iter()
                .filter(|row| row.accounting_scope == "provider_call")
                .copied()
                .collect::<Vec<_>>();

            if let Some(row) = stage_rollup {
                ResolvedStageAccountingView {
                    stage: stage_name,
                    anchor_event_id,
                    accounting_scope: "stage_rollup".to_string(),
                    pricing_status: row.pricing_status.clone(),
                    estimated_cost: row.estimated_cost,
                    settled_estimated_cost: row.estimated_cost,
                    in_flight_estimated_cost: None,
                    currency: row.currency.clone(),
                    usage_event_id: row.usage_event_id,
                    cost_ledger_id: row.cost_ledger_id,
                    pricing_catalog_entry_id: row.pricing_catalog_entry_id,
                    attribution_source: stage_attribution_source(row, &row.stage).to_string(),
                }
            } else if !provider_calls.is_empty() {
                provider_call_stage_accounting_view(
                    &stage_name,
                    anchor_event_id,
                    anchor_event_status,
                    &provider_calls,
                )
            } else {
                ResolvedStageAccountingView {
                    stage: stage_name,
                    anchor_event_id,
                    accounting_scope: "missing".to_string(),
                    pricing_status: "unpriced".to_string(),
                    estimated_cost: None,
                    settled_estimated_cost: None,
                    in_flight_estimated_cost: None,
                    currency: None,
                    usage_event_id: None,
                    cost_ledger_id: None,
                    pricing_catalog_entry_id: None,
                    attribution_source: "stage_native".to_string(),
                }
            }
        })
        .collect()
}

fn provider_call_stage_accounting_view(
    stage_name: &str,
    anchor_event_id: Option<Uuid>,
    anchor_event_status: Option<&str>,
    provider_calls: &[&AttemptStageAccountingRow],
) -> ResolvedStageAccountingView {
    let estimated_cost = provider_calls
        .iter()
        .filter_map(|row| row.estimated_cost)
        .fold(Decimal::ZERO, |acc, value| acc + value);
    let has_estimated_cost = provider_calls.iter().any(|row| row.estimated_cost.is_some());
    let has_unpriced_or_missing = provider_calls
        .iter()
        .any(|row| !matches!(row.pricing_status.as_str(), "priced" | "in_flight_unsettled"));
    let is_terminal_stage = anchor_event_status
        .map(|status| matches!(status, "completed" | "failed" | "skipped"))
        .unwrap_or(false);
    let pricing_status = if is_terminal_stage {
        if has_estimated_cost && !has_unpriced_or_missing {
            "priced"
        } else if has_estimated_cost || has_unpriced_or_missing {
            "partial"
        } else {
            "unpriced"
        }
    } else {
        "in_flight_unsettled"
    };

    ResolvedStageAccountingView {
        stage: stage_name.to_string(),
        anchor_event_id,
        accounting_scope: "provider_call".to_string(),
        pricing_status: pricing_status.to_string(),
        estimated_cost: has_estimated_cost.then_some(estimated_cost),
        settled_estimated_cost: (is_terminal_stage && has_estimated_cost).then_some(estimated_cost),
        // Provider-call rows remain visible before the stage rollup settles; once the stage is
        // terminal they become the settled fallback source of truth if no stage rollup exists yet.
        in_flight_estimated_cost: (!is_terminal_stage && has_estimated_cost)
            .then_some(estimated_cost),
        currency: provider_calls.iter().find_map(|row| row.currency.clone()),
        usage_event_id: None,
        cost_ledger_id: None,
        pricing_catalog_entry_id: None,
        attribution_source: "stage_native".to_string(),
    }
}

#[must_use]
pub fn summarize_resolved_attempt_stage_accounting(
    resolved_stage_accounting: &[ResolvedStageAccountingView],
) -> AttemptAccountingSummaryView {
    let total_estimated_cost = resolved_stage_accounting
        .iter()
        .filter_map(|row| row.estimated_cost)
        .fold(Decimal::ZERO, |acc, value| acc + value);
    let settled_estimated_cost = resolved_stage_accounting
        .iter()
        .filter_map(|row| row.settled_estimated_cost)
        .fold(Decimal::ZERO, |acc, value| acc + value);
    let in_flight_estimated_cost = resolved_stage_accounting
        .iter()
        .filter_map(|row| row.in_flight_estimated_cost)
        .fold(Decimal::ZERO, |acc, value| acc + value);
    let priced_stage_count = i32::try_from(
        resolved_stage_accounting
            .iter()
            .filter(|row| {
                row.accounting_scope != "missing"
                    && row.pricing_status == "priced"
                    && row.in_flight_estimated_cost.is_none()
            })
            .count(),
    )
    .unwrap_or(i32::MAX);
    let unpriced_stage_count = i32::try_from(
        resolved_stage_accounting
            .iter()
            .filter(|row| {
                row.accounting_scope != "missing"
                    && row.pricing_status != "priced"
                    && row.in_flight_estimated_cost.is_none()
            })
            .count(),
    )
    .unwrap_or(i32::MAX);
    let in_flight_stage_count = i32::try_from(
        resolved_stage_accounting
            .iter()
            .filter(|row| row.in_flight_estimated_cost.is_some())
            .count(),
    )
    .unwrap_or(i32::MAX);
    let missing_stage_count = i32::try_from(
        resolved_stage_accounting.iter().filter(|row| row.accounting_scope == "missing").count(),
    )
    .unwrap_or(i32::MAX);
    let accounting_status = classify_accounting_truth_status(
        priced_stage_count,
        unpriced_stage_count,
        in_flight_stage_count,
        missing_stage_count,
    );

    AttemptAccountingSummaryView {
        total_estimated_cost: resolved_stage_accounting
            .iter()
            .any(|row| row.estimated_cost.is_some())
            .then_some(total_estimated_cost),
        settled_estimated_cost: resolved_stage_accounting
            .iter()
            .any(|row| row.settled_estimated_cost.is_some())
            .then_some(settled_estimated_cost),
        in_flight_estimated_cost: resolved_stage_accounting
            .iter()
            .any(|row| row.in_flight_estimated_cost.is_some())
            .then_some(in_flight_estimated_cost),
        currency: resolved_stage_accounting.iter().find_map(|row| row.currency.clone()),
        priced_stage_count,
        unpriced_stage_count,
        in_flight_stage_count,
        missing_stage_count,
        accounting_status: accounting_truth_status_key(&accounting_status).to_string(),
    }
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

fn stage_attribution_source(row: &AttemptStageAccountingRow, event_stage: &str) -> &'static str {
    let metadata_source = row
        .pricing_snapshot_json
        .get("stage_ownership")
        .or_else(|| row.token_usage_json.get("stage_ownership"))
        .and_then(|value| value.get("attribution_source"))
        .and_then(serde_json::Value::as_str);
    match metadata_source {
        Some("stage_native") => "stage_native",
        Some("reconciled") => "reconciled",
        _ if row.stage == event_stage => "stage_native",
        _ => "reconciled",
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::infra::repositories::{AttemptStageAccountingRow, RuntimeIngestionStageEventRow};

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

    #[test]
    fn provider_call_accounting_is_visible_before_stage_rollup_exists() {
        let stage_event_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let stage_events = vec![RuntimeIngestionStageEventRow {
            id: stage_event_id,
            ingestion_run_id: run_id,
            attempt_no: 1,
            stage: "extracting_graph".to_string(),
            status: "started".to_string(),
            message: None,
            metadata_json: json!({}),
            provider_kind: Some("openai".to_string()),
            model_name: Some("gpt-5.4-mini".to_string()),
            started_at: Utc::now(),
            finished_at: None,
            elapsed_ms: None,
            created_at: Utc::now(),
        }];
        let stage_accounting = vec![AttemptStageAccountingRow {
            id: Uuid::now_v7(),
            ingestion_run_id: run_id,
            stage_event_id,
            stage: "extracting_graph".to_string(),
            accounting_scope: "provider_call".to_string(),
            call_sequence_no: 1,
            workspace_id: None,
            project_id: None,
            provider_kind: Some("openai".to_string()),
            model_name: Some("gpt-5.4-mini".to_string()),
            capability: "graph_extract".to_string(),
            billing_unit: "per_1m_tokens".to_string(),
            usage_event_id: Some(Uuid::now_v7()),
            cost_ledger_id: Some(Uuid::now_v7()),
            pricing_catalog_entry_id: Some(Uuid::now_v7()),
            pricing_status: "priced".to_string(),
            estimated_cost: Some(Decimal::new(375, 5)),
            currency: Some("USD".to_string()),
            token_usage_json: json!({
                "prompt_tokens": 1200,
                "completion_tokens": 340,
                "total_tokens": 1540,
            }),
            pricing_snapshot_json: json!({
                "stage_ownership": {
                    "attribution_source": "stage_native"
                }
            }),
            created_at: Utc::now(),
        }];

        let resolved = resolve_attempt_stage_accounting(&stage_events, &stage_accounting);
        let summary = summarize_resolved_attempt_stage_accounting(&resolved);

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].accounting_scope, "provider_call");
        assert_eq!(resolved[0].pricing_status, "in_flight_unsettled");
        assert_eq!(resolved[0].estimated_cost, Some(Decimal::new(375, 5)));
        assert_eq!(resolved[0].in_flight_estimated_cost, Some(Decimal::new(375, 5)));
        assert_eq!(summary.accounting_status, "in_flight_unsettled");
        assert_eq!(summary.in_flight_stage_count, 1);
        assert_eq!(summary.total_estimated_cost, Some(Decimal::new(375, 5)));
        assert_eq!(summary.in_flight_estimated_cost, Some(Decimal::new(375, 5)));
    }

    #[test]
    fn partial_accounting_remains_distinct_from_unpriced_when_some_work_settles() {
        let status = classify_accounting_truth_status(1, 0, 0, 1);

        assert_eq!(status, RuntimeAccountingTruthStatus::Partial);
        assert_eq!(accounting_truth_status_key(&status), "partial");
    }

    #[test]
    fn provider_call_accounting_settles_when_stage_is_terminal_without_rollup() {
        let stage_event_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let stage_events = vec![RuntimeIngestionStageEventRow {
            id: stage_event_id,
            ingestion_run_id: run_id,
            attempt_no: 1,
            stage: "extracting_graph".to_string(),
            status: "failed".to_string(),
            message: Some("upstream timeout".to_string()),
            metadata_json: json!({}),
            provider_kind: Some("openai".to_string()),
            model_name: Some("gpt-5.4-mini".to_string()),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            elapsed_ms: Some(9_000),
            created_at: Utc::now(),
        }];
        let stage_accounting = vec![AttemptStageAccountingRow {
            id: Uuid::now_v7(),
            ingestion_run_id: run_id,
            stage_event_id,
            stage: "extracting_graph".to_string(),
            accounting_scope: "provider_call".to_string(),
            call_sequence_no: 1,
            workspace_id: None,
            project_id: None,
            provider_kind: Some("openai".to_string()),
            model_name: Some("gpt-5.4-mini".to_string()),
            capability: "graph_extract".to_string(),
            billing_unit: "per_1m_tokens".to_string(),
            usage_event_id: Some(Uuid::now_v7()),
            cost_ledger_id: Some(Uuid::now_v7()),
            pricing_catalog_entry_id: Some(Uuid::now_v7()),
            pricing_status: "priced".to_string(),
            estimated_cost: Some(Decimal::new(420, 5)),
            currency: Some("USD".to_string()),
            token_usage_json: json!({ "total_tokens": 1540 }),
            pricing_snapshot_json: json!({}),
            created_at: Utc::now(),
        }];

        let resolved = resolve_attempt_stage_accounting(&stage_events, &stage_accounting);
        let summary = summarize_resolved_attempt_stage_accounting(&resolved);

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].accounting_scope, "provider_call");
        assert_eq!(resolved[0].pricing_status, "priced");
        assert_eq!(resolved[0].settled_estimated_cost, Some(Decimal::new(420, 5)));
        assert_eq!(resolved[0].in_flight_estimated_cost, None);
        assert_eq!(summary.accounting_status, "priced");
        assert_eq!(summary.priced_stage_count, 1);
        assert_eq!(summary.in_flight_stage_count, 0);
        assert_eq!(summary.settled_estimated_cost, Some(Decimal::new(420, 5)));
    }
}
