//! Background scheduler for recurring maintenance sweepers.
//!
//! The scheduler lives in the worker role only (see
//! `Settings::runs_maintenance_scheduler`). Each tick:
//!
//! 1. Reaps `leased` rows whose heartbeat has gone stale.
//! 2. Cancels query executions that outlived the configured turn deadline.
//! 3. Deletes a statement- and row-bounded sweep of expired `PostgreSQL`
//!    query-result winners.
//! 4. Cancels abandoned provider-call reservations and repairs the bounded set
//!    of affected execution-cost rollups.
//! 5. Repairs a bounded batch of durable dirty execution-cost generations.
//! 6. Bootstraps any missing `maintenance_job_run` rows for known
//!    (class, scope) pairs, so a freshly-added library shows up in
//!    the next tick without operator intervention.
//! 7. For every class enabled in this build, atomically leases up to the
//!    class's per-tick batch of due rows and runs the matching sweeper for
//!    each under a tokio task that refreshes the lease heartbeat every 30 s.
//!
//! Rolling-budget cadence: each tick takes a batch of (class, scope) pairs per
//! class, sized so every active row is visited within `class_interval`. The
//! batch is split by cursor population — rows mid-sweep (`cursor_json <> '{}'`)
//! and rows that have not started (`cursor_json = '{}'`) each get
//! `ceil(population / floor(class_interval / tick_interval))` picks per tick
//! (see `walk_quotas`), reduced together to
//! `MAX_MAINTENANCE_ROWS_PER_CLASS_PER_TICK`. Sizing each population from whole
//! ticks keeps the walk guarantee for both: an unconditional in-flight-first
//! ordering let a large mid-sweep set starve the never-started rows. Above the
//! ceiling the walk is best-effort. Vector, runtime-graph and
//! historical-projection classes have independent leases and each receives its
//! own batch; no backlog consumes another's budget.
//!
//! Changed-only walk: a (class, library) row is re-armed by whichever comes
//! first — its own `next_due_at`, which a finished sweep pushes out by the
//! safety `full_pass_interval`, or the library's
//! `catalog_library.source_truth_version` advancing after that row's own
//! `last_started_at`, throttled by the `class_interval` cooldown since its
//! `last_completed_at`. `next_due_at` is therefore the single time-based
//! schedule the acquire gate, the CLI and the backlog metric all read, and it
//! never reports a row as overdue while the fence withholds it. Idle libraries
//! — 99.5 % of the runtime-graph rows on the live instance never removed a row
//! in their lifetime — dominated the old unconditional hourly walk, so the walk
//! now follows the source fence while every row still gets a full pass every
//! `full_pass_interval`. Fencing on the start, not the completion, means a
//! generation bump landing while the row is leased re-arms it on the next tick.
//!
//! `ActiveIngest` is treated as a soft retry signal — the (class,
//! scope) pair is re-queued with a short backoff and `attempts` is NOT
//! incremented, so a library with continuously-in-flight ingest never
//! drifts into dead-letter just because the sweeper could not get the
//! advisory lock.

mod billing;
mod nonterminal_records;
mod query_executions;
mod query_result_cache;
mod vector_lifecycle_inventory;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod observation_tests;

pub use billing::{
    BillingExecutionCostRepairReport, BillingProviderCallReaperReport,
    reap_stale_billing_provider_calls_once, repair_dirty_billing_execution_costs_once,
};
pub use query_result_cache::gc_expired_query_result_cache_once;

use std::time::Duration;

use sqlx::PgPool;
use tokio::{sync::broadcast, task::JoinHandle, time::sleep};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{
        catalog_repository, content_mutation_repository, ingest_repository,
        runtime_graph_repository,
    },
    services::maintenance::{
        gc::{
            self, GcError, GcSupersededVectorsOptions, HistoricalProjectionRetentionGcReport,
            RuntimeGraphGcSchedulerReport, SupersededVectorGcReport,
        },
        lease::{self, MaintenanceClass, MaintenanceJobRun, Scope},
        orphan_runtime_reconcile::{self, ORPHAN_RUNTIME_BATCH_LIMIT, ORPHAN_RUNTIME_STALE_AFTER},
        vector_lifecycle_metrics::{
            GcDispatchError, GcErrorCode, GcRunObservation, GcRunOutcome, GcSkipReason,
            VectorLifecycleInstruments, classify_gc_error,
        },
    },
    shared::cancellation::AbortOnDrop,
};

use billing::{
    STALE_BILLING_PROVIDER_CALL_AFTER, STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT,
    reap_stale_billing_provider_calls, repair_dirty_billing_execution_costs,
};
use query_executions::{reap_stale_query_executions, reconcile_orphan_query_operations};
use query_result_cache::{
    QUERY_RESULT_CACHE_GC_BACKLOG_PROBE_LIMIT, QUERY_RESULT_CACHE_GC_BATCH_LIMIT,
    QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK, gc_expired_query_result_cache,
    query_result_cache_gc_max_rows_per_tick,
};

/// Default heartbeat refresh cadence while a sweeper is running.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Backoff used when the sweeper refuses to run because the library is
/// busy with ingest. Short enough that the next tick within a few
/// minutes can pick the work up once ingest quiesces.
pub const ACTIVE_INGEST_RETRY: Duration = Duration::from_mins(5);

/// Hard ceiling on how many rows one class drains per tick.
///
/// The batch is sized to keep every active row inside its `class_interval`
/// (see [`walk_quotas`]), but a single tick must not become an unbounded unit of
/// work: the ceiling caps a class's per-tick latency and the number of sweeper
/// dispatches a single tick can fire.
pub(crate) const MAX_MAINTENANCE_ROWS_PER_CLASS_PER_TICK: u32 = 64;

