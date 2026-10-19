use std::{future::Future, sync::Arc, time::Duration};

use tokio::{sync::broadcast, time};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{catalog_repository, ingest_repository},
};

/// How often the lease-recovery sweep runs to reclaim stale leases.
const INGEST_LEASE_RECOVERY_INTERVAL: Duration = Duration::from_secs(15);
// Steady-state stale-lease threshold. Was 120s; that is 8× the heartbeat
// interval and lets the dispatcher self-deadlock for two minutes after a
// worker crashes. 60s = 4× heartbeat, still safe against transient DB
// latency, and gets the queue moving again much faster.
const INGEST_STALE_LEASE_SECONDS: i64 = 60;
/// Aggressive threshold used **only** for the one-shot sweep that runs when
/// the worker pool boots. At pool startup we know nothing in this process is
/// currently holding a lease, so any `leased` row older than two heartbeat
/// intervals is guaranteed to be orphaned by a previous process that crashed
/// or was restarted before it could finalize. We pick a threshold well above
/// the heartbeat interval (`INGEST_HEARTBEAT_INTERVAL`) so a healthy
/// sibling worker in a multi-worker deployment is never falsely reclaimed.
const INGEST_STARTUP_LEASE_RECOVERY_SECONDS: i64 = 30;

/// One-shot startup reclamation. Runs synchronously before the dispatcher
/// starts claiming new jobs so the first thing the new process does is flush
/// any orphaned `leased` rows from the crashed/restarted predecessor. Uses a
/// shorter threshold than the steady-state loop because at boot we know this
/// process holds zero active leases — two missed heartbeats is already enough
/// evidence that the old owner is gone.
pub(super) async fn reclaim_orphaned_leases_on_startup(state: &Arc<AppState>) {
    let threshold = chrono::Duration::seconds(INGEST_STARTUP_LEASE_RECOVERY_SECONDS);
    match ingest_repository::recover_stale_ingest_leases(&state.persistence.postgres, threshold)
        .await
    {
        Ok(recovery) => log_startup_lease_recovery(&recovery),
        Err(error) => {
            error!(
                ?error,
                "startup lease sweep failed; dispatcher will proceed and rely on the periodic recovery loop"
            );
        }
    }
}

/// Cluster-wide durable graph-repair admission interval.
const GRAPH_BACKFILL_TICK: Duration = Duration::from_mins(2);
const GRAPH_MAINTENANCE_TICK: Duration = Duration::from_mins(5);
/// Opportunistic per-library projection reconciliation remains bounded so one
/// large drifted library cannot monopolize a graph-maintenance pass.
const DOCUMENT_FILE_NAME_REPAIR_BATCH_SIZE: u32 = 200;
const ACTIVE_GRAPH_MAINTENANCE_LIBRARIES_QUERY: &str = "select workspace_id, id
     from catalog_library
     where lifecycle_state = 'active'::catalog_library_lifecycle_state
     order by id";

/// Maximum number of graph repair candidates admitted by one scheduler tick.
/// The queue orders operator-visible `queue_rank` before `priority`, so even
/// low-priority repair jobs can sit ahead of foreground work that arrives
/// later. Keeping the repair burst to at most one bounded global worker wave
/// limits that head-of-line delay.
const GRAPH_REEXTRACT_GLOBAL_TICK_MAX: u32 = 32;
/// Bounds database probes even when every inspected library produces only
/// dedupes or enqueue failures and therefore consumes no job-admission budget.
const GRAPH_REEXTRACT_LIBRARY_SCAN_MAX: usize = 32;
/// Bounds candidate insert attempts separately from successfully admitted jobs.
/// Dedupe conflicts and failed inserts return enqueue budget to later libraries,
/// but cannot turn one tick into an unbounded database-write retry sweep.
const GRAPH_REEXTRACT_CANDIDATE_ATTEMPT_MAX: u32 = 128;

#[must_use]
fn graph_reextract_tick_budget(max_parallel_jobs_global: usize) -> u32 {
    u32::try_from(max_parallel_jobs_global.max(1))
        .unwrap_or(u32::MAX)
        .min(GRAPH_REEXTRACT_GLOBAL_TICK_MAX)
}

