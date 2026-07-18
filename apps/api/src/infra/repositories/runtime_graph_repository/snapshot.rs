use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, Postgres};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
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

/// Returns whether a build epoch still owns the non-terminal snapshot claim.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph snapshot.
pub async fn runtime_graph_snapshot_build_is_current<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
    build_epoch: Uuid,
) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    sqlx::query_scalar::<_, bool>(
        "select true
         from runtime_graph_snapshot
         where library_id = $1
           and projection_version = $2
           and build_epoch = $3
           and graph_status = 'building'",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(build_epoch)
    .fetch_optional(executor)
    .await
    .map(|value| value.unwrap_or(false))
}

/// Claims or terminally publishes a runtime graph snapshot.
///
/// `building` claims are last-writer-wins within one projection version and
/// persist `build_epoch` as the durable owner. A terminal write is a CAS: it
/// succeeds only for that owner, except when it creates the first row. Only a
/// `building` claim may advance an existing row to a newer projection. Older
/// projections, unclaimed/superseded owners, and terminal retries return
/// `None` without changing `topology_generation` or the library source generation.
///
/// `advance_source_truth_version` is explicit because lifecycle-owned graph
/// projections must defer the library generation change to the atomic
/// lifecycle publisher. Standalone maintenance projections may still publish
/// their snapshot and generation in one transaction. Query-result cache
/// writers and replayers take a conflicting SHARE lock, so neither can commit
/// an answer across an immediate graph publication boundary.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph snapshot.
pub async fn upsert_runtime_graph_snapshot(
    pool: &PgPool,
    library_id: Uuid,
    graph_status: &str,
    projection_version: i64,
    build_epoch: Uuid,
    node_count: i32,
    edge_count: i32,
    provenance_coverage_percent: Option<f64>,
    last_error_message: Option<&str>,
    advance_source_truth_version: bool,
) -> Result<Option<RuntimeGraphSnapshotRow>, sqlx::Error> {
    let is_terminal = matches!(graph_status, "ready" | "empty" | "failed");
    if graph_status != "building" && !is_terminal {
        return Err(sqlx::Error::Protocol(format!(
            "unsupported runtime graph snapshot status: {graph_status}"
        )));
    }

    let mut transaction = pool.begin().await?;
    sqlx::query_scalar::<_, String>(
        "select set_config(
            'ironrag.runtime_graph_snapshot_build_epoch',
            $1,
            true
         )",
    )
    .bind(build_epoch.to_string())
    .fetch_one(&mut *transaction)
    .await?;
    let library_exists = sqlx::query_scalar::<_, bool>(
        "select true
         from catalog_library
         where id = $1
         for no key update",
    )
    .bind(library_id)
    .fetch_optional(&mut *transaction)
    .await?
    .unwrap_or(false);
    if !library_exists {
        transaction.rollback().await?;
        return Err(sqlx::Error::RowNotFound);
    }

    let snapshot = sqlx::query_as::<_, RuntimeGraphSnapshotRow>(
        "insert into runtime_graph_snapshot (
            library_id, graph_status, projection_version, node_count, edge_count,
            provenance_coverage_percent, last_built_at, last_error_message, topology_generation,
            build_epoch, writer_protocol_version
         ) values (
            $1, $2, $3, $4, $5, $6, now(), $7,
            case when $2 in ('ready', 'empty', 'failed') then 1 else 0 end,
            $8, 2
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
             build_epoch = excluded.build_epoch,
             writer_protocol_version = 2,
             updated_at = now()
         where (
                excluded.projection_version > runtime_graph_snapshot.projection_version
                and excluded.graph_status = 'building'
            )
            or (
                excluded.projection_version = runtime_graph_snapshot.projection_version
                and (
                    excluded.graph_status = 'building'
                    or (
                        excluded.graph_status in ('ready', 'empty', 'failed')
                        and runtime_graph_snapshot.graph_status = 'building'
                        and excluded.build_epoch = runtime_graph_snapshot.build_epoch
                    )
                )
            )
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
    .bind(build_epoch)
    .fetch_optional(&mut *transaction)
    .await?;

    if snapshot.is_some() && is_terminal && advance_source_truth_version {
        sqlx::query(
            "update catalog_library
             set source_truth_version = greatest(
                    coalesce(source_truth_version, 0) + 1,
                    (extract(epoch from clock_timestamp()) * 1000000)::bigint
                 )
             where id = $1",
        )
        .bind(library_id)
        .execute(&mut *transaction)
        .await?;
    }

    transaction.commit().await?;
    Ok(snapshot)
}