/// Per-tick pick targets for a class, split by cursor population.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WalkQuotas {
    in_flight: u32,
    fresh: u32,
}

impl WalkQuotas {
    const fn total(self) -> u32 {
        self.in_flight.saturating_add(self.fresh)
    }
}

/// Picks each cursor population needs per tick so every row in it is visited
/// within `class_interval`.
///
/// A class holds `count` rows due on a `class_interval` cadence, but the
/// scheduler only ticks every `tick_interval`, so a full walk gets
/// `floor(class_interval / tick_interval)` whole ticks and must drain
/// `ceil(count / ticks_per_walk)` rows per tick. Sizing from whole ticks —
/// rather than the raw ratio — keeps the guarantee when the class interval is
/// not an exact multiple of the tick.
///
/// The two populations are sized independently and then reduced together to the
/// hard ceiling: an unconditional in-flight-first ordering let a large mid-sweep
/// set consume every slot and starve the never-started rows, while a plain
/// half-and-half split would leave the dominant population at twice its
/// interval. An empty population asks for no picks, so a class with only one
/// population walks exactly as before. A zero interval degrades to one pick.
fn walk_quotas(
    in_flight: i64,
    fresh: i64,
    tick_interval: Duration,
    class_interval: Duration,
) -> WalkQuotas {
    let tick_nanos = tick_interval.as_nanos().max(1);
    let ticks_per_walk = (class_interval.as_nanos().max(1) / tick_nanos).max(1);
    let picks = |count: i64| -> u32 {
        if count <= 0 {
            return 0;
        }
        u32::try_from((count as u128).div_ceil(ticks_per_walk)).unwrap_or(u32::MAX)
    };
    let mut quotas = WalkQuotas { in_flight: picks(in_flight), fresh: picks(fresh) };
    while quotas.total() > MAX_MAINTENANCE_ROWS_PER_CLASS_PER_TICK {
        if quotas.in_flight >= quotas.fresh && quotas.in_flight > 1 {
            quotas.in_flight -= 1;
        } else if quotas.fresh > 1 {
            quotas.fresh -= 1;
        } else {
            break;
        }
    }
    quotas
}

/// Every durable class the scheduler boots rows for and walks each tick, in
/// dispatch order. One list keeps [`bootstrap_rows`] and [`run_tick`] from
/// drifting from [`MaintenanceClass`] and from [`dispatch_kind`]: a class that
/// is not here is never scheduled, and a class here without a dispatch arm is
/// caught by `scheduled_classes_are_all_dispatched`.
///
/// [`MaintenanceClass::GraphReextractAdmission`] is intentionally absent. It is
/// not a per-library sweeper: it owns one instance-scope row used as a
/// cluster-wide admission/rotation lock, and is driven by
/// `run_graph_backfill_loop` in the ingest worker instead of this scheduler.
/// Adding it here would route it to the `Unsupported` dispatch arm.
const SCHEDULED_CLASSES: [MaintenanceClass; 3] = [
    MaintenanceClass::GcSupersededVectors,
    MaintenanceClass::GcRuntimeGraph,
    MaintenanceClass::GcHistoricalProjection,
];

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
        full_pass_interval_secs = config.full_pass_interval.as_secs(),
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
        let _inventory_sampler =
            vector_lifecycle_inventory::spawn(state.clone(), shutdown.resubscribe());
        let mut historical_preparations_reconciled = false;
        reconcile_historical_preparations_once(&state, &mut historical_preparations_reconciled)
            .await;
        // Bootstrap is best-effort — if it fails the next tick will retry.
        if let Err(error) = bootstrap_rows(&state).await {
            warn!(?error, "maintenance scheduler bootstrap failed; will retry on next tick");
        }
        reap_stale_query_executions(&state, config.stale_lease_after).await;
        reconcile_orphan_query_operations(&state, config.stale_lease_after).await;
        gc_expired_query_result_cache(&state).await;
        reap_stale_billing_provider_calls(&state).await;
        repair_dirty_billing_execution_costs(&state).await;
        reconcile_orphan_content_mutations(&state).await;
        let mut next_nonterminal_sample =
            std::time::Instant::now() + nonterminal_records::NONTERMINAL_RECORD_SAMPLE_INTERVAL;
        loop {
            tokio::select! {
                _ = shutdown.recv() => {
                    info!("maintenance scheduler received shutdown signal; exiting");
                    return;
                }
                () = sleep(config.tick_interval) => {}
            }
            if !historical_preparations_reconciled {
                reconcile_historical_preparations_once(
                    &state,
                    &mut historical_preparations_reconciled,
                )
                .await;
            }
            if std::time::Instant::now() >= next_nonterminal_sample {
                nonterminal_records::sample_and_publish(&state, config.class_interval).await;
                next_nonterminal_sample = std::time::Instant::now()
                    + nonterminal_records::NONTERMINAL_RECORD_SAMPLE_INTERVAL;
            }
            if let Err(error) = run_tick(&state, &config, &owner_node).await {
                warn!(?error, "maintenance scheduler tick failed; continuing");
            }
        }
    }))
}

async fn reconcile_historical_preparations_once(state: &AppState, complete: &mut bool) {
    match ingest_repository::reconcile_terminal_failed_preparations(&state.persistence.postgres)
        .await
    {
        Ok(settled) => {
            if settled > 0 {
                log_reconciled_terminal_failed_preparations(settled);
            }
            *complete =
                settled < u64::from(ingest_repository::TERMINAL_PREPARATION_RECONCILE_BATCH);
        }
        Err(error) => log_terminal_preparation_reconcile_failed(&error),
    }
}

