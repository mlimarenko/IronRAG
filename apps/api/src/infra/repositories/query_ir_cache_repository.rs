//! Persistent tier of the two-level QueryCompiler cache.
//!
//! Redis is the hot tier (24h TTL, opaque JSON blobs keyed by
//! `ir_cache:{library_id}:{question_hash}`); this repository backs the
//! Postgres tier that survives Redis restarts and lets operators audit
//! every (library, question) → IR compilation decision offline. Rows are
//! scoped by `schema_version`, and the question hash includes the compiler
//! runtime fingerprint, so stale entries are skipped without requiring a
//! compatibility reader. A `compiled_at` TTL
//! (`QUERY_IR_CACHE_MAX_AGE_DAYS`) keeps cold rows from accumulating
//! indefinitely.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::domains::query_ir::QUERY_IR_CACHE_MAX_AGE_DAYS;

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
/// it was written under the current `schema_version` AND within the last
/// `QUERY_IR_CACHE_MAX_AGE_DAYS` days. Rows that fail either gate are
/// treated as cache misses so the compiler will regenerate the IR against
/// the new schema / fresh provider output.
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
           and schema_version = $3
           and compiled_at > now() - ($4::bigint * interval '1 day')",
    )
    .bind(library_id)
    .bind(question_hash)
    .bind(schema_version)
    .bind(QUERY_IR_CACHE_MAX_AGE_DAYS)
    .fetch_optional(pool)
    .await
}

/// Purges expired `query_ir_cache` rows (older than
/// `QUERY_IR_CACHE_MAX_AGE_DAYS`). Returns the number of rows deleted.
///
/// Canonical helper — callers that want a periodic cleanup job wire it
/// in explicitly (ops endpoint, scheduled task, or a database-side
/// `pg_cron` hook). Reads are self-healing via the age gate in
/// [`get_query_ir_cache`], so this helper is a storage-hygiene knob
/// rather than a correctness requirement.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting expired rows.
pub async fn delete_expired_query_ir_cache(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from query_ir_cache
         where compiled_at < now() - ($1::bigint * interval '1 day')",
    )
    .bind(QUERY_IR_CACHE_MAX_AGE_DAYS)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::query_ir::QUERY_IR_SCHEMA_VERSION;

    #[test]
    fn query_ir_cache_ttl_and_schema_constants_are_canonical() {
        // Changing either of these without bumping the other is almost
        // always a bug: the TTL constant names the eviction horizon,
        // the schema version names the compile-contract. Pinning them
        // here keeps the repository / compiler / migration story in
        // sync so an accidental downgrade fails CI.
        assert!(
            QUERY_IR_CACHE_MAX_AGE_DAYS >= 1,
            "TTL must be positive or the cache never serves anything"
        );
        assert!(
            QUERY_IR_SCHEMA_VERSION >= 2,
            "schema_version must be ≥ 2 after the 0.3.1 consolidation / pack-budget contract bump"
        );
    }
}
