//! Background scheduler for recurring maintenance sweepers.
//!
//! The scheduler lives in the worker role only (see
//! `Settings::runs_maintenance_scheduler`). Each tick:
//!
//! 1. Reaps `leased` rows whose heartbeat has gone stale.
//! 2. Cancels query executions that outlived the canonical turn deadline.
//! 3. Deletes a statement- and row-bounded sweep of expired `PostgreSQL`
//!    query-result winners.
//! 4. Cancels abandoned provider-call reservations and repairs the bounded set
//!    of affected execution-cost rollups.
//! 5. Repairs a bounded batch of durable dirty execution-cost generations.
//! 6. Bootstraps any missing `maintenance_job_run` rows for known
//!    (class, scope) pairs, so a freshly-added library shows up in
//!    the next tick without operator intervention.
//! 7. For every class enabled in this build, atomically picks the
//!    oldest pending row whose `next_due_at` has passed and runs the
//!    matching sweeper under a tokio task that refreshes the lease
//!    heartbeat every 30 s.
//!
//! Rolling-budget cadence: each tick takes **at most one** (class,
//! scope) pair per class. With the default 30 s tick interval and one
//! class enabled (`gc.stale-chunks`) the scheduler walks the full
//! library set in `library_count * 30 s` wall time. That keeps load
//! predictable instead of stampeding every library at the same wall
//! clock.
//!
//! `ActiveIngest` is treated as a soft retry signal — the (class,
//! scope) pair is re-queued with a short backoff and `attempts` is NOT
//! incremented, so a library with continuously-in-flight ingest never
//! drifts into dead-letter just because the sweeper could not get the
//! advisory lock.

use std::{sync::OnceLock, time::Duration};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Gauge},
};
use sqlx::PgPool;
use tokio::{sync::broadcast, task::JoinHandle, time::sleep};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{
        billing_repository, catalog_repository, query_repository, query_result_cache_repository,
    },
    services::maintenance::{
        gc::{self, GcStaleChunksError, GcStaleChunksOptions, LibraryGcReport},
        lease::{self, MaintenanceClass, MaintenanceJobRun, Scope},
    },
};

/// Default heartbeat refresh cadence while a sweeper is running.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Backoff used when the sweeper refuses to run because the library is
/// busy with ingest. Short enough that the next tick within a few
/// minutes can pick the work up once ingest quiesces.
pub const ACTIVE_INGEST_RETRY: Duration = Duration::from_mins(5);

const STALE_QUERY_EXECUTION_BATCH_LIMIT: i64 = 100;
const QUERY_RESULT_CACHE_GC_BATCH_LIMIT: i64 =
    query_result_cache_repository::MAX_QUERY_RESULT_CACHE_GC_BATCH_LIMIT;
const QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT: i64 =
    query_result_cache_repository::MAX_QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT;
/// Hard per-tick work budget: at the canonical 30-second cadence this drains
/// up to 5,000 expired keys (~166/s) while every statement remains a small
/// `SKIP LOCKED` batch.
const QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK: u64 = 10;

/// A provider-backed rerank has a three-second hard deadline. Five minutes
/// leaves a 100x safety margin for reservation persistence, scheduling delays,
/// and future bounded provider stages while still recovering process crashes.
const STALE_BILLING_PROVIDER_CALL_AFTER: Duration = Duration::from_mins(5);
const STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT: i64 =
    billing_repository::MAX_STALE_PROVIDER_CALL_REAP_BATCH_LIMIT;
const DIRTY_EXECUTION_COST_REPAIR_BATCH_LIMIT: i64 =
    billing_repository::MAX_DIRTY_EXECUTION_COST_REPAIR_BATCH_LIMIT;
const DIRTY_EXECUTION_COST_CLAIM_LEASE: Duration = Duration::from_mins(1);
const DIRTY_EXECUTION_COST_MAX_BACKOFF: Duration = Duration::from_mins(5);

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

struct QueryResultCacheGcMetrics {
    runs: Counter<u64>,
    batches: Counter<u64>,
    rows_deleted: Counter<u64>,
    budget_exhausted: Counter<u64>,
    backlog_probe_failures: Counter<u64>,
    backlog_sample_rows: Gauge<u64>,
    oldest_expired_age_seconds: Gauge<f64>,
}