fn log_reconciled_terminal_failed_preparations(settled: u64) {
    info!(settled, "reconciled terminal failed revision preparations");
}

fn log_terminal_preparation_reconcile_failed<E: std::fmt::Debug>(error: &E) {
    warn!(?error, "terminal preparation reconciliation failed; retrying next tick");
}

/// Closes the runtime rows an ingest attempt left `accepted`/`running` or
/// `processing` when the attempt itself already reached a terminal state (or
/// vanished). Bounded and idempotent, so a tick that overlaps a still-running
/// attempt only skips it until the next tick.
async fn reconcile_orphan_runtime_rows(state: &AppState) {
    match orphan_runtime_reconcile::reconcile_orphan_runtime_rows_once(
        &state.persistence.postgres,
        ORPHAN_RUNTIME_STALE_AFTER,
        ORPHAN_RUNTIME_BATCH_LIMIT,
    )
    .await
    {
        Ok(counts) if counts.extractions_terminalized > 0 || counts.executions_terminalized > 0 => {
            log_reconciled_orphan_runtime_rows(&counts);
        }
        Ok(_) => {}
        Err(error) => log_orphan_runtime_reconcile_failed(&error),
    }
}

fn log_reconciled_orphan_runtime_rows(counts: &runtime_graph_repository::OrphanRuntimeCounts) {
    info!(
        extractions = counts.extractions_terminalized,
        executions = counts.executions_terminalized,
        "maintenance scheduler reconciled orphaned runtime rows",
    );
}

fn log_orphan_runtime_reconcile_failed<E: std::fmt::Debug>(error: &E) {
    warn!(?error, "orphan runtime reconcile failed; continuing");
}

/// Look-back before an accepted/running `content_mutation` with no live ingest
/// job is treated as stranded. Wider than the runtime reconciler because a
/// delete admission deletes the document before it finalizes the mutation, its
/// items and its async operation as separate statements.
const ORPHAN_CONTENT_MUTATION_STALE_AFTER: Duration = Duration::from_secs(3600);

/// Maximum `content_mutation` rows settled by one sweep; bounds the transaction.
const ORPHAN_CONTENT_MUTATION_BATCH_LIMIT: i64 = 100;

/// Settles accepted/running `content_mutation` rows stranded after their
/// document was deleted, mirroring passes 3 and 4 of
/// `scripts/ops/reconcile-nonterminal-owners.sql`.
async fn reconcile_orphan_content_mutations(state: &AppState) {
    let stale_before = chrono::Utc::now()
        - chrono::Duration::from_std(ORPHAN_CONTENT_MUTATION_STALE_AFTER)
            .unwrap_or(chrono::Duration::MAX);
    match content_mutation_repository::reconcile_orphan_content_mutations_once(
        &state.persistence.postgres,
        stale_before,
        ORPHAN_CONTENT_MUTATION_BATCH_LIMIT,
    )
    .await
    {
        Ok(counts) if counts.mutations > 0 || counts.items > 0 || counts.operations > 0 => {
            log_reconciled_orphan_content_mutations(&counts);
        }
        Ok(_) => {}
        Err(error) => log_orphan_content_mutation_reconcile_failed(&error),
    }
}

fn log_reconciled_orphan_content_mutations(
    counts: &content_mutation_repository::ContentMutationReconcileCounts,
) {
    info!(
        mutations = counts.mutations,
        items = counts.items,
        operations = counts.operations,
        "maintenance scheduler reconciled orphaned content mutations",
    );
}

fn log_orphan_content_mutation_reconcile_failed<E: std::fmt::Debug>(error: &E) {
    warn!(?error, "orphan content mutation reconcile failed; continuing");
}

