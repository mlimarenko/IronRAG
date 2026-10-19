use sqlx::{Executor, PgPool, Postgres};
use uuid::Uuid;

use super::query_fragments::runtime_graph_edge_columns;
use super::types::{RuntimeGraphEdgeAdjacencyRow, RuntimeGraphEdgeRow};

/// Upserts a active runtime graph edge.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph edge.
pub async fn upsert_runtime_graph_edge(
    pool: &PgPool,
    input: RuntimeGraphEdgeInput<'_>,
) -> Result<RuntimeGraphEdgeRow, sqlx::Error> {
    upsert_runtime_graph_edge_with_executor(pool, input).await
}

pub async fn upsert_runtime_graph_edge_with_executor<'e, E>(
    executor: E,
    input: RuntimeGraphEdgeInput<'_>,
) -> Result<RuntimeGraphEdgeRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "insert into runtime_graph_edge (
            id, library_id, from_node_id, to_node_id, relation_type, \u{63}anonical_key, summary,
            weight, support_count, metadata_json, projection_version
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         on conflict (library_id, \u{63}anonical_key, projection_version) do update
         set from_node_id = excluded.from_node_id,
             to_node_id = excluded.to_node_id,
             relation_type = excluded.relation_type,
             summary = excluded.summary,
             weight = excluded.weight,
             support_count = excluded.support_count,
             metadata_json = excluded.metadata_json,
             updated_at = now()
         returning id, library_id, from_node_id, to_node_id, relation_type, \u{63}anonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.library_id)
    .bind(input.from_node_id)
    .bind(input.to_node_id)
    .bind(input.relation_type)
    .bind(input.identity_key)
    .bind(input.summary)
    .bind(input.weight)
    .bind(input.support_count)
    .bind(input.metadata_json)
    .bind(input.projection_version)
    .fetch_one(executor)
    .await
}

/// Values required to upsert one active runtime graph edge.
///
/// The named payload keeps the edge's two endpoint ids and its identity fields
/// from being passed positionally next to the projection and support counters.
pub struct RuntimeGraphEdgeInput<'a> {
    pub library_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: &'a str,
    pub identity_key: &'a str,
    pub summary: Option<&'a str>,
    pub weight: Option<f64>,
    pub support_count: i32,
    pub metadata_json: serde_json::Value,
    pub projection_version: i64,
}

/// Loads one active runtime graph edge for a projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph edge.
pub async fn get_runtime_graph_edge_by_key(
    pool: &PgPool,
    library_id: Uuid,
    identity_key: &str,
    projection_version: i64,
) -> Result<Option<RuntimeGraphEdgeRow>, sqlx::Error> {
    get_runtime_graph_edge_by_key_with_executor(pool, library_id, identity_key, projection_version)
        .await
}

pub async fn get_runtime_graph_edge_by_key_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    identity_key: &str,
    projection_version: i64,
) -> Result<Option<RuntimeGraphEdgeRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(concat!(
        "select ",
        runtime_graph_edge_columns!(),
        "
         from runtime_graph_edge
         where library_id = $1 and \u{63}anonical_key = $2 and projection_version = $3"
    ))
    .bind(library_id)
    .bind(identity_key)
    .bind(projection_version)
    .fetch_optional(executor)
    .await
}

/// Loads one active runtime graph edge by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph edge.
pub async fn get_runtime_graph_edge_by_id(
    pool: &PgPool,
    library_id: Uuid,
    id: Uuid,
) -> Result<Option<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(concat!(
        "select ",
        runtime_graph_edge_columns!(),
        "
         from runtime_graph_edge
         where library_id = $1 and id = $2"
    ))
    .bind(library_id)
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Lists active runtime graph edges for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph edges.
pub async fn list_runtime_graph_edges_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(concat!(
        "select ",
        runtime_graph_edge_columns!(),
        "
         from runtime_graph_edge
         where library_id = $1 and projection_version = $2
         order by relation_type asc, created_at asc, id asc"
    ))
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Lists `(from, to, support_count)` triples for every edge — the lean
/// projection consumed by community detection / label propagation. Skips
/// heavy text columns (`summary`, `relation_type`, `identity_key`,
/// `metadata_json`) and `ORDER BY` because label propagation produces its
/// own deterministic node ordering. Cuts the in-RAM Vec from KB-scale edge
/// payloads to the adjacency fields required by the algorithm.
///
/// # Errors
/// Returns any `SQLx` error raised while querying edges.
#[tracing::instrument(skip(pool), fields(library_id = %library_id, projection_version))]
pub async fn list_runtime_graph_edges_adjacency_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeAdjacencyRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeAdjacencyRow>(
        "select from_node_id, to_node_id, support_count
         from runtime_graph_edge
         where library_id = $1 and projection_version = $2",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}
