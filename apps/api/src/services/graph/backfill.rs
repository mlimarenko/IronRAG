//! Self-healing pass that reconciles documents whose LLM graph extraction
//! finished but whose `reconcile_revision_graph` never produced a document
//! node in the projection.
//!
//! Root cause this module addresses: the `extract_graph` stage can succeed
//! at the per-chunk level (`runtime_graph_extraction` rows flipped to
//! `ready`) yet still fail at the stage level — typically via the 600-second
//! canonical wall-clock timeout on a long document, or a projection write
//! failure. When the stage fails, the worker skips the downstream
//! `reconcile_revision_graph` call, so the extracted entities/relations
//! never become `runtime_graph_node` / `runtime_graph_edge` rows. The
//! dashboard keeps showing the document as "readable" while the graph
//! viewer never learns it exists.
//!
//! The backfill pass queries documents in this exact state and replays
//! `reconcile_revision_graph` against the already-persisted extraction
//! records. No LLM call is made — the merge runs purely over the `ready`
//! records in Postgres. Typically cheap (milliseconds per document).
//!
//! # Debounce
//!
//! Library-wide passes cost O(missing-docs × reconcile-cost). Under a full
//! queue burst every finishing job would kick another pass while the
//! previous pass is still walking the same set of documents. A dedicated
//! 60 s slot per library compresses that burst; the slot is independent of
//! the maintenance slot so heavy maintenance work does not starve
//! backfill, and vice versa.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::Context;
use chrono::Utc;
use futures::stream::{self, StreamExt};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{
        self,
        ingest_repository::{self, NewIngestJob},
    },
};

/// Minimum wall-clock gap between two backfill passes on the same library.
/// Chosen to comfortably clear a mid-sized missing-node batch (a few hundred
/// documents × low-millisecond reconcile per doc) before the next burst.
pub const BACKFILL_INTERVAL: Duration = Duration::from_secs(60);

/// Upper bound on documents reconciled per pass. Bounded so one backfill
/// cannot starve the rest of the worker loop; follow-up passes pick up any
/// documents that exceed the limit on the next window.
pub const BACKFILL_BATCH_SIZE: i64 = 256;

/// Concurrent reconcile fan-out inside a single pass. Reconcile is I/O
/// bound (Postgres round-trips; the LLM is NOT invoked) so a small fan-out
/// amortises round-trip latency without stressing the pool.
pub const BACKFILL_CONCURRENCY: usize = 4;

fn last_run() -> &'static Mutex<HashMap<Uuid, Instant>> {
    static LAST_RUN: OnceLock<Mutex<HashMap<Uuid, Instant>>> = OnceLock::new();
    LAST_RUN.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns `true` when the caller has been granted the backfill slot for
