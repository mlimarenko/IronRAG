use std::time::Duration;

use anyhow::{Context, Result};
use futures::StreamExt as _;
use redis::AsyncCommands;
use tokio::sync::oneshot;
use tracing::warn;
use uuid::Uuid;

use super::{
    CachedQueryResult, QUERY_RESULT_CACHE_LOCK_RENEW_INTERVAL, QUERY_RESULT_CACHE_LOCK_TTL_SECONDS,
    QUERY_RESULT_CACHE_TTL_SECONDS,
};

#[must_use]
pub(crate) fn lock_key(cache_key: &str) -> String {
    format!("query_result_lock:{cache_key}")
}

#[must_use]
pub(crate) fn notification_channel(cache_key: &str) -> String {
    format!("query_result_ready:{cache_key}")
}

pub(crate) struct QueryResultCacheWaiter {
    pubsub: redis::aio::PubSub,
}

impl QueryResultCacheWaiter {
    /// Waits for a cache-fill notification without treating timeout as an
    /// error. Callers always re-read the authoritative cache after a wakeup.
    pub(crate) async fn wait_for_notification(&mut self, timeout: Duration) -> Result<bool> {
        match tokio::time::timeout(timeout, self.pubsub.on_message().next()).await {
            Ok(Some(_message)) => Ok(true),
            Ok(None) => anyhow::bail!("redis query result cache notification stream ended"),
            Err(_) => Ok(false),
        }
    }
}

pub(crate) async fn subscribe_fill_notifications(
    client: &redis::Client,
    key: &str,
) -> Result<QueryResultCacheWaiter> {
    let mut pubsub = client
        .get_async_pubsub()
        .await
        .context("connect to redis for query result cache notifications")?;
    pubsub
        .subscribe(notification_channel(key))
        .await
        .context("subscribe to query result cache notifications")?;
    Ok(QueryResultCacheWaiter { pubsub })
}

#[derive(Debug)]
pub(crate) struct QueryResultCacheFillGuard {
    client: redis::Client,
    cache_key: String,
    owner: Uuid,
    stop_renewal: Option<oneshot::Sender<()>>,
}

impl QueryResultCacheFillGuard {
    #[must_use]
    fn new(client: redis::Client, cache_key: &str, owner: Uuid) -> Self {
        let cache_key = cache_key.to_string();
        let stop_renewal = spawn_fill_lock_renewal(client.clone(), cache_key.clone(), owner);
        Self { client, cache_key, owner, stop_renewal }
    }
}

impl Drop for QueryResultCacheFillGuard {
    fn drop(&mut self) {
        if let Some(stop_renewal) = self.stop_renewal.take() {
            let _ = stop_renewal.send(());
        }
        let client = self.client.clone();
        let cache_key = self.cache_key.clone();
        let owner = self.owner;
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if let Err(error) = release_fill_lock(&client, &cache_key, owner).await {
                        warn!(
                            error = %error,
                            cache_key = %cache_key,
                            owner = %owner,
                            "query result cache fill lock release failed"
                        );
                    }
                });
            }
            Err(error) => {
                warn!(
                    error = %error,
                    cache_key = %cache_key,
                    owner = %owner,
                    "query result cache fill lock dropped outside tokio runtime"
                );
            }
        }
    }
}

fn spawn_fill_lock_renewal(
    client: redis::Client,
    cache_key: String,
    owner: Uuid,
) -> Option<oneshot::Sender<()>> {
    let (stop_tx, mut stop_rx) = oneshot::channel();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                loop {
                    tokio::select! {
                        () = tokio::time::sleep(QUERY_RESULT_CACHE_LOCK_RENEW_INTERVAL) => {
                            match renew_fill_lock(&client, &cache_key, owner).await {
                                Ok(true) => {}
                                Ok(false) => {
                                    warn!(
                                        cache_key = %cache_key,
                                        owner = %owner,
                                        "query result cache fill lock renewal lost ownership"
                                    );
                                    break;
                                }
                                Err(error) => {
                                    warn!(
                                        error = %error,
                                        cache_key = %cache_key,
                                        owner = %owner,
                                        "query result cache fill lock renewal failed"
                                    );
                                    break;
                                }
                            }
                        }
                        _ = &mut stop_rx => break,
                    }
                }
            });
            Some(stop_tx)
        }
        Err(error) => {
            warn!(
                error = %error,
                cache_key = %cache_key,
                owner = %owner,
                "query result cache fill lock renewal unavailable outside tokio runtime"
            );
            None
        }
    }
}

#[cfg(test)]
pub(crate) async fn get_cached_execution_id(
    client: &redis::Client,
    key: &str,
) -> Result<Option<Uuid>> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for query result cache read")?;
    let raw: Option<Vec<u8>> = conn.get(key).await.context("redis GET query result cache")?;
    match raw {
        Some(bytes) => {
            let entry: CachedQueryResult =
                serde_json::from_slice(&bytes).context("decode query result cache payload")?;
            Ok(Some(entry.source_execution_id))
        }
        None => Ok(None),
    }
}

