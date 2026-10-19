//! `repair.*` operator-driven recovery sweepers.
//!
//! Unlike `gc.*` (which removes content) the repair entry points
//! attempt to bring a malformed state back into a defined shape.
//! Recurring auto-repair masks underlying ingest bugs, so the
//! background scheduler exposes only an **audit** variant of these
//! classes by default; destructive repair stays operator-CLI.

use std::collections::BTreeMap;

use anyhow::Context;
use serde::Serialize;
use sqlx::FromRow;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{
        catalog_repository, content_document_head_repository::null_head_repair_eligibility,
        content_knowledge_projection_repository,
    },
    interfaces::http::router_support::ApiError,
    services::content::service::graph_state_after_successful_extract,
};

#[derive(Debug, FromRow)]
struct NullHeadCandidate {
    document_id: Uuid,
    revision_id: Uuid,
}

/// Default ceiling for `promote_null_heads_auto`. After this many
/// consecutive failures with the same error code the document is
/// marked `dead_letter_at` and excluded from auto-recovery until an
/// operator clears the mark.
pub const DEFAULT_RECOVERY_MAX_ATTEMPTS: i32 = 3;

/// Cool-down between successive auto-recovery attempts on the same
/// document. Spaces out retries so a flaky upstream provider does not
/// burn the entire budget in seconds.
pub const DEFAULT_RECOVERY_COOLDOWN: std::time::Duration = std::time::Duration::from_hours(1);

/// Aggregate report for one [`promote_null_heads`] run.
///
/// `promoted` counts conditional writes; a candidate that became ineligible
/// after selection is skipped rather than counted as a promotion or failure.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct NullHeadRepairReport {
    pub libraries_scanned: usize,
    pub promoted: usize,
    pub skipped_no_chunks: usize,
}

/// Both sweepers use the same conditional parent/head-locked repair write.
/// The candidate revision is only diagnostic; the write reselects current truth.
async fn repair_null_head(state: &AppState, document_id: Uuid) -> Result<bool, ApiError> {
    content_knowledge_projection_repository::repair_null_document_head_with_projection(
        &state.persistence.postgres,
        document_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "null-head repair failed"))
}

/// Promote `content_document_head` for documents that have at least one
/// revision with persisted chunks but whose head still carries
/// `readable_revision_id == NULL` AND `active_revision_id == NULL`.
///
/// Uses a conditional lifecycle-locked repair and the shared head/projection
/// writer, so selection can never undo a later admission or READY publication.
///
/// `library_filter = None` walks every library; `Some(uuid)` restricts
/// to one. Skipped-no-chunks documents are documents whose head is null
/// AND who have no chunk-bearing revision — those need re-ingest, not a
/// head promotion, and are merely counted here so the operator sees the
/// gap.
pub async fn promote_null_heads(
    state: &AppState,
    library_filter: Option<Uuid>,
) -> anyhow::Result<NullHeadRepairReport> {
    promote_null_heads_inner(state, library_filter, None).await
}

/// Pauses a manual sweep after its candidate read to exercise concurrent admission.
#[cfg(feature = "test-support")]
pub async fn promote_null_heads_after_candidates_for_test<F>(
    state: &AppState,
    library_filter: Option<Uuid>,
    after_candidates: F,
) -> anyhow::Result<NullHeadRepairReport>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    promote_null_heads_inner(state, library_filter, Some(Box::pin(after_candidates))).await
}

async fn promote_null_heads_inner(
    state: &AppState,
    library_filter: Option<Uuid>,
    mut after_candidates: Option<futures::future::BoxFuture<'static, ()>>,
) -> anyhow::Result<NullHeadRepairReport> {
    let mut libraries =
        catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    if let Some(target) = library_filter {
        libraries.retain(|library| library.id == target);
    }
    if libraries.is_empty() {
        anyhow::bail!("no libraries matched null-head repair target");
    }

    let mut report = NullHeadRepairReport::default();
    for library in libraries {
        report.libraries_scanned += 1;
        let candidates = list_null_head_candidates(state, library.id).await?;
        let no_chunks_count = count_null_head_docs_without_chunks(state, library.id).await?;
        report.skipped_no_chunks =
            report.skipped_no_chunks.saturating_add(no_chunks_count.max(0) as usize);

        log_promoting_null_head_documents(&library, candidates.len(), no_chunks_count);

        if let Some(hook) = after_candidates.take() {
            hook.await;
        }

        for candidate in candidates {
            promote_candidate(state, &candidate, &mut report).await;
        }
    }

    log_null_head_promotion_finished(&report);
    Ok(report)
}