/// Configuration captured at scheduler startup; immutable while running.
#[derive(Debug, Clone, Copy)]
struct SchedulerConfig {
    tick_interval: Duration,
    class_interval: Duration,
    stale_lease_after: Duration,
    full_pass_interval: Duration,
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
            full_pass_interval: Duration::from_secs(
                settings.maintenance_full_pass_interval_seconds.max(60),
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
    let retired = lease::retire_obsolete_rows(&state.persistence.postgres).await?;
    if retired > 0 {
        log_retired_obsolete_maintenance_rows(retired);
    }
    for class in SCHEDULED_CLASSES {
        let seeded = lease::ensure_library_rows(&state.persistence.postgres, class).await?;
        if seeded > 0 {
            log_seeded_maintenance_rows(seeded, class);
        }
    }
    Ok(())
}

fn log_retired_obsolete_maintenance_rows(retired: u64) {
    info!(retired, class = "gc.stale-chunks", "retired obsolete maintenance rows");
}

fn log_seeded_maintenance_rows(seeded: u64, class: MaintenanceClass) {
    info!(seeded, class = class.as_str(), "seeded maintenance_job_run rows for new libraries",);
}

async fn run_tick(
    state: &AppState,
    config: &SchedulerConfig,
    owner_node: &str,
) -> anyhow::Result<()> {
    let pool = &state.persistence.postgres;
    reap_stale_leases_for_tick(pool, config.stale_lease_after).await;
    reap_stale_query_executions(state, config.stale_lease_after).await;
    reconcile_orphan_query_operations(state, config.stale_lease_after).await;
    gc_expired_query_result_cache(state).await;
    reap_stale_billing_provider_calls(state).await;
    repair_dirty_billing_execution_costs(state).await;
    reconcile_orphan_runtime_rows(state).await;
    reconcile_orphan_content_mutations(state).await;
    // New libraries show up between bootstrap and this tick — ensure
    // their rows now so they are eligible for acquisition immediately.
    if let Err(error) = bootstrap_rows(state).await {
        log_scheduler_bootstrap_refresh_failed(&error);
    }
    for class in SCHEDULED_CLASSES {
        let quotas = class_walk_quotas(pool, class, config).await;
        process_class(state, config, owner_node, class, quotas).await;
    }
    Ok(())
}

async fn reap_stale_leases_for_tick(pool: &PgPool, stale_lease_after: Duration) {
    match lease::reap_stale_leases(pool, stale_lease_after).await {
        Ok(reaped) => {
            crate::observability::metrics::record_reap_stale_leases(reaped);
            if reaped > 0 {
                log_scheduler_reaped_stale_leases(reaped);
            }
        }
        Err(error) => log_stale_lease_reaper_failed(&error),
    }
}

fn log_scheduler_reaped_stale_leases(reaped: u64) {
    info!(reaped, "maintenance scheduler reaped stale leases");
}

fn log_stale_lease_reaper_failed<E: std::fmt::Debug>(error: &E) {
    warn!(?error, "stale-lease reaper failed; continuing");
}

fn log_scheduler_bootstrap_refresh_failed<E: std::fmt::Debug>(error: &E) {
    warn!(?error, "scheduler tick bootstrap refresh failed");
}

/// Per-tick pick quotas for one class. A cursor-split read failure degrades to
/// one in-flight pick rather than skipping the class, matching the pre-split
/// behaviour.
async fn class_walk_quotas(
    pool: &PgPool,
    class: MaintenanceClass,
    config: &SchedulerConfig,
) -> WalkQuotas {
    match lease::count_active_by_cursor(pool, class).await {
        Ok((in_flight, fresh)) => {
            walk_quotas(in_flight, fresh, config.tick_interval, config.class_interval)
        }
        Err(error) => {
            log_maintenance_class_cursor_split_failed(class, &error);
            WalkQuotas { in_flight: 1, fresh: 0 }
        }
    }
}

fn log_maintenance_class_cursor_split_failed<E: std::fmt::Debug>(
    class: MaintenanceClass,
    error: &E,
) {
    warn!(
        class = class.as_str(),
        ?error,
        "maintenance class cursor split failed; assuming one in-flight row",
    );
}

/// Choose the next cursor population to acquire from, consuming one unit of its
/// remaining quota. Returns `None` when both quotas are spent, which ends the
/// tick's walk.
///
/// The larger remaining quota is served first, so the two populations are
/// interleaved as their budgets shrink and neither is left until last.
fn next_preference(
    in_flight_left: &mut u32,
    fresh_left: &mut u32,
) -> Option<lease::AcquirePreference> {
    if *in_flight_left == 0 && *fresh_left == 0 {
        return None;
    }
    if *in_flight_left >= *fresh_left {
        *in_flight_left -= 1;
        Some(lease::AcquirePreference::InFlight)
    } else {
        *fresh_left -= 1;
        Some(lease::AcquirePreference::Fresh)
    }
}

async fn process_class(
    state: &AppState,
    config: &SchedulerConfig,
    owner_node: &str,
    class: MaintenanceClass,
    quotas: WalkQuotas,
) {
    let pool = &state.persistence.postgres;
    // Sequential, one row at a time: leasing the whole batch up front would
    // starve the heartbeat of rows waiting behind a slow sweeper. A row
    // completed this tick is requeued in the future (`tick_interval` or
    // `class_interval`), so the loop walks distinct rows and terminates.
    //
    // The preference alternates toward whichever population still has quota to
    // spare, so a mid-sweep row and a never-started row each get their share of
    // the tick instead of one order starving the other.
    let (mut in_flight_left, mut fresh_left) = (quotas.in_flight, quotas.fresh);
    for _ in 0..quotas.total() {
        let Some(preference) = next_preference(&mut in_flight_left, &mut fresh_left) else {
            return;
        };
        let run = match lease::acquire_next_due(
            pool,
            class,
            owner_node,
            preference,
            config.class_interval,
        )
        .await
        {
            Ok(Some(run)) => run,
            Ok(None) => return,
            Err(error) => {
                warn!(class = class.as_str(), ?error, "lease acquire failed");
                return;
            }
        };
        let _heartbeat = spawn_heartbeat(pool.clone(), run.id, owner_node.to_owned());
        let dispatch_started = std::time::Instant::now();
        let dispatch = dispatch(state, class, &run, owner_node).await;
        let dispatch_duration = dispatch_started.elapsed();
        handle_outcome(
            state,
            config,
            class,
            owner_node,
            run,
            CompletedDispatch { outcome: dispatch, duration: dispatch_duration },
        )
        .await;
    }
}

/// Run the production lease/dispatch/completion path for one due runtime-graph
/// row. Integration tests use this seam to prove scheduler state transitions;
/// it deliberately does not expose the private dispatch outcome.
#[cfg(feature = "test-support")]
pub async fn process_runtime_graph_class_once_for_test(state: &AppState, owner_node: &str) {
    let config = SchedulerConfig::from_settings(&state.settings);
    let quotas = WalkQuotas { in_flight: 1, fresh: 0 };
    process_class(state, &config, owner_node, MaintenanceClass::GcRuntimeGraph, quotas).await;
}

/// Run one full scheduler tick with the production batch sizing. Integration
/// tests use this seam to prove a single tick drains more than one due row.
#[cfg(feature = "test-support")]
pub async fn run_tick_for_test(state: &AppState, owner_node: &str) {
    let config = SchedulerConfig::from_settings(&state.settings);
    if let Err(error) = run_tick(state, &config, owner_node).await {
        warn!(?error, "maintenance scheduler test tick failed");
    }
}

/// Consecutive heartbeat failures tolerated before the task gives up.
/// A single transient pool/connection hiccup must not permanently stop
/// heartbeating while the sweeper is still running.
const HEARTBEAT_MAX_CONSECUTIVE_FAILURES: u32 = 3;

// The lease-heartbeat task is owned by a shared [`AbortOnDrop`] guard, not a
// bespoke type: it must never outlive the dispatch it renews the lease for, and
// a `JoinHandle` dropped on its own only detaches the task — so a dropped or
// panicked `process_class` future would otherwise keep renewing the lease
// forever. See `crates/shared/cancellation.rs` for the class.

fn spawn_heartbeat(pool: PgPool, run_id: Uuid, owner_node: String) -> AbortOnDrop<()> {
    AbortOnDrop::new(tokio::spawn(async move {
        let mut consecutive_failures = 0u32;
        loop {
            sleep(HEARTBEAT_INTERVAL).await;
            match lease::heartbeat(&pool, run_id, &owner_node).await {
                Ok(()) => consecutive_failures = 0,
                Err(_) => {
                    consecutive_failures += 1;
                    if consecutive_failures >= HEARTBEAT_MAX_CONSECUTIVE_FAILURES {
                        return;
                    }
                }
            }
        }
    }))
}

/// Counts and soft-skip detail carried by one concrete sweep outcome.
#[derive(Debug, Clone, Copy)]
struct GcSweepDetail {
    rows_removed: i64,
    bytes_reclaimed: i64,
    work_remaining: bool,
    candidate_documents: u64,
    deleted_superseded_vectors: u64,
    deleted_null_head_vectors: u64,
}

impl GcSweepDetail {
    const fn no_work() -> Self {
        Self {
            rows_removed: 0,
            bytes_reclaimed: 0,
            work_remaining: false,
            candidate_documents: 0,
            deleted_superseded_vectors: 0,
            deleted_null_head_vectors: 0,
        }
    }
}

/// One concrete sweep outcome for the scheduler. Either the current persisted
/// row success counts, a soft retry signal carrying its real reason, or a real
/// failure with a code from the contract's bounded `error.code` domain — never
/// a raw error string.
enum DispatchOutcome {
    Ok(GcSweepDetail),
    Skipped { reason: GcSkipReason },
    Failed { wire_error_code: &'static str, error_code: GcErrorCode, error_text: String },
}

/// Concrete sweeper a scheduled `(class, scope)` pair routes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DispatchKind {
    SupersededVectors,
    RuntimeGraph,
    HistoricalProjection,
    Unsupported,
}

/// Pure routing for one durable class. Kept separate from [`dispatch`] so a
/// unit test can prove every scheduled class reaches a real sweeper instead of
/// the `Unsupported` fallback without a database.
fn dispatch_kind(class: MaintenanceClass, scope: Scope) -> DispatchKind {
    match (class, scope) {
        (MaintenanceClass::GcSupersededVectors, Scope::Library(_)) => {
            DispatchKind::SupersededVectors
        }
        (MaintenanceClass::GcRuntimeGraph, Scope::Library(_)) => DispatchKind::RuntimeGraph,
        (MaintenanceClass::GcHistoricalProjection, Scope::Library(_)) => {
            DispatchKind::HistoricalProjection
        }
        _ => DispatchKind::Unsupported,
    }
}

async fn dispatch(
    state: &AppState,
    class: MaintenanceClass,
    run: &MaintenanceJobRun,
    owner_node: &str,
) -> DispatchOutcome {
    let scope = run.scope();
    let Scope::Library(library_id) = scope else {
        return unsupported_class_scope(class, scope);
    };
    match dispatch_kind(class, scope) {
        DispatchKind::SupersededVectors => {
            dispatch_gc_superseded_vectors(state, run, owner_node, library_id).await
        }
        DispatchKind::RuntimeGraph => {
            dispatch_gc_runtime_graph(state, run, owner_node, library_id).await
        }
        DispatchKind::HistoricalProjection => {
            dispatch_gc_historical_projection(state, run, owner_node, library_id).await
        }
        DispatchKind::Unsupported => unsupported_class_scope(class, scope),
    }
}

/// The `unsupported_class_scope` failure shared by the scope and class guards.
fn unsupported_class_scope(class: MaintenanceClass, scope: Scope) -> DispatchOutcome {
    DispatchOutcome::Failed {
        wire_error_code: GcErrorCode::UnsupportedClassScope.label(),
        error_code: GcErrorCode::UnsupportedClassScope,
        error_text: format!(
            "scheduler dispatch has no implementation for class={} scope={:?}",
            class.as_str(),
            scope,
        ),
    }
}

/// Preserve the sweeper's existing retry policy and durable error code while
/// normalizing only its telemetry. Superseded-vector busy errors remain fatal;
/// runtime-graph and historical-projection busy errors remain soft requeues.
fn skipped_or_failed(
    error: &GcError,
    library_id: Uuid,
    class: MaintenanceClass,
) -> DispatchOutcome {
    if matches!(error, GcError::LibraryBusy { .. })
        && class != MaintenanceClass::GcSupersededVectors
    {
        info!(
            library_id = %library_id,
            class = class.as_str(),
            "maintenance gc deferred: library catalog row is busy",
        );
        return DispatchOutcome::Skipped { reason: GcSkipReason::LibraryBusy };
    }
    let error_code = match classify_gc_error(error) {
        GcDispatchError::Failed(code) => code,
        GcDispatchError::Skipped(_) => match error {
            GcError::Graph(_) => GcErrorCode::RuntimeGraph,
            _ => GcErrorCode::VectorPlane,
        },
    };
    DispatchOutcome::Failed {
        wire_error_code: error.code(),
        error_code,
        error_text: error.to_string(),
    }
}

async fn dispatch_gc_runtime_graph(
    state: &AppState,
    run: &MaintenanceJobRun,
    owner_node: &str,
    library_id: Uuid,
) -> DispatchOutcome {
    let exists = match sqlx::query_scalar::<_, bool>(
        "select exists (select 1 from catalog_library where id = $1)",
    )
    .bind(library_id)
    .fetch_one(&state.persistence.postgres)
    .await
    {
        Ok(exists) => exists,
        Err(error) => {
            return DispatchOutcome::Failed {
                wire_error_code: GcErrorCode::CatalogLookup.label(),
                error_code: GcErrorCode::CatalogLookup,
                error_text: error.to_string(),
            };
        }
    };
    if !exists {
        return DispatchOutcome::Ok(GcSweepDetail::no_work());
    }

    match gc::run_runtime_graph_scheduler_batch(state, library_id, Some(&run.cursor_json)).await {
        Ok(report) => {
            log_runtime_graph_batch_committed(library_id, &report);
            let next_cursor =
                report.next_cursor_json.clone().unwrap_or_else(|| serde_json::json!({}));
            if let Err(error) =
                lease::update_cursor(&state.persistence.postgres, run.id, owner_node, next_cursor)
                    .await
            {
                return DispatchOutcome::Failed {
                    wire_error_code: GcErrorCode::LeaseCursor.label(),
                    error_code: GcErrorCode::LeaseCursor,
                    error_text: error.to_string(),
                };
            }
            DispatchOutcome::Ok(GcSweepDetail {
                rows_removed: report.total_rows_removed(),
                bytes_reclaimed: 0,
                work_remaining: report.work_remaining,
                candidate_documents: 0,
                deleted_superseded_vectors: 0,
                deleted_null_head_vectors: 0,
            })
        }
        Err(GcError::ActiveIngest { active_jobs, .. }) => {
            log_runtime_graph_deferred_active_ingest(library_id, active_jobs);
            DispatchOutcome::Skipped { reason: GcSkipReason::ActiveIngest }
        }
        Err(error) => skipped_or_failed(&error, library_id, MaintenanceClass::GcRuntimeGraph),
    }
}

fn log_runtime_graph_batch_committed(library_id: Uuid, report: &RuntimeGraphGcSchedulerReport) {
    info!(
        library_id = %library_id,
        candidate_roots = report.candidate_roots,
        nodes_removed = report.nodes_removed,
        edges_removed = report.edges_removed,
        evidence_removed = report.evidence_removed,
        dependent_rows_removed = report.dependent_rows_removed,
        physical_rows_removed = report.physical_rows_removed,
        work_remaining = report.work_remaining,
        "gc.runtime-graph committed bounded phase",
    );
}

fn log_runtime_graph_deferred_active_ingest(library_id: Uuid, active_jobs: i64) {
    info!(
        library_id = %library_id,
        active_jobs,
        "gc.runtime-graph deferred: library has active ingest",
    );
}

async fn dispatch_gc_superseded_vectors(
    state: &AppState,
    run: &MaintenanceJobRun,
    owner_node: &str,
    library_id: Uuid,
) -> DispatchOutcome {
    let pool = &state.persistence.postgres;
    // Resolve the workspace_id from the current catalog row. A
    // library that vanished between scheduler bootstrap and dispatch
    // (e.g. operator deleted it) shows up here as missing; that is a
    // benign "no work" signal, not a failure.
    let libraries = match catalog_repository::list_libraries(pool, None).await {
        Ok(libraries) => libraries,
        Err(error) => {
            return DispatchOutcome::Failed {
                wire_error_code: GcErrorCode::CatalogLookup.label(),
                error_code: GcErrorCode::CatalogLookup,
                error_text: error.to_string(),
            };
        }
    };
    let Some(library) = libraries.into_iter().find(|library| library.id == library_id) else {
        return DispatchOutcome::Ok(GcSweepDetail::no_work());
    };

    match gc::run_superseded_vectors_for_library_with_cursor(
        state,
        library.workspace_id,
        library.id,
        GcSupersededVectorsOptions {
            max_batches: Some(1),
            ..GcSupersededVectorsOptions::default()
        },
        Some(&run.cursor_json),
    )
    .await
    {
        Ok(report) => {
            let next_cursor =
                report.next_cursor_json.clone().unwrap_or_else(|| serde_json::json!({}));
            if let Err(error) = lease::update_cursor(pool, run.id, owner_node, next_cursor).await {
                return DispatchOutcome::Failed {
                    wire_error_code: GcErrorCode::LeaseCursor.label(),
                    error_code: GcErrorCode::LeaseCursor,
                    error_text: error.to_string(),
                };
            }
            DispatchOutcome::Ok(GcSweepDetail {
                rows_removed: total_rows(&report),
                bytes_reclaimed: 0,
                work_remaining: report.work_remaining,
                candidate_documents: count_non_negative(report.documents_with_superseded_vectors),
                deleted_superseded_vectors: count_non_negative(report.superseded_vectors_removed),
                deleted_null_head_vectors: count_non_negative(report.null_head_vectors_removed),
            })
        }
        Err(GcError::ActiveIngest { active_jobs, .. }) => {
            info!(
                library_id = %library_id,
                active_jobs,
                "gc.superseded-vectors deferred: library has active ingest",
            );
            DispatchOutcome::Skipped { reason: GcSkipReason::ActiveIngest }
        }
        Err(error) => skipped_or_failed(&error, library_id, MaintenanceClass::GcSupersededVectors),
    }
}

async fn dispatch_gc_historical_projection(
    state: &AppState,
    _run: &MaintenanceJobRun,
    _owner_node: &str,
    library_id: Uuid,
) -> DispatchOutcome {
    // Resolve the workspace from the current catalog row, exactly as the
    // superseded-vector sweeper does: a library that vanished between
    // scheduler bootstrap and dispatch is a benign "no work" signal.
    let libraries =
        match catalog_repository::list_libraries(&state.persistence.postgres, None).await {
            Ok(libraries) => libraries,
            Err(error) => {
                return DispatchOutcome::Failed {
                    wire_error_code: GcErrorCode::CatalogLookup.label(),
                    error_code: GcErrorCode::CatalogLookup,
                    error_text: error.to_string(),
                };
            }
        };
    let Some(library) = libraries.into_iter().find(|library| library.id == library_id) else {
        return DispatchOutcome::Ok(GcSweepDetail::no_work());
    };

    match gc::run_historical_projection_window_for_library(
        state,
        library.workspace_id,
        library.id,
        false,
    )
    .await
    {
        Ok(report) => {
            log_historical_projection_completed(library_id, &report);
            DispatchOutcome::Ok(GcSweepDetail {
                rows_removed: i64::try_from(report.total_rows_removed()).unwrap_or(i64::MAX),
                bytes_reclaimed: 0,
                work_remaining: false,
                candidate_documents: 0,
                deleted_superseded_vectors: 0,
                deleted_null_head_vectors: 0,
            })
        }
        Err(GcError::ActiveIngest { active_jobs, .. }) => {
            log_historical_projection_deferred_active_ingest(library_id, active_jobs);
            DispatchOutcome::Skipped { reason: GcSkipReason::ActiveIngest }
        }
        Err(error) => {
            skipped_or_failed(&error, library_id, MaintenanceClass::GcHistoricalProjection)
        }
    }
}

fn log_historical_projection_completed(
    library_id: Uuid,
    report: &HistoricalProjectionRetentionGcReport,
) {
    info!(
        library_id = %library_id,
        documents_visited = report.documents_visited,
        documents_purged = report.documents_purged,
        knowledge_revisions_removed = report.knowledge_revisions_removed,
        "gc.historical-projection completed",
    );
}

fn log_historical_projection_deferred_active_ingest(library_id: Uuid, active_jobs: i64) {
    info!(
        library_id = %library_id,
        active_jobs,
        "gc.historical-projection deferred: library has active ingest",
    );
}

fn total_rows(report: &SupersededVectorGcReport) -> i64 {
    report.total_rows_removed()
}

/// Shared identity of the leased row a dispatch outcome applies to.
struct SweepContext<'a> {
    pool: &'a PgPool,
    class: MaintenanceClass,
    owner_node: &'a str,
    run: &'a MaintenanceJobRun,
}

