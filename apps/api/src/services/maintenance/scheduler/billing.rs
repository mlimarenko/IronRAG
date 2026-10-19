use std::{sync::OnceLock, time::Duration};

use opentelemetry::{KeyValue, global, metrics::Counter};
use tracing::{info, warn};

use crate::{app::state::AppState, infra::repositories::billing_repository};

/// A provider-backed rerank has a five-second hard deadline. Ten minutes
/// leaves a 120x safety margin for reservation persistence, scheduling delays,
/// and future bounded provider stages while still recovering process crashes.
pub(super) const STALE_BILLING_PROVIDER_CALL_AFTER: Duration = Duration::from_mins(10);
pub(super) const STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT: i64 =
    billing_repository::MAX_STALE_PROVIDER_CALL_REAP_BATCH_LIMIT;
const DIRTY_EXECUTION_COST_REPAIR_BATCH_LIMIT: i64 =
    billing_repository::MAX_DIRTY_EXECUTION_COST_REPAIR_BATCH_LIMIT;
const DIRTY_EXECUTION_COST_CLAIM_LEASE: Duration = Duration::from_mins(1);
pub(super) const DIRTY_EXECUTION_COST_MAX_BACKOFF: Duration = Duration::from_mins(5);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BillingProviderCallReaperReport {
    pub reaped_provider_calls: u64,
    pub affected_executions: u64,
    pub rollups_refreshed: u64,
    pub rollup_failures: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BillingExecutionCostRepairReport {
    pub examined: u64,
    pub repaired: u64,
    pub failed: u64,
    pub superseded: u64,
}

struct BillingProviderCallReaperMetrics {
    runs: Counter<u64>,
    canceled_calls: Counter<u64>,
    execution_rollups: Counter<u64>,
}

fn billing_provider_call_reaper_metrics() -> &'static BillingProviderCallReaperMetrics {
    static METRICS: OnceLock<BillingProviderCallReaperMetrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        let meter = global::meter("ironrag.maintenance");
        BillingProviderCallReaperMetrics {
            runs: meter
                .u64_counter("ironrag.maintenance.billing_provider_call_reaper.runs")
                .with_description("Completed stale provider-call reservation sweeps")
                .with_unit("{run}")
                .build(),
            canceled_calls: meter
                .u64_counter("ironrag.maintenance.billing_provider_call_reaper.canceled_calls")
                .with_description("Stale started provider-call reservations canceled")
                .with_unit("{call}")
                .build(),
            execution_rollups: meter
                .u64_counter("ironrag.maintenance.billing_provider_call_reaper.execution_rollups")
                .with_description("Affected execution-cost rollup repair outcomes")
                .with_unit("{execution}")
                .build(),
        }
    })
}

/// Runs one bounded stale provider-call reservation sweep and repairs each
/// unique affected execution rollup once. The repository hard-caps the batch,
/// so this loop can never become an unbounded database fanout.
///
/// The explicit age and batch size keep the operation deterministic for the
/// operator/integration-test surface. Eligibility is evaluated against
/// `PostgreSQL` `now()`, not the scheduler node clock.
pub async fn reap_stale_billing_provider_calls_once(
    state: &AppState,
    stale_after: Duration,
    batch_limit: i64,
) -> anyhow::Result<BillingProviderCallReaperReport> {
    let affected_executions = billing_repository::reap_stale_started_provider_calls(
        &state.persistence.postgres,
        stale_after,
        batch_limit,
    )
    .await?;
    let mut report = BillingProviderCallReaperReport {
        reaped_provider_calls: affected_executions
            .iter()
            .map(|affected| u64::try_from(affected.reaped_provider_calls).unwrap_or(0))
            .sum(),
        affected_executions: u64::try_from(affected_executions.len()).unwrap_or(u64::MAX),
        ..BillingProviderCallReaperReport::default()
    };

    for affected in affected_executions {
        match state
            .application_services
            .billing
            .get_execution_cost(
                state,
                &affected.owning_execution_kind,
                affected.owning_execution_id,
            )
            .await
        {
            Ok(_) => report.rollups_refreshed += 1,
            Err(error) => {
                report.rollup_failures += 1;
                warn!(
                    owning_execution_kind = %affected.owning_execution_kind,
                    owning_execution_id = %affected.owning_execution_id,
                    reaped_provider_calls = affected.reaped_provider_calls,
                    %error,
                    "stale provider-call reservations were canceled but execution-cost rollup repair failed",
                );
            }
        }
    }

    Ok(report)
}