fn query_result_cache_gc_metrics() -> &'static QueryResultCacheGcMetrics {
    static METRICS: OnceLock<QueryResultCacheGcMetrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        let meter = global::meter("ironrag.maintenance");
        QueryResultCacheGcMetrics {
            runs: meter
                .u64_counter("ironrag.maintenance.query_result_cache_gc.runs")
                .with_description("Completed PostgreSQL query-result cache GC sweeps")
                .with_unit("{run}")
                .build(),
            batches: meter
                .u64_counter("ironrag.maintenance.query_result_cache_gc.batches")
                .with_description("Bounded PostgreSQL query-result cache GC statements")
                .with_unit("{batch}")
                .build(),
            rows_deleted: meter
                .u64_counter("ironrag.maintenance.query_result_cache_gc.rows_deleted")
                .with_description("Expired PostgreSQL query-result cache rows deleted")
                .with_unit("{row}")
                .build(),
            budget_exhausted: meter
                .u64_counter("ironrag.maintenance.query_result_cache_gc.budget_exhausted")
                .with_description("GC sweeps that consumed the complete per-tick batch budget")
                .with_unit("{run}")
                .build(),
            backlog_probe_failures: meter
                .u64_counter("ironrag.maintenance.query_result_cache_gc.backlog_probe_failures")
                .with_description("Failed bounded post-sweep backlog probes")
                .with_unit("{probe}")
                .build(),
            backlog_sample_rows: meter
                .u64_gauge("ironrag.maintenance.query_result_cache_gc.backlog_sample_rows")
                .with_description(
                    "Expired rows remaining in the bounded post-sweep sample; capped at 501",
                )
                .with_unit("{row}")
                .build(),
            oldest_expired_age_seconds: meter
                .f64_gauge("ironrag.maintenance.query_result_cache_gc.oldest_expired_age_seconds")
                .with_description("Seconds the oldest sampled cache winner is past its TTL")
                .with_unit("s")
                .build(),
        }
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct QueryResultCacheGcSweepProgress {
    rows_deleted: u64,
    batches: u64,
    last_batch_was_full: bool,
}

impl QueryResultCacheGcSweepProgress {
    fn record_batch(&mut self, rows_deleted: u64) {
        self.rows_deleted = self.rows_deleted.saturating_add(rows_deleted);
        self.batches = self.batches.saturating_add(1);
        self.last_batch_was_full =
            rows_deleted == u64::try_from(QUERY_RESULT_CACHE_GC_BATCH_LIMIT).unwrap_or(u64::MAX);
    }

    #[must_use]
    const fn budget_exhausted(self) -> bool {
        self.batches == QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK && self.last_batch_was_full
    }
}

