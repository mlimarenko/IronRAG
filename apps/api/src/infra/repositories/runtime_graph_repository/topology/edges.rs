use sqlx::{Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;

use super::super::query_fragments::runtime_graph_edge_columns;
use super::super::types::{RuntimeGraphEdgeRow, RuntimeGraphQueryEdgeRow};
use super::scan_transaction::begin_topology_scan_tx;

/// Lists admitted runtime graph edges for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
#[tracing::instrument(
    level = "debug",
    name = "runtime_graph.list_admitted_edges_by_library",
    skip_all,
    fields(%library_id, projection_version)
)]
pub async fn list_admitted_runtime_graph_edges_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    list_admitted_runtime_graph_edges_by_library_with_executor(pool, library_id, projection_version)
        .await
}

pub async fn list_admitted_runtime_graph_edges_by_library_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(concat!(
        "select ",
        runtime_graph_edge_columns!(),
        "
         from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, created_at asc, id asc"
    ))
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(executor)
    .await
}

/// Lists admitted runtime graph edges for one projection version using the slim
/// query row type — drops `metadata_json` and `identity_key` to cut
/// per-row heap usage for the query-time graph index path.
///
/// This is the direct replacement for `list_admitted_runtime_graph_edges_by_library`
/// on the query path. Ingest and graph-stream callers that need the full row
/// continue to use the fat variant.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
pub async fn list_admitted_runtime_graph_query_edges_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphQueryEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphQueryEdgeRow>(
        "select id, library_id, from_node_id, to_node_id, relation_type,
            summary, weight, support_count, projection_version, created_at, updated_at
         from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, created_at asc, id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Compact edge row — only the columns consumed by the NDJSON topology
/// (`build_compact_topology` in `services/knowledge/graph_stream.rs`).
/// Dropping the wide columns cuts the row width ~5× and lets Postgres
/// serve the full result set from index leaf pages without heap fetches.
#[derive(Debug, Clone, FromRow)]
pub struct RuntimeGraphEdgeCompactRow {
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub support_count: i32,
}

pub async fn list_admitted_runtime_graph_edges_compact_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeCompactRow>, sqlx::Error> {
    let mut tx = begin_topology_scan_tx(pool).await?;
    let rows = sqlx::query_as::<_, RuntimeGraphEdgeCompactRow>(
        "select from_node_id, to_node_id, relation_type, support_count
         from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, support_count desc, from_node_id asc, to_node_id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// Counts admitted runtime graph relations whose endpoints are both non-document
/// nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while counting graph edges.
pub async fn count_admitted_runtime_graph_relations_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from runtime_graph_edge as edge
         inner join runtime_graph_node as source
            on source.library_id = edge.library_id
           and source.id = edge.from_node_id
           and source.projection_version = edge.projection_version
           and source.node_type <> 'document'
         inner join runtime_graph_node as target
            on target.library_id = edge.library_id
           and target.id = edge.to_node_id
           and target.projection_version = edge.projection_version
           and target.node_type <> 'document'
         where edge.library_id = $1
           and edge.projection_version = $2
           and btrim(edge.relation_type) <> ''
           and edge.from_node_id <> edge.to_node_id",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Lists the strongest admitted runtime graph relations whose endpoints are
/// both non-document nodes.
///
/// # Errors
/// Returns any `SQLx` error raised while querying ranked graph edges.
pub async fn list_top_admitted_runtime_graph_relations_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    limit: usize,
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select edge.id, edge.library_id, edge.from_node_id, edge.to_node_id, edge.relation_type,
            edge.\u{63}anonical_key, edge.summary, edge.weight, edge.support_count, edge.metadata_json,
            edge.projection_version, edge.created_at, edge.updated_at
         from runtime_graph_edge as edge
         inner join runtime_graph_node as source
            on source.library_id = edge.library_id
           and source.id = edge.from_node_id
           and source.projection_version = edge.projection_version
           and source.node_type <> 'document'
         inner join runtime_graph_node as target
            on target.library_id = edge.library_id
           and target.id = edge.to_node_id
           and target.projection_version = edge.projection_version
           and target.node_type <> 'document'
         where edge.library_id = $1
           and edge.projection_version = $2
           and btrim(edge.relation_type) <> ''
           and edge.from_node_id <> edge.to_node_id
         order by edge.support_count desc, edge.relation_type asc, edge.created_at asc, edge.id asc
         limit $3",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
}

/// Lists admitted runtime graph edges by id for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
pub async fn list_admitted_runtime_graph_edges_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    edge_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    list_admitted_runtime_graph_edges_by_ids_with_executor(
        pool,
        library_id,
        projection_version,
        edge_ids,
    )
    .await
}

pub async fn list_admitted_runtime_graph_edges_by_ids_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
    edge_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    if edge_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphEdgeRow>(concat!(
        "select ",
        runtime_graph_edge_columns!(),
        "
         from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and id = any($3)
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, created_at asc, id asc"
    ))
    .bind(library_id)
    .bind(projection_version)
    .bind(edge_ids)
    .fetch_all(executor)
    .await
}

/// Lists admitted runtime graph edges that touch any of the supplied node ids.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
pub async fn list_admitted_runtime_graph_edges_by_node_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    list_admitted_runtime_graph_edges_by_node_ids_with_executor(
        pool,
        library_id,
        projection_version,
        node_ids,
    )
    .await
}

pub async fn list_admitted_runtime_graph_edges_by_node_ids_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphEdgeRow>(concat!(
        "select ",
        runtime_graph_edge_columns!(),
        "
         from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and (from_node_id = any($3) or to_node_id = any($3))
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, created_at asc, id asc"
    ))
    .bind(library_id)
    .bind(projection_version)
    .bind(node_ids)
    .fetch_all(executor)
    .await
}