/// Pure per-tick admission state. `start_index` comes from the database-backed
/// cluster rotation reservation. The scan advances past every inspected
/// library, including a library skipped because it already has live repair
/// work. The database live-job check and stable dedupe key remain the final
/// race guard for operator-triggered enqueue paths.
struct GraphReextractTickAdmission {
    library_count: usize,
    start_index: usize,
    visited: usize,
    scan_limit: usize,
    remaining_enqueue_budget: u32,
    remaining_candidate_attempt_budget: u32,
}

impl GraphReextractTickAdmission {
    fn new(library_count: usize, start_index: usize, budget: u32) -> Self {
        let start_index = if library_count == 0 { 0 } else { start_index % library_count };
        Self {
            library_count,
            start_index,
            visited: 0,
            scan_limit: library_count.min(GRAPH_REEXTRACT_LIBRARY_SCAN_MAX),
            remaining_enqueue_budget: budget,
            remaining_candidate_attempt_budget: GRAPH_REEXTRACT_CANDIDATE_ATTEMPT_MAX,
        }
    }

    fn next_library(&mut self, requested_per_library: u32) -> Option<(usize, u32)> {
        if self.visited >= self.scan_limit
            || self.remaining_enqueue_budget == 0
            || self.remaining_candidate_attempt_budget == 0
            || requested_per_library == 0
        {
            return None;
        }
        let library_index = self.start_index.wrapping_add(self.visited) % self.library_count;
        self.visited += 1;
        Some((
            library_index,
            requested_per_library
                .min(self.remaining_enqueue_budget)
                .min(self.remaining_candidate_attempt_budget),
        ))
    }

    fn record_outcome(
        &mut self,
        outcome: &crate::services::graph::backfill::GraphReextractOutcome,
    ) {
        let enqueued = u32::try_from(outcome.enqueued).unwrap_or(u32::MAX);
        let attempted = u32::try_from(outcome.candidates).unwrap_or(u32::MAX);
        self.remaining_enqueue_budget = self.remaining_enqueue_budget.saturating_sub(enqueued);
        self.remaining_candidate_attempt_budget =
            self.remaining_candidate_attempt_budget.saturating_sub(attempted);
    }

    #[must_use]
    const fn remaining(&self) -> u32 {
        self.remaining_enqueue_budget
    }

    #[must_use]
    fn last_inspected_index(&self) -> Option<usize> {
        self.visited
            .checked_sub(1)
            .map(|offset| self.start_index.wrapping_add(offset) % self.library_count)
    }
}

pub(super) async fn run_graph_backfill_loop(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
) {
    info!("starting graph backfill loop");
    loop {
        if !wait_for_graph_backfill_tick(&mut shutdown).await {
            break;
        }
        run_graph_backfill_tick(&state).await;
    }
}

async fn wait_for_graph_backfill_tick(shutdown: &mut broadcast::Receiver<()>) -> bool {
    tokio::select! {
        _ = shutdown.recv() => {
            info!("stopping graph backfill loop");
            false
        }
        () = time::sleep(GRAPH_BACKFILL_TICK) => true,
    }
}

async fn run_graph_backfill_tick(state: &AppState) {
    let libraries =
        match catalog_repository::list_active_library_ids(&state.persistence.postgres).await {
            Ok(rows) => rows,
            Err(error) => {
                warn!(?error, "graph backfill loop failed to list libraries");
                return;
            }
        };
    let tick_budget =
        graph_reextract_tick_budget(state.settings.ingestion_max_parallel_jobs_global);
    let Some(reservation) = reserve_graph_reextract_tick(state, &libraries, tick_budget).await
    else {
        return;
    };
    let admission =
        admit_graph_reextract_libraries(state, &libraries, &reservation, tick_budget).await;
    complete_graph_reextract_tick(state, &libraries, reservation, &admission, tick_budget).await;
}