fn query_result_cache_gc_max_rows_per_tick() -> u64 {
    u64::try_from(QUERY_RESULT_CACHE_GC_BATCH_LIMIT)
        .unwrap_or(0)
        .saturating_mul(QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK)
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

/// Build a scheduler-task handle for the lifetime of the process.
/// Returns `None` when the role / kill-switch combination means the
/// scheduler must not run.
#[must_use]
pub fn spawn_maintenance_scheduler(
    state: AppState,
    mut shutdown: broadcast::Receiver<()>,
) -> Option<JoinHandle<()>> {
    if !state.settings.runs_maintenance_scheduler() {
        return None;
    }
    let config = SchedulerConfig::from_settings(&state.settings);
    let owner_node = node_owner_identity();
    info!(
        tick_interval_secs = config.tick_interval.as_secs(),
        class_interval_secs = config.class_interval.as_secs(),
        stale_lease_after_secs = config.stale_lease_after.as_secs(),
        stale_provider_call_after_secs = STALE_BILLING_PROVIDER_CALL_AFTER.as_secs(),
        stale_provider_call_batch_limit = STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT,
        query_result_cache_ttl_secs =
            crate::services::query::result_cache::QUERY_RESULT_CACHE_TTL_SECONDS,
        query_result_cache_gc_batch_limit = QUERY_RESULT_CACHE_GC_BATCH_LIMIT,
        query_result_cache_gc_max_batches_per_tick = QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK,
        query_result_cache_gc_max_rows_per_tick = query_result_cache_gc_max_rows_per_tick(),
        query_result_cache_gc_backlog_probe_limit = QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT,
        owner_node = %owner_node,
        "maintenance scheduler starting",
    );
    Some(tokio::spawn(async move {
        // Bootstrap is best-effort — if it fails the next tick will retry.
        if let Err(error) = bootstrap_rows(&state).await {
            warn!(?error, "maintenance scheduler bootstrap failed; will retry on next tick");
        }
        reap_stale_query_executions(&state, config.stale_lease_after).await;
        gc_expired_query_result_cache(&state).await;
        reap_stale_billing_provider_calls(&state).await;
        repair_dirty_billing_execution_costs(&state).await;
        loop {
            tokio::select! {
                _ = shutdown.recv() => {
                    info!("maintenance scheduler received shutdown signal; exiting");
                    return;
                }
                () = sleep(config.tick_interval) => {}
            }
            if let Err(error) = run_tick(&state, &config, &owner_node).await {
                warn!(?error, "maintenance scheduler tick failed; continuing");
            }
        }
    }))
}

/// Configuration captured at scheduler startup; immutable while running.
#[derive(Debug, Clone, Copy)]
struct SchedulerConfig {
    tick_interval: Duration,
    class_interval: Duration,
    stale_lease_after: Duration,
}

impl SchedulerConfig {
    fn from_settings(settings: &crate::app::config::Settings) -> Self {
        Self {
            tick_interval: Duration::from_secs(settings.maintenance_tick_interval_seconds.max(1)),
            class_interval: Duration::from_secs(
                settings.maintenance_class_interval_seconds.max(60),
            ),
            stale_lease_after: Duration::from_secs(
                settings.maintenance_stale_lease_seconds.max(60),
            ),
        }
    }
}

/// Stable identifier for the node running this scheduler. Uses the
/// `HOSTNAME` env so containers report a useful label out of the box.
fn node_owner_identity() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| format!("ironrag-worker-{}", Uuid::now_v7()))
}

async fn bootstrap_rows(state: &AppState) -> anyhow::Result<()> {
    let pool = &state.persistence.postgres;
    let libraries = catalog_repository::list_libraries(pool, None).await?;
    for library in &libraries {
        if let Err(error) = lease::ensure_row(
            pool,
            MaintenanceClass::GcStaleChunks,
            Scope::Library(library.id),
            None,
        )
        .await
        {
            warn!(
                library_id = %library.id,
                class = MaintenanceClass::GcStaleChunks.as_str(),
                ?error,
                "failed to ensure maintenance_job_run row at bootstrap",
            );
        }
    }
    Ok(())
}

async fn run_tick(
    state: &AppState,
    config: &SchedulerConfig,
    owner_node: &str,
) -> anyhow::Result<()> {
    let pool = &state.persistence.postgres;
    match lease::reap_stale_leases(pool, config.stale_lease_after).await {
        Ok(reaped) if reaped > 0 => {
            info!(reaped, "maintenance scheduler reaped stale leases");
        }
        Ok(_) => {}
        Err(error) => warn!(?error, "stale-lease reaper failed; continuing"),
    }
    reap_stale_query_executions(state, config.stale_lease_after).await;
    gc_expired_query_result_cache(state).await;
    reap_stale_billing_provider_calls(state).await;
    repair_dirty_billing_execution_costs(state).await;
    // New libraries show up between bootstrap and this tick — ensure
    // their rows now so they are eligible for acquisition immediately.
    if let Err(error) = bootstrap_rows(state).await {
        warn!(?error, "scheduler tick bootstrap refresh failed");
    }
    process_class(state, config, owner_node, MaintenanceClass::GcStaleChunks).await;
    Ok(())
}

/// Runs one bounded `PostgreSQL` result-cache GC batch.
///
/// The repository applies both the hard batch ceiling and the database-clock
/// TTL predicate. Exposing this one-shot surface keeps integration tests and
/// future operator tooling on the exact scheduler path.
pub async fn gc_expired_query_result_cache_once(
    state: &AppState,
    ttl: Duration,
    batch_limit: i64,
) -> anyhow::Result<u64> {
    Ok(query_result_cache_repository::delete_expired_query_result_cache_batch(
        &state.persistence.postgres,
        ttl.as_secs(),
        batch_limit,
    )
    .await?)
}

