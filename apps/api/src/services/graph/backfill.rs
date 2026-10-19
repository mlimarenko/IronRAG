//! Durable self-healing admission for readable document graph state.
//!
//! Root cause this module addresses: the `extract_graph` stage can succeed
//! at the per-chunk level (`runtime_graph_extraction` rows flipped to
//! `ready`) yet still fail at the stage level — typically via the 600-second
//! configured wall-clock timeout on a long document, or a projection write
//! failure. When the stage fails, the worker skips the downstream
//! `reconcile_revision_graph` call, so the extracted entities/relations
//! never become `runtime_graph_node` / `runtime_graph_edge` rows. The
//! dashboard keeps showing the document as "readable" while the graph
//! viewer never learns it exists.
//!
//! The durable periodic scheduler repairs exact-revision active-projection
//! publication gaps backed by a positive current completion, current-version
//! extraction history, or an exact-head interrupted graph stage, then enqueues
//! the standard authority-owning
//! `graph_refresh` job. An unexplained version-coverage gap remains an explicit
//! operator migration, and a valid zero-contribution completion is terminal.
//! The shared dedupe key prevents competing periodic and operator paths from
//! publishing a revision twice.

use anyhow::Context;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{
        self, AUTOMATIC_GRAPH_REEXTRACT_PRIORITY, EXPLICIT_GRAPH_REEXTRACT_PRIORITY,
        GraphReextractSelectionMode,
        ingest_repository::{self, NewIngestJob},
    },
    services::graph::{
        error::GraphServiceError,
        extract::{GRAPH_EXTRACTION_CONTRACT_VERSION, GRAPH_EXTRACTION_VERSION},
    },
};

/// Upper bound on re-extract jobs enqueued per pass. Prevents one pass
/// from saturating the ingest queue and blocking other mutations.
pub(crate) const REEXTRACT_BATCH_SIZE: i64 = 64;

/// Hard ceiling for an operator-triggered re-extract batch. Operators can
/// choose smaller batches, but one request cannot flood the ingest queue with
/// an unbounded library-wide insert.
pub(crate) const REEXTRACT_MAX_BATCH_SIZE: i64 = 256;

/// Number of worker-capacity waves admitted by one periodic pass. A short
/// queue keeps every configured library slot useful without letting repair
/// work monopolize the ingest queue.
const PERIODIC_REEXTRACT_BATCH_WAVES: u32 = 4;

/// Absolute cap for the automatic pass. Operator-triggered repair keeps its
/// separate, larger limit, while background recovery remains conservative.
const PERIODIC_REEXTRACT_MAX_BATCH_SIZE: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PeriodicGraphReextractPolicy {
    pub batch_size: u32,
}

#[must_use]
pub(crate) fn periodic_graph_reextract_policy(
    max_parallel_jobs_per_library: usize,
) -> PeriodicGraphReextractPolicy {
    let library_parallelism =
        u32::try_from(max_parallel_jobs_per_library.max(1)).unwrap_or(u32::MAX);
    PeriodicGraphReextractPolicy {
        batch_size: library_parallelism
            .saturating_mul(PERIODIC_REEXTRACT_BATCH_WAVES)
            .min(PERIODIC_REEXTRACT_MAX_BATCH_SIZE),
    }
}

#[must_use]
const fn cap_periodic_graph_reextract_batch(
    policy: PeriodicGraphReextractPolicy,
    requested_max_batch_size: u32,
) -> u32 {
    if policy.batch_size < requested_max_batch_size {
        policy.batch_size
    } else {
        requested_max_batch_size
    }
}

#[must_use]
pub(crate) fn normalize_graph_reextract_batch_size(requested: Option<u32>) -> i64 {
    requested.map_or(REEXTRACT_BATCH_SIZE, i64::from).clamp(1, REEXTRACT_MAX_BATCH_SIZE)
}

#[must_use]
const fn periodic_graph_reextract_can_admit(has_live_graph_refresh: bool) -> bool {
    !has_live_graph_refresh
}