async fn reserve_graph_reextract_tick(
    state: &AppState,
    libraries: &[Uuid],
    tick_budget: u32,
) -> Option<ingest_repository::GraphReextractAdmissionReservation> {
    // Commit the cluster reservation on the dedicated scheduler pool before scanning libraries.
    match ingest_repository::try_reserve_graph_reextract_admission_tick(
        &state.persistence.scheduler_postgres,
        libraries,
        GRAPH_BACKFILL_TICK,
        tick_budget,
    )
    .await
    {
        Ok(Some(reservation)) => Some(reservation),
        Ok(None) => {
            log_graph_reextract_reservation_busy();
            None
        }
        Err(error) => {
            warn!(?error, "graph re-extract tick skipped after admission lock failure");
            None
        }
    }
}

fn log_graph_reextract_reservation_busy() {
    tracing::debug!(
        "graph re-extract tick skipped because the cluster admission window is already reserved"
    );
}

async fn admit_graph_reextract_libraries(
    state: &AppState,
    libraries: &[Uuid],
    reservation: &ingest_repository::GraphReextractAdmissionReservation,
    tick_budget: u32,
) -> GraphReextractTickAdmission {
    let per_library_policy = crate::services::graph::backfill::periodic_graph_reextract_policy(
        state.settings.ingestion_max_parallel_jobs_per_library,
    );
    let mut admission = GraphReextractTickAdmission::new(
        libraries.len(),
        reservation.start_index,
        reservation.remaining_capacity,
    );
    while let Some((library_index, requested_batch_size)) =
        admission.next_library(per_library_policy.batch_size)
    {
        let library_id = libraries[library_index];
        // The callee suppresses a new batch while this library has live graph-refresh work.
        match crate::services::graph::backfill::run_library_graph_reextract(
            state,
            library_id,
            requested_batch_size,
            tick_budget,
        )
        .await
        {
            Ok(outcome) => admission.record_outcome(&outcome),
            Err(error) => warn!(%library_id, ?error, "graph re-extract tick failed"),
        }
    }
    admission
}

async fn complete_graph_reextract_tick(
    state: &AppState,
    libraries: &[Uuid],
    reservation: ingest_repository::GraphReextractAdmissionReservation,
    admission: &GraphReextractTickAdmission,
    tick_budget: u32,
) {
    log_graph_reextract_admission(&reservation, admission, tick_budget);
    let last_inspected_library_id = admission
        .last_inspected_index()
        .and_then(|library_index| libraries.get(library_index))
        .copied();
    match ingest_repository::complete_graph_reextract_admission_tick(
        &state.persistence.scheduler_postgres,
        reservation.token,
        last_inspected_library_id,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            log_graph_reextract_reservation_changed();
        }
        Err(error) => warn!(?error, "failed to persist graph re-extract cursor completion"),
    }
}

fn log_graph_reextract_reservation_changed() {
    tracing::debug!("graph re-extract cursor completion skipped after reservation changed");
}

fn log_graph_reextract_admission(
    reservation: &ingest_repository::GraphReextractAdmissionReservation,
    admission: &GraphReextractTickAdmission,
    tick_budget: u32,
) {
    let enqueued_jobs = reservation.remaining_capacity.saturating_sub(admission.remaining());
    tracing::debug!(
        tick_budget,
        available_tick_budget = reservation.remaining_capacity,
        enqueued_jobs,
        start_library_index = reservation.start_index,
        "graph re-extract tick admission completed"
    );
}

fn log_startup_lease_recovery(recovery: &ingest_repository::StaleIngestLeaseRecovery) {
    if recovery.requeued == 0 && recovery.orphaned == 0 {
        log_startup_no_orphaned_leases();
        return;
    }
    warn!(
        requeued = recovery.requeued,
        orphaned = recovery.orphaned,
        threshold_seconds = INGEST_STARTUP_LEASE_RECOVERY_SECONDS,
        "startup lease sweep: reclaimed orphaned ingest leases after worker pool boot"
    );
}

fn log_startup_no_orphaned_leases() {
    info!("startup lease sweep: no orphaned ingest leases to reclaim");
}

pub(super) async fn run_graph_maintenance_loop(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
) {
    info!("starting graph maintenance loop");
    // Tokio intervals deliver their first tick immediately. A restarted worker
    // therefore begins draining durable file-name drift without waiting five
    // minutes, while `Skip` prevents a slow multi-library pass from replaying a
    // burst of missed ticks.
    let mut maintenance_tick = graph_maintenance_interval();
    while wait_for_graph_maintenance_tick(&mut shutdown, &mut maintenance_tick).await {
        run_graph_maintenance_tick(&state).await;
    }
}