async fn gc_expired_query_result_cache(state: &AppState) {
    let metrics = query_result_cache_gc_metrics();
    let ttl =
        Duration::from_secs(crate::services::query::result_cache::QUERY_RESULT_CACHE_TTL_SECONDS);
    let mut progress = QueryResultCacheGcSweepProgress::default();
    let mut status = "success";
    for _ in 0..QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK {
        match gc_expired_query_result_cache_once(state, ttl, QUERY_RESULT_CACHE_GC_BATCH_LIMIT)
            .await
        {
            Ok(batch_rows) => {
                progress.record_batch(batch_rows);
                if !progress.last_batch_was_full {
                    break;
                }
            }
            Err(error) => {
                status = if progress.batches == 0 { "failed" } else { "partial_failure" };
                warn!(
                    ?error,
                    completed_batches = progress.batches,
                    rows_deleted = progress.rows_deleted,
                    ttl_seconds = ttl.as_secs(),
                    batch_limit = QUERY_RESULT_CACHE_GC_BATCH_LIMIT,
                    "PostgreSQL query-result cache GC batch failed; ending this bounded sweep",
                );
                break;
            }
        }
    }
    let budget_exhausted = progress.budget_exhausted();
    let backlog_probe = query_result_cache_repository::probe_expired_query_result_cache_backlog(
        &state.persistence.postgres,
        ttl.as_secs(),
        QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT,
    )
    .await;
    match &backlog_probe {
        Ok(probe) => {
            let attributes = [
                KeyValue::new("has_backlog", probe.sampled_expired_rows > 0),
                KeyValue::new("sample_at_capacity", probe.sample_at_capacity()),
                KeyValue::new("budget_exhausted", budget_exhausted),
            ];
            metrics.backlog_sample_rows.record(probe.sampled_expired_rows, &attributes);
            metrics
                .oldest_expired_age_seconds
                .record(probe.oldest_expired_age_seconds.unwrap_or(0.0), &attributes);
        }
        Err(error) => {
            metrics.backlog_probe_failures.add(1, &[]);
            warn!(
                ?error,
                ttl_seconds = ttl.as_secs(),
                sample_limit = QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT,
                "bounded PostgreSQL query-result cache backlog probe failed",
            );
        }
    }
    metrics.runs.add(1, &[KeyValue::new("status", status)]);
    metrics.batches.add(progress.batches, &[]);
    metrics.rows_deleted.add(progress.rows_deleted, &[]);
    if budget_exhausted {
        metrics.budget_exhausted.add(1, &[]);
    }
    let backlog_observed = backlog_probe.as_ref().is_ok_and(|probe| probe.sampled_expired_rows > 0);
    if progress.rows_deleted > 0 || budget_exhausted || backlog_observed {
        let (backlog_sample_rows, backlog_sample_at_capacity, oldest_expired_age_seconds) =
            backlog_probe.as_ref().map_or((None, None, None), |probe| {
                (
                    Some(probe.sampled_expired_rows),
                    Some(probe.sample_at_capacity()),
                    probe.oldest_expired_age_seconds,
                )
            });
        info!(
            rows_deleted = progress.rows_deleted,
            batches = progress.batches,
            budget_exhausted,
            ?backlog_sample_rows,
            ?backlog_sample_at_capacity,
            ?oldest_expired_age_seconds,
            ttl_seconds = ttl.as_secs(),
            batch_limit = QUERY_RESULT_CACHE_GC_BATCH_LIMIT,
            "maintenance scheduler completed PostgreSQL query-result cache GC sweep",
        );
    }
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
            .canonical_services
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

async fn reap_stale_billing_provider_calls(state: &AppState) {
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
            if report.reaped_provider_calls > 0 {
                if report.rollup_failures == 0 {
                    info!(
                        reaped_provider_calls = report.reaped_provider_calls,
                        affected_executions = report.affected_executions,
                        rollups_refreshed = report.rollups_refreshed,
                        batch_limit = STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT,
                        "maintenance scheduler canceled stale provider-call reservations",
                    );
                } else {
                    warn!(
                        reaped_provider_calls = report.reaped_provider_calls,
                        affected_executions = report.affected_executions,
                        rollups_refreshed = report.rollups_refreshed,
                        rollup_failures = report.rollup_failures,
                        batch_limit = STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT,
                        "maintenance scheduler canceled stale provider-call reservations with rollup repair failures",
                    );
                }
            }
        }
        Err(error) => {
            metrics.runs.add(1, &[KeyValue::new("status", "failed")]);
            warn!(
                ?error,
                stale_after_secs = STALE_BILLING_PROVIDER_CALL_AFTER.as_secs(),
                batch_limit = STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT,
                "stale provider-call reservation reaper failed; continuing",
            );
        }
    }
}