/// Pick, per-document, the latest revision that has at least one row in
/// `content_chunk` — the most recent revision the ingest pipeline is known to
/// have produced material for.
async fn list_null_head_candidates(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<Vec<NullHeadCandidate>> {
    sqlx::query_as::<_, NullHeadCandidate>(concat!(
        "SELECT DISTINCT ON (r.document_id) \
             r.document_id, \
             r.id AS revision_id \
         FROM content_revision r \
         JOIN content_document d ON d.id = r.document_id \
         JOIN content_document_head h ON h.document_id = r.document_id \
          WHERE d.library_id = $1 \
            AND (",
        null_head_repair_eligibility!(),
        ") \
           AND EXISTS (SELECT 1 FROM content_chunk c WHERE c.revision_id = r.id) \
         ORDER BY r.document_id, r.revision_number DESC",
    ))
    .bind(library_id)
    .fetch_all(&state.persistence.postgres)
    .await
    .with_context(|| format!("failed to list null-head candidates for library {library_id}"))
}

/// Null-head documents with no chunk-bearing revision: they need re-ingest, not
/// a head promotion.
async fn count_null_head_docs_without_chunks(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<i64> {
    sqlx::query_scalar::<_, i64>(concat!(
        "SELECT count(DISTINCT h.document_id) \
         FROM content_document_head h \
         JOIN content_document d ON d.id = h.document_id \
          WHERE d.library_id = $1 \
            AND (",
        null_head_repair_eligibility!(),
        ") \
           AND NOT EXISTS ( \
               SELECT 1 FROM content_revision r \
               JOIN content_chunk c ON c.revision_id = r.id \
               WHERE r.document_id = h.document_id \
           )",
    ))
    .bind(library_id)
    .fetch_one(&state.persistence.postgres)
    .await
    .with_context(|| format!("failed to count no-chunk null-head docs for library {library_id}"))
}

async fn promote_candidate(
    state: &AppState,
    candidate: &NullHeadCandidate,
    report: &mut NullHeadRepairReport,
) {
    match repair_null_head(state, candidate.document_id).await {
        Ok(true) => report.promoted += 1,
        Ok(false) => {}
        Err(error) => log_promote_document_head_failed(candidate, &error),
    }
}

fn log_promoting_null_head_documents(
    library: &catalog_repository::CatalogLibraryRow,
    backfill_candidates: usize,
    skipped_no_chunks: i64,
) {
    info!(
        library_id = %library.id,
        library_name = %library.display_name,
        backfill_candidates,
        skipped_no_chunks,
        "promoting null-head documents",
    );
}

fn log_promote_document_head_failed<E: std::fmt::Debug>(candidate: &NullHeadCandidate, error: &E) {
    warn!(
        document_id = %candidate.document_id,
        revision_id = %candidate.revision_id,
        ?error,
        "promote_document_head failed; continuing with next document",
    );
}

fn log_null_head_promotion_finished(report: &NullHeadRepairReport) {
    info!(
        promoted = report.promoted,
        skipped_no_chunks = report.skipped_no_chunks,
        "null-head promotion finished",
    );
}

// ============================================================================
// promote_null_heads_auto — rate-limited recovery pass
// ============================================================================

/// Aggregate report for a [`promote_null_heads_auto`] pass.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct NullHeadAutoReport {
    pub libraries_scanned: usize,
    pub candidates_considered: usize,
    pub promoted: usize,
    pub failed: usize,
    pub dead_lettered: usize,
    pub cooldown_skipped: usize,
}

#[derive(Debug, FromRow)]
struct AutoCandidate {
    document_id: Uuid,
    revision_id: Uuid,
    last_recovery_error_code: Option<String>,
    recovery_attempts_count: i32,
}

/// Rate-limited recovery pass. For every library in scope (or every
/// library when `library_filter == None`):
///
/// 1. Pick null-head documents with at least one chunk-bearing
///    revision that are NOT dead-lettered and whose last recovery
///    attempt is either absent or older than [`DEFAULT_RECOVERY_COOLDOWN`].
/// 2. Attempt `promote_document_head` for each.
/// 3. Update `content_document_head.recovery_*` to reflect the
///    outcome. Same-error failures within the cool-down window
///    increment `recovery_attempts_count`; a different error code
///    resets the counter to 1. When the counter reaches
///    [`DEFAULT_RECOVERY_MAX_ATTEMPTS`] the row gains a
///    `dead_letter_at` stamp and is excluded from future auto
///    recovery until an operator clears the mark.
///
/// On success the counter and last-error code are reset; the doc is
/// fully recovered.
pub async fn promote_null_heads_auto(
    state: &AppState,
    library_filter: Option<Uuid>,
) -> anyhow::Result<NullHeadAutoReport> {
    let pool = &state.persistence.postgres;
    let mut libraries = catalog_repository::list_libraries(pool, None).await?;
    if let Some(target) = library_filter {
        libraries.retain(|library| library.id == target);
    }

    let cooldown_secs = DEFAULT_RECOVERY_COOLDOWN.as_secs() as i64;
    let mut report = NullHeadAutoReport::default();
    for library in &libraries {
        report.libraries_scanned += 1;
        let candidates = list_auto_recovery_candidates(pool, library.id, cooldown_secs).await?;
        let cooldown_skipped =
            count_auto_recovery_cooldown_skipped(pool, library.id, cooldown_secs).await?;
        report.cooldown_skipped =
            report.cooldown_skipped.saturating_add(cooldown_skipped.max(0) as usize);

        log_auto_recovery_pass_starting(library, candidates.len(), cooldown_skipped);

        for candidate in candidates {
            process_auto_recovery_candidate(state, pool, candidate, &mut report).await;
        }
    }

    log_auto_recovery_finished(&report);
    Ok(report)
}

/// Null-head documents with a chunk-bearing revision that are NOT dead-lettered
/// and whose last recovery attempt is either absent or older than the cooldown.
async fn list_auto_recovery_candidates(
    pool: &sqlx::PgPool,
    library_id: Uuid,
    cooldown_secs: i64,
) -> anyhow::Result<Vec<AutoCandidate>> {
    sqlx::query_as::<_, AutoCandidate>(concat!(
        "SELECT DISTINCT ON (r.document_id) \
             r.document_id, \
             r.id AS revision_id, \
             h.last_recovery_error_code, \
             h.recovery_attempts_count \
         FROM content_revision r \
         JOIN content_document d ON d.id = r.document_id \
         JOIN content_document_head h ON h.document_id = r.document_id \
         WHERE d.library_id = $1 \
            AND (", null_head_repair_eligibility!(), ") \
           AND h.dead_letter_at IS NULL \
           AND (h.last_recovery_attempt_at IS NULL \
                OR h.last_recovery_attempt_at < now() - make_interval(secs => $2::double precision)) \
           AND EXISTS (SELECT 1 FROM content_chunk c WHERE c.revision_id = r.id) \
         ORDER BY r.document_id, r.revision_number DESC",
    ))
    .bind(library_id)
    .bind(cooldown_secs as f64)
    .fetch_all(pool)
    .await
    .with_context(|| {
        format!("failed to list auto-recovery candidates for library {library_id}")
    })
}

/// Docs with a chunk-bearing revision held back inside the cooldown window, so
/// the operator sees what was withheld rather than the row appearing to vanish.
async fn count_auto_recovery_cooldown_skipped(
    pool: &sqlx::PgPool,
    library_id: Uuid,
    cooldown_secs: i64,
) -> anyhow::Result<i64> {
    sqlx::query_scalar::<_, i64>(concat!(
        "SELECT count(*) FROM content_document_head h \
         JOIN content_document d ON d.id = h.document_id \
         WHERE d.library_id = $1 \
            AND (",
        null_head_repair_eligibility!(),
        ") \
           AND h.dead_letter_at IS NULL \
           AND h.last_recovery_attempt_at IS NOT NULL \
           AND h.last_recovery_attempt_at >= now() - make_interval(secs => $2::double precision) \
           AND EXISTS ( \
               SELECT 1 FROM content_revision r \
               JOIN content_chunk c ON c.revision_id = r.id \
               WHERE r.document_id = h.document_id \
           )",
    ))
    .bind(library_id)
    .bind(cooldown_secs as f64)
    .fetch_one(pool)
    .await
    .with_context(|| {
        format!("failed to count cooldown-skipped null-head docs for library {library_id}")
    })
}

fn log_auto_recovery_pass_starting(
    library: &catalog_repository::CatalogLibraryRow,
    candidates: usize,
    cooldown_skipped: i64,
) {
    info!(
        library_id = %library.id,
        library_name = %library.display_name,
        candidates,
        cooldown_skipped,
        "auto-recovery pass starting",
    );
}

fn log_auto_recovery_finished(report: &NullHeadAutoReport) {
    info!(
        libraries_scanned = report.libraries_scanned,
        candidates = report.candidates_considered,
        promoted = report.promoted,
        failed = report.failed,
        dead_lettered = report.dead_lettered,
        cooldown_skipped = report.cooldown_skipped,
        "auto-recovery finished",
    );
}

async fn process_auto_recovery_candidate(
    state: &AppState,
    pool: &sqlx::PgPool,
    candidate: AutoCandidate,
    report: &mut NullHeadAutoReport,
) {
    report.candidates_considered += 1;
    let promotion = repair_null_head(state, candidate.document_id).await;
    if matches!(promotion, Ok(false)) {
        return;
    }
    let Err(error) = promotion else {
        report.promoted += 1;
        clear_recovery_state_after_promotion(pool, &candidate).await;
        return;
    };

    let error_code = classify_error(&error);
    let dead_lettered = match record_failure(
        pool,
        candidate.document_id,
        error_code,
        candidate.last_recovery_error_code.as_deref(),
        candidate.recovery_attempts_count,
        DEFAULT_RECOVERY_MAX_ATTEMPTS,
    )
    .await
    {
        Ok(dead_lettered) => dead_lettered,
        Err(record_error) => {
            log_record_recovery_failure_failed(&candidate, &record_error);
            false
        }
    };
    if dead_lettered {
        report.dead_lettered += 1;
    } else {
        report.failed += 1;
    }
    log_auto_recovery_promotion_failed(&candidate, error_code, &error, dead_lettered);
}

async fn clear_recovery_state_after_promotion(pool: &sqlx::PgPool, candidate: &AutoCandidate) {
    if let Err(error) = clear_recovery_state(pool, candidate.document_id).await {
        log_clear_recovery_state_failed(candidate, &error);
    }
}

fn log_clear_recovery_state_failed<E: std::fmt::Debug>(candidate: &AutoCandidate, error: &E) {
    warn!(
        document_id = %candidate.document_id,
        ?error,
        "failed to reset recovery counters after successful promotion",
    );
}

fn log_record_recovery_failure_failed<E: std::fmt::Debug>(
    candidate: &AutoCandidate,
    record_error: &E,
) {
    warn!(
        document_id = %candidate.document_id,
        ?record_error,
        "failed to persist auto-recovery failure state",
    );
}

fn log_auto_recovery_promotion_failed<E: std::fmt::Debug>(
    candidate: &AutoCandidate,
    error_code: &str,
    error: &E,
    dead_lettered: bool,
) {
    warn!(
        document_id = %candidate.document_id,
        revision_id = %candidate.revision_id,
        error_code = %error_code,
        ?error,
        dead_lettered,
        "auto-recovery promote_document_head failed",
    );
}

/// Reset the recovery counters on `content_document_head` after a
/// successful promotion.
async fn clear_recovery_state(pool: &sqlx::PgPool, document_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE content_document_head \
         SET recovery_attempts_count = 0, \
             last_recovery_error_code = NULL, \
             last_recovery_attempt_at = now() \
         WHERE document_id = $1",
    )
    .bind(document_id)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Record a failed recovery attempt, returning `true` if the row
/// ended up dead-lettered (i.e. attempts ≥ `max_attempts` with the same
/// error code).
async fn record_failure(
    pool: &sqlx::PgPool,
    document_id: Uuid,
    error_code: &str,
    previous_error_code: Option<&str>,
    previous_attempts: i32,
    max_attempts: i32,
) -> Result<bool, sqlx::Error> {
    let same_error = previous_error_code == Some(error_code);
    let new_attempts = if same_error { previous_attempts.saturating_add(1) } else { 1 };
    let dead_letter = new_attempts >= max_attempts;
    sqlx::query(
        "UPDATE content_document_head \
         SET recovery_attempts_count = $2, \
             last_recovery_error_code = $3, \
             last_recovery_attempt_at = now(), \
             dead_letter_at = CASE WHEN $4 THEN now() ELSE dead_letter_at END \
         WHERE document_id = $1",
    )
    .bind(document_id)
    .bind(new_attempts)
    .bind(error_code)
    .bind(dead_letter)
    .execute(pool)
    .await?;
    Ok(dead_letter)
}

/// Reduce an API error to its stable typed kind used to bucket retries.
/// Two failures share a bucket when this returns the same code, which
/// is what the rate-limit treats as "same error" — three consecutive
/// hits with the same bucket inside the cool-down window cross into
/// dead-letter. Dynamic messages never participate in retry identity.
fn classify_error(error: &ApiError) -> &'static str {
    error.kind()
}

