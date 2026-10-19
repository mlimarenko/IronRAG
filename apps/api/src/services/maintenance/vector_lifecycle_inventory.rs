//! Production collector for the vector-lifecycle `inventory.*` series.
//!
//! The bounded read-only probe lives in
//! `infra::postgres::pg_search_store::vector_inventory`; this module maps its
//! rows onto the contract's `InventorySnapshot` and adds the durable
//! last-success timestamp the maintenance lease already owns. No new lifecycle
//! state is derived here: the probe's categories and the lease's
//! `last_completed_at` are the only inputs.
//!
//! One sample is global: a single read-only classification over every shared
//! shard, with no per-library identity in the emitted series. Library isolation
//! stays in operator SQL and structured logs.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    infra::postgres::pg_search_store::read_vector_inventory,
    services::maintenance::{
        lease::MaintenanceClass,
        vector_lifecycle_metrics::{
            GcBacklogObservation, GcErrorCode, InventorySnapshot, LifecycleCategory,
            PhysicalShardObservation, VectorInventoryObservation, VectorKind,
        },
    },
};

/// Probe the whole installation once and build the bounded global inventory
/// snapshot.
///
/// # Errors
/// Returns a [`GcErrorCode`] from the contract's bounded `error.code` domain;
/// the underlying error is logged here, never emitted as a label.
pub(crate) async fn sample_global_inventory(
    pool: &PgPool,
) -> Result<InventorySnapshot, GcErrorCode> {
    let inventory = read_vector_inventory(pool, None).await.map_err(|error| {
        tracing::warn!(?error, "vector-lifecycle inventory probe failed; sample skipped");
        GcErrorCode::CatalogLookup
    })?;

    let gc_last_success_unix_seconds =
        read_gc_last_success_unix_seconds(pool).await.map_err(|error| {
            tracing::warn!(?error, "vector-lifecycle last-success read failed; sample omitted");
            GcErrorCode::Postgres
        })?;

    let vector_inventory = inventory
        .lifecycle_rows
        .into_iter()
        .map(|row| VectorInventoryObservation {
            vector_kind: VectorKind::Chunk,
            dimensions: u32::try_from(row.dimensions).unwrap_or(u32::MAX),
            lifecycle_category: LifecycleCategory::from_tag(row.lifecycle_category),
            vector_rows: u64::try_from(row.vector_rows).unwrap_or(0),
            logical_embedding_value_bytes: u64::try_from(row.logical_embedding_value_bytes)
                .unwrap_or(0),
        })
        .collect();
    let physical_shards = inventory
        .physical_shards
        .into_iter()
        .map(|shard| PhysicalShardObservation {
            vector_kind: if shard.vector_kind == "entity" {
                VectorKind::Entity
            } else {
                VectorKind::Chunk
            },
            dimensions: u32::try_from(shard.dimensions).unwrap_or(u32::MAX),
            physical_shard_bytes: u64::try_from(shard.physical_shard_bytes).unwrap_or(0),
        })
        .collect();

    Ok(InventorySnapshot {
        sampled_at_unix_seconds: unix_now_seconds(),
        vector_inventory,
        physical_shards,
        backlog: Some(GcBacklogObservation {
            vectors: u64::try_from(inventory.backlog_vectors).unwrap_or(0),
            oldest_age_seconds: inventory.backlog_oldest_age_seconds,
        }),
        gc_last_success_unix_seconds,
    })
}

/// The durable last-success timestamp for the vector-lifecycle class, in unix
/// seconds. It is the latest `last_completed_at` across every library scope of
/// the class — the newest durable completion of ANY library, never a claim that
/// the whole backlog is drained. A missing row or a NULL `last_completed_at`
/// yields `None` rather than an invented `0`.
async fn read_gc_last_success_unix_seconds(pool: &PgPool) -> anyhow::Result<Option<f64>> {
    // The column is nullable and may have no non-NULL row, so the aggregate is
    // decoded as `Option<DateTime>`.
    let decoded: Option<DateTime<Utc>> = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "select max(last_completed_at)
         from maintenance_job_run
         where class = $1
           and scope_kind = 'library'",
    )
    .bind(MaintenanceClass::GcSupersededVectors.as_str())
    .fetch_one(pool)
    .await?;
    Ok(decoded.map(|timestamp| timestamp.timestamp() as f64))
}

fn unix_now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}
