use sqlx::{Executor, FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::reextract_sql::{
    AUTOMATIC_GRAPH_REEXTRACT_PRIORITY, AUTOMATIC_GRAPH_REEXTRACT_TERMINAL_BACKOFF_SECONDS,
    AUTOMATIC_GRAPH_REEXTRACT_TERMINAL_JOB_LIMIT, EXPLICIT_GRAPH_REEXTRACT_PRIORITY,
    GraphReextractSelectionMode, graph_rebuild_document_completions_query,
    graph_reextract_candidates_query, graph_reextract_progress_query,
};

const GRAPH_REEXTRACT_STATEMENT_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromRow)]
pub struct GraphReextractProgressRow {
    pub ready_document_count: i64,
    pub known_eligible_document_count: i64,
    pub current_extraction_document_count: i64,
    pub missing_current_extraction_document_count: i64,
    pub outdated_or_incomplete_document_count: i64,
    pub never_extracted_document_count: i64,
    pub queued_job_count: i64,
    pub leased_job_count: i64,
    pub paused_job_count: i64,
}

#[derive(Debug, Clone, FromRow)]
struct GraphRebuildDocumentCompletionQueryRow {
    workspace_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    activated_by_attempt_id: Option<Uuid>,
    graph_contribution_count_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphRebuildDocumentCompletionRow {
    pub workspace_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    /// Successful ingest attempt that produced the selected completion marker.
    /// `None` means no valid current-version completion exists.
    pub activated_by_attempt_id: Option<Uuid>,
    pub graph_contribution_count: Option<usize>,
}

/// Lists readable active-library documents eligible for the requested graph
/// recovery boundary.
/// Completion is accepted only from a completed ingest job, so a ready row for
/// one chunk cannot hide an interrupted multi-chunk revision. Publication
/// suppresses repair only when exact-revision evidence targets the document
/// node at `runtime_graph_snapshot.projection_version` (or projection 1 before
/// a snapshot exists), so a stale prior-revision or prior-projection node cannot
/// hide missing active topology. Valid zero-contribution completion is terminal
/// and is never retried merely because no publication evidence exists — but
/// only when the completion carries the running extraction contract
/// (`extractionContractVersion`): explicit version recovery treats a completion
/// stamped by another contract, or by no contract at all (pre-contract rows), as
/// a coverage gap.
///
/// [`GraphReextractSelectionMode::ProjectionRepair`] requires the active
/// exact-revision active-projection publication to be absent before considering
/// missing, malformed, or positive completions, and admits a missing/malformed
/// completion only when current-version/current-revision extraction history or
/// a latest exact-revision started/failed graph stage proves a real interrupted
/// attempt. Explicit version recovery can
/// additionally replace a missing or malformed current completion even when an
/// active node exists, and retains the operator's history-only option. The
/// extraction-contract gap is crossed by explicit recovery only; automatic
/// projection repair keeps its conservative current-completion view. Both
/// boundaries honor the same canonical durable terminal-failure ceiling and
/// cooldown for their own priority, so a terminal job that frees the dedupe key
/// cannot be re-admitted without bound.
pub async fn list_library_documents_needing_graph_reextract_for_version(
    pool: &PgPool,
    library_id: Uuid,
    extraction_version: &str,
    extraction_contract_version: &str,
    selection_mode: GraphReextractSelectionMode,
    limit: i64,
) -> Result<Vec<(Uuid, Uuid, Uuid)>, sqlx::Error> {
    let query = graph_reextract_candidates_query(selection_mode);
    let mut transaction = begin_reextract_timeout_tx(pool).await?;
    // The durable terminal-failure history is scoped to the calling path's own
    // repair priority: automatic projection repair counts priority-250 jobs,
    // explicit version recovery counts priority-200 jobs. The ceiling and
    // cooldown constants are the one canonical policy shared by both.
    let priority = match selection_mode {
        GraphReextractSelectionMode::ProjectionRepair => AUTOMATIC_GRAPH_REEXTRACT_PRIORITY,
        GraphReextractSelectionMode::VersionRecovery { .. } => EXPLICIT_GRAPH_REEXTRACT_PRIORITY,
    };
    let mut statement = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(sqlx::AssertSqlSafe(&*query))
        .bind(library_id)
        .bind(extraction_version)
        .bind(selection_mode.include_never_extracted())
        .bind(limit)
        .bind(priority)
        .bind(AUTOMATIC_GRAPH_REEXTRACT_TERMINAL_JOB_LIMIT)
        .bind(AUTOMATIC_GRAPH_REEXTRACT_TERMINAL_BACKOFF_SECONDS as f64);
    // The extraction-contract boundary lives only in the explicit
    // version-recovery predicate, so the automatic projection-repair query does
    // not reference `$8` and must not bind it.
    if matches!(selection_mode, GraphReextractSelectionMode::VersionRecovery { .. }) {
        statement = statement.bind(extraction_contract_version);
    }
    let rows = statement.fetch_all(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(rows)
}

/// Returns extraction-version coverage and live `graph_refresh` queue depth for
/// the readable documents of one active library.
pub async fn get_library_graph_reextract_progress(
    pool: &PgPool,
    library_id: Uuid,
    extraction_version: &str,
) -> Result<GraphReextractProgressRow, sqlx::Error> {
    let query = graph_reextract_progress_query();
    let mut transaction = begin_reextract_timeout_tx(pool).await?;
    let row = sqlx::query_as::<_, GraphReextractProgressRow>(sqlx::AssertSqlSafe(&*query))
        .bind(library_id)
        .bind(extraction_version)
        .fetch_one(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(row)
}

/// Returns every active/readable document revision in a library together with
/// the contribution count from its latest successful, current-version
/// `extract_graph` completion. Missing or malformed counts remain `None` so
/// rebuild publication and recovery fail closed.
pub async fn list_graph_rebuild_document_completions(
    pool: &PgPool,
    library_id: Uuid,
    extraction_version: &str,
) -> Result<Vec<GraphRebuildDocumentCompletionRow>, sqlx::Error> {
    list_graph_rebuild_document_completions_with_executor(pool, library_id, extraction_version)
        .await
}

/// Returns graph-rebuild document completions using the supplied database
/// executor so the query and its parsing can participate in a wider snapshot.
/// Missing or malformed contribution counts remain `None`.
///
/// # Errors
/// Returns any `SQLx` error raised while querying completion records.
pub async fn list_graph_rebuild_document_completions_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    extraction_version: &str,
) -> Result<Vec<GraphRebuildDocumentCompletionRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let query = graph_rebuild_document_completions_query();
    let rows =
        sqlx::query_as::<_, GraphRebuildDocumentCompletionQueryRow>(sqlx::AssertSqlSafe(&*query))
            .bind(library_id)
            .bind(extraction_version)
            .fetch_all(executor)
            .await?;

    Ok(rows
        .into_iter()
        .map(|row| GraphRebuildDocumentCompletionRow {
            workspace_id: row.workspace_id,
            document_id: row.document_id,
            revision_id: row.revision_id,
            activated_by_attempt_id: row.activated_by_attempt_id,
            graph_contribution_count: parse_graph_contribution_count(
                row.graph_contribution_count_text.as_deref(),
            ),
        })
        .collect())
}

fn parse_graph_contribution_count(value: Option<&str>) -> Option<usize> {
    value?.parse().ok()
}

async fn begin_reextract_timeout_tx(
    pool: &PgPool,
) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query("select set_config('statement_timeout', $1, true)")
        .bind(format!("{GRAPH_REEXTRACT_STATEMENT_TIMEOUT_MS}ms"))
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

#[cfg(test)]
#[path = "reextract_reads_tests.rs"]
mod tests;