/// Clear the `dead_letter_at` mark and counters for a document so the
/// next auto-recovery pass picks it up again. Operator-only.
pub async fn clear_recovery_dead_letter(
    pool: &sqlx::PgPool,
    document_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let updated = sqlx::query(
        "UPDATE content_document_head \
         SET dead_letter_at = NULL, \
             recovery_attempts_count = 0, \
             last_recovery_error_code = NULL \
         WHERE document_id = $1 AND dead_letter_at IS NOT NULL",
    )
    .bind(document_id)
    .execute(pool)
    .await?;
    Ok(updated.rows_affected() > 0)
}

// ============================================================================
// reconcile_terminal_graph_states — terminal state from completed extraction
// ============================================================================

/// Revisions transitioned per write pass. Bounds each transaction so one
/// operator invocation never holds a lock over the whole backlog.
pub const TERMINAL_GRAPH_STATE_BATCH_SIZE: i64 = 5_000;

/// Hard ceiling on write passes in one invocation. A caller cannot loop over an
/// unbounded backlog; the pass fails loudly if work remains after this many.
const TERMINAL_GRAPH_STATE_MAX_PASSES: u32 = 20;

/// The latest completed `extract_graph` completion per revision, plus the sets
/// of revisions with live work and of documents that already have a document
/// node. Mirrors the completion-evidence contract the re-extract taxonomy uses:
/// a succeeded attempt of a completed job, ordered by the canonical recency key.
const TERMINAL_GRAPH_STATE_CTES: &str = "\
with latest_completion as materialized (
    select distinct on (job.knowledge_revision_id)
           job.knowledge_revision_id as revision_id,
           event.details_json ->> 'graphContributionCount' as graph_contribution_count,
           event.details_json ->> 'graphReady' as graph_ready
      from ingest_stage_event event
      join ingest_attempt attempt
        on attempt.id = event.attempt_id
       and attempt.attempt_state = 'succeeded'::ingest_attempt_state
      join ingest_job job on job.id = attempt.job_id
     where event.stage_name = 'extract_graph'
       and event.stage_state = 'completed'::ingest_stage_state
       and job.knowledge_revision_id is not null
     order by job.knowledge_revision_id,
              job.completed_at desc nulls last,
              job.id desc,
              attempt.attempt_number desc,
              event.ordinal desc,
              event.recorded_at desc
), active_revisions as materialized (
    select distinct knowledge_revision_id as revision_id
      from ingest_job
     where queue_state in ('queued'::ingest_queue_state, 'leased'::ingest_queue_state)
       and knowledge_revision_id is not null
    union
    select distinct job.knowledge_revision_id
      from ingest_attempt attempt
      join ingest_job job on job.id = attempt.job_id
     where attempt.attempt_state in (
               'leased'::ingest_attempt_state,
               'running'::ingest_attempt_state
           )
       and job.knowledge_revision_id is not null
), document_nodes as materialized (
    select distinct library_id, document_id
      from runtime_graph_node
     where node_type = 'document'
)";

