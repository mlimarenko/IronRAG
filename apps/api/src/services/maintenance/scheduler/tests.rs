use super::billing::{
    DIRTY_EXECUTION_COST_MAX_BACKOFF, STALE_BILLING_PROVIDER_CALL_AFTER,
    STALE_BILLING_PROVIDER_CALL_BATCH_LIMIT, dirty_execution_cost_retry_backoff,
};
use super::query_result_cache::{
    QUERY_RESULT_CACHE_GC_BATCH_LIMIT, QUERY_RESULT_CACHE_GC_MAX_BATCHES_PER_TICK,
    QueryResultCacheGcSweepProgress, query_result_cache_gc_max_rows_per_tick,
};
use super::*;
use crate::infra::repositories::billing_repository;

fn test_settings() -> crate::app::config::Settings {
    crate::app::config::Settings::from_env().expect("settings defaults should load")
}

#[test]
fn telemetry_preserves_each_sweepers_busy_retry_policy_and_wire_error() {
    let library_id = Uuid::from_u128(1);
    let error = GcError::LibraryBusy { library_id };
    for class in SCHEDULED_CLASSES {
        let outcome = skipped_or_failed(&error, library_id, class);
        if class == MaintenanceClass::GcSupersededVectors {
            match outcome {
                DispatchOutcome::Failed { wire_error_code, .. } => {
                    assert_eq!(wire_error_code, error.code())
                }
                _ => panic!(
                    "vector telemetry must not turn a fatal busy dispatch into a soft requeue"
                ),
            }
        } else {
            assert!(matches!(
                outcome,
                DispatchOutcome::Skipped { reason: GcSkipReason::LibraryBusy }
            ));
        }
    }
}

#[test]
fn telemetry_normalization_does_not_replace_persisted_runtime_graph_error_codes() {
    for error in [
        GcError::InvalidPhysicalMutationBudget(0),
        GcError::UnsupportedRuntimeGraphCursorVersion { found: 9, expected: 1 },
    ] {
        match skipped_or_failed(&error, Uuid::nil(), MaintenanceClass::GcRuntimeGraph) {
            DispatchOutcome::Failed { wire_error_code, error_code, .. } => {
                assert_eq!(wire_error_code, error.code());
                assert_eq!(error_code, GcErrorCode::RuntimeGraph);
                assert_ne!(wire_error_code, error_code.label());
            }
            _ => panic!("unsupported graph cursors and budgets remain fatal"),
        }
    }
}

#[test]
fn config_clamps_durations_to_safe_minimums() {
    let mut settings = test_settings();
    settings.maintenance_tick_interval_seconds = 0;
    settings.maintenance_class_interval_seconds = 0;
    settings.maintenance_stale_lease_seconds = 0;
    settings.maintenance_full_pass_interval_seconds = 0;
    let config = SchedulerConfig::from_settings(&settings);
    assert!(config.tick_interval >= Duration::from_secs(1));
    assert!(config.class_interval >= Duration::from_secs(60));
    assert!(config.stale_lease_after >= Duration::from_secs(60));
    assert!(config.full_pass_interval >= Duration::from_secs(60));
}

#[test]
fn config_passes_through_when_above_minimum() {
    let mut settings = test_settings();
    settings.maintenance_tick_interval_seconds = 45;
    settings.maintenance_class_interval_seconds = 7200;
    settings.maintenance_stale_lease_seconds = 600;
    settings.maintenance_full_pass_interval_seconds = 172_800;
    let config = SchedulerConfig::from_settings(&settings);
    assert_eq!(config.tick_interval, Duration::from_secs(45));
    assert_eq!(config.class_interval, Duration::from_secs(7200));
    assert_eq!(config.stale_lease_after, Duration::from_secs(600));
    assert_eq!(config.full_pass_interval, Duration::from_secs(172_800));
}

#[test]
fn maintenance_full_pass_interval_defaults_to_one_day() {
    assert_eq!(test_settings().maintenance_full_pass_interval_seconds, 86_400);
}