/// A dispatch result plus the wall time it took, carried together so the
/// completion seam does not grow another argument.
struct CompletedDispatch {
    outcome: DispatchOutcome,
    duration: Duration,
}

async fn handle_outcome(
    state: &AppState,
    config: &SchedulerConfig,
    class: MaintenanceClass,
    owner_node: &str,
    run: MaintenanceJobRun,
    completed: CompletedDispatch,
) {
    let library_id = match run.scope() {
        Scope::Library(library_id) => library_id,
        Scope::Instance => Uuid::nil(),
    };
    let context = SweepContext { pool: &state.persistence.postgres, class, owner_node, run: &run };
    let mut observation = GcRunObservation {
        maintenance_class: class.as_str(),
        outcome: GcRunOutcome::Failed,
        skip_reason: None,
        error_code: None,
        candidate_documents: 0,
        deleted_superseded_vectors: 0,
        deleted_null_head_vectors: 0,
        duration: completed.duration,
    };
    match completed.outcome {
        DispatchOutcome::Ok(detail) => {
            if complete_sweep(&context, config, &detail, completed.duration).await {
                observation.outcome = GcRunOutcome::Success;
                observation.candidate_documents = detail.candidate_documents;
                observation.deleted_superseded_vectors = detail.deleted_superseded_vectors;
                observation.deleted_null_head_vectors = detail.deleted_null_head_vectors;
            } else {
                observation.error_code = Some(GcErrorCode::LeaseComplete);
            }
        }
        DispatchOutcome::Skipped { reason } => {
            if requeue_active_ingest(&context, reason).await {
                observation.outcome = GcRunOutcome::from_skip_reason(reason);
                observation.skip_reason = Some(reason);
            } else {
                observation.error_code = Some(GcErrorCode::LeaseRequeue);
            }
        }
        DispatchOutcome::Failed { wire_error_code, error_code, error_text } => {
            let lease_failed = !fail_sweep(&context, wire_error_code, &error_text).await;
            observation.error_code =
                Some(if lease_failed { GcErrorCode::LeaseFail } else { error_code });
        }
    }
    // The metric is aggregate and low-card, so the library scope and the bounded
    // error code are logged as the per-library drill-down the removed metric
    // label used to provide.
    log_dispatch_failure(library_id, class, observation.error_code);
    VectorLifecycleInstruments::global().record_gc_run(&observation);
}