/// Revisions left in `processing` whose latest completed extraction contributed
/// no graph target and that are not in flight. `graphReady=false` is exactly the
/// zero-contribution marker [`graph_state_after_successful_extract`] consumes, so
/// the terminal state is computed from the persisted evidence, not re-extracted.
const TERMINAL_GRAPH_STATE_SELECTION: &str = "\
select revision.revision_id, revision.library_id
  from knowledge_revision revision
  join latest_completion completion on completion.revision_id = revision.revision_id
  join content_document_head head on head.document_id = revision.document_id
  join content_document document on document.id = revision.document_id
  join catalog_library library on library.id = document.library_id
  left join active_revisions active on active.revision_id = revision.revision_id
  left join document_nodes node
    on node.library_id = revision.library_id
   and node.document_id = revision.document_id
 where revision.graph_state = 'processing'
   and completion.graph_contribution_count = '0'
   and completion.graph_ready = 'false'
   and head.active_revision_id = revision.revision_id
   and head.readable_revision_id = revision.revision_id
   and document.document_state = 'active'::content_document_state
   and document.deleted_at is null
   and library.lifecycle_state = 'active'::catalog_library_lifecycle_state
   and active.revision_id is null
   and node.document_id is null
   and ($1::uuid is null or revision.library_id = $1)";

