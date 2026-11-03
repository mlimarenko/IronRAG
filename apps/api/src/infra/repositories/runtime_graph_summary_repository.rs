use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Executor, FromRow, PgConnection, PgPool, Postgres};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphTargetSummaryRow {
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UpsertRuntimeGraphTargetSummaryInput {
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

/// Probes raw candidate identities without admitting or hydrating graph rows.
/// A result reaching `limit` is only a lower bound on the projection's size.
///
/// # Errors
/// Returns a database error or rejects a non-positive probe limit.
pub async fn probe_runtime_graph_summary_targets_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
    limit: i64,
) -> Result<Vec<(String, Uuid)>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    if limit <= 0 {
        return Err(sqlx::Error::Protocol("summary candidate limit must be positive".into()));
    }
    sqlx::query_as::<_, (String, Uuid)>(
        "select target_kind, target_id from (
             (select 'node'::text as target_kind, id as target_id
              from runtime_graph_node
              where library_id = $1 and projection_version = $2 limit $3)
             union all
             (select 'edge'::text as target_kind, id as target_id
              from runtime_graph_edge
              where library_id = $1 and projection_version = $2 limit $3)
         ) as candidates limit $3",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(limit)
    .fetch_all(executor)
    .await
}

pub async fn supersede_runtime_graph_summaries_for_target_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    keep_source_truth_version: i64,
) -> Result<u64, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(
        "delete from runtime_graph_\u{63}anonical_summary
         where library_id = $1
           and target_kind = $2
           and target_id = $3
           and source_truth_version <> $4",
    )
    .bind(library_id)
    .bind(target_kind)
    .bind(target_id)
    .bind(keep_source_truth_version)
    .execute(executor)
    .await
    .map(|result| result.rows_affected())
}

pub async fn supersede_runtime_graph_summaries_for_library_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    keep_source_truth_version: i64,
) -> Result<u64, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(
        "delete from runtime_graph_\u{63}anonical_summary
         where library_id = $1
           and source_truth_version <> $2",
    )
    .bind(library_id)
    .bind(keep_source_truth_version)
    .execute(executor)
    .await
    .map(|result| result.rows_affected())
}

pub async fn supersede_runtime_graph_summaries_for_targets_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    keep_source_truth_version: i64,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<u64, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    if node_ids.is_empty() && edge_ids.is_empty() {
        return Ok(0);
    }

    sqlx::query(
        "delete from runtime_graph_\u{63}anonical_summary
         where library_id = $1
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
    .execute(executor)
    .await
    .map(|result| result.rows_affected())
}

pub async fn upsert_runtime_graph_target_summary_with_connection(
    connection: &mut PgConnection,
    input: &UpsertRuntimeGraphTargetSummaryInput,
) -> Result<RuntimeGraphTargetSummaryRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphTargetSummaryRow>(
        "with superseded as (
             delete from runtime_graph_\u{63}anonical_summary
             where library_id = $3 and target_kind = $4 and target_id = $5
               and source_truth_version <> $9
         )
         insert into runtime_graph_\u{63}anonical_summary (
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
             updated_at = now()
         returning id, workspace_id, library_id, target_kind, target_id, summary_text,
            confidence_status, support_count, source_truth_version, generated_from_mutation_id,
            warning_text, generated_at, created_at, updated_at",
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
    .fetch_one(&mut *connection)
    .await
}

/// Loads the active graph summary for one graph target.
///
/// # Errors
/// Returns any `SQLx` error raised while querying active graph summary rows.
pub async fn get_active_runtime_graph_summary_by_target(
    pool: &PgPool,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<Option<RuntimeGraphTargetSummaryRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphTargetSummaryRow>(
        "select id, workspace_id, library_id, target_kind, target_id, summary_text,
            confidence_status, support_count, source_truth_version, generated_from_mutation_id,
            warning_text, generated_at, created_at, updated_at
         from runtime_graph_\u{63}anonical_summary
         where library_id = $1
           and target_kind = $2
           and target_id = $3
         order by generated_at desc, created_at desc
         limit 1",
    )
    .bind(library_id)
    .bind(target_kind)
    .bind(target_id)
    .fetch_optional(pool)
    .await
}