/// Emit the structured failure log for one dispatch, keyed by library and the
/// bounded error code.
fn log_dispatch_failure(
    library_id: Uuid,
    class: MaintenanceClass,
    error_code: Option<GcErrorCode>,
) {
    if let Some(error_code) = error_code {
        warn!(
            library_id = %library_id,
            class = class.as_str(),
            error_code = error_code.label(),
            "vector-lifecycle gc dispatch failed",
        );
    }
}

/// Returns `true` when the durable lease completion succeeded.
async fn complete_sweep(
    context: &SweepContext<'_>,
    config: &SchedulerConfig,
    detail: &GcSweepDetail,
    duration: Duration,
) -> bool {
    // A finished sweep carries the full-pass deadline in `next_due_at`
    // — the gate's time-based re-arm — so the changed-library arm is
    // the only path that runs a row sooner. A sweep that still has work
    // continues on the next tick.
    let next_interval =
        if detail.work_remaining { config.tick_interval } else { config.full_pass_interval };
    match lease::complete(
        context.pool,
        context.run.id,
        context.owner_node,
        detail.rows_removed,
        detail.bytes_reclaimed,
        next_interval,
    )
    .await
    {
        Ok(()) => {
            log_maintenance_sweep_completed(context.class, context.run, detail, duration);
            true
        }
        Err(error) => {
            log_lease_complete_failed(context.class, context.run.id, &error);
            false
        }
    }
}

