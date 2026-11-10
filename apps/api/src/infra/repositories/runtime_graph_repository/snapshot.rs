use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Executor, FromRow, PgPool, Postgres};
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

/// Identity of the exact graph projection selected for a retained read payload.
/// A version alone is insufficient: reconciliation can republish that version
/// with a different topology generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RuntimeGraphProjectionIdentity {
    pub library_id: Uuid,
    pub projection_version: i64,
    pub topology_generation: i64,
}

impl RuntimeGraphSnapshotRow {
    /// Carries this snapshot's identity through loading, caching and final
    /// publication-membership validation without resolving a successor snapshot.
    pub fn projection_identity(&self) -> RuntimeGraphProjectionIdentity {
        RuntimeGraphProjectionIdentity {
            library_id: self.library_id,
            projection_version: self.projection_version,
            topology_generation: self.topology_generation,
        }
    }
}

const UPSERT_RUNTIME_GRAPH_SNAPSHOT_SQL: &str = "insert into runtime_graph_snapshot (
        library_id, graph_status, projection_version, node_count, edge_count,
        provenance_coverage_percent, last_built_at, last_error_message, topology_generation,
        build_epoch
     ) values (
        $1, $2, $3, $4, $5, $6, now(), $7,
         case when $2 in ('ready', 'empty', 'failed', 'stale') then 1 else 0 end,
        $8
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
             when excluded.graph_status in ('ready', 'empty', 'failed', 'stale')
             then runtime_graph_snapshot.topology_generation + 1
             else runtime_graph_snapshot.topology_generation
         end,
         build_epoch = excluded.build_epoch,
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
                    excluded.graph_status in ('ready', 'empty', 'failed', 'stale')
                    and runtime_graph_snapshot.graph_status = 'building'
                    and excluded.build_epoch = runtime_graph_snapshot.build_epoch
                )
            )
        )
     returning library_id, graph_status, projection_version, topology_generation, node_count,
        edge_count, provenance_coverage_percent, last_built_at, last_error_message, created_at,
        updated_at";

/// Loads the active runtime graph snapshot for one library.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph snapshot.
pub async fn get_runtime_graph_snapshot(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Option<RuntimeGraphSnapshotRow>, sqlx::Error> {
    get_runtime_graph_snapshot_with_executor(pool, library_id).await
}

pub async fn get_runtime_graph_snapshot_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
) -> Result<Option<RuntimeGraphSnapshotRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphSnapshotRow>(
        "select library_id, graph_status, projection_version, topology_generation, node_count, edge_count,
            provenance_coverage_percent, last_built_at, last_error_message, created_at, updated_at
         from runtime_graph_snapshot
         where library_id = $1",
    )
    .bind(library_id)
    .fetch_optional(executor)
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

/// Values required to claim or terminally publish a runtime graph snapshot.
///
/// The named payload keeps the snapshot's status, projection and build-epoch
/// identity from being passed positionally next to the counts and coverage.
pub struct RuntimeGraphSnapshotInput<'a> {
    pub library_id: Uuid,
    pub graph_status: &'a str,
    pub projection_version: i64,
    pub build_epoch: Uuid,
    pub node_count: i32,
    pub edge_count: i32,
    pub provenance_coverage_percent: Option<f64>,
    pub last_error_message: Option<&'a str>,
}

impl RuntimeGraphSnapshotInput<'_> {
    fn is_terminal(&self) -> Result<bool, sqlx::Error> {
        match self.graph_status {
            "building" => Ok(false),
            "ready" | "empty" | "failed" | "stale" => Ok(true),
            status => Err(sqlx::Error::Protocol(format!(
                "unsupported runtime graph snapshot status: {status}"
            ))),
        }
    }
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
    input: RuntimeGraphSnapshotInput<'_>,
    advance_source_truth_version: bool,
) -> Result<Option<RuntimeGraphSnapshotRow>, sqlx::Error> {
    let library_id = input.library_id;
    let is_terminal = input.is_terminal()?;

    let mut transaction = pool.begin().await?;
    sqlx::query_scalar::<_, String>(
        "select set_config(
            'ironrag.runtime_graph_snapshot_build_epoch',
            $1,
            true
         )",
    )
    .bind(input.build_epoch.to_string())
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

    let snapshot = upsert_runtime_graph_snapshot_with_executor(&mut *transaction, input).await?;

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

/// Transaction-bound snapshot claim/publication used by ingest graph
/// reconcile. The caller owns the catalog lock and commits this row together
/// with graph data and lifecycle publication.
pub async fn upsert_runtime_graph_snapshot_with_executor<'e, E>(
    executor: E,
    input: RuntimeGraphSnapshotInput<'_>,
) -> Result<Option<RuntimeGraphSnapshotRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    input.is_terminal()?;
    let RuntimeGraphSnapshotInput {
        library_id,
        graph_status,
        projection_version,
        build_epoch,
        node_count,
        edge_count,
        provenance_coverage_percent,
        last_error_message,
    } = input;
    sqlx::query_as::<_, RuntimeGraphSnapshotRow>(UPSERT_RUNTIME_GRAPH_SNAPSHOT_SQL)
        .bind(library_id)
        .bind(graph_status)
        .bind(projection_version)
        .bind(node_count)
        .bind(edge_count)
        .bind(provenance_coverage_percent)
        .bind(last_error_message)
        .bind(build_epoch)
        .fetch_optional(executor)
        .await
}

