use sqlx::{Executor, PgPool, Postgres};
use uuid::Uuid;

use super::super::query_fragments::{
    admitted_runtime_graph_nodes_query, runtime_graph_node_columns,
};
use super::super::types::{RuntimeGraphNodeRow, RuntimeGraphQueryNodeRow};
use super::scan_transaction::begin_topology_scan_tx;

/// Lists admitted runtime graph nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph nodes.
#[tracing::instrument(
    level = "debug",
    name = "runtime_graph.list_admitted_nodes_by_library",
    skip_all,
    fields(%library_id, projection_version)
)]
pub async fn list_admitted_runtime_graph_nodes_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    list_admitted_runtime_graph_nodes_by_library_with_executor(pool, library_id, projection_version)
        .await
}

pub async fn list_admitted_runtime_graph_nodes_by_library_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphNodeRow>(sqlx::AssertSqlSafe(
        admitted_runtime_graph_nodes_query(""),
    ))
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(executor)
    .await
}

/// Counts admitted non-document runtime graph nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while counting graph nodes.
pub async fn count_admitted_runtime_graph_entities_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and node_type <> 'document'",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Lists the strongest admitted non-document runtime graph nodes for one
/// projection version, ranked by support count and label stability.
///
/// # Errors
/// Returns any `SQLx` error raised while querying ranked graph nodes.
pub async fn list_top_admitted_runtime_graph_entities_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    limit: usize,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(concat!(
        "select ",
        runtime_graph_node_columns!(),
        "
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and node_type <> 'document'
         order by support_count desc, label asc, created_at asc, id asc
         limit $3"
    ))
    .bind(library_id)
    .bind(projection_version)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
}

/// Lists admitted runtime graph nodes by id for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph nodes.
pub async fn list_admitted_runtime_graph_nodes_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    list_admitted_runtime_graph_nodes_by_ids_with_executor(
        pool,
        library_id,
        projection_version,
        node_ids,
    )
    .await
}

pub async fn list_admitted_runtime_graph_nodes_by_ids_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphNodeRow>(sqlx::AssertSqlSafe(
        admitted_runtime_graph_nodes_query("and node.id = any($3)"),
    ))
    .bind(library_id)
    .bind(projection_version)
    .bind(node_ids)
    .fetch_all(executor)
    .await
}

/// Fetches slim node rows for a pre-computed set of admitted ids plus every
/// `document`-type node. Mirrors
/// `list_runtime_graph_nodes_by_ids_or_document_type` but selects only the
/// columns consumed by the query-time graph index, omitting `metadata_json`.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph nodes.
pub async fn list_runtime_graph_query_nodes_by_ids_or_document_type(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    admitted_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphQueryNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphQueryNodeRow>(
        "select id, library_id, \u{63}anonical_key, label, node_type, aliases_json,
            summary, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and (node_type = 'document' or id = any($3::uuid[]))
         order by node_type asc, label asc, created_at asc, id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(admitted_ids)
    .fetch_all(pool)
    .await
}

/// Fetches the full node rows for a pre-computed set of admitted ids
/// plus every `document`-type node in the library+projection bucket.
/// Replaces `list_admitted_runtime_graph_nodes_by_library` on the
/// topology path so the node query no longer duplicates the edge scan
/// via the `admitted_edges` CTE — the caller derives the admitted ids
/// once from the compact edge list and passes them through here.
pub async fn list_runtime_graph_nodes_by_ids_or_document_type(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    admitted_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    let mut tx = begin_topology_scan_tx(pool).await?;
    let rows = sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, library_id, \u{63}anonical_key, label, node_type, aliases_json,
            summary, metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and (node_type = 'document' or id = any($3::uuid[]))
         order by node_type asc, label asc, created_at asc, id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(admitted_ids)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// Fetches runtime graph nodes by id for one projection version without
/// re-evaluating the admitted-edge predicate. Use this only when the caller
/// already proved the ids came from an admitted graph edge/entity query.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph nodes.
pub async fn list_runtime_graph_nodes_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, library_id, \u{63}anonical_key, label, node_type, aliases_json,
            summary, metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and id = any($3::uuid[])
         order by label asc, created_at asc, id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(node_ids)
    .fetch_all(pool)
    .await
}
