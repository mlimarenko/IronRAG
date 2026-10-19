use super::*;

// ---------------------------------------------------------------------------
// Redis helpers
// ---------------------------------------------------------------------------

/// redis-rs 1.2 defaults the per-command response timeout to 500 ms,
/// which is fine for ~100-byte IR cache reads but blows up on the
/// graph topology payload (17+ MB on a reference-sized library).
/// The prod symptom was `redis SET EX graph topology cache: timed out:
/// timed out` right after every prewarm — the cache appeared populated
/// in the log then disappeared because the SET had already failed at
/// the client. 10 s comfortably covers a 20 MB SET over the bridged
/// Docker network; any slower and something else is wrong.
const TOPOLOGY_CACHE_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn topology_connection_config() -> redis::AsyncConnectionConfig {
    redis::AsyncConnectionConfig::new().set_response_timeout(Some(TOPOLOGY_CACHE_RESPONSE_TIMEOUT))
}

pub(super) async fn redis_get_bytes(
    client: &redis::Client,
    key: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let mut conn = client
        .get_multiplexed_async_connection_with_config(&topology_connection_config())
        .await
        .context("connect to redis for graph topology cache read")?;
    let value: Option<Vec<u8>> = conn.get(key).await.context("redis GET graph topology cache")?;
    Ok(value)
}

pub(super) enum CacheWrite {
    Written,
    SkippedTooLarge,
}

pub(super) async fn redis_set_bytes(
    client: &redis::Client,
    key: &str,
    value: &[u8],
    ttl_seconds: i64,
) -> anyhow::Result<CacheWrite> {
    if value.len() > MAX_CACHE_VALUE_BYTES {
        return Ok(CacheWrite::SkippedTooLarge);
    }
    let mut conn = client
        .get_multiplexed_async_connection_with_config(&topology_connection_config())
        .await
        .context("connect to redis for graph topology cache write")?;
    let _: () = conn
        .set_ex(key, value, ttl_seconds.max(1) as u64)
        .await
        .context("redis SET EX graph topology cache")?;
    Ok(CacheWrite::Written)
}