macro_rules! invalidate_snapshot_sql {
    ($predicate:literal) => {
        concat!(
            "update runtime_graph_snapshot snapshot
             set graph_status = 'stale', topology_generation = topology_generation + 1,
                 build_epoch = uuidv7(), updated_at = now()
             where library_id = $1 and (",
            $predicate,
            ")"
        )
    };
}

/// Retires the selected identity while preserving its last published counts.
/// The caller owns the graph/library locks and the surrounding transaction.
pub async fn invalidate_runtime_graph_snapshot(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    advance_source_truth_version: bool,
) -> Result<(), sqlx::Error> {
    let changed = sqlx::query(invalidate_snapshot_sql!("true"))
        .bind(library_id)
        .execute(&mut *connection)
        .await?
        .rows_affected();
    if changed > 0 && advance_source_truth_version {
        super::super::catalog_repository::touch_library_source_truth_version_with_executor(
            connection, library_id,
        )
        .await?;
    }
    Ok(())
}

/// Invalidates before deleting a selected target or its evidence. Historical
/// orphan cleanup leaves the published identity alone. A later error rolls back
/// both this invalidation and the bounded deletion batch.
pub async fn invalidate_runtime_graph_snapshot_for_targets(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
    evidence_ids: &[Uuid],
) -> Result<(), sqlx::Error> {
    let changed = sqlx::query(invalidate_snapshot_sql!(
        "exists (
             select 1 from runtime_graph_node node
             where node.library_id = snapshot.library_id
               and node.projection_version = snapshot.projection_version
               and (node.id = any($2) or exists (
                   select 1 from runtime_graph_evidence evidence
                   where evidence.id = any($4) and evidence.target_kind = 'node'
                     and evidence.library_id = node.library_id and evidence.target_id = node.id
               ))
         ) or exists (
             select 1 from runtime_graph_edge edge
             where edge.library_id = snapshot.library_id
               and edge.projection_version = snapshot.projection_version
               and (edge.id = any($3) or exists (
                   select 1 from runtime_graph_evidence evidence
                   where evidence.id = any($4) and evidence.target_kind = 'edge'
                     and evidence.library_id = edge.library_id and evidence.target_id = edge.id
               ))
         )"
    ))
    .bind(library_id)
    .bind(node_ids)
    .bind(edge_ids)
    .bind(evidence_ids)
    .execute(&mut *connection)
    .await?
    .rows_affected();
    if changed > 0 {
        super::super::catalog_repository::touch_library_source_truth_version_with_executor(
            connection, library_id,
        )
        .await?;
    }
    Ok(())
}

/// Lifecycle publication may commit its own successful revision, but must not
/// certify a library projection containing another source's denied membership.
pub async fn invalidate_unpublished_runtime_graph_snapshot(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    advance_source_truth_version: bool,
) -> Result<(), sqlx::Error> {
    let Some(snapshot) =
        get_runtime_graph_snapshot_with_executor(&mut *connection, library_id).await?
    else {
        return Ok(());
    };
    if matches!(snapshot.graph_status.as_str(), "ready" | "empty")
        && !super::super::content_fingerprint_repository::graph_projection_is_current_with_executor(
            &mut *connection,
            snapshot.projection_identity(),
        )
        .await?
    {
        invalidate_runtime_graph_snapshot(connection, library_id, advance_source_truth_version)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_input(graph_status: &str) -> RuntimeGraphSnapshotInput<'_> {
        RuntimeGraphSnapshotInput {
            library_id: Uuid::nil(),
            graph_status,
            projection_version: 1,
            build_epoch: Uuid::nil(),
            node_count: 3,
            edge_count: 7,
            provenance_coverage_percent: Some(55.0),
            last_error_message: None,
        }
    }

    #[test]
    fn snapshot_status_preserves_claim_and_terminal_classification() {
        assert!(!snapshot_input("building").is_terminal().unwrap());
        for status in ["ready", "empty", "failed", "stale"] {
            assert!(snapshot_input(status).is_terminal().unwrap());
        }
    }

    #[test]
    fn snapshot_status_rejects_unsupported_values() {
        for status in ["", "queued", "Ready"] {
            match snapshot_input(status).is_terminal() {
                Err(sqlx::Error::Protocol(message)) => {
                    assert_eq!(
                        message,
                        format!("unsupported runtime graph snapshot status: {status}")
                    );
                }
                result => panic!("unsupported snapshot status was accepted: {result:?}"),
            }
        }
    }
}
