use super::*;
use crate::infra::repositories::query_ir_cache_repository::QueryIrCacheWrite;

/// Stable Redis key prefix for the hot IR cache.
pub(super) const REDIS_IR_CACHE_PREFIX: &str = "ir_cache";

/// Abstraction over the two-tier (Redis + Postgres) compiled-IR cache so
/// unit tests can substitute an in-memory fake while production wires the
/// real `Persistence` handles. The trait is intentionally thin — the
/// compiler only needs a keyed get / put; cache coherence between the
/// tiers (Redis warmup on pg hit, writing to both on miss) belongs to the
/// concrete implementation.
#[async_trait]
pub trait QueryIrCache: Send + Sync {
    /// Return a cached outcome for `(library_id, question_hash)` if one is
    /// available under the current schema version, or `None` on miss /
    /// transient error (errors are logged and treated as misses — the
    /// cache must never fail the compile pipeline).
    async fn get(&self, library_id: Uuid, question_hash: &str) -> Option<CachedIrEntry>;

    /// Write a freshly compiled IR to every tier that can accept it.
    /// Errors are logged inside the implementation; callers continue
    /// regardless so a cache outage never propagates into the query
    /// pipeline.
    async fn put(&self, library_id: Uuid, question_hash: &str, entry: &CachedIrEntry);
}

/// Shape persisted under one cache key. `provider_kind` / `model_name` /
/// `usage_json` are retained so a cache-served outcome can still render
/// accurate diagnostics in the query execution record.
#[derive(Debug, Clone)]
pub struct CachedIrEntry {
    pub ir: QueryIR,
    pub provider_kind: String,
    pub model_name: String,
    pub usage_json: Value,
}

/// Production cache implementation. Redis is the hot tier (24h TTL);
/// Postgres is the persistent (debug) tier. A cache miss on Redis but a
/// hit on Postgres triggers a Redis warmup so subsequent reads stay
/// fast.
pub struct PersistenceQueryIrCache<'a> {
    pub pool: &'a PgPool,
    pub redis: &'a RedisClient,
    pub schema_version: u16,
}

impl<'a> PersistenceQueryIrCache<'a> {
    #[must_use]
    pub fn new(pool: &'a PgPool, redis: &'a RedisClient) -> Self {
        Self { pool, redis, schema_version: QUERY_IR_SCHEMA_VERSION }
    }

    fn schema_version_pg(&self) -> i16 {
        i16::try_from(self.schema_version).unwrap_or(i16::MAX)
    }
}

#[async_trait]
impl<'a> QueryIrCache for PersistenceQueryIrCache<'a> {
    async fn get(&self, library_id: Uuid, question_hash: &str) -> Option<CachedIrEntry> {
        if let Some(entry) = redis_get_ir(self.redis, library_id, question_hash).await {
            return Some(CachedIrEntry {
                ir: entry,
                provider_kind: CACHE_HIT_REDIS_PROVIDER_KIND.to_string(),
                model_name: String::new(),
                usage_json: json!({"source": "redis"}),
            });
        }

        let row = match get_query_ir_cache(
            self.pool,
            library_id,
            question_hash,
            self.schema_version_pg(),
        )
        .await
        {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(
                    %library_id,
                    question_hash,
                    ?error,
                    "query_ir_cache postgres lookup failed — treating as miss"
                );
                return None;
            }
        };

        let row = row?;
        let ir: QueryIR = match serde_json::from_value(row.query_ir_json.clone()) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    %library_id,
                    question_hash,
                    ?error,
                    "query_ir_cache row failed to parse as QueryIR — treating as miss"
                );
                return None;
            }
        };

        // Warm the hot tier so the next read does not pay the pg round trip.
        redis_set_ir(self.redis, library_id, question_hash, &ir, REDIS_IR_CACHE_TTL_SECS).await;

        Some(CachedIrEntry {
            ir,
            provider_kind: CACHE_HIT_POSTGRES_PROVIDER_KIND.to_string(),
            model_name: String::new(),
            usage_json: json!({
                "source": "postgres",
                "original_provider_kind": row.provider_kind,
                "original_model_name": row.model_name,
            }),
        })
    }

    async fn put(&self, library_id: Uuid, question_hash: &str, entry: &CachedIrEntry) {
        let ir_json = match serde_json::to_value(&entry.ir) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    %library_id,
                    question_hash,
                    ?error,
                    "query_ir_cache failed to serialize IR — skipping cache write"
                );
                return;
            }
        };

        if let Err(error) = upsert_query_ir_cache(
            self.pool,
            QueryIrCacheWrite {
                library_id,
                question_hash,
                schema_version: self.schema_version_pg(),
                query_ir_json: ir_json,
                provider_kind: Some(entry.provider_kind.as_str()).filter(|v| !v.is_empty()),
                model_name: Some(entry.model_name.as_str()).filter(|v| !v.is_empty()),
                usage_json: entry.usage_json.clone(),
            },
        )
        .await
        {
            tracing::warn!(
                %library_id,
                question_hash,
                ?error,
                "query_ir_cache postgres upsert failed — continuing without persistent cache"
            );
        }

        redis_set_ir(self.redis, library_id, question_hash, &entry.ir, REDIS_IR_CACHE_TTL_SECS)
            .await;
    }
}