/// Per-library count of revisions a pass transitions or would transition.
#[derive(Debug, Clone, Serialize)]
pub struct TerminalGraphStateLibraryCount {
    pub library_id: Uuid,
    pub revisions: usize,
}

/// Aggregate report for one [`reconcile_terminal_graph_states`] run.
#[derive(Debug, Clone, Serialize)]
pub struct TerminalGraphStateRepairReport {
    /// Canonical terminal state the pass writes, from
    /// `graph_state_after_successful_extract`.
    pub terminal_state: String,
    pub dry_run: bool,
    pub candidates: usize,
    pub updated: usize,
    pub by_library: Vec<TerminalGraphStateLibraryCount>,
}

/// Read-only count, per library, of the revisions
/// [`reconcile_terminal_graph_states`] would transition. `None` counts every
/// library.
///
/// # Errors
/// Returns any `SQLx` error raised while scanning the completion evidence.
pub async fn count_revisions_needing_terminal_graph_state(
    pool: &sqlx::PgPool,
    library_filter: Option<Uuid>,
) -> anyhow::Result<Vec<(Uuid, i64)>> {
    let sql = format!(
        "{TERMINAL_GRAPH_STATE_CTES} \
         select candidate.library_id, count(*)::bigint \
           from ({TERMINAL_GRAPH_STATE_SELECTION}) candidate \
          group by candidate.library_id \
          order by 2 desc, 1"
    );
    let rows = sqlx::query_as::<_, (Uuid, i64)>(sqlx::AssertSqlSafe(sql))
        .bind(library_filter)
        .fetch_all(pool)
        .await
        .context("failed to count revisions needing terminal graph state")?;
    Ok(rows)
}

/// Transition at most `batch_limit` matching revisions and return the library of
/// each updated row. Empty means the backlog is drained.
async fn update_terminal_graph_states_once(
    pool: &sqlx::PgPool,
    library_filter: Option<Uuid>,
    terminal_state: &str,
    batch_limit: i64,
) -> anyhow::Result<Vec<Uuid>> {
    let sql = format!(
        "{TERMINAL_GRAPH_STATE_CTES} \
         update knowledge_revision revision \
            set graph_state = $2 \
           from ({TERMINAL_GRAPH_STATE_SELECTION} limit $3) candidate \
          where revision.revision_id = candidate.revision_id \
          returning revision.library_id"
    );
    let rows = sqlx::query_as::<_, (Uuid,)>(sqlx::AssertSqlSafe(sql))
        .bind(library_filter)
        .bind(terminal_state)
        .bind(batch_limit)
        .fetch_all(pool)
        .await
        .context("failed to transition terminal graph states")?;
    Ok(rows.into_iter().map(|(library_id,)| library_id).collect())
}

fn accumulate_library(
    by_library: &mut BTreeMap<Uuid, usize>,
    library_id: Uuid,
) -> Result<(), &'static str> {
    let entry = by_library.entry(library_id).or_insert(0);
    *entry = entry.checked_add(1).ok_or("terminal graph-state library count overflowed")?;
    Ok(())
}

