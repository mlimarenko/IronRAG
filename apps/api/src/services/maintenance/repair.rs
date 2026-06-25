//! `repair.*` operator-driven recovery sweepers.
//!
//! Unlike `gc.*` (which removes content) the repair entry points
//! attempt to bring a malformed state back into a canonical shape.
//! Recurring auto-repair masks underlying ingest bugs, so the
//! background scheduler exposes only an **audit** variant of these
//! classes by default; destructive repair stays operator-CLI.

use anyhow::Context;
use serde::Serialize;
use sqlx::FromRow;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState, infra::repositories::catalog_repository,
    services::content::service::PromoteHeadCommand,
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
pub const DEFAULT_RECOVERY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Aggregate report for one [`promote_null_heads`] run.
///
/// `promoted` is the number of documents whose head was upserted; the
/// canonical `promote_document_head` path is idempotent so re-running on
/// an already-promoted doc is a no-op upsert.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct NullHeadRepairReport {
    pub libraries_scanned: usize,
    pub promoted: usize,
    pub skipped_no_chunks: usize,
}

/// Promote `content_document_head` for documents that have at least one
/// revision with persisted chunks but whose head still carries
/// `readable_revision_id == NULL` AND `active_revision_id == NULL`.
///
/// Uses the canonical `promote_document_head` so both Postgres `head` and the
/// knowledge projection are written through the same path the ingest pipeline
/// uses on success.
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
        // Pick, per-document, the latest revision that has at least one
        // row in `content_chunk` — that's the most recent revision the
        // ingest pipeline is known to have produced material for.
        let candidates: Vec<NullHeadCandidate> = sqlx::query_as(
            "SELECT DISTINCT ON (r.document_id) \
                 r.document_id, \
                 r.id AS revision_id \
             FROM content_revision r \
             JOIN content_document d ON d.id = r.document_id \
             JOIN content_document_head h ON h.document_id = r.document_id \
             WHERE d.library_id = $1 \
               AND h.readable_revision_id IS NULL \
               AND h.active_revision_id IS NULL \
               AND EXISTS (SELECT 1 FROM content_chunk c WHERE c.revision_id = r.id) \
             ORDER BY r.document_id, r.revision_number DESC",
        )
        .bind(library.id)
        .fetch_all(&state.persistence.postgres)
        .await
        .with_context(|| {
            format!("failed to list null-head candidates for library {}", library.id)
        })?;

        let no_chunks_count: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT h.document_id) \
             FROM content_document_head h \
             JOIN content_document d ON d.id = h.document_id \
             WHERE d.library_id = $1 \
               AND h.readable_revision_id IS NULL \
               AND h.active_revision_id IS NULL \
               AND NOT EXISTS ( \
                   SELECT 1 FROM content_revision r \
                   JOIN content_chunk c ON c.revision_id = r.id \
                   WHERE r.document_id = h.document_id \
               )",
        )
        .bind(library.id)
        .fetch_one(&state.persistence.postgres)
        .await
        .with_context(|| {
            format!("failed to count no-chunk null-head docs for library {}", library.id)
        })?;
        report.skipped_no_chunks =
            report.skipped_no_chunks.saturating_add(no_chunks_count.max(0) as usize);

        info!(
            library_id = %library.id,
            library_name = %library.display_name,
            backfill_candidates = candidates.len(),
            skipped_no_chunks = no_chunks_count,
            "promoting null-head documents",
        );

        for candidate in candidates {
            match state
                .canonical_services
                .content
                .promote_document_head(
                    state,
                    PromoteHeadCommand {
                        document_id: candidate.document_id,
                        active_revision_id: Some(candidate.revision_id),
                        readable_revision_id: Some(candidate.revision_id),
                        latest_mutation_id: None,
                        latest_successful_attempt_id: None,
                    },
                )
                .await
            {
                Ok(_) => report.promoted += 1,
                Err(error) => warn!(
                    document_id = %candidate.document_id,
                    revision_id = %candidate.revision_id,
                    ?error,
                    "promote_document_head failed; continuing with next document",
                ),
            }
        }
    }

    info!(
        promoted = report.promoted,
        skipped_no_chunks = report.skipped_no_chunks,
        "null-head promotion finished",
    );
    Ok(report)
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
        // `DISTINCT ON (r.document_id)` plus the chunk-bearing revision
        // join gives us the freshest chunk-bearing revision per doc,
        // identical to the operator-driven `promote_null_heads`. The
        // extra clause skips docs that are still inside the cooldown
        // window so a flaky provider does not burn the budget in
        // seconds.
        let candidates: Vec<AutoCandidate> = sqlx::query_as(
            "SELECT DISTINCT ON (r.document_id) \
                 r.document_id, \
                 r.id AS revision_id, \
                 h.last_recovery_error_code, \
                 h.recovery_attempts_count \
             FROM content_revision r \
             JOIN content_document d ON d.id = r.document_id \
             JOIN content_document_head h ON h.document_id = r.document_id \
             WHERE d.library_id = $1 \
               AND h.readable_revision_id IS NULL \
               AND h.active_revision_id IS NULL \
               AND h.dead_letter_at IS NULL \
               AND (h.last_recovery_attempt_at IS NULL \
                    OR h.last_recovery_attempt_at < now() - make_interval(secs => $2::double precision)) \
               AND EXISTS (SELECT 1 FROM content_chunk c WHERE c.revision_id = r.id) \
             ORDER BY r.document_id, r.revision_number DESC",
        )
        .bind(library.id)
        .bind(cooldown_secs as f64)
        .fetch_all(pool)
        .await
        .with_context(|| {
            format!(
                "failed to list auto-recovery candidates for library {}",
                library.id
            )
        })?;

        // Count docs that have a chunk-bearing revision but are within
        // the cooldown window so the operator sees what was held back
        // rather than the row appearing to vanish.
        let cooldown_skipped: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM content_document_head h \
             JOIN content_document d ON d.id = h.document_id \
             WHERE d.library_id = $1 \
               AND h.readable_revision_id IS NULL \
               AND h.active_revision_id IS NULL \
               AND h.dead_letter_at IS NULL \
               AND h.last_recovery_attempt_at IS NOT NULL \
               AND h.last_recovery_attempt_at >= now() - make_interval(secs => $2::double precision) \
               AND EXISTS ( \
                   SELECT 1 FROM content_revision r \
                   JOIN content_chunk c ON c.revision_id = r.id \
                   WHERE r.document_id = h.document_id \
               )",
        )
        .bind(library.id)
        .bind(cooldown_secs as f64)
        .fetch_one(pool)
        .await
        .with_context(|| {
            format!(
                "failed to count cooldown-skipped null-head docs for library {}",
                library.id
            )
        })?;
        report.cooldown_skipped =
            report.cooldown_skipped.saturating_add(cooldown_skipped.max(0) as usize);

        info!(
            library_id = %library.id,
            library_name = %library.display_name,
            candidates = candidates.len(),
            cooldown_skipped,
            "auto-recovery pass starting",
        );

        for candidate in candidates {
            report.candidates_considered += 1;
            match state
                .canonical_services
                .content
                .promote_document_head(
                    state,
                    PromoteHeadCommand {
                        document_id: candidate.document_id,
                        active_revision_id: Some(candidate.revision_id),
                        readable_revision_id: Some(candidate.revision_id),
                        latest_mutation_id: None,
                        latest_successful_attempt_id: None,
                    },
                )
                .await
            {
                Ok(_) => {
                    report.promoted += 1;
                    let _ = clear_recovery_state(pool, candidate.document_id).await;
                }
                Err(error) => {
                    let error_code = classify_error(&error);
                    let dead_lettered = record_failure(
                        pool,
                        candidate.document_id,
                        &error_code,
                        candidate.last_recovery_error_code.as_deref(),
                        candidate.recovery_attempts_count,
                        DEFAULT_RECOVERY_MAX_ATTEMPTS,
                    )
                    .await
                    .unwrap_or(false);
                    if dead_lettered {
                        report.dead_lettered += 1;
                    } else {
                        report.failed += 1;
                    }
                    warn!(
                        document_id = %candidate.document_id,
                        revision_id = %candidate.revision_id,
                        error_code = %error_code,
                        ?error,
                        dead_lettered,
                        "auto-recovery promote_document_head failed",
                    );
                }
            }
        }
    }

    info!(
        libraries_scanned = report.libraries_scanned,
        candidates = report.candidates_considered,
        promoted = report.promoted,
        failed = report.failed,
        dead_lettered = report.dead_lettered,
        cooldown_skipped = report.cooldown_skipped,
        "auto-recovery finished",
    );
    Ok(report)
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
/// ended up dead-lettered (i.e. attempts ≥ max_attempts with the same
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