async fn wait_for_graph_maintenance_tick(
    shutdown: &mut broadcast::Receiver<()>,
    maintenance_tick: &mut time::Interval,
) -> bool {
    tokio::select! {
        _ = shutdown.recv() => {
            info!("stopping graph maintenance loop");
            false
        }
        _ = maintenance_tick.tick() => true,
    }
}

fn graph_maintenance_interval() -> time::Interval {
    let mut interval = time::interval(GRAPH_MAINTENANCE_TICK);
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    interval
}

async fn run_graph_maintenance_tick(state: &AppState) {
    let libraries =
        match sqlx::query_as::<_, (Uuid, Uuid)>(ACTIVE_GRAPH_MAINTENANCE_LIBRARIES_QUERY)
            .fetch_all(&state.persistence.postgres)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                warn!(?error, "graph maintenance loop failed to list active libraries");
                return;
            }
        };

    run_graph_maintenance_phases(
        || repair_missing_document_file_names(state, &libraries),
        || ingest_queue_is_idle(state),
        || run_idle_graph_community_maintenance(state, &libraries),
    )
    .await;
}

/// Runs cheap bounded projection repair before consulting the global ingest
/// idle gate. A busy job in another workspace can therefore delay only the
/// expensive community work, never this exact-scoped reconciliation.
async fn run_graph_maintenance_phases<
    Repair,
    RepairFuture,
    Idle,
    IdleFuture,
    Community,
    CommunityFuture,
>(
    repair: Repair,
    idle: Idle,
    community: Community,
) -> bool
where
    Repair: FnOnce() -> RepairFuture,
    RepairFuture: Future<Output = ()>,
    Idle: FnOnce() -> IdleFuture,
    IdleFuture: Future<Output = bool>,
    Community: FnOnce() -> CommunityFuture,
    CommunityFuture: Future<Output = ()>,
{
    repair().await;
    if !idle().await {
        return false;
    }
    community().await;
    true
}

async fn repair_missing_document_file_names(state: &AppState, libraries: &[(Uuid, Uuid)]) {
    for &(workspace_id, library_id) in libraries {
        run_document_file_name_repair(
            workspace_id,
            library_id,
            |workspace_id, library_id, limit| {
                state.document_store.repair_missing_document_file_names(
                    workspace_id,
                    library_id,
                    limit,
                )
            },
        )
        .await;
    }
}

async fn run_document_file_name_repair<Repair, RepairFuture>(
    workspace_id: Uuid,
    library_id: Uuid,
    repair: Repair,
) where
    Repair: FnOnce(Uuid, Uuid, u32) -> RepairFuture,
    RepairFuture: Future<Output = anyhow::Result<u64>>,
{
    match repair(workspace_id, library_id, DOCUMENT_FILE_NAME_REPAIR_BATCH_SIZE).await {
        Ok(0) => {}
        Ok(repaired) => log_document_file_name_repair(workspace_id, library_id, repaired),
        Err(error) => warn!(
            %workspace_id,
            %library_id,
            ?error,
            "document file-name projection maintenance failed",
        ),
    }
}

fn log_document_file_name_repair(workspace_id: Uuid, library_id: Uuid, repaired: u64) {
    info!(%workspace_id, %library_id, repaired, "repaired missing document projection file names");
}

async fn run_idle_graph_community_maintenance(state: &AppState, libraries: &[(Uuid, Uuid)]) {
    for &(_, library_id) in libraries {
        if crate::services::graph::maintenance::try_acquire_graph_maintenance_slot(library_id) {
            run_graph_community_maintenance_pass(state, library_id).await;
        }
    }
}

async fn ingest_queue_is_idle(state: &AppState) -> bool {
    if !state.worker_runtime.is_idle().await {
        return false;
    }

    match sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint from ingest_job where queue_state in ('queued', 'leased')",
    )
    .fetch_one(&state.persistence.heartbeat_postgres)
    .await
    {
        Ok(in_flight_jobs) => in_flight_jobs == 0,
        Err(error) => {
            warn!(?error, "failed to inspect ingest queue before idle graph tick");
            false
        }
    }
}