/// Soft re-queue. Returns `true` when the durable lease completion succeeded.
async fn requeue_active_ingest(context: &SweepContext<'_>, reason: GcSkipReason) -> bool {
    // Re-queue WITHOUT touching attempts — this is not a real
    // failure. `complete` resets attempts; pass a short backoff as the
    // row's `next_due_at` so the busy library is checked again soon.
    match lease::complete(
        context.pool,
        context.run.id,
        context.owner_node,
        0,
        0,
        ACTIVE_INGEST_RETRY,
    )
    .await
    {
        Ok(()) => true,
        Err(error) => {
            match reason {
                GcSkipReason::ActiveIngest => {
                    log_active_ingest_requeue_failed(context.class, context.run.id, &error);
                }
                GcSkipReason::LibraryBusy => {
                    log_library_busy_requeue_failed(context.class, context.run.id, &error);
                }
            }
            false
        }
    }
}

async fn fail_sweep(context: &SweepContext<'_>, error_code_label: &str, error_text: &str) -> bool {
    match lease::fail(
        context.pool,
        context.run.id,
        context.owner_node,
        error_code_label,
        error_text,
        lease::DEFAULT_MAX_ATTEMPTS,
    )
    .await
    {
        Ok(lease::FailOutcome::Retry { attempts, backoff }) => {
            log_maintenance_sweep_requeued(
                context.class,
                context.run,
                attempts,
                backoff,
                error_code_label,
                error_text,
            );
            true
        }
        Ok(lease::FailOutcome::DeadLetter { attempts }) => {
            log_maintenance_sweep_dead_lettered(
                context.class,
                context.run,
                attempts,
                error_code_label,
                error_text,
            );
            true
        }
        Err(error) => {
            log_lease_fail_update_failed(context.class, context.run.id, &error);
            false
        }
    }
}

