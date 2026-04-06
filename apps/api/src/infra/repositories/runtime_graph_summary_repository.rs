use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphCanonicalSummaryRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub summary_text: String,
    pub confidence_status: String,
    pub support_count: i32,
    pub source_truth_version: i64,
    pub generated_from_mutation_id: Option<Uuid>,
    pub warning_text: Option<String>,
    pub generated_at: DateTime<Utc>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UpsertRuntimeGraphCanonicalSummaryInput {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub summary_text: String,
    pub confidence_status: String,
    pub support_count: i32,
    pub source_truth_version: i64,
    pub generated_from_mutation_id: Option<Uuid>,
    pub warning_text: Option<String>,
}

/// Marks active canonical summaries for one target stale when a newer truth version exists.
///
/// # Errors
/// Returns any `SQLx` error raised while updating canonical summary rows.
pub async fn supersede_runtime_graph_canonical_summaries_for_target(
    pool: &PgPool,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    keep_source_truth_version: i64,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "update runtime_graph_canonical_summary
         set superseded_at = now(),
             updated_at = now()
         where library_id = $1
           and target_kind = $2
           and target_id = $3
           and superseded_at is null
           and source_truth_version <> $4",
    )
    .bind(library_id)
    .bind(target_kind)
    .bind(target_id)
    .bind(keep_source_truth_version)
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
}

/// Marks every active canonical summary for one library stale when the source-truth version changes.
///
/// # Errors
/// Returns any `SQLx` error raised while updating canonical summary rows.
pub async fn supersede_runtime_graph_canonical_summaries_for_library(
    pool: &PgPool,
    library_id: Uuid,
    keep_source_truth_version: i64,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "update runtime_graph_canonical_summary
         set superseded_at = now(),
             updated_at = now()
         where library_id = $1
           and superseded_at is null
           and source_truth_version <> $2",
    )
    .bind(library_id)
    .bind(keep_source_truth_version)
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
}

/// Marks active canonical summaries stale for a targeted set of nodes and relationships.
///
/// # Errors
/// Returns any `SQLx` error raised while updating canonical summary rows.
pub async fn supersede_runtime_graph_canonical_summaries_for_targets(
    pool: &PgPool,
    library_id: Uuid,
    keep_source_truth_version: i64,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    if node_ids.is_empty() && edge_ids.is_empty() {
        return Ok(0);
    }

    sqlx::query(
        "update runtime_graph_canonical_summary
         set superseded_at = now(),
             updated_at = now()
         where library_id = $1
           and superseded_at is null
           and source_truth_version <> $2
           and (
                (target_kind = 'node' and target_id = any($3))
             or (target_kind = 'edge' and target_id = any($4))
           )",
    )
    .bind(library_id)
    .bind(keep_source_truth_version)
    .bind(node_ids)
    .bind(edge_ids)
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
}

/// Upserts one canonical summary row for a graph node or edge.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the canonical summary row.
pub async fn upsert_runtime_graph_canonical_summary(
    pool: &PgPool,
    input: &UpsertRuntimeGraphCanonicalSummaryInput,
) -> Result<RuntimeGraphCanonicalSummaryRow, sqlx::Error> {
    supersede_runtime_graph_canonical_summaries_for_target(
        pool,
        input.library_id,
        &input.target_kind,
        input.target_id,
        input.source_truth_version,
    )
    .await?;

    sqlx::query_as::<_, RuntimeGraphCanonicalSummaryRow>(
        "insert into runtime_graph_canonical_summary (
            id, workspace_id, library_id, target_kind, target_id, summary_text,
            confidence_status, support_count, source_truth_version, generated_from_mutation_id,
            warning_text
         ) values (
            $1, $2, $3, $4, $5, $6,
            $7, $8, $9, $10,
            $11
         )
         on conflict (library_id, target_kind, target_id, source_truth_version) do update
         set workspace_id = excluded.workspace_id,
             summary_text = excluded.summary_text,
             confidence_status = excluded.confidence_status,
             support_count = excluded.support_count,
             generated_from_mutation_id = excluded.generated_from_mutation_id,
             warning_text = excluded.warning_text,
             generated_at = now(),
             superseded_at = null,
             updated_at = now()
         returning id, workspace_id, library_id, target_kind, target_id, summary_text,
            confidence_status, support_count, source_truth_version, generated_from_mutation_id,
            warning_text, generated_at, superseded_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(&input.target_kind)
    .bind(input.target_id)
    .bind(&input.summary_text)
    .bind(&input.confidence_status)
    .bind(input.support_count)
    .bind(input.source_truth_version)
    .bind(input.generated_from_mutation_id)
    .bind(input.warning_text.as_deref())
    .fetch_one(pool)
    .await
}

/// Loads the active canonical summary for one graph target.
///
/// # Errors
/// Returns any `SQLx` error raised while querying canonical summary rows.
pub async fn get_active_runtime_graph_canonical_summary_by_target(
    pool: &PgPool,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<Option<RuntimeGraphCanonicalSummaryRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphCanonicalSummaryRow>(
        "select id, workspace_id, library_id, target_kind, target_id, summary_text,
            confidence_status, support_count, source_truth_version, generated_from_mutation_id,
            warning_text, generated_at, superseded_at, created_at, updated_at
         from runtime_graph_canonical_summary
         where library_id = $1
           and target_kind = $2
           and target_id = $3
           and superseded_at is null
         order by generated_at desc, created_at desc
         limit 1",
    )
    .bind(library_id)
    .bind(target_kind)
    .bind(target_id)
    .fetch_optional(pool)
    .await
}