pub(super) async fn reap_stale_billing_provider_calls(state: &AppState) {
    let metrics = billing_provider_call_reaper_metrics();
    match reap_stale_billing_provider_calls_once(
        state,
        STALE_BILLING_PROVIDER_CALL_AFTER,
        STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT,
    )
    .await
    {
        Ok(report) => {
            metrics.runs.add(1, &[KeyValue::new("status", "success")]);
            metrics.canceled_calls.add(report.reaped_provider_calls, &[]);
            metrics
                .execution_rollups
                .add(report.rollups_refreshed, &[KeyValue::new("status", "refreshed")]);
            metrics
                .execution_rollups
                .add(report.rollup_failures, &[KeyValue::new("status", "failed")]);
            log_provider_call_reaper_outcome(&report);
        }
        Err(error) => {
            metrics.runs.add(1, &[KeyValue::new("status", "failed")]);
            log_provider_call_reaper_failed(&error);
        }
    }
}

/// Reports a completed sweep only when it canceled at least one reservation;
/// an empty sweep is not worth a log line.
fn log_provider_call_reaper_outcome(report: &BillingProviderCallReaperReport) {
    if report.reaped_provider_calls == 0 {
        return;
    }
    if report.rollup_failures == 0 {
        log_provider_call_reaper_canceled(report);
    } else {
        log_provider_call_reaper_canceled_with_failures(report);
    }
}

fn log_provider_call_reaper_canceled(report: &BillingProviderCallReaperReport) {
    info!(
        reaped_provider_calls = report.reaped_provider_calls,
        affected_executions = report.affected_executions,
        rollups_refreshed = report.rollups_refreshed,
        batch_limit = STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT,
        "maintenance scheduler canceled stale provider-call reservations",
    );
}

fn log_provider_call_reaper_canceled_with_failures(report: &BillingProviderCallReaperReport) {
    warn!(
        reaped_provider_calls = report.reaped_provider_calls,
        affected_executions = report.affected_executions,
        rollups_refreshed = report.rollups_refreshed,
        rollup_failures = report.rollup_failures,
        batch_limit = STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT,
        "maintenance scheduler canceled stale provider-call reservations with rollup repair failures",
    );
}

fn log_provider_call_reaper_failed<E: std::fmt::Debug>(error: &E) {
    warn!(
        ?error,
        stale_after_secs = STALE_BILLING_PROVIDER_CALL_AFTER.as_secs(),
        batch_limit = STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT,
        "stale provider-call reservation reaper failed; continuing",
    );
}