async fn run_graph_community_maintenance_pass(state: &AppState, library_id: Uuid) {
    // Detection and summaries are deterministic over the live projection, so a
    // pass whose output already matches it would only rewrite identical rows
    // at full node/edge-scan cost. Probe failures fall through to the pass:
    // running redundantly is safe, silently never converging is not.
    if graph_community_pass_converged(state, library_id).await {
        return;
    }

    if let Err(error) =
        crate::services::graph::community_detection::detect_after_ingestion(state, library_id).await
    {
        warn!(%library_id, ?error, "community detection maintenance failed");
        return;
    }

    generate_graph_community_summaries(state, library_id).await;
}

async fn graph_community_pass_converged(state: &AppState, library_id: Uuid) -> bool {
    match crate::services::graph::community_detection::community_pass_converged(state, library_id)
        .await
    {
        Ok(converged) => converged,
        Err(error) => {
            warn!(%library_id, ?error, "community convergence probe failed");
            false
        }
    }
}

async fn generate_graph_community_summaries(state: &AppState, library_id: Uuid) {
    if let Err(error) =
        crate::services::graph::community_detection::generate_community_summaries(state, library_id)
            .await
    {
        warn!(%library_id, ?error, "community summary maintenance failed");
    }
}

pub(super) async fn run_ingest_lease_recovery_loop(
    state: Arc<AppState>,
    mut shutdown: broadcast::Receiver<()>,
) {
    info!("starting ingest lease recovery loop");
    while wait_for_ingest_lease_recovery_tick(&mut shutdown).await {
        recover_ingest_leases(&state).await;
    }
}

async fn wait_for_ingest_lease_recovery_tick(shutdown: &mut broadcast::Receiver<()>) -> bool {
    tokio::select! {
        _ = shutdown.recv() => {
            info!("stopping ingest lease recovery loop");
            false
        }
        () = time::sleep(INGEST_LEASE_RECOVERY_INTERVAL) => true,
    }
}

async fn recover_ingest_leases(state: &AppState) {
    let threshold = chrono::Duration::seconds(INGEST_STALE_LEASE_SECONDS);
    match ingest_repository::recover_stale_ingest_leases(&state.persistence.postgres, threshold)
        .await
    {
        Ok(recovery) => {
            crate::observability::metrics::record_lease_recovery(
                crate::observability::metrics::LeaseReaper::Tokio,
                crate::observability::metrics::LeaseRecoveryKind::Requeued,
                recovery.requeued,
            );
            crate::observability::metrics::record_lease_recovery(
                crate::observability::metrics::LeaseReaper::Tokio,
                crate::observability::metrics::LeaseRecoveryKind::OrphanedAttempt,
                recovery.orphaned,
            );
            log_recovered_ingest_leases(&recovery);
        }
        Err(error) => warn!(?error, "failed to recover stale ingest leases"),
    }
}

fn log_recovered_ingest_leases(recovery: &ingest_repository::StaleIngestLeaseRecovery) {
    if recovery.requeued > 0 || recovery.orphaned > 0 {
        warn!(
            requeued = recovery.requeued,
            orphaned = recovery.orphaned,
            "recovered stale ingest job leases"
        );
    }
}

