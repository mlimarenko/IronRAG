//! PostgreSQL integration coverage for runtime graph snapshot publication fencing.

use std::sync::Arc;

use anyhow::{Context, Result};
use ironrag_backend::{
    app::config::Settings,
    infra::repositories::{self, catalog_repository},
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::sync::Barrier;
use uuid::Uuid;

struct Fixture {
    workspace_id: Uuid,
    library_id: Uuid,
}

impl Fixture {
    async fn create(pool: &PgPool) -> Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            pool,
            &format!("graph-snapshot-fence-{suffix}"),
            "Graph Snapshot Fence",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            pool,
            workspace.id,
            &format!("graph-snapshot-fence-{suffix}"),
            "Graph Snapshot Fence",
            None,
            None,
        )
        .await?;
        Ok(Self { workspace_id: workspace.id, library_id: library.id })
    }

    async fn cleanup(&self, pool: &PgPool) -> Result<()> {
        sqlx::query("delete from catalog_workspace where id = $1")
            .bind(self.workspace_id)
            .execute(pool)
            .await?;
        Ok(())
    }
}

async fn connect_postgres() -> Result<PgPool> {
    let settings = Settings::from_env().context("load graph snapshot fence settings")?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&settings.database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_merge_lock_serializes_workers_across_database_connections() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let result = async {
        let first =
            repositories::acquire_runtime_library_graph_merge_lock(&pool, fixture.library_id)
                .await?;
        let second_pool = pool.clone();
        let library_id = fixture.library_id;
        let second = tokio::spawn(async move {
            repositories::acquire_runtime_library_graph_merge_lock(&second_pool, library_id).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!second.is_finished(), "a second worker must wait for the library merge lock");

        repositories::release_runtime_library_graph_merge_lock(first, fixture.library_id).await?;
        let second = tokio::time::timeout(std::time::Duration::from_secs(2), second)
            .await
            .context("second worker did not acquire the released merge lock")???;
        repositories::release_runtime_library_graph_merge_lock(second, fixture.library_id).await?;
        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn terminal_publish_rejects_stale_owner_and_never_rewinds_projection() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let result = async {
        let source_before =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        let library_id = fixture.library_id;
        let first_epoch = Uuid::now_v7();
        let second_epoch = Uuid::now_v7();

        let first_claim = repositories::upsert_runtime_graph_snapshot(
            &pool,
            fixture.library_id,
            "building",
            4,
            first_epoch,
            1,
            0,
            Some(100.0),
            None,
            true,
        )
        .await?;
        assert!(first_claim.is_some());
        let second_claim = repositories::upsert_runtime_graph_snapshot(
            &pool,
            fixture.library_id,
            "building",
            4,
            second_epoch,
            2,
            1,
            Some(100.0),
            None,
            true,
        )
        .await?;
        assert!(second_claim.is_some());

        let barrier = Arc::new(Barrier::new(3));
        let stale_task = {
            let pool = pool.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                repositories::upsert_runtime_graph_snapshot(
                    &pool,
                    library_id,
                    "ready",
                    4,
                    first_epoch,
                    1,
                    0,
                    Some(100.0),
                    None,
                    true,
                )
                .await
            })
        };
        let owner_task = {
            let pool = pool.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                repositories::upsert_runtime_graph_snapshot(
                    &pool,
                    library_id,
                    "ready",
                    4,
                    second_epoch,
                    2,
                    1,
                    Some(100.0),
                    None,
                    true,
                )
                .await
            })
        };
        barrier.wait().await;

        assert!(stale_task.await??.is_none(), "superseded owner must not publish");
        let accepted = owner_task.await??.context("current owner must publish")?;
        assert_eq!(accepted.projection_version, 4);
        assert_eq!(accepted.topology_generation, 1);

        let source_after =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        assert!(source_after > source_before);

        let stale_generation = repositories::upsert_runtime_graph_snapshot(
            &pool,
            fixture.library_id,
            "building",
            3,
            Uuid::now_v7(),
            99,
            99,
            Some(1.0),
            None,
            true,
        )
        .await?;
        assert!(stale_generation.is_none(), "an older projection must never reclaim ownership");
        let snapshot = repositories::get_runtime_graph_snapshot(&pool, fixture.library_id)
            .await?
            .context("snapshot must exist")?;
        assert_eq!(snapshot.projection_version, 4);
        assert_eq!(snapshot.topology_generation, 1);

        let unclaimed_terminal = repositories::upsert_runtime_graph_snapshot(
            &pool,
            fixture.library_id,
            "ready",
            5,
            Uuid::now_v7(),
            100,
            100,
            Some(100.0),
            None,
            true,
        )
        .await?;
        assert!(
            unclaimed_terminal.is_none(),
            "a newer terminal write must claim building ownership first"
        );
        assert_eq!(
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?,
            source_after,
            "rejected writes must not invalidate result-cache generations"
        );

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn first_terminal_insert_bumps_source_generation_exactly_once() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let result = async {
        let source_before =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        let epoch = Uuid::now_v7();
        let published = repositories::upsert_runtime_graph_snapshot(
            &pool,
            fixture.library_id,
            "empty",
            1,
            epoch,
            0,
            0,
            Some(0.0),
            None,
            true,
        )
        .await?
        .context("first terminal snapshot must publish")?;
        assert_eq!(published.topology_generation, 1);
        let source_after =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        assert!(source_after > source_before);

        let duplicate = repositories::upsert_runtime_graph_snapshot(
            &pool,
            fixture.library_id,
            "empty",
            1,
            epoch,
            0,
            0,
            Some(0.0),
            None,
            true,
        )
        .await?;
        assert!(duplicate.is_none(), "terminal retries must be idempotent");
        let snapshot = repositories::get_runtime_graph_snapshot(&pool, fixture.library_id)
            .await?
            .context("snapshot must exist")?;
        assert_eq!(snapshot.topology_generation, 1);
        assert_eq!(
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?,
            source_after
        );
        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn deferred_terminal_publish_leaves_source_generation_for_lifecycle_commit() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let result = async {
        let source_before =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        let epoch = Uuid::now_v7();
        repositories::upsert_runtime_graph_snapshot(
            &pool,
            fixture.library_id,
            "building",
            1,
            epoch,
            1,
            0,
            Some(100.0),
            None,
            false,
        )
        .await?
        .context("deferred graph build claim must publish")?;
        repositories::upsert_runtime_graph_snapshot(
            &pool,
            fixture.library_id,
            "ready",
            1,
            epoch,
            1,
            0,
            Some(100.0),
            None,
            false,
        )
        .await?
        .context("deferred terminal graph snapshot must publish")?;

        assert_eq!(
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?,
            source_before,
            "lifecycle-owned graph projection must not publish source truth early",
        );

        let lifecycle_generation =
            catalog_repository::touch_library_source_truth_version(&pool, fixture.library_id)
                .await?;
        assert!(lifecycle_generation > source_before);
        assert_eq!(
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?,
            lifecycle_generation,
        );
        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn v2_claim_rejects_a_rolling_v1_snapshot_upsert() -> Result<()> {
    let pool = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let result = async {
        let epoch = Uuid::now_v7();
        repositories::upsert_runtime_graph_snapshot(
            &pool,
            fixture.library_id,
            "building",
            1,
            epoch,
            1,
            0,
            Some(100.0),
            None,
            true,
        )
        .await?
        .context("v2 build claim must publish")?;

        let legacy_result = sqlx::query(
            "insert into runtime_graph_snapshot (
                library_id, graph_status, projection_version, node_count, edge_count,
                provenance_coverage_percent, last_built_at, last_error_message,
                topology_generation
             ) values ($1, 'building', 1, 9, 9, 100.0, now(), null, 0)
             on conflict (library_id) do update
             set graph_status = excluded.graph_status,
                 projection_version = excluded.projection_version,
                 node_count = excluded.node_count,
                 edge_count = excluded.edge_count,
                 provenance_coverage_percent = excluded.provenance_coverage_percent,
                 last_built_at = now(),
                 last_error_message = excluded.last_error_message,
                 updated_at = now()",
        )
        .bind(fixture.library_id)
        .execute(&pool)
        .await;
        let legacy_error = match legacy_result {
            Ok(_) => anyhow::bail!("rolling v1 writer unexpectedly crossed a v2 claim"),
            Err(error) => error,
        };
        assert_eq!(
            legacy_error.as_database_error().and_then(|error| error.code()).as_deref(),
            Some("40001")
        );

        let published = repositories::upsert_runtime_graph_snapshot(
            &pool,
            fixture.library_id,
            "ready",
            1,
            epoch,
            1,
            0,
            Some(100.0),
            None,
            true,
        )
        .await?
        .context("v2 owner must still publish after rejected v1 write")?;
        assert_eq!(published.node_count, 1);
        assert_eq!(published.topology_generation, 1);
        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}