/// `library_id` in the current window. The caller MUST run the backfill
/// pass — there is no explicit release; the slot becomes available again
/// once [`BACKFILL_INTERVAL`] has elapsed.
#[must_use]
pub fn try_acquire_graph_backfill_slot(library_id: Uuid) -> bool {
    let now = Instant::now();
    let mut guard = last_run().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.get(&library_id) {
        Some(last) if now.duration_since(*last) < BACKFILL_INTERVAL => false,
        _ => {
            guard.insert(library_id, now);
            true
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GraphBackfillOutcome {
    pub candidates: usize,
    pub reconciled: usize,
    pub produced_graph: usize,
    pub failures: usize,
}

/// Runs one backfill pass for `library_id`. Idempotent: documents that are
/// already in the graph are not revisited, and documents where the replay
/// still yields zero contributions will reappear on subsequent passes
/// until extraction produces graph content (or they are handled via a
/// terminal marker in a follow-up change).
pub async fn run_library_graph_backfill(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<GraphBackfillOutcome> {
    let candidates = repositories::list_library_documents_missing_graph_node(
        &state.persistence.postgres,
        library_id,
        BACKFILL_BATCH_SIZE,
    )
    .await
    .context("failed to list documents missing graph node for backfill")?;

    if candidates.is_empty() {
        return Ok(GraphBackfillOutcome::default());
    }

    tracing::info!(
        %library_id,
        candidates = candidates.len(),
        "graph backfill pass: replaying reconcile for documents missing graph node"
    );

    let total = candidates.len();
    let results: Vec<Result<bool, anyhow::Error>> =
        stream::iter(candidates.into_iter().map(|(document_id, revision_id)| {
            let graph_service = state.canonical_services.graph.clone();
            async move {
                let outcome = graph_service
                    .reconcile_revision_graph(state, library_id, document_id, revision_id, None)
                    .await
                    .with_context(|| {
                        format!(
                            "graph backfill reconcile failed for document {document_id} \
                             revision {revision_id}"
                        )
                    })?;
                Ok::<bool, anyhow::Error>(outcome.graph_contribution_count > 0)
            }
        }))
        .buffer_unordered(BACKFILL_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    let mut outcome = GraphBackfillOutcome { candidates: total, ..GraphBackfillOutcome::default() };
    for result in results {
        match result {
            Ok(true) => {
                outcome.reconciled += 1;
                outcome.produced_graph += 1;
            }
            Ok(false) => {
                outcome.reconciled += 1;
            }
            Err(error) => {
                outcome.failures += 1;
                tracing::warn!(%library_id, ?error, "graph backfill reconcile failed for document");
            }
        }
    }

    tracing::info!(
        %library_id,
        candidates = outcome.candidates,
        reconciled = outcome.reconciled,
        produced_graph = outcome.produced_graph,
        failures = outcome.failures,
        "graph backfill pass completed"
    );

    Ok(outcome)
}

/// Process-local debounce state for the graph re-extract pass. Separate
/// from the backfill slot because the two passes solve different problems
/// on different time horizons: backfill is cheap (no LLM), re-extract is
/// expensive (new LLM calls per doc).
fn last_reextract_run() -> &'static Mutex<HashMap<Uuid, Instant>> {
    static LAST_RUN: OnceLock<Mutex<HashMap<Uuid, Instant>>> = OnceLock::new();
    LAST_RUN.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Minimum wall-clock gap between two re-extract passes on the same
/// library. Larger than backfill because the downstream jobs spend LLM
/// budget — a tight retry loop on a systematically failing document would
/// be expensive.
pub const REEXTRACT_INTERVAL: Duration = Duration::from_secs(300);

/// Upper bound on re-extract jobs enqueued per pass. Prevents one pass
/// from saturating the ingest queue and blocking other mutations.
pub const REEXTRACT_BATCH_SIZE: i64 = 64;

/// Returns `true` when the caller has been granted the re-extract slot
/// for `library_id` in the current window.
#[must_use]
pub fn try_acquire_graph_reextract_slot(library_id: Uuid) -> bool {
    let now = Instant::now();
    let mut guard = last_reextract_run().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.get(&library_id) {
        Some(last) if now.duration_since(*last) < REEXTRACT_INTERVAL => false,
        _ => {
            guard.insert(library_id, now);
            true
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GraphReextractOutcome {
    pub candidates: usize,
    pub enqueued: usize,
    pub skipped_dedupe: usize,
    pub failures: usize,
}

/// Runs one re-extract pass for `library_id`. Finds documents whose active
/// revision has NO extraction record (World B — orphaned on revision
/// transition) and enqueues a canonical `content_mutation` job so the
/// ingest worker replays the full pipeline against the current revision.
///
/// Loop protection is provided by the unique
/// `idx_ingest_job_dedupe_key` index on `(library_id, dedupe_key)`:
/// the pass derives a stable `graph_reextract:{revision_id}` key, so if
/// an earlier pass already enqueued a job for the same revision (whether
/// still queued, leased, completed, or failed) the insert fails with a
/// unique violation and the pass moves on. A failed prior run therefore
/// does not re-trigger the LLM — ops can inspect and retry manually.
pub async fn run_library_graph_reextract(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<GraphReextractOutcome> {
    let candidates = repositories::list_library_documents_needing_graph_reextract(
        &state.persistence.postgres,
        library_id,
        REEXTRACT_BATCH_SIZE,
    )
    .await
    .context("failed to list documents needing graph re-extract")?;

    if candidates.is_empty() {
        return Ok(GraphReextractOutcome::default());
    }

    tracing::info!(
        %library_id,
        candidates = candidates.len(),
        "graph re-extract pass: enqueueing content_mutation jobs for orphaned active revisions"
    );

    let total = candidates.len();
    let mut outcome =
        GraphReextractOutcome { candidates: total, ..GraphReextractOutcome::default() };
    let now = Utc::now();
    for (workspace_id, document_id, revision_id) in candidates {
        let dedupe_key = Some(format!("graph_reextract:{revision_id}"));
        let job = NewIngestJob {
            workspace_id,
            library_id,
            mutation_id: None,
            connector_id: None,
            async_operation_id: None,
            knowledge_document_id: Some(document_id),
            knowledge_revision_id: Some(revision_id),
            job_kind: "content_mutation".to_string(),
            queue_state: "queued".to_string(),
            priority: 200,
            dedupe_key,
            queued_at: Some(now),
            available_at: Some(now),
            completed_at: None,
        };
        match ingest_repository::create_ingest_job(&state.persistence.postgres, &job).await {
            Ok(_) => outcome.enqueued += 1,
            Err(error) => {
                if matches!(&error, sqlx::Error::Database(db)
                    if db.constraint() == Some("idx_ingest_job_dedupe_key"))
                {
                    outcome.skipped_dedupe += 1;
                } else {
                    outcome.failures += 1;
                    tracing::warn!(
                        %library_id,
                        %document_id,
                        %revision_id,
                        ?error,
                        "failed to enqueue graph re-extract job"
                    );
                }
            }
        }
    }

    tracing::info!(
        %library_id,
        candidates = outcome.candidates,
        enqueued = outcome.enqueued,
        skipped_dedupe = outcome.skipped_dedupe,
        failures = outcome.failures,
        "graph re-extract pass completed"
    );

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_caller_acquires_backfill_slot() {
        let library_id = Uuid::now_v7();
        assert!(try_acquire_graph_backfill_slot(library_id));
    }

    #[test]
    fn second_caller_in_same_window_is_rejected() {
        let library_id = Uuid::now_v7();
        assert!(try_acquire_graph_backfill_slot(library_id));
        assert!(!try_acquire_graph_backfill_slot(library_id));
    }

    #[test]
    fn distinct_libraries_do_not_contend() {
        let library_a = Uuid::now_v7();
        let library_b = Uuid::now_v7();
        assert!(try_acquire_graph_backfill_slot(library_a));
        assert!(try_acquire_graph_backfill_slot(library_b));
    }
}