#[derive(Debug, Clone, Default)]
pub(crate) struct GraphReextractOutcome {
    pub candidates: usize,
    pub enqueued: usize,
    pub skipped_dedupe: usize,
    pub failures: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GraphReextractProgress {
    pub readable_document_count: i64,
    pub known_eligible_document_count: i64,
    pub current_extraction_document_count: i64,
    pub missing_current_extraction_document_count: i64,
    pub outdated_or_incomplete_document_count: i64,
    pub never_extracted_document_count: i64,
    pub queued_job_count: i64,
    pub leased_job_count: i64,
    pub paused_job_count: i64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct GraphReextractBatchResult {
    pub batch_size: i64,
    pub outcome: GraphReextractOutcome,
    pub progress: GraphReextractProgress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GraphReextractCandidate {
    pub workspace_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
}

fn graph_reextract_job(
    library_id: Uuid,
    candidate: GraphReextractCandidate,
    priority: i32,
    now: DateTime<Utc>,
) -> NewIngestJob {
    NewIngestJob {
        workspace_id: candidate.workspace_id,
        library_id,
        mutation_id: None,
        mutation_item_id: None,
        connector_id: None,
        async_operation_id: None,
        knowledge_document_id: Some(candidate.document_id),
        knowledge_revision_id: Some(candidate.revision_id),
        job_kind: "graph_refresh".to_string(),
        queue_state: "queued".to_string(),
        priority,
        dedupe_key: Some(format!(
            "graph_reextract:{GRAPH_EXTRACTION_VERSION}:{}",
            candidate.revision_id
        )),
        queued_at: Some(now),
        available_at: Some(now),
        completed_at: None,
    }
}

pub(crate) async fn enqueue_graph_reextract_jobs(
    state: &AppState,
    library_id: Uuid,
    candidates: &[GraphReextractCandidate],
) -> GraphReextractOutcome {
    enqueue_graph_reextract_jobs_with_priority(
        state,
        library_id,
        candidates,
        EXPLICIT_GRAPH_REEXTRACT_PRIORITY,
        None,
    )
    .await
}

async fn enqueue_graph_reextract_jobs_with_priority(
    state: &AppState,
    library_id: Uuid,
    candidates: &[GraphReextractCandidate],
    priority: i32,
    automatic_global_live_job_cap: Option<u32>,
) -> GraphReextractOutcome {
    let mut outcome =
        GraphReextractOutcome { candidates: candidates.len(), ..GraphReextractOutcome::default() };
    let now = Utc::now();
    for candidate in candidates {
        let job = graph_reextract_job(library_id, *candidate, priority, now);
        let enqueue = match automatic_global_live_job_cap {
            Some(global_cap) => {
                ingest_repository::enqueue_automatic_graph_refresh_job_with_live_dedupe(
                    &state.persistence.postgres,
                    &job,
                    global_cap,
                )
                .await
            }
            None => {
                ingest_repository::enqueue_active_library_ingest_job_with_live_dedupe(
                    &state.persistence.postgres,
                    &job,
                )
                .await
            }
        };
        match enqueue {
            Ok(Some(_)) => outcome.enqueued += 1,
            Ok(None) => outcome.skipped_dedupe += 1,
            Err(error) => {
                outcome.failures += 1;
                tracing::warn!(
                    %library_id,
                    document_id = %candidate.document_id,
                    revision_id = %candidate.revision_id,
                    ?error,
                    "failed to enqueue graph re-extract job"
                );
            }
        }
    }
    outcome
}

pub(crate) async fn load_library_graph_reextract_progress(
    state: &AppState,
    library_id: Uuid,
) -> Result<GraphReextractProgress, GraphServiceError> {
    let row = repositories::get_library_graph_reextract_progress(
        &state.persistence.postgres,
        library_id,
        GRAPH_EXTRACTION_VERSION,
    )
    .await
    .context("failed to load graph re-extract progress")?;
    Ok(GraphReextractProgress {
        readable_document_count: row.ready_document_count,
        known_eligible_document_count: row.known_eligible_document_count,
        current_extraction_document_count: row.current_extraction_document_count,
        missing_current_extraction_document_count: row.missing_current_extraction_document_count,
        outdated_or_incomplete_document_count: row.outdated_or_incomplete_document_count,
        never_extracted_document_count: row.never_extracted_document_count,
        queued_job_count: row.queued_job_count,
        leased_job_count: row.leased_job_count,
        paused_job_count: row.paused_job_count,
    })
}

async fn enqueue_library_graph_reextract_batch(
    state: &AppState,
    library_id: Uuid,
    requested_batch_size: Option<u32>,
    selection_mode: GraphReextractSelectionMode,
    automatic_global_live_job_cap: Option<u32>,
) -> Result<(i64, GraphReextractOutcome), GraphServiceError> {
    let batch_size = normalize_graph_reextract_batch_size(requested_batch_size);
    let candidates = repositories::list_library_documents_needing_graph_reextract_for_version(
        &state.persistence.postgres,
        library_id,
        GRAPH_EXTRACTION_VERSION,
        GRAPH_EXTRACTION_CONTRACT_VERSION,
        selection_mode,
        batch_size,
    )
    .await
    .context("failed to list documents needing graph re-extract")?;

    let candidates = candidates
        .into_iter()
        .map(|(workspace_id, document_id, revision_id)| GraphReextractCandidate {
            workspace_id,
            document_id,
            revision_id,
        })
        .collect::<Vec<_>>();
    let priority = match selection_mode {
        GraphReextractSelectionMode::ProjectionRepair => AUTOMATIC_GRAPH_REEXTRACT_PRIORITY,
        GraphReextractSelectionMode::VersionRecovery { .. } => EXPLICIT_GRAPH_REEXTRACT_PRIORITY,
    };
    let outcome = enqueue_graph_reextract_jobs_with_priority(
        state,
        library_id,
        &candidates,
        priority,
        automatic_global_live_job_cap,
    )
    .await;
    Ok((batch_size, outcome))
}

pub(crate) async fn run_library_graph_reextract_batch(
    state: &AppState,
    library_id: Uuid,
    requested_batch_size: Option<u32>,
    include_never_extracted: bool,
) -> Result<GraphReextractBatchResult, GraphServiceError> {
    let (batch_size, outcome) = enqueue_library_graph_reextract_batch(
        state,
        library_id,
        requested_batch_size,
        GraphReextractSelectionMode::VersionRecovery { include_never_extracted },
        None,
    )
    .await?;
    let progress = load_library_graph_reextract_progress(state, library_id).await?;
    Ok(GraphReextractBatchResult { batch_size, outcome, progress })
}

/// Runs one durable repair pass for `library_id`. It enqueues the shared
/// `graph_refresh` job only for active readable heads lacking exact-revision
/// document-node publication evidence in the active projection whose current
/// extraction is missing/malformed or has a valid positive contribution.
/// Missing/malformed completions additionally need current-version,
/// current-revision extraction history or exact-revision interrupted-stage
/// evidence; an unexplained legacy gap is left for explicit operator migration.
/// Valid zero-contribution completions are terminal. Canceled exact-revision
/// jobs suppress automatic resurrection; terminal automatic jobs observe a
/// durable cooldown and finite dead-letter ceiling. Broader extraction-version
/// migration and overrides remain explicit operator actions.
/// Graph repair must not masquerade as a user content mutation or rewrite
/// content-head ownership.
///
/// The periodic pass waits until the library has no live `graph_refresh` work,
/// then admits at most `requested_max_batch_size` jobs, additionally bounded by
/// the per-library worker-capacity policy. The stable
/// `graph_reextract:{version}:{revision_id}` key remains the race-safe final
/// guard if two enqueue paths observe the empty library queue concurrently. Periodic callers
/// first reserve a durable cluster-wide tick, while the dedupe key still
/// protects against operator or rebuild enqueue paths outside that scheduler.
/// There is deliberately no second process-local time debounce: once the prior
/// batch clears, the next cluster tick may refill the library's bounded worker
/// wave instead of leaving it idle until an unrelated five-minute timer ends.
pub(crate) async fn run_library_graph_reextract(
    state: &AppState,
    library_id: Uuid,
    requested_max_batch_size: u32,
    global_live_job_cap: u32,
) -> Result<GraphReextractOutcome, GraphServiceError> {
    let policy =
        periodic_graph_reextract_policy(state.settings.ingestion_max_parallel_jobs_per_library);
    let batch_size = cap_periodic_graph_reextract_batch(policy, requested_max_batch_size);
    if batch_size == 0 {
        return Ok(GraphReextractOutcome::default());
    }

    let has_live_graph_refresh = ingest_repository::has_live_ingest_jobs_by_kind(
        &state.persistence.postgres,
        library_id,
        "graph_refresh",
    )
    .await
    .context("failed to inspect live graph re-extract jobs")?;
    if !periodic_graph_reextract_can_admit(has_live_graph_refresh) {
        log_graph_reextract_skipped(library_id);
        return Ok(GraphReextractOutcome::default());
    }
    let (_, outcome) = enqueue_library_graph_reextract_batch(
        state,
        library_id,
        Some(batch_size),
        GraphReextractSelectionMode::ProjectionRepair,
        Some(global_live_job_cap),
    )
    .await?;

    log_graph_reextract_completed(library_id, batch_size, requested_max_batch_size, &outcome);

    Ok(outcome)
}

fn log_graph_reextract_skipped(library_id: Uuid) {
    tracing::debug!(
        %library_id,
        "graph re-extract pass skipped while prior graph-refresh batch remains live"
    );
}

fn log_graph_reextract_completed(
    library_id: Uuid,
    batch_size: u32,
    requested_max_batch_size: u32,
    outcome: &GraphReextractOutcome,
) {
    tracing::info!(
        %library_id,
        batch_size,
        requested_max_batch_size,
        selection_mode = "projection_repair",
        candidates = outcome.candidates,
        enqueued = outcome.enqueued,
        skipped_dedupe = outcome.skipped_dedupe,
        failures = outcome.failures,
        "graph re-extract pass completed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reextract_candidate_uses_standard_graph_refresh_job() {
        let library_id = Uuid::now_v7();
        let candidate = GraphReextractCandidate {
            workspace_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
        };

        let job = graph_reextract_job(
            library_id,
            candidate,
            EXPLICIT_GRAPH_REEXTRACT_PRIORITY,
            Utc::now(),
        );

        assert_eq!(job.library_id, library_id);
        assert_eq!(job.knowledge_document_id, Some(candidate.document_id));
        assert_eq!(job.knowledge_revision_id, Some(candidate.revision_id));
        assert_eq!(job.job_kind, "graph_refresh");
        assert_eq!(job.priority, EXPLICIT_GRAPH_REEXTRACT_PRIORITY);
        assert_eq!(
            job.dedupe_key,
            Some(format!("graph_reextract:{GRAPH_EXTRACTION_VERSION}:{}", candidate.revision_id))
        );
    }

    #[test]
    fn operator_reextract_batch_size_is_bounded() {
        assert_eq!(normalize_graph_reextract_batch_size(None), REEXTRACT_BATCH_SIZE);
        assert_eq!(normalize_graph_reextract_batch_size(Some(0)), 1);
        assert_eq!(normalize_graph_reextract_batch_size(Some(17)), 17);
        assert_eq!(normalize_graph_reextract_batch_size(Some(u32::MAX)), REEXTRACT_MAX_BATCH_SIZE);
    }

    #[test]
    fn periodic_reextract_batch_scales_with_library_concurrency_and_is_bounded() {
        assert_eq!(periodic_graph_reextract_policy(1).batch_size, 4);
        assert_eq!(periodic_graph_reextract_policy(2).batch_size, 8);
        assert_eq!(periodic_graph_reextract_policy(4).batch_size, 16);
        assert_eq!(periodic_graph_reextract_policy(128).batch_size, 32);
    }

    #[test]
    fn periodic_reextract_batch_respects_remaining_tick_budget() {
        let policy = periodic_graph_reextract_policy(4);

        assert_eq!(cap_periodic_graph_reextract_batch(policy, 32), 16);
        assert_eq!(cap_periodic_graph_reextract_batch(policy, 3), 3);
        assert_eq!(cap_periodic_graph_reextract_batch(policy, 0), 0);
    }

    #[test]
    fn periodic_reextract_refills_on_the_next_tick_after_the_live_batch_clears() {
        let first_tick_has_live_batch = true;
        let next_tick_has_live_batch = false;

        assert!(!periodic_graph_reextract_can_admit(first_tick_has_live_batch));
        assert!(periodic_graph_reextract_can_admit(next_tick_has_live_batch));
    }
}
