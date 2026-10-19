use sqlx::PgPool;

/// Opens a transaction with Postgres parallel query workers disabled for
/// its scope, used by the graph-topology scans below.
///
/// The topology document-link aggregate and the full edge scan are large
/// enough that the planner picks a parallel plan. Each parallel gather
/// allocates a dynamic-shared-memory segment in the Postgres server's
/// `/dev/shm`. On the stock container that mount defaults to 64 MiB, and
/// under concurrent graph loads (e.g. a browser retrying the topology
/// endpoint while a projection republish invalidates the Redis cache)
/// those segments exhaust it, surfacing as
/// `could not resize shared memory segment ... No space left on device`
/// and a HTTP 500 with no graph. Forcing a non-parallel plan keeps each
/// scan single-process so it never touches `/dev/shm`, making the topology
/// build robust on any deployment regardless of the configured shm size.
/// `SET LOCAL` is scoped to the transaction, so the connection returns to
/// the pool with the default parallel settings intact. The topology bytes
/// are cached in Redis for 24h, so this slower single-process path runs
/// only on cache miss / projection publish, never per UI request.
pub(super) async fn begin_topology_scan_tx(
    pool: &PgPool,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("set local max_parallel_workers_per_gather = 0").execute(&mut *tx).await?;
    Ok(tx)
}