pub(crate) async fn put_cached_execution_id(
    client: &redis::Client,
    key: &str,
    source_execution_id: Uuid,
    ttl_seconds: u64,
) -> Result<()> {
    anyhow::ensure!(ttl_seconds > 0, "query result cache TTL must be positive");
    let ttl_seconds = ttl_seconds.min(QUERY_RESULT_CACHE_TTL_SECONDS);
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for query result cache write")?;
    let bytes = serde_json::to_vec(&CachedQueryResult { source_execution_id })
        .context("encode query result cache payload")?;
    let _: () =
        conn.set_ex(key, bytes, ttl_seconds).await.context("redis SET EX query result cache")?;
    let _: usize = conn
        .publish(notification_channel(key), source_execution_id.to_string())
        .await
        .context("redis PUBLISH query result cache notification")?;
    Ok(())
}

/// Bounds the remaining lifetime computed by PostgreSQL. Returning that
/// original lifetime when warming Redis prevents an early Redis eviction from
/// extending a persistent row, without trusting an API replica's wall clock.
#[must_use]
pub(crate) fn bounded_db_remaining_ttl_seconds(remaining_seconds: i64) -> Option<u64> {
    if remaining_seconds <= 0 {
        return None;
    }
    Some(u64::try_from(remaining_seconds).ok()?.min(QUERY_RESULT_CACHE_TTL_SECONDS))
}

/// Deletes a Redis winner only when it still points at the stale execution the
/// caller inspected. A concurrent replacement must survive old-reader cleanup.
pub(crate) async fn delete_cached_execution_id_if_matches(
    client: &redis::Client,
    key: &str,
    expected_source_execution_id: Uuid,
) -> Result<bool> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for conditional query result cache delete")?;
    let expected = serde_json::to_vec(&CachedQueryResult {
        source_execution_id: expected_source_execution_id,
    })
    .context("encode expected query result cache payload")?;
    let deleted: i64 = redis::cmd("EVAL")
        .arg(
            "if redis.call('GET', KEYS[1]) == ARGV[1] \
             then return redis.call('DEL', KEYS[1]) \
             else return 0 end",
        )
        .arg(1)
        .arg(key)
        .arg(expected)
        .query_async(&mut conn)
        .await
        .context("redis EVAL conditional query result cache delete")?;
    Ok(deleted > 0)
}

/// Structural test for a Redis *availability* failure (server down,
/// unreachable, connection dropped, or timed out) as opposed to a
/// protocol/server error that signals something genuinely wrong.
///
/// The fill-lock fast path must degrade to cache-miss latency when Redis
/// is simply unavailable, never surface a 5xx — the answer pipeline does
/// not need Redis to compute. We classify on the typed [`redis::RedisError`]
/// error kind, never on message-string matching.
#[must_use]
fn redis_error_is_connectivity(error: &redis::RedisError) -> bool {
    error.is_io_error()
        || error.is_connection_refusal()
        || error.is_connection_dropped()
        || error.is_timeout()
}

/// Decide whether a fill-lock acquisition error should fail *open* (proceed
/// without the lock, degrading to cache-miss behaviour) or fail *closed*
/// (surface coordination-unavailable to the caller).
///
/// Errors returned by [`try_acquire_fill_guard`] are always Redis
/// connect/command failures — genuine lock contention is reported as
/// `Ok(None)`, not `Err`. We still distinguish the two error *classes*
/// structurally: a connectivity failure (Redis down) fails open; any other
/// Redis error (e.g. an unexpected server/protocol fault) keeps the
/// conservative fail-closed semantics so a real malfunction stays visible.
#[must_use]
pub(crate) fn fill_lock_error_fails_open(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<redis::RedisError>().is_some_and(redis_error_is_connectivity)
    })
}

pub(crate) async fn try_acquire_fill_guard(
    client: &redis::Client,
    key: &str,
    owner: Uuid,
) -> Result<Option<QueryResultCacheFillGuard>> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for query result cache lock")?;
    let response: Option<String> = redis::cmd("SET")
        .arg(lock_key(key))
        .arg(owner.to_string())
        .arg("NX")
        .arg("EX")
        .arg(QUERY_RESULT_CACHE_LOCK_TTL_SECONDS)
        .query_async(&mut conn)
        .await
        .context("redis SET NX query result cache lock")?;
    Ok(response.is_some().then(|| QueryResultCacheFillGuard::new(client.clone(), key, owner)))
}

async fn release_fill_lock(client: &redis::Client, key: &str, owner: Uuid) -> Result<bool> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for query result cache lock release")?;
    let released: i64 = redis::cmd("EVAL")
        .arg(
            "if redis.call('GET', KEYS[1]) == ARGV[1] \
             then return redis.call('DEL', KEYS[1]) \
             else return 0 end",
        )
        .arg(1)
        .arg(lock_key(key))
        .arg(owner.to_string())
        .query_async(&mut conn)
        .await
        .context("redis EVAL query result cache lock release")?;
    Ok(released > 0)
}

async fn renew_fill_lock(client: &redis::Client, key: &str, owner: Uuid) -> Result<bool> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for query result cache lock renewal")?;
    let renewed: i64 = redis::cmd("EVAL")
        .arg(
            "if redis.call('GET', KEYS[1]) == ARGV[1] \
             then return redis.call('EXPIRE', KEYS[1], ARGV[2]) \
             else return 0 end",
        )
        .arg(1)
        .arg(lock_key(key))
        .arg(owner.to_string())
        .arg(QUERY_RESULT_CACHE_LOCK_TTL_SECONDS)
        .query_async(&mut conn)
        .await
        .context("redis EVAL query result cache lock renewal")?;
    Ok(renewed > 0)
}