/// Reduce an arbitrary error to a stable string used to bucket retries.
/// Two failures share a bucket when this returns the same code, which
/// is what the rate-limit treats as "same error" — three consecutive
/// hits with the same bucket inside the cool-down window cross into
/// dead-letter. The bucket is the first 64 chars of the error
/// `Display`, lowercased; that is short enough to round-trip through
/// the `text` column cheaply and stable enough that the same error
/// type produces the same bucket across runs.
fn classify_error(error: &dyn std::error::Error) -> String {
    error.to_string().chars().take(64).collect::<String>().to_ascii_lowercase()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_truncates_to_sixty_four_chars() {
        let long = "x".repeat(200);
        let err = anyhow::anyhow!("{long}");
        let bucket = classify_error(err.as_ref());
        assert_eq!(bucket.len(), 64);
        assert!(bucket.chars().all(|c| c == 'x'));
    }

    #[test]
    fn classify_lowercases_for_stable_buckets() {
        let err = anyhow::anyhow!("Postgres Connection Refused on 5432");
        let bucket = classify_error(err.as_ref());
        assert_eq!(bucket, "postgres connection refused on 5432");
    }

    #[test]
    fn classify_distinguishes_distinct_messages() {
        let a = anyhow::anyhow!("provider returned 502 bad gateway");
        let b = anyhow::anyhow!("provider returned 504 gateway timeout");
        assert_ne!(classify_error(a.as_ref()), classify_error(b.as_ref()));
    }
}