/// Repairs one bounded batch of derived execution-cost generations.
///
/// The durable state row is dirtied in the same transaction as every
/// canonical provider-call mutation. `get_execution_cost` performs a
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
            .canonical_services
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
                    warn!(
                        owning_execution_kind = %repair.owning_execution_kind,
                        owning_execution_id = %repair.owning_execution_id,
                        dirty_generation = repair.dirty_generation,
                        retry_after_secs = backoff.as_secs(),
                        %error,
                        "durable execution-cost rollup repair failed",
                    );
                } else {
                    // Another canonical write or repair advanced the
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

fn dirty_execution_cost_retry_backoff(repair_attempts: i32) -> Duration {
    let exponent = u32::try_from(repair_attempts.clamp(0, 8)).unwrap_or(8);
    let seconds = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
    Duration::from_secs(seconds.min(DIRTY_EXECUTION_COST_MAX_BACKOFF.as_secs()))
}

async fn repair_dirty_billing_execution_costs(state: &AppState) {
    match repair_dirty_billing_execution_costs_once(state, DIRTY_EXECUTION_COST_REPAIR_BATCH_LIMIT)
        .await
    {
        Ok(report) if report.examined > 0 && report.failed == 0 => info!(
            examined = report.examined,
            repaired = report.repaired,
            superseded = report.superseded,
            "maintenance scheduler repaired durable execution-cost rollups",
        ),
        Ok(report) if report.examined > 0 => warn!(
            examined = report.examined,
            repaired = report.repaired,
            failed = report.failed,
            superseded = report.superseded,
            "maintenance scheduler completed execution-cost repair sweep with failures",
        ),
        Ok(_) => {}
        Err(error) => warn!(
            ?error,
            batch_limit = DIRTY_EXECUTION_COST_REPAIR_BATCH_LIMIT,
            "durable execution-cost repair sweep failed; continuing",
        ),
    }
}

async fn reap_stale_query_executions(state: &AppState, stale_after: Duration) {
    let stale_after = ChronoDuration::from_std(stale_after).unwrap_or(ChronoDuration::MAX);
    let stale_before =
        Utc::now().checked_sub_signed(stale_after).unwrap_or(DateTime::<Utc>::MIN_UTC);
    match query_repository::reap_stale_query_executions(
        &state.persistence.postgres,
        stale_before,
        STALE_QUERY_EXECUTION_BATCH_LIMIT,
    )
    .await
    {
        Ok(reaped) if reaped > 0 => {
            info!(reaped, "maintenance scheduler canceled stale query executions");
        }
        Ok(_) => {}
        Err(error) => warn!(?error, "stale query execution reaper failed; continuing"),
    }
}

async fn process_class(
    state: &AppState,
    config: &SchedulerConfig,
    owner_node: &str,
    class: MaintenanceClass,
) {
    let pool = &state.persistence.postgres;
    let run = match lease::acquire_next_due(pool, class, owner_node).await {
        Ok(Some(run)) => run,
        Ok(None) => return,
        Err(error) => {
            warn!(class = class.as_str(), ?error, "lease acquire failed");
            return;
        }
    };
    let heartbeat_handle = spawn_heartbeat(pool.clone(), run.id);
    let dispatch = dispatch(state, class, &run).await;
    heartbeat_handle.abort();
    handle_outcome(state, config, class, run, dispatch).await;
}

fn spawn_heartbeat(pool: PgPool, run_id: Uuid) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            sleep(HEARTBEAT_INTERVAL).await;
            if lease::heartbeat(&pool, run_id).await.is_err() {
                // Heartbeat failure is non-fatal; the reaper will return
                // the lease to pending if the sweeper actually crashes.
                return;
            }
        }
    })
}