/// External stale-lease reaper running on a dedicated OS thread with a
/// minimal `current_thread` tokio runtime. This exists to survive the
/// failure mode where the main runtime becomes fully saturated by
/// CPU-bound ingest work and every in-tokio control task (heartbeat,
/// cancellation poll, periodic recovery loop above) starves. Because
/// this thread is an OS thread outside the main runtime, the scheduler
/// always gives it CPU and it always gets to run its reaper query —
/// releasing orphaned leases even when the worker has gone hot-stuck
/// in `merge_chunk_graph_candidates` or similar.
///
/// Uses the dedicated `heartbeat_postgres` pool so it never contends
/// with the main pool the stuck worker is holding.
pub(super) fn spawn_external_stale_lease_reaper(heartbeat_pool: sqlx::PgPool) {
    const REAP_TICK: Duration = Duration::from_secs(15);
    const REAP_THRESHOLD_SECS: i64 = 90;
    let spawn_result = std::thread::Builder::new()
        .name("ironrag-lease-reaper".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(error) => {
                    error!(%error, "external stale-lease reaper failed to start runtime");
                    return;
                }
            };
            runtime.block_on(async move {
                info!(
                    tick_secs = REAP_TICK.as_secs(),
                    threshold_secs = REAP_THRESHOLD_SECS,
                    "external stale-lease reaper running on dedicated OS thread",
                );
                loop {
                    tokio::time::sleep(REAP_TICK).await;
                    let threshold = chrono::Duration::seconds(REAP_THRESHOLD_SECS);
                    match ingest_repository::recover_stale_ingest_leases(
                        &heartbeat_pool,
                        threshold,
                    )
                    .await
                    {
                        Ok(recovery) => {
                            crate::observability::metrics::record_lease_recovery(
                                crate::observability::metrics::LeaseReaper::External,
                                crate::observability::metrics::LeaseRecoveryKind::Requeued,
                                recovery.requeued,
                            );
                            crate::observability::metrics::record_lease_recovery(
                                crate::observability::metrics::LeaseReaper::External,
                                crate::observability::metrics::LeaseRecoveryKind::OrphanedAttempt,
                                recovery.orphaned,
                            );
                            if recovery.requeued > 0 || recovery.orphaned > 0 {
                                warn!(
                                    requeued = recovery.requeued,
                                    orphaned = recovery.orphaned,
                                    "external reaper released stale ingest job leases — the main tokio runtime was likely saturated, investigate merge/reconcile for hot loops"
                                );
                            }
                        }
                        Err(error) => {
                            warn!(?error, "external stale-lease reaper query failed");
                        }
                    }
                }
            });
        });
    if let Err(error) = spawn_result {
        error!(
            %error,
            "failed to spawn external stale-lease reaper thread; ingest lease recovery will still run on the tokio side, but the dedicated OS-thread safety net is disabled until the worker restarts"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn consume_full_requested_batch(
        admission: &mut GraphReextractTickAdmission,
        requested_per_library: u32,
    ) -> u32 {
        let mut enqueued = 0;
        while let Some((_, requested)) = admission.next_library(requested_per_library) {
            enqueued += requested;
            admission.record_outcome(&crate::services::graph::backfill::GraphReextractOutcome {
                candidates: requested as usize,
                enqueued: requested as usize,
                ..crate::services::graph::backfill::GraphReextractOutcome::default()
            });
        }
        enqueued
    }

    #[tokio::test(start_paused = true)]
    async fn graph_maintenance_interval_runs_immediately_then_waits_for_the_next_period() {
        let mut interval = graph_maintenance_interval();

        tokio::time::timeout(Duration::from_millis(1), interval.tick())
            .await
            .expect("the first maintenance tick must be immediately ready");
        assert!(
            tokio::time::timeout(Duration::from_millis(1), interval.tick()).await.is_err(),
            "the next tick must not spin before the maintenance period"
        );
        tokio::time::advance(GRAPH_MAINTENANCE_TICK).await;
        tokio::time::timeout(Duration::from_millis(1), interval.tick())
            .await
            .expect("the periodic maintenance tick did not become ready");
    }

    #[tokio::test]
    async fn busy_ingest_still_drains_one_exact_scoped_file_name_batch_per_tick() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let repaired_scopes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let phases = Arc::new(std::sync::Mutex::new(Vec::new()));

        for _ in 0..2 {
            let repair_scopes = Arc::clone(&repaired_scopes);
            let repair_phases = Arc::clone(&phases);
            let idle_phases = Arc::clone(&phases);
            let community_phases = Arc::clone(&phases);
            let community_ran = run_graph_maintenance_phases(
                move || async move {
                    repair_phases
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push("repair");
                    run_document_file_name_repair(
                        workspace_id,
                        library_id,
                        move |workspace_id, library_id, limit| async move {
                            repair_scopes
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push((workspace_id, library_id, limit));
                            Ok(u64::from(limit))
                        },
                    )
                    .await;
                },
                move || async move {
                    idle_phases
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push("busy");
                    false
                },
                move || async move {
                    community_phases
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push("community");
                },
            )
            .await;
            assert!(!community_ran);
        }

        assert_eq!(
            *repaired_scopes.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![
                (workspace_id, library_id, DOCUMENT_FILE_NAME_REPAIR_BATCH_SIZE),
                (workspace_id, library_id, DOCUMENT_FILE_NAME_REPAIR_BATCH_SIZE),
            ],
            "each busy tick must make another bounded exact-scope repair step"
        );
        assert_eq!(
            *phases.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["repair", "busy", "repair", "busy"],
            "the global busy gate must be consulted only after projection repair"
        );
        assert_eq!(DOCUMENT_FILE_NAME_REPAIR_BATCH_SIZE, 200);
        assert!(ACTIVE_GRAPH_MAINTENANCE_LIBRARIES_QUERY.contains("select workspace_id, id"));
        assert!(ACTIVE_GRAPH_MAINTENANCE_LIBRARIES_QUERY.contains("lifecycle_state = 'active'"));
    }

    #[tokio::test]
    async fn idle_tick_runs_community_only_after_file_name_repair() {
        let phases = Arc::new(std::sync::Mutex::new(Vec::new()));
        let repair_phases = Arc::clone(&phases);
        let idle_phases = Arc::clone(&phases);
        let community_phases = Arc::clone(&phases);
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();

        let community_ran = run_graph_maintenance_phases(
            move || async move {
                repair_phases
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("repair");
                run_document_file_name_repair(workspace_id, library_id, |_, _, _| async {
                    Err(anyhow::anyhow!("synthetic repair failure"))
                })
                .await;
            },
            move || async move {
                idle_phases.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("idle");
                true
            },
            move || async move {
                community_phases
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("community");
            },
        )
        .await;

        assert!(community_ran);
        assert_eq!(
            *phases.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["repair", "idle", "community"],
            "a scoped repair failure must not suppress idle-only community maintenance"
        );
    }

    #[test]
    fn one_hundred_libraries_never_exceed_global_reextract_tick_budget() {
        let mut admission =
            GraphReextractTickAdmission::new(100, 0, graph_reextract_tick_budget(64));

        let enqueued = consume_full_requested_batch(&mut admission, 32);

        assert_eq!(enqueued, GRAPH_REEXTRACT_GLOBAL_TICK_MAX);
        assert_eq!(admission.remaining(), 0);
    }

    #[test]
    fn reextract_tick_budget_tracks_global_concurrency_up_to_the_repair_ceiling() {
        assert_eq!(graph_reextract_tick_budget(0), 1);
        assert_eq!(graph_reextract_tick_budget(7), 7);
        assert_eq!(graph_reextract_tick_budget(usize::MAX), GRAPH_REEXTRACT_GLOBAL_TICK_MAX);
    }

    #[test]
    fn durable_rotation_start_selects_late_libraries_after_replica_restarts() {
        let mut visited = vec![false; 100];
        for durable_start_index in 0..100 {
            // Constructing fresh state models alternating replicas or process
            // restarts; only the database-backed start position survives.
            let mut restarted_replica =
                GraphReextractTickAdmission::new(100, durable_start_index, 1);
            let (library_index, requested) = restarted_replica.next_library(32).unwrap();
            restarted_replica.record_outcome(
                &crate::services::graph::backfill::GraphReextractOutcome {
                    candidates: requested as usize,
                    enqueued: requested as usize,
                    ..crate::services::graph::backfill::GraphReextractOutcome::default()
                },
            );
            visited[library_index] = true;
        }

        assert!(visited.into_iter().all(|was_visited| was_visited));
    }

    #[test]
    fn remaining_reextract_budget_caps_the_next_library_and_stops_the_loop() {
        let mut admission = GraphReextractTickAdmission::new(100, 0, 5);

        let (_, first_requested) = admission.next_library(4).unwrap();
        admission.record_outcome(&crate::services::graph::backfill::GraphReextractOutcome {
            candidates: first_requested as usize,
            enqueued: first_requested as usize,
            ..crate::services::graph::backfill::GraphReextractOutcome::default()
        });
        let (_, second_requested) = admission.next_library(4).unwrap();
        admission.record_outcome(&crate::services::graph::backfill::GraphReextractOutcome {
            candidates: second_requested as usize,
            enqueued: second_requested as usize,
            ..crate::services::graph::backfill::GraphReextractOutcome::default()
        });

        assert_eq!(first_requested, 4);
        assert_eq!(second_requested, 1);
        assert!(admission.next_library(4).is_none());
    }

    #[test]
    fn dedupes_and_failures_redistribute_enqueue_budget_with_a_bounded_scan() {
        let mut admission = GraphReextractTickAdmission::new(100, 0, 5);
        let mut inspected = Vec::new();

        while let Some((library_index, requested)) = admission.next_library(4) {
            inspected.push(library_index);
            let actually_enqueued = match library_index {
                0 | 1 => 0,
                2 => 3,
                3 => 2,
                _ => requested as usize,
            };
            admission.record_outcome(&crate::services::graph::backfill::GraphReextractOutcome {
                candidates: requested as usize,
                enqueued: actually_enqueued,
                skipped_dedupe: requested as usize - actually_enqueued,
                ..crate::services::graph::backfill::GraphReextractOutcome::default()
            });
        }

        assert_eq!(inspected, vec![0, 1, 2, 3]);
        assert_eq!(admission.remaining(), 0);
        assert_eq!(admission.last_inspected_index(), Some(3));

        let mut all_failed = GraphReextractTickAdmission::new(100, 0, 32);
        let mut failed_scan_count = 0;
        while let Some((_, requested)) = all_failed.next_library(8) {
            failed_scan_count += 1;
            all_failed.record_outcome(&crate::services::graph::backfill::GraphReextractOutcome {
                candidates: requested as usize,
                failures: requested as usize,
                ..crate::services::graph::backfill::GraphReextractOutcome::default()
            });
        }
        assert_eq!(failed_scan_count, 16);
        assert_eq!(all_failed.remaining(), 32);
        assert_eq!(all_failed.remaining_candidate_attempt_budget, 0);

        let mut all_empty = GraphReextractTickAdmission::new(100, 0, 5);
        let mut empty_scan_count = 0;
        while all_empty.next_library(8).is_some() {
            empty_scan_count += 1;
            all_empty.record_outcome(
                &crate::services::graph::backfill::GraphReextractOutcome::default(),
            );
        }
        assert_eq!(empty_scan_count, GRAPH_REEXTRACT_LIBRARY_SCAN_MAX);
        assert_eq!(
            all_empty.remaining_candidate_attempt_budget,
            GRAPH_REEXTRACT_CANDIDATE_ATTEMPT_MAX
        );
    }

    #[test]
    fn full_first_library_budget_advances_one_start_per_restarted_tick() {
        let mut next_start_index = 0;
        let mut starts = Vec::new();

        for _ in 0..100 {
            let mut restarted_replica = GraphReextractTickAdmission::new(100, next_start_index, 32);
            let (library_index, requested) = restarted_replica.next_library(32).unwrap();
            restarted_replica.record_outcome(
                &crate::services::graph::backfill::GraphReextractOutcome {
                    candidates: requested as usize,
                    enqueued: requested as usize,
                    ..crate::services::graph::backfill::GraphReextractOutcome::default()
                },
            );
            starts.push(library_index);
            next_start_index =
                restarted_replica.last_inspected_index().map_or(0, |index| (index + 1) % 100);
        }

        assert_eq!(starts, (0..100).collect::<Vec<_>>());
        assert_eq!(starts.last(), Some(&99));
    }

    #[test]
    fn bounded_failed_scans_cover_one_hundred_libraries_across_restarted_ticks() {
        let mut next_start_index = 0;
        let mut visited = vec![false; 100];

        for _ in 0..7 {
            let mut restarted_replica = GraphReextractTickAdmission::new(100, next_start_index, 32);
            while let Some((library_index, requested)) = restarted_replica.next_library(8) {
                visited[library_index] = true;
                restarted_replica.record_outcome(
                    &crate::services::graph::backfill::GraphReextractOutcome {
                        candidates: requested as usize,
                        failures: requested as usize,
                        ..crate::services::graph::backfill::GraphReextractOutcome::default()
                    },
                );
            }
            next_start_index =
                restarted_replica.last_inspected_index().map_or(0, |index| (index + 1) % 100);
        }

        assert!(visited.into_iter().all(|was_visited| was_visited));
    }
}
