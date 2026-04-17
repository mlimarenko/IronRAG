//! Persistent tier of the two-level QueryCompiler cache.
//!
//! Redis is the hot tier (24h TTL, opaque JSON blobs keyed by
//! `ir_cache:v1:{library_id}:{question_hash}`); this repository backs the
//! Postgres tier that survives Redis restarts and lets operators audit
//! every (library, question) → IR compilation decision offline. Rows are
//! scoped by `schema_version` so a schema bump automatically skips stale
//! entries without requiring an explicit purge.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct QueryIrCacheRow {
    pub query_ir_json: Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub usage_json: Value,
    pub compiled_at: DateTime<Utc>,
}

/// Inserts or refreshes one cache row keyed by `(library_id, question_hash)`.
///
/// # Errors
/// Returns any `SQLx` error raised while persisting the cache row.
pub async fn upsert_query_ir_cache(
    pool: &PgPool,
    library_id: Uuid,
    question_hash: &str,
    schema_version: i16,
    query_ir_json: Value,
    provider_kind: Option<&str>,
    model_name: Option<&str>,
    usage_json: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into query_ir_cache (
            library_id, question_hash, schema_version, query_ir_json,
            provider_kind, model_name, usage_json, compiled_at
         ) values ($1, $2, $3, $4, $5, $6, $7, now())
         on conflict (library_id, question_hash) do update
         set schema_version = excluded.schema_version,
             query_ir_json = excluded.query_ir_json,
             provider_kind = excluded.provider_kind,
             model_name = excluded.model_name,
             usage_json = excluded.usage_json,
             compiled_at = excluded.compiled_at",
    )
    .bind(library_id)
    .bind(question_hash)
    .bind(schema_version)
    .bind(query_ir_json)
    .bind(provider_kind)
    .bind(model_name)
    .bind(usage_json)
    .execute(pool)
    .await?;
    Ok(())
}

/// Loads one cache row for the given `(library_id, question_hash)` provided
/// it was written under the current `schema_version`. Rows from older schema
/// versions are treated as cache misses so the compiler will regenerate the
/// IR against the new schema.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the cache row.
pub async fn get_query_ir_cache(
    pool: &PgPool,
    library_id: Uuid,
    question_hash: &str,
    schema_version: i16,
) -> Result<Option<QueryIrCacheRow>, sqlx::Error> {
    sqlx::query_as::<_, QueryIrCacheRow>(
        "select query_ir_json, provider_kind, model_name, usage_json, compiled_at
         from query_ir_cache
         where library_id = $1
           and question_hash = $2
           and schema_version = $3",
    )
    .bind(library_id)
    .bind(question_hash)
    .bind(schema_version)
    .fetch_optional(pool)
    .await
}