pub(super) async fn redis_get_ir(
    redis: &RedisClient,
    library_id: Uuid,
    question_hash: &str,
) -> Option<QueryIR> {
    let key = redis_key(library_id, question_hash);
    let mut conn = match redis.get_multiplexed_async_connection().await {
        Ok(conn) => conn,
        Err(error) => {
            log_redis_read_connect_failure(&error);
            return None;
        }
    };
    let raw: Option<String> = match conn.get(&key).await {
        Ok(value) => value,
        Err(error) => {
            log_redis_get_failure(&key, &error);
            return None;
        }
    };
    let raw = raw?;
    match serde_json::from_str::<QueryIR>(&raw) {
        Ok(ir) => Some(ir),
        Err(error) => {
            log_redis_payload_invalid(&key, &error);
            None
        }
    }
}

fn log_redis_read_connect_failure(error: &redis::RedisError) {
    tracing::warn!(?error, "query_ir_cache redis connect failed — treating as miss");
}

fn log_redis_get_failure(key: &str, error: &redis::RedisError) {
    tracing::warn!(key, ?error, "query_ir_cache redis GET failed — treating as miss");
}

fn log_redis_payload_invalid(key: &str, error: &serde_json::Error) {
    tracing::warn!(key, ?error, "query_ir_cache redis payload is not valid IR — miss");
}

pub(super) async fn redis_set_ir(
    redis: &RedisClient,
    library_id: Uuid,
    question_hash: &str,
    ir: &QueryIR,
    ttl_secs: u64,
) {
    let key = redis_key(library_id, question_hash);
    let payload = match serde_json::to_string(ir) {
        Ok(value) => value,
        Err(error) => {
            log_redis_serialize_failure(&key, &error);
            return;
        }
    };
    let mut conn = match redis.get_multiplexed_async_connection().await {
        Ok(conn) => conn,
        Err(error) => {
            log_redis_write_connect_failure(&error);
            return;
        }
    };
    if let Err(error) = conn.set_ex::<_, _, ()>(&key, payload, ttl_secs.max(1)).await {
        log_redis_set_failure(&key, &error);
    }
}

fn log_redis_serialize_failure(key: &str, error: &serde_json::Error) {
    tracing::warn!(key, ?error, "query_ir_cache redis serialize failed — skipping");
}

fn log_redis_write_connect_failure(error: &redis::RedisError) {
    tracing::warn!(?error, "query_ir_cache redis connect failed — skipping write");
}

fn log_redis_set_failure(key: &str, error: &redis::RedisError) {
    tracing::warn!(key, ?error, "query_ir_cache redis SET EX failed — skipping");
}

pub(super) fn redis_key(library_id: Uuid, question_hash: &str) -> String {
    format!("{REDIS_IR_CACHE_PREFIX}:{library_id}:{question_hash}")
}

/// Lift a cached entry into the normal success-path outcome shape. The
/// `provider_kind` field carries a `cache:*` sentinel so downstream
/// diagnostics can tell LLM-compiled from cache-served compilations apart
/// without a separate flag.
pub(super) fn cached_outcome(entry: CachedIrEntry) -> CompileQueryOutcome {
    CompileQueryOutcome {
        ir: entry.ir,
        provider_kind: entry.provider_kind,
        model_name: entry.model_name,
        usage_json: entry.usage_json,
        served_from_cache: true,
    }
}

pub(super) fn validated_cached_outcome(
    entry: CachedIrEntry,
    question: &str,
    history: &[CompileHistoryTurn],
) -> Option<CompileQueryOutcome> {
    if let Err(error) = validate_compiled_ir_for_request(question, history, &entry.ir) {
        tracing::warn!(
            provider = %entry.provider_kind,
            model = %entry.model_name,
            ?error,
            "query compiler cache entry failed typed request validation; recompiling"
        );
        return None;
    }
    Some(cached_outcome(entry))
}