/// Move historical zero-contribution `processing` revisions to the canonical
/// terminal graph state computed from their already-completed extraction.
///
/// Pre-IRG-211 code left a successful extract with no graph contribution in
/// `processing` forever. Re-running extraction repairs them only by spending
/// provider calls; this pass instead recomputes the terminal state with the
/// same rule the live pipeline uses (`graph_state_after_successful_extract`)
/// and writes it, so no provider is called. `dry_run` reports the cohort
/// without writing. Idempotent: a second run finds nothing in `processing`.
///
/// # Errors
/// Fails when a query fails, when the library count overflows `usize`, or when
/// the backlog is not drained within `TERMINAL_GRAPH_STATE_MAX_PASSES`.
pub async fn reconcile_terminal_graph_states(
    state: &AppState,
    library_filter: Option<Uuid>,
    dry_run: bool,
) -> anyhow::Result<TerminalGraphStateRepairReport> {
    let terminal_state = graph_state_after_successful_extract(false).to_string();
    let pool = &state.persistence.postgres;

    if dry_run {
        let counts = count_revisions_needing_terminal_graph_state(pool, library_filter).await?;
        let by_library = counts
            .into_iter()
            .map(|(library_id, revisions)| TerminalGraphStateLibraryCount {
                library_id,
                revisions: usize::try_from(revisions).unwrap_or(usize::MAX),
            })
            .collect::<Vec<_>>();
        let candidates = by_library.iter().map(|row| row.revisions).sum();
        return Ok(TerminalGraphStateRepairReport {
            terminal_state,
            dry_run: true,
            candidates,
            updated: 0,
            by_library,
        });
    }

    let mut accumulators: BTreeMap<Uuid, usize> = BTreeMap::new();
    let mut updated = 0usize;
    for pass in 0..TERMINAL_GRAPH_STATE_MAX_PASSES {
        let libraries = update_terminal_graph_states_once(
            pool,
            library_filter,
            &terminal_state,
            TERMINAL_GRAPH_STATE_BATCH_SIZE,
        )
        .await?;
        if libraries.is_empty() {
            return Ok(finish_report(terminal_state, accumulators, updated));
        }
        for library_id in libraries {
            updated = updated
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("terminal graph-state update count overflowed"))?;
            accumulate_library(&mut accumulators, library_id).map_err(anyhow::Error::msg)?;
        }
        if pass + 1 == TERMINAL_GRAPH_STATE_MAX_PASSES {
            let remaining = count_revisions_needing_terminal_graph_state(pool, library_filter)
                .await?
                .into_iter()
                .try_fold(0i64, |total, (_, count)| total.checked_add(count))
                .unwrap_or(i64::MAX);
            anyhow::ensure!(
                remaining == 0,
                "terminal graph-state reconciliation stopped after \
                 {TERMINAL_GRAPH_STATE_MAX_PASSES} passes with {remaining} revisions remaining"
            );
        }
    }
    Ok(finish_report(terminal_state, accumulators, updated))
}

fn finish_report(
    terminal_state: String,
    accumulators: BTreeMap<Uuid, usize>,
    updated: usize,
) -> TerminalGraphStateRepairReport {
    let mut by_library = accumulators
        .into_iter()
        .map(|(library_id, revisions)| TerminalGraphStateLibraryCount { library_id, revisions })
        .collect::<Vec<_>>();
    by_library.sort_by(|left, right| {
        right.revisions.cmp(&left.revisions).then_with(|| left.library_id.cmp(&right.library_id))
    });
    TerminalGraphStateRepairReport {
        terminal_state,
        dry_run: false,
        candidates: updated,
        updated,
        by_library,
    }
}

// ============================================================================
// reconcile_stranded_fan_out_operations — terminal parent from terminal children
// ============================================================================

/// Parents settled per write pass. Bounds each transaction so one operator
/// invocation never holds a lock over the whole backlog.
pub const STRANDED_FAN_OUT_BATCH_SIZE: i64 = 1_000;

/// Hard ceiling on write passes in one invocation. A caller cannot loop over an
/// unbounded backlog; the pass fails loudly if work remains after this many.
const STRANDED_FAN_OUT_MAX_PASSES: u32 = 20;

/// A fan-out parent still non-terminal while every one of its children has
/// settled. Its terminal state is a pure function of the children, but no row
/// stores it, so nothing will ever transition it: the detached worker that owns
/// the transition is gone (a rolling deployment aborted it, or it panicked).
const STRANDED_FAN_OUT_CANDIDATES: &str = "\
select parent.operation_kind, parent.id, null::text as failure_code, parent.created_at
  from ops_async_operation parent
 where parent.parent_async_operation_id is null
   and parent.status in ('accepted', 'processing')
   and exists (
       select 1
         from ops_async_operation child
        where child.parent_async_operation_id = parent.id
   )
   and not exists (
       select 1
         from ops_async_operation child
        where child.parent_async_operation_id = parent.id
          and child.status in ('accepted', 'processing')
   )";

/// A fan-out parent recorded terminal `failed` by a background graph step while
/// every child is `ready`. A release before IRG-218 wrote `graph_projection_failed`
/// / `graph_cleanup_failed` as the parent's status even though the deletions
/// succeeded; the parent's outcome must be its children's, and the background
/// failure belongs in the operation detail. This cohort is exactly that
/// contradiction and nothing else: only those two background codes, a
/// non-empty child set, and every child `ready`.
const CONTRADICTED_FAN_OUT_CANDIDATES: &str = "\
select parent.operation_kind, parent.id, parent.failure_code, parent.created_at
  from ops_async_operation parent
 where parent.parent_async_operation_id is null
   and parent.status = 'failed'
   and parent.failure_code in ('graph_projection_failed', 'graph_cleanup_failed')
   and exists (
       select 1
         from ops_async_operation child
        where child.parent_async_operation_id = parent.id
   )
   and not exists (
       select 1
         from ops_async_operation child
        where child.parent_async_operation_id = parent.id
          and child.status <> 'ready'
   )";

/// Per-operation-kind count of parents a pass settles or would settle.
#[derive(Debug, Clone, Serialize)]
pub struct FanOutOperationKindCount {
    pub operation_kind: String,
    pub operations: usize,
}

