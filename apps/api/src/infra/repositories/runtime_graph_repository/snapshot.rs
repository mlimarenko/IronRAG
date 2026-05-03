use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphSnapshotRow {
    pub library_id: Uuid,
    pub graph_status: String,
    pub projection_version: i64,
    pub topology_generation: i64,
    pub node_count: i32,
    pub edge_count: i32,
    pub provenance_coverage_percent: Option<f64>,
    pub last_built_at: Option<DateTime<Utc>>,
    pub last_error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Loads the active runtime graph snapshot for one library.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph snapshot.
pub async fn get_runtime_graph_snapshot(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Option<RuntimeGraphSnapshotRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphSnapshotRow>(
        "select library_id, graph_status, projection_version, topology_generation, node_count, edge_count,
            provenance_coverage_percent, last_built_at, last_error_message, created_at, updated_at
         from runtime_graph_snapshot
         where library_id = $1",
    )
    .bind(library_id)
    .fetch_optional(pool)
    .await
}

/// Upserts a runtime graph snapshot.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph snapshot.
pub async fn upsert_runtime_graph_snapshot(
    pool: &PgPool,
    library_id: Uuid,
    graph_status: &str,
    projection_version: i64,
    node_count: i32,
    edge_count: i32,
    provenance_coverage_percent: Option<f64>,
    last_error_message: Option<&str>,
) -> Result<RuntimeGraphSnapshotRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphSnapshotRow>(
        "insert into runtime_graph_snapshot (
            library_id, graph_status, projection_version, node_count, edge_count,
            provenance_coverage_percent, last_built_at, last_error_message, topology_generation
         ) values (
            $1, $2, $3, $4, $5, $6, now(), $7,
            case when $2 in ('ready', 'empty', 'failed') then 1 else 0 end
         )
         on conflict (library_id) do update
         set graph_status = excluded.graph_status,
             projection_version = excluded.projection_version,
             node_count = excluded.node_count,
             edge_count = excluded.edge_count,
             provenance_coverage_percent = excluded.provenance_coverage_percent,
             last_built_at = now(),
             last_error_message = excluded.last_error_message,
             topology_generation = case
                 when excluded.graph_status in ('ready', 'empty', 'failed')
                 then runtime_graph_snapshot.topology_generation + 1
                 else runtime_graph_snapshot.topology_generation
             end,
             updated_at = now()
         returning library_id, graph_status, projection_version, topology_generation, node_count, edge_count,
            provenance_coverage_percent, last_built_at, last_error_message, created_at, updated_at",
    )
    .bind(library_id)
    .bind(graph_status)
    .bind(projection_version)
    .bind(node_count)
    .bind(edge_count)
    .bind(provenance_coverage_percent)
    .bind(last_error_message)
    .fetch_one(pool)
    .await
}
