use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct QueryResultCacheRow {
    pub cache_key: String,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub source_execution_id: Uuid,
    pub readable_content_fingerprint: String,
    pub graph_projection_version: i64,
    pub graph_topology_generation: i64,
    pub binding_fingerprint: String,
    pub hit_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UpsertQueryResultCacheInput<'a> {
    pub cache_key: &'a str,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub source_execution_id: Uuid,
    pub readable_content_fingerprint: &'a str,
    pub graph_projection_version: i64,
    pub graph_topology_generation: i64,
    pub binding_fingerprint: &'a str,
}

#[derive(Debug, Clone, FromRow)]
pub struct QueryExecutionReplayRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub request_turn_id: Uuid,
    pub response_turn_id: Uuid,
    pub source_execution_id: Uuid,
    pub cache_key: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewQueryExecutionReplay<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub conversation_id: Uuid,
    pub request_turn_id: Uuid,
    pub response_turn_id: Uuid,
    pub source_execution_id: Uuid,
    pub cache_key: &'a str,
}

pub async fn get_query_result_cache(
    postgres: &PgPool,
    cache_key: &str,
) -> Result<Option<QueryResultCacheRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryResultCacheRow>(
        "select
            cache_key,
            workspace_id,
            library_id,
            source_execution_id,
            readable_content_fingerprint,
            graph_projection_version,
            graph_topology_generation,
            binding_fingerprint,
            hit_count,
            created_at,
            updated_at
         from query_result_cache
         where cache_key = $1",
    )
    .bind(cache_key)
    .fetch_optional(postgres)
    .await
}

pub async fn delete_query_result_cache(
    postgres: &PgPool,
    cache_key: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("delete from query_result_cache where cache_key = $1")
        .bind(cache_key)
        .execute(postgres)
        .await?;
    Ok(result.rows_affected())
}

pub async fn upsert_query_result_cache_winner(
    postgres: &PgPool,
    input: &UpsertQueryResultCacheInput<'_>,
) -> Result<QueryResultCacheRow, sqlx::Error> {
    sqlx::query_as::<_, QueryResultCacheRow>(
        "insert into query_result_cache (
            cache_key,
            workspace_id,
            library_id,
            source_execution_id,
            readable_content_fingerprint,
            graph_projection_version,
            graph_topology_generation,
            binding_fingerprint,
            hit_count,
            created_at,
            updated_at
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, 0, now(), now())
        on conflict (cache_key) do update
            set hit_count = query_result_cache.hit_count + 1,
                updated_at = now()
        returning
            cache_key,
            workspace_id,
            library_id,
            source_execution_id,
            readable_content_fingerprint,
            graph_projection_version,
            graph_topology_generation,
            binding_fingerprint,
            hit_count,
            created_at,
            updated_at",
    )
    .bind(input.cache_key)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.source_execution_id)
    .bind(input.readable_content_fingerprint)
    .bind(input.graph_projection_version)
    .bind(input.graph_topology_generation)
    .bind(input.binding_fingerprint)
    .fetch_one(postgres)
    .await
}

pub async fn record_query_execution_replay(
    postgres: &PgPool,
    input: &NewQueryExecutionReplay<'_>,
) -> Result<QueryExecutionReplayRow, sqlx::Error> {
    sqlx::query_as::<_, QueryExecutionReplayRow>(
        "insert into query_execution_replay (
            id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            source_execution_id,
            cache_key,
            created_at
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, now())
        returning
            id,
            workspace_id,
            library_id,
            conversation_id,
            request_turn_id,
            response_turn_id,
            source_execution_id,
            cache_key,
            created_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.conversation_id)
    .bind(input.request_turn_id)
    .bind(input.response_turn_id)
    .bind(input.source_execution_id)
    .bind(input.cache_key)
    .fetch_one(postgres)
    .await
}