/// Aggregate report for one fan-out repair run.
#[derive(Debug, Clone, Serialize)]
pub struct FanOutRepairReport {
    pub dry_run: bool,
    pub candidates: usize,
    pub settled: usize,
    pub by_kind: Vec<FanOutOperationKindCount>,
}

/// Which cohort of fan-out parents a repair run reconciles.
#[derive(Debug, Clone, Copy)]
pub enum FanOutRepairCohort {
    /// A parent still non-terminal while every child has settled — a release
    /// aborted the detached worker that owns its terminal transition.
    Stranded,
    /// A parent terminal `failed` whose recorded outcome contradicts its
    /// children: a background graph step was wrongly written as the deletion's
    /// failure while every child is `ready`. The repair re-derives the outcome
    /// from the children and keeps the old failure code as a detail.
    ContradictedOutcome,
}

impl FanOutRepairCohort {
    const fn candidates_sql(self) -> &'static str {
        match self {
            Self::Stranded => STRANDED_FAN_OUT_CANDIDATES,
            Self::ContradictedOutcome => CONTRADICTED_FAN_OUT_CANDIDATES,
        }
    }

    const fn guard(self) -> crate::infra::repositories::ops_repository::FanOutSettleGuard {
        use crate::infra::repositories::ops_repository::FanOutSettleGuard;
        match self {
            Self::Stranded => FanOutSettleGuard::InFlight,
            Self::ContradictedOutcome => FanOutSettleGuard::ContradictedFailure,
        }
    }

    const fn settled_message(self) -> &'static str {
        match self {
            Self::Stranded => "stranded fan-out parent settled from its terminal children",
            Self::ContradictedOutcome => {
                "contradicted fan-out parent re-derived from its ready children"
            }
        }
    }

    /// Detail recorded on a contradicted parent: the background failure code is
    /// preserved while the children decide the status. `None` for the stranded
    /// cohort, which has no background failure to record.
    fn detail(self, failure_code: Option<String>) -> Option<serde_json::Value> {
        match (self, failure_code) {
            (Self::ContradictedOutcome, Some(failure_code)) => Some(serde_json::json!({
                "graph_convergence": { "failure_code": failure_code, "reconciled": true }
            })),
            _ => None,
        }
    }
}