/// Repairs one bounded batch of derived execution-cost generations.
///
/// The durable state row is dirtied in the same transaction as every
/// tracked provider-call mutation. `get_execution_cost` performs a
/// generation-fenced rebuild, so replaying this sweep or running it on
/// multiple worker nodes is safe. Failures use a bounded exponential backoff
/// and remain durable across process restarts.
pub async fn repair_dirty_billing_execution_costs_once(
    state: &AppState,
    batch_limit: i64,
) -> anyhow::Result<BillingExecutionCostRepairReport> {
    let repairs = billing_repository::claim_due_execution_cost_rollup_repairs(
        &state.persistence.postgres,
        batch_limit,
        DIRTY_EXECUTION_COST_CLAIM_LEASE,
    )
    .await?;
    let mut report = BillingExecutionCostRepairReport {
        examined: u64::try_from(repairs.len()).unwrap_or(u64::MAX),
        ..BillingExecutionCostRepairReport::default()
    };

    for repair in repairs {
        if billing_repository::delete_orphaned_execution_cost_rollup_state(
            &state.persistence.postgres,
            &repair.owning_execution_kind,
            repair.owning_execution_id,
            repair.dirty_generation,
        )
        .await?
        {
            report.repaired += 1;
            continue;
        }
        match state
            .application_services
            .billing
            .get_execution_cost(state, &repair.owning_execution_kind, repair.owning_execution_id)
            .await
        {
            Ok(_) => report.repaired += 1,
            Err(error) => {
                let backoff = dirty_execution_cost_retry_backoff(repair.repair_attempts);
                let recorded = billing_repository::record_execution_cost_rollup_failure(
                    &state.persistence.postgres,
                    &repair.owning_execution_kind,
                    repair.owning_execution_id,
                    repair.dirty_generation,
                    backoff,
                    "rollup_failed",
                )
                .await?;
                if recorded {
                    report.failed += 1;
                    log_rollup_repair_failure(&repair, backoff, &error);
                } else {
                    // Another concurrent write or repair advanced the
                    // generation while this attempt was running. Its current
                    // state owns the next decision; never back off that newer
                    // work using a stale failure.
                    report.superseded += 1;
                }
            }
        }
    }

    Ok(report)
}

fn log_rollup_repair_failure<E: std::fmt::Display>(
    repair: &billing_repository::BillingExecutionCostRollupStateRow,
    backoff: Duration,
    error: &E,
) {
    warn!(
        owning_execution_kind = %repair.owning_execution_kind,
        owning_execution_id = %repair.owning_execution_id,
        dirty_generation = repair.dirty_generation,
        retry_after_secs = backoff.as_secs(),
        %error,
        "durable execution-cost rollup repair failed",
    );
}

pub(super) fn dirty_execution_cost_retry_backoff(repair_attempts: i32) -> Duration {
    let exponent = u32::try_from(repair_attempts.clamp(0, 8)).unwrap_or(8);
    let seconds = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
    Duration::from_secs(seconds.min(DIRTY_EXECUTION_COST_MAX_BACKOFF.as_secs()))
}

pub(super) async fn repair_dirty_billing_execution_costs(state: &AppState) {
    match repair_dirty_billing_execution_costs_once(state, DIRTY_EXECUTION_COST_REPAIR_BATCH_LIMIT)
        .await
    {
        Ok(report) => log_dirty_execution_cost_repair(&report),
        Err(error) => log_dirty_execution_cost_repair_failed(&error),
    }
}

fn log_dirty_execution_cost_repair(report: &BillingExecutionCostRepairReport) {
    if report.examined == 0 {
        return;
    }
    if report.failed == 0 {
        log_dirty_execution_cost_repaired(report);
    } else {
        log_dirty_execution_cost_repair_with_failures(report);
    }
}

fn log_dirty_execution_cost_repaired(report: &BillingExecutionCostRepairReport) {
    info!(
        examined = report.examined,
        repaired = report.repaired,
        superseded = report.superseded,
        "maintenance scheduler repaired durable execution-cost rollups",
    );
}

fn log_dirty_execution_cost_repair_with_failures(report: &BillingExecutionCostRepairReport) {
    warn!(
        examined = report.examined,
        repaired = report.repaired,
        failed = report.failed,
        superseded = report.superseded,
        "maintenance scheduler completed execution-cost repair sweep with failures",
    );
}

fn log_dirty_execution_cost_repair_failed<E: std::fmt::Debug>(error: &E) {
    warn!(
        ?error,
        batch_limit = DIRTY_EXECUTION_COST_REPAIR_BATCH_LIMIT,
        "durable execution-cost repair sweep failed; continuing",
    );
}