/// Convert a report count to the unsigned counter type; a negative count is
/// clamped to zero rather than wrapping.
fn count_non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn log_lease_complete_failed<E: std::fmt::Debug>(class: MaintenanceClass, run_id: Uuid, error: &E) {
    warn!(class = class.as_str(), run_id = %run_id, ?error, "lease complete failed");
}

/// Observe every durably completed dispatch, including zero-deletion work.
/// A cursor continuation is not a whole sweep; the two cursor fields keep
/// a zero-deletion batch distinct from a fresh, finished dispatch. Zero
/// deletions alone do not prove that the dispatch did no useful work.
fn log_maintenance_sweep_completed(
    class: MaintenanceClass,
    run: &MaintenanceJobRun,
    detail: &GcSweepDetail,
    duration: Duration,
) {
    info!(
        class = class.as_str(),
        run_id = %run.id,
        scope = ?run.scope(),
        rows_removed = detail.rows_removed,
        bytes_reclaimed = detail.bytes_reclaimed,
        continuing_sweep = run.cursor_json != serde_json::json!({}),
        work_remaining = detail.work_remaining,
        duration_seconds = duration.as_secs_f64(),
        "maintenance sweep completed",
    );
}

fn log_active_ingest_requeue_failed<E: std::fmt::Debug>(
    class: MaintenanceClass,
    run_id: Uuid,
    error: &E,
) {
    warn!(class = class.as_str(), run_id = %run_id, ?error, "active-ingest re-queue failed");
}

fn log_library_busy_requeue_failed<E: std::fmt::Debug>(
    class: MaintenanceClass,
    run_id: Uuid,
    error: &E,
) {
    warn!(class = class.as_str(), run_id = %run_id, ?error, "library-busy re-queue failed");
}

fn log_maintenance_sweep_requeued(
    class: MaintenanceClass,
    run: &MaintenanceJobRun,
    attempts: i32,
    backoff: Duration,
    error_code: &str,
    error_text: &str,
) {
    warn!(
        class = class.as_str(),
        scope = ?run.scope(),
        attempts,
        backoff_secs = backoff.as_secs(),
        error_code,
        error_text,
        "maintenance sweep failed; re-queued",
    );
}

fn log_maintenance_sweep_dead_lettered(
    class: MaintenanceClass,
    run: &MaintenanceJobRun,
    attempts: i32,
    error_code: &str,
    error_text: &str,
) {
    warn!(
        class = class.as_str(),
        scope = ?run.scope(),
        attempts,
        error_code,
        error_text,
        "maintenance sweep dead-lettered after exhausting attempts",
    );
}

fn log_lease_fail_update_failed<E: std::fmt::Debug>(
    class: MaintenanceClass,
    run_id: Uuid,
    error: &E,
) {
    warn!(
        class = class.as_str(),
        run_id = %run_id,
        ?error,
        "lease fail update failed",
    );
}