/// One concrete sweep outcome for the scheduler. Either the canonical
/// success counts (`rows_removed`, `bytes_reclaimed`), a soft retry
/// signal that should not consume an attempt, or a real failure.
enum DispatchOutcome {
    Ok { rows_removed: i64, bytes_reclaimed: i64 },
    ActiveIngest,
    Failed { error_code: &'static str, error_text: String },
}

async fn dispatch(
    state: &AppState,
    class: MaintenanceClass,
    run: &MaintenanceJobRun,
) -> DispatchOutcome {
    match (class, run.scope()) {
        (MaintenanceClass::GcStaleChunks, Scope::Library(library_id)) => {
            dispatch_gc_stale_chunks(state, library_id).await
        }
        (class, scope) => DispatchOutcome::Failed {
            error_code: "unsupported_class_scope",
            error_text: format!(
                "scheduler dispatch has no implementation for class={} scope={:?}",
                class.as_str(),
                scope,
            ),
        },
    }
}

async fn dispatch_gc_stale_chunks(state: &AppState, library_id: Uuid) -> DispatchOutcome {
    let pool = &state.persistence.postgres;
    // Resolve the workspace_id from the canonical catalog row. A
    // library that vanished between scheduler bootstrap and dispatch
    // (e.g. operator deleted it) shows up here as missing; that is a
    // benign "no work" signal, not a failure.
    let libraries = match catalog_repository::list_libraries(pool, None).await {
        Ok(libraries) => libraries,
        Err(error) => {
            return DispatchOutcome::Failed {
                error_code: "catalog_lookup",
                error_text: error.to_string(),
            };
        }
    };
    let Some(library) = libraries.into_iter().find(|library| library.id == library_id) else {
        return DispatchOutcome::Ok { rows_removed: 0, bytes_reclaimed: 0 };
    };

    match gc::run_for_library(
        state,
        library.workspace_id,
        library.id,
        GcStaleChunksOptions::default(),
    )
    .await
    {
        Ok(report) => DispatchOutcome::Ok { rows_removed: total_rows(&report), bytes_reclaimed: 0 },
        Err(GcStaleChunksError::ActiveIngest { active_jobs, .. }) => {
            info!(
                library_id = %library_id,
                active_jobs,
                "gc.stale-chunks deferred: library has active ingest",
            );
            DispatchOutcome::ActiveIngest
        }
        Err(error) => {
            DispatchOutcome::Failed { error_code: error.code(), error_text: error.to_string() }
        }
    }
}

fn total_rows(report: &LibraryGcReport) -> i64 {
    report.total_rows_removed()
}

async fn handle_outcome(
    state: &AppState,
    config: &SchedulerConfig,
    class: MaintenanceClass,
    run: MaintenanceJobRun,
    outcome: DispatchOutcome,
) {
    let pool = &state.persistence.postgres;
    match outcome {
        DispatchOutcome::Ok { rows_removed, bytes_reclaimed } => {
            if let Err(error) =
                lease::complete(pool, run.id, rows_removed, bytes_reclaimed, config.class_interval)
                    .await
            {
                warn!(class = class.as_str(), run_id = %run.id, ?error, "lease complete failed");
            } else if rows_removed > 0 {
                info!(
                    class = class.as_str(),
                    scope = ?run.scope(),
                    rows_removed,
                    bytes_reclaimed,
                    "maintenance sweep completed",
                );
            }
        }
        DispatchOutcome::ActiveIngest => {
            // Re-queue WITHOUT touching attempts — this is not a real
            // failure. `complete` resets attempts and pushes
            // `next_due_at` out by `class_interval`; pass a short
            // backoff so the busy library is checked again soon.
            if let Err(error) = lease::complete(pool, run.id, 0, 0, ACTIVE_INGEST_RETRY).await {
                warn!(class = class.as_str(), run_id = %run.id, ?error, "active-ingest re-queue failed");
            }
        }
        DispatchOutcome::Failed { error_code, error_text } => {
            match lease::fail(pool, run.id, error_code, &error_text, lease::DEFAULT_MAX_ATTEMPTS)
                .await
            {
                Ok(lease::FailOutcome::Retry { attempts, backoff }) => warn!(
                    class = class.as_str(),
                    scope = ?run.scope(),
                    attempts,
                    backoff_secs = backoff.as_secs(),
                    error_code,
                    error_text,
                    "maintenance sweep failed; re-queued",
                ),
                Ok(lease::FailOutcome::DeadLetter { attempts }) => warn!(
                    class = class.as_str(),
                    scope = ?run.scope(),
                    attempts,
                    error_code,
                    error_text,
                    "maintenance sweep dead-lettered after exhausting attempts",
                ),
                Err(error) => warn!(
                    class = class.as_str(),
                    run_id = %run.id,
                    ?error,
                    "lease fail update failed",
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> crate::app::config::Settings {
        crate::app::config::Settings::from_env().expect("settings defaults should load")
    }

    #[test]
    fn config_clamps_durations_to_safe_minimums() {
        let mut settings = test_settings();
        settings.maintenance_tick_interval_seconds = 0;
        settings.maintenance_class_interval_seconds = 0;
        settings.maintenance_stale_lease_seconds = 0;
        let config = SchedulerConfig::from_settings(&settings);
        assert!(config.tick_interval >= Duration::from_secs(1));
        assert!(config.class_interval >= Duration::from_secs(60));
        assert!(config.stale_lease_after >= Duration::from_secs(60));
    }

    #[test]
    fn config_passes_through_when_above_minimum() {
        let mut settings = test_settings();
        settings.maintenance_tick_interval_seconds = 45;
        settings.maintenance_class_interval_seconds = 7200;
        settings.maintenance_stale_lease_seconds = 600;
        let config = SchedulerConfig::from_settings(&settings);
        assert_eq!(config.tick_interval, Duration::from_secs(45));
        assert_eq!(config.class_interval, Duration::from_secs(7200));
        assert_eq!(config.stale_lease_after, Duration::from_secs(600));
    }

    #[test]
    fn billing_provider_call_reaper_keeps_a_hundred_x_provider_timeout_margin() {
        let hard_provider_timeout_ms = crate::app::state::SEMANTIC_RERANK_HARD_MAX_TIMEOUT_MS;
        assert!(
            STALE_BILLING_PROVIDER_CALL_AFTER.as_millis()
                >= u128::from(hard_provider_timeout_ms) * 100,
        );
        assert_eq!(
            STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT,
            billing_repository::MAX_STALE_PROVIDER_CALL_REAP_BATCH_LIMIT,
        );
    }

    #[test]
    fn query_result_cache_gc_drains_a_stream_faster_than_seventeen_keys_per_second() {
        const TICK_SECONDS: u64 = 30;
        const ARRIVAL_RATE_PER_SECOND: u64 = 18;
        let capacity = query_result_cache_gc_max_rows_per_tick();
        let arrivals_per_tick = ARRIVAL_RATE_PER_SECOND * TICK_SECONDS;
        assert_eq!(capacity, 5_000);
        assert!(
            capacity > arrivals_per_tick,
            "the bounded sweep must sustain more than the old one-batch 16.7 keys/s ceiling",
        );

        // A pre-existing tail drains rather than growing while the stream
        // continues at 18 keys/s. Each step models one canonical 30 s tick.
        let mut backlog = 10_000_u64;
        for _ in 0..3 {
            backlog = backlog.saturating_add(arrivals_per_tick).saturating_sub(capacity);
        }
        assert_eq!(backlog, 0);
    }

    #[test]
    fn query_result_cache_gc_reports_when_the_complete_tick_budget_is_consumed() {
        let batch_limit = u64::try_from(QUERY_RESULT_CACHE_GC_BATCH_LIMIT).unwrap_or(0);
        assert_eq!(batch_limit, 500);
        let mut exhausted = QueryResultCacheGcSweepProgress::default();
        for _ in 0..QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK {
            exhausted.record_batch(batch_limit);
        }
        assert_eq!(exhausted.rows_deleted, query_result_cache_gc_max_rows_per_tick());
        assert!(exhausted.budget_exhausted());

        let mut completed_early = QueryResultCacheGcSweepProgress::default();
        for _ in 0..(QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK - 1) {
            completed_early.record_batch(batch_limit);
        }
        completed_early.record_batch(batch_limit.saturating_sub(1));
        assert!(!completed_early.budget_exhausted());
    }

    #[test]
    fn dirty_execution_cost_repair_backoff_is_bounded() {
        assert_eq!(dirty_execution_cost_retry_backoff(0), Duration::from_secs(1));
        assert_eq!(dirty_execution_cost_retry_backoff(4), Duration::from_secs(16));
        assert_eq!(dirty_execution_cost_retry_backoff(i32::MAX), Duration::from_secs(256),);
        assert!(dirty_execution_cost_retry_backoff(i32::MAX) <= DIRTY_EXECUTION_COST_MAX_BACKOFF);
    }
}