#[test]
fn billing_provider_call_reaper_keeps_a_hundred_x_provider_timeout_margin() {
    let hard_provider_timeout_ms = crate::app::state::SEMANTIC_RERANK_HARD_MAX_TIMEOUT_MS;
    assert!(
        STALE_BILLING_PROVIDER_CALL_AFTER.as_millis() >= u128::from(hard_provider_timeout_ms) * 100,
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
    // continues at 18 keys/s. Each step models one configured 30 s tick.
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
fn runtime_graph_gc_has_an_explicit_physical_mutation_budget() {
    assert_eq!(gc::RUNTIME_GRAPH_DELETE_MUTATION_BUDGET, 10_000);
}

#[test]
fn dirty_execution_cost_repair_backoff_is_bounded() {
    assert_eq!(dirty_execution_cost_retry_backoff(0), Duration::from_secs(1));
    assert_eq!(dirty_execution_cost_retry_backoff(4), Duration::from_secs(16));
    assert_eq!(dirty_execution_cost_retry_backoff(i32::MAX), Duration::from_secs(256),);
    assert!(dirty_execution_cost_retry_backoff(i32::MAX) <= DIRTY_EXECUTION_COST_MAX_BACKOFF);
}

#[test]
fn scheduled_classes_are_all_dispatched_to_a_real_sweeper() {
    // gc.historical-projection is the class this test exists for: retention
    // only ran from the CLI until it was added to the tick's class list, and a
    // class in the enum but not here is never scheduled.
    assert!(
        SCHEDULED_CLASSES.contains(&MaintenanceClass::GcHistoricalProjection),
        "gc.historical-projection must be scheduled, not CLI-only",
    );
    for class in SCHEDULED_CLASSES {
        assert_ne!(
            dispatch_kind(class, Scope::Library(Uuid::nil())),
            DispatchKind::Unsupported,
            "scheduled class {} has no dispatch arm",
            class.as_str(),
        );
    }
    assert_eq!(
        dispatch_kind(MaintenanceClass::GcHistoricalProjection, Scope::Instance),
        DispatchKind::Unsupported,
        "the retention GC is per-library; instance scope must stay unsupported",
    );
}

#[test]
fn walk_quotas_sizes_from_whole_ticks_when_the_tick_does_not_divide_the_interval() {
    // 100s class interval / 30s tick = 3 whole ticks per walk, so 10 active
    // rows need 4 per tick — the raw ratio 30/100 would size only 3.
    let tick = Duration::from_secs(30);
    let class = Duration::from_secs(100);
    assert_eq!(walk_quotas(10, 0, tick, class), WalkQuotas { in_flight: 4, fresh: 0 });

    // The full-walk guarantee holds below the hard ceiling: ceil(192/3) = 64 =
    // MAX.
    let ticks_per_walk = class.as_secs() / tick.as_secs();
    for in_flight in 1..=192_i64 {
        let quotas = walk_quotas(in_flight, 0, tick, class);
        assert!(
            u64::from(quotas.total()) * ticks_per_walk
                >= u64::try_from(in_flight).unwrap_or(u64::MAX),
            "in_flight={in_flight} quotas={quotas:?} must cover a full class walk",
        );
    }
}

/// The acquire orders the population named by `$3` first, then the oldest due.
/// An unconditional in-flight-first ordering starved the plain `{}` rows — with
/// more mid-sweep rows due than the per-tick batch, every slot went to an
/// in-flight row and a never-started row was never selected.
#[test]
fn acquire_next_due_orders_the_bound_population_first() {
    let sql = crate::services::maintenance::lease::acquire_next_due_sql();
    assert!(sql.contains("state = 'pending'"));
    assert!(sql.contains("next_due_at <= now()"));
    assert!(sql.contains("for update skip locked"));
    assert!(sql.contains("limit 1"));
    assert!(
        sql.contains("((cursor_json <> '{}'::jsonb) = $4) desc"),
        "the preference must be a bound ordering key: {sql}",
    );
    let order = sql.find("order by").expect("order by missing");
    let pref = sql.find("= $4").expect("preference key missing");
    let due = sql.find("next_due_at asc").expect("next_due_at key missing");
    assert!(order < pref && pref < due, "preference first, then next_due_at: {sql}");
}

/// IRG-219: the shared gate is the lease's whole predicate. `next_due_at` is
/// the gate's time-based re-arm and its cooldown is the class interval on the
/// changed-library arm; the acquire statement must not re-AND `next_due_at`, or
/// a deadline the gate reports as due would never lease.
#[test]
fn gate_is_the_single_schedule_and_honours_next_due_at() {
    let gate = crate::services::maintenance::lease::gate_eligible_predicate("$3");
    assert!(gate.contains("next_due_at <= now()"), "the gate owns the time arm: {gate}");
    assert!(
        gate.contains("last_completed_at <= now() - make_interval(secs => $3::double precision)"),
        "the changed-library arm must be throttled by the class interval: {gate}",
    );
    assert!(
        gate.contains("cursor_json = '{}'::jsonb"),
        "the changed arm is for finished rows: {gate}"
    );
    let sql = crate::services::maintenance::lease::acquire_next_due_sql();
    assert_eq!(
        sql.matches("next_due_at <= now()").count(),
        1,
        "next_due_at must be applied exactly once, inside the gate: {sql}",
    );
}

/// The per-tick batch is split by cursor population so a mid-sweep set larger
/// than the batch can no longer starve the never-started rows. Each non-empty
/// population gets `ceil(count / ticks_per_walk)` picks, so both are visited
/// within one `class_interval`; the two are then reduced to the hard ceiling.
#[test]
fn walk_quotas_walks_both_cursor_populations_within_the_class_interval() {
    const TICK: Duration = Duration::from_secs(30);
    const CLASS: Duration = Duration::from_secs(3600);
    const WALK: u64 = 120;

    // Live shape: runtime-graph has 2409 mid-sweep rows and 51 never-started.
    // The old unconditional in-flight-first order gave all 21 slots to the
    // in-flight set and the 51 fresh rows were never picked.
    let graph = walk_quotas(2409, 51, TICK, CLASS);
    assert_eq!((graph.in_flight, graph.fresh), (21, 1));
    assert!(u64::from(graph.in_flight) * WALK >= 2409, "in-flight must fit the walk");
    assert!(u64::from(graph.fresh) * WALK >= 51, "fresh rows must be picked at all");

    // Plain dominance: a few mid-sweep rows must still make progress.
    let vectors = walk_quotas(4, 2338, TICK, CLASS);
    assert_eq!((vectors.in_flight, vectors.fresh), (1, 20));
    assert!(u64::from(vectors.fresh) * WALK >= 2338);

    // A class with one population walks exactly as before.
    let historical = walk_quotas(0, 2341, TICK, CLASS);
    assert_eq!((historical.in_flight, historical.fresh), (0, 20));
    assert!(u64::from(historical.fresh) * WALK >= 2341);

    // Empty class: no picks, no divide-by-zero.
    assert_eq!(walk_quotas(0, 0, TICK, CLASS), WalkQuotas { in_flight: 0, fresh: 0 });
    // Zero interval degrades to one pick per non-empty population.
    assert_eq!(walk_quotas(3, 0, Duration::ZERO, CLASS), WalkQuotas { in_flight: 1, fresh: 0 });
    // The hard ceiling bounds the summed batch.
    let huge = walk_quotas(i64::MAX, i64::MAX, TICK, CLASS);
    assert!(huge.total() <= MAX_MAINTENANCE_ROWS_PER_CLASS_PER_TICK);
}

/// The walk interleaves the two populations as their quotas shrink and stops
/// only once both are spent, so neither is served after its budget runs out.
#[test]
fn next_preference_interleaves_both_populations_until_both_are_spent() {
    use crate::services::maintenance::lease::AcquirePreference::{Fresh, InFlight};

    // Live runtime-graph shape: 21 in-flight, 1 fresh. The single fresh pick
    // must appear inside the tick, not be deferred past every in-flight pick.
    let (mut in_flight, mut fresh) = (21u32, 1u32);
    let mut picks = Vec::new();
    while let Some(preference) = next_preference(&mut in_flight, &mut fresh) {
        picks.push(preference);
    }
    assert_eq!(picks.len(), 22);
    assert_eq!(picks.iter().filter(|p| **p == Fresh).count(), 1);
    assert_eq!(picks.iter().filter(|p| **p == InFlight).count(), 21);
    assert!(picks.contains(&Fresh), "the fresh quota must actually be consumed");

    // Plain dominance: 1 in-flight, 20 fresh — the mid-sweep row is not starved.
    let (mut in_flight, mut fresh) = (1u32, 20u32);
    let mut picks = Vec::new();
    while let Some(preference) = next_preference(&mut in_flight, &mut fresh) {
        picks.push(preference);
    }
    assert_eq!(picks.len(), 21);
    assert_eq!(picks.iter().filter(|p| **p == InFlight).count(), 1);
    assert_eq!(picks.iter().filter(|p| **p == Fresh).count(), 20);

    // Empty quotas end the walk immediately.
    assert_eq!(next_preference(&mut 0, &mut 0), None);
}

/// The per-tick batch sizes from live rows only, split by cursor population: a
/// dead-lettered row is not active work and must not inflate either quota.
#[test]
fn count_active_by_cursor_sql_excludes_dead_letter_rows_and_splits_the_populations() {
    let sql = crate::services::maintenance::lease::COUNT_ACTIVE_BY_CURSOR_SQL;
    assert!(sql.contains("state <> 'dead_letter'"), "{sql}");
    assert!(sql.contains("class = $1"), "{sql}");
    assert!(sql.contains("cursor_json <> '{}'::jsonb"), "in-flight population missing: {sql}");
    assert!(sql.contains("cursor_json = '{}'::jsonb"), "fresh population missing: {sql}");
}

/// The state summary must cover only the live states. `completed` and `failed`
/// are write-dead Postgres enum variants with no Rust writer.
#[test]
fn count_by_state_sql_covers_only_live_states() {
    let sql = crate::services::maintenance::lease::COUNT_BY_STATE_SQL;
    assert!(sql.contains("'pending'"), "{sql}");
    assert!(sql.contains("'leased'"), "{sql}");
    assert!(sql.contains("'dead_letter'"), "{sql}");
    assert!(!sql.contains("'completed'"), "{sql}");
    assert!(!sql.contains("'failed'"), "{sql}");

    let counts =
        crate::services::maintenance::lease::StateCounts { pending: 1, leased: 2, dead_letter: 3 };
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.leased, 2);
    assert_eq!(counts.dead_letter, 3);
}

/// Dropping the guard must abort the heartbeat task. A `JoinHandle` dropped on
/// its own merely detaches the task, which is how a heartbeat could keep
/// renewing a lease forever after the dispatch future was dropped or panicked.
#[tokio::test]
async fn heartbeat_guard_aborts_its_task_on_drop() {
    struct DropMarker(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let future_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let guard = {
        let marker = DropMarker(std::sync::Arc::clone(&future_dropped));
        AbortOnDrop::new(tokio::spawn(async move {
            let _marker = marker;
            std::future::pending::<()>().await;
        }))
    };
    assert!(
        !future_dropped.load(std::sync::atomic::Ordering::Relaxed),
        "the heartbeat future must be alive before the guard is dropped",
    );

    drop(guard);

    let aborted = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !future_dropped.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(aborted.is_ok(), "dropping the heartbeat guard must abort its task");
}