/// Read-only count, per operation kind, of the parents a cohort pass would
/// settle.
///
/// # Errors
/// Returns any `SQLx` error raised while scanning the operation fan-out.
pub async fn count_fan_out_candidates(
    pool: &sqlx::PgPool,
    cohort: FanOutRepairCohort,
) -> anyhow::Result<Vec<(String, i64)>> {
    let sql = format!(
        "select candidate.operation_kind, count(*)::bigint \
           from ({}) candidate \
          group by candidate.operation_kind \
          order by 2 desc, 1",
        cohort.candidates_sql()
    );
    let rows = sqlx::query_as::<_, (String, i64)>(sqlx::AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
        .context("failed to count fan-out repair candidates")?;
    Ok(rows)
}

/// Bounded page of a cohort's candidates, oldest first.
async fn list_fan_out_candidates(
    pool: &sqlx::PgPool,
    cohort: FanOutRepairCohort,
    limit: i64,
) -> anyhow::Result<Vec<(String, Uuid, Option<String>)>> {
    let sql = format!(
        "select candidate.operation_kind, candidate.id, candidate.failure_code \
           from ({}) candidate \
          order by candidate.created_at, candidate.id \
          limit $1",
        cohort.candidates_sql()
    );
    let rows = sqlx::query_as::<_, (String, Uuid, Option<String>)>(sqlx::AssertSqlSafe(sql))
        .bind(limit)
        .fetch_all(pool)
        .await
        .context("failed to list fan-out repair candidates")?;
    Ok(rows)
}

/// Settle a cohort of fan-out parents through the shared aggregate
/// [`crate::infra::repositories::ops_repository::settle_fan_out_operation_from_children`]
/// — the same function the live worker and its abnormal-exit guard use.
///
/// A batch endpoint's parent is transitioned only by the detached worker that
/// owns it; a release that aborts that worker (or a panic) leaves the parent
/// stranded, and a release that wrote a background graph failure as the outcome
/// leaves it misattributed. Nothing else touches a `library`-subject parent, so
/// this pass is the operator's recovery. It never invents a state: it only
/// writes the outcome already implied by the persisted children. `dry_run`
/// reports the cohort without writing. Idempotent: a second run finds nothing.
///
/// # Errors
/// Fails when a query fails or when the backlog is not drained within
/// `STRANDED_FAN_OUT_MAX_PASSES`.
pub async fn reconcile_fan_out_operations(
    state: &AppState,
    cohort: FanOutRepairCohort,
    dry_run: bool,
) -> anyhow::Result<FanOutRepairReport> {
    let pool = &state.persistence.postgres;

    if dry_run {
        return count_fan_out_repair_report(pool, cohort).await;
    }

    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut candidates = 0usize;
    let mut settled = 0usize;
    for pass in 0..STRANDED_FAN_OUT_MAX_PASSES {
        let page = list_fan_out_candidates(pool, cohort, STRANDED_FAN_OUT_BATCH_SIZE).await?;
        if page.is_empty() {
            break;
        }
        candidates += page.len();
        let progressed = settle_fan_out_page(pool, cohort, page, &mut by_kind).await?;
        settled += progressed;
        if progressed == 0 {
            break;
        }
        if pass + 1 == STRANDED_FAN_OUT_MAX_PASSES {
            ensure_fan_out_backlog_drained(pool, cohort).await?;
        }
    }

    let by_kind = by_kind
        .into_iter()
        .map(|(operation_kind, operations)| FanOutOperationKindCount { operation_kind, operations })
        .collect();
    Ok(FanOutRepairReport { dry_run: false, candidates, settled, by_kind })
}

/// Read-only cohort report: per-kind candidate counts and their total.
async fn count_fan_out_repair_report(
    pool: &sqlx::PgPool,
    cohort: FanOutRepairCohort,
) -> anyhow::Result<FanOutRepairReport> {
    let counts = count_fan_out_candidates(pool, cohort).await?;
    let by_kind = counts
        .into_iter()
        .map(|(operation_kind, operations)| FanOutOperationKindCount {
            operation_kind,
            operations: usize::try_from(operations).unwrap_or(usize::MAX),
        })
        .collect::<Vec<_>>();
    let candidates = by_kind.iter().map(|row| row.operations).sum();
    Ok(FanOutRepairReport { dry_run: true, candidates, settled: 0, by_kind })
}

/// Settle one page of candidates through the shared aggregate, returning how
/// many rows actually progressed and accumulating the per-kind counts.
async fn settle_fan_out_page(
    pool: &sqlx::PgPool,
    cohort: FanOutRepairCohort,
    page: Vec<(String, Uuid, Option<String>)>,
    by_kind: &mut BTreeMap<String, usize>,
) -> anyhow::Result<usize> {
    let mut progressed = 0usize;
    for (operation_kind, operation_id, failure_code) in page {
        let detail = cohort.detail(failure_code);
        match crate::infra::repositories::ops_repository::settle_fan_out_operation_from_children(
            pool,
            operation_id,
            None,
            None,
            detail.as_ref(),
            cohort.guard(),
        )
        .await
        .with_context(|| format!("failed to settle fan-out parent {operation_id}"))?
        {
            Some(settled_row) => {
                log_fan_out_parent_settled(cohort, operation_id, &settled_row);
                progressed += 1;
                *by_kind.entry(operation_kind).or_insert(0) += 1;
            }
            None => {
                // The parent settled (or changed between the page read and
                // the write). It is no longer a candidate; do not count it
                // as progress, or the loop would spin.
            }
        }
    }
    Ok(progressed)
}

/// On the final allowed pass, fail loudly if the backlog is not drained.
async fn ensure_fan_out_backlog_drained(
    pool: &sqlx::PgPool,
    cohort: FanOutRepairCohort,
) -> anyhow::Result<()> {
    let remaining: i64 =
        count_fan_out_candidates(pool, cohort).await?.iter().map(|(_, count)| count).sum();
    anyhow::ensure!(
        remaining == 0,
        "fan-out repair left {remaining} parent(s) after \
         {STRANDED_FAN_OUT_MAX_PASSES} passes"
    );
    Ok(())
}

fn log_fan_out_parent_settled(
    cohort: FanOutRepairCohort,
    operation_id: Uuid,
    settled_row: &crate::infra::repositories::ops_repository::OpsAsyncOperationRow,
) {
    info!(
        %operation_id,
        operation_kind = %settled_row.operation_kind,
        status = %settled_row.status,
        "{}",
        cohort.settled_message()
    );
}

/// Settle fan-out parents an aborted detached worker left non-terminal while
/// every child already reached a terminal state.
///
/// # Errors
/// See [`reconcile_fan_out_operations`].
pub async fn reconcile_stranded_fan_out_operations(
    state: &AppState,
    dry_run: bool,
) -> anyhow::Result<FanOutRepairReport> {
    reconcile_fan_out_operations(state, FanOutRepairCohort::Stranded, dry_run).await
}

/// Re-derive fan-out parents a release wrongly marked `failed` with a
/// background graph failure code (`graph_projection_failed` /
/// `graph_cleanup_failed`) while every child is `ready`. The parent's outcome is
/// its children's; the repair records the old failure code as a
/// `graph_convergence` detail and settles the row `ready`.
///
/// # Errors
/// See [`reconcile_fan_out_operations`].
pub async fn reconcile_contradicted_fan_out_outcomes(
    state: &AppState,
    dry_run: bool,
) -> anyhow::Result<FanOutRepairReport> {
    reconcile_fan_out_operations(state, FanOutRepairCohort::ContradictedOutcome, dry_run).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_groups_same_api_error_kind_regardless_of_message() {
        let first = ApiError::BadRequest("first dynamic detail".to_string());
        let second = ApiError::BadRequest("completely different detail".to_string());

        assert_eq!(classify_error(&first), classify_error(&second));
        assert_eq!(classify_error(&first), "bad_request");
    }

    #[test]
    fn classify_keeps_distinct_api_error_kinds_separate() {
        let bad_request = ApiError::BadRequest("same detail".to_string());
        let conflict = ApiError::Conflict("same detail".to_string());

        assert_ne!(classify_error(&bad_request), classify_error(&conflict));
    }
}
