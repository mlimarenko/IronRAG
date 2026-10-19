//! PostgreSQL integration coverage for runtime graph snapshot publication fencing.

use crate::temp_postgres_database;

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

async fn connect_postgres() -> Result<(PgPool, temp_postgres_database::TempPostgresDatabase)> {
    let settings = Settings::from_env().context("load graph snapshot fence settings")?;
    let temp_database = temp_postgres_database::TempPostgresDatabase::create(
        &settings.database_url,
        "graph_snapshot_fencing",
    )
    .await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&temp_database.database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok((pool, temp_database))
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_merge_lock_serializes_workers_across_database_connections() -> Result<()> {
    let (pool, temp_database) = connect_postgres().await?;
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
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn terminal_publish_rejects_stale_owner_and_never_rewinds_projection() -> Result<()> {
    let (pool, temp_database) = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let result = async {
        let source_before =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        let library_id = fixture.library_id;
        let first_epoch = Uuid::now_v7();
        let second_epoch = Uuid::now_v7();

        let first_claim = repositories::upsert_runtime_graph_snapshot(
            &pool,
            repositories::RuntimeGraphSnapshotInput {
                library_id: fixture.library_id,
                graph_status: "building",
                projection_version: 4,
                build_epoch: first_epoch,
                node_count: 1,
                edge_count: 0,
                provenance_coverage_percent: Some(100.0),
                last_error_message: None,
            },
            true,
        )
        .await?;
        assert!(first_claim.is_some());
        let second_claim = repositories::upsert_runtime_graph_snapshot(
            &pool,
            repositories::RuntimeGraphSnapshotInput {
                library_id: fixture.library_id,
                graph_status: "building",
                projection_version: 4,
                build_epoch: second_epoch,
                node_count: 2,
                edge_count: 1,
                provenance_coverage_percent: Some(100.0),
                last_error_message: None,
            },
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
                    repositories::RuntimeGraphSnapshotInput {
                        library_id,
                        graph_status: "ready",
                        projection_version: 4,
                        build_epoch: first_epoch,
                        node_count: 1,
                        edge_count: 0,
                        provenance_coverage_percent: Some(100.0),
                        last_error_message: None,
                    },
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
                    repositories::RuntimeGraphSnapshotInput {
                        library_id,
                        graph_status: "ready",
                        projection_version: 4,
                        build_epoch: second_epoch,
                        node_count: 2,
                        edge_count: 1,
                        provenance_coverage_percent: Some(100.0),
                        last_error_message: None,
                    },
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
            repositories::RuntimeGraphSnapshotInput {
                library_id: fixture.library_id,
                graph_status: "building",
                projection_version: 3,
                build_epoch: Uuid::now_v7(),
                node_count: 99,
                edge_count: 99,
                provenance_coverage_percent: Some(1.0),
                last_error_message: None,
            },
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
            repositories::RuntimeGraphSnapshotInput {
                library_id: fixture.library_id,
                graph_status: "ready",
                projection_version: 5,
                build_epoch: Uuid::now_v7(),
                node_count: 100,
                edge_count: 100,
                provenance_coverage_percent: Some(100.0),
                last_error_message: None,
            },
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
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn first_terminal_insert_bumps_source_generation_exactly_once() -> Result<()> {
    let (pool, temp_database) = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let result = async {
        let source_before =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        let epoch = Uuid::now_v7();
        let published = repositories::upsert_runtime_graph_snapshot(
            &pool,
            repositories::RuntimeGraphSnapshotInput {
                library_id: fixture.library_id,
                graph_status: "empty",
                projection_version: 1,
                build_epoch: epoch,
                node_count: 0,
                edge_count: 0,
                provenance_coverage_percent: Some(0.0),
                last_error_message: None,
            },
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
            repositories::RuntimeGraphSnapshotInput {
                library_id: fixture.library_id,
                graph_status: "empty",
                projection_version: 1,
                build_epoch: epoch,
                node_count: 0,
                edge_count: 0,
                provenance_coverage_percent: Some(0.0),
                last_error_message: None,
            },
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
    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn deferred_terminal_publish_leaves_source_generation_for_lifecycle_commit() -> Result<()> {
    let (pool, temp_database) = connect_postgres().await?;
    let fixture = Fixture::create(&pool).await?;

    let result = async {
        let source_before =
            catalog_repository::get_library_source_truth_version(&pool, fixture.library_id).await?;
        let epoch = Uuid::now_v7();
        repositories::upsert_runtime_graph_snapshot(
            &pool,
            repositories::RuntimeGraphSnapshotInput {
                library_id: fixture.library_id,
                graph_status: "building",
                projection_version: 1,
                build_epoch: epoch,
                node_count: 1,
                edge_count: 0,
                provenance_coverage_percent: Some(100.0),
                last_error_message: None,
            },
            false,
        )
        .await?
        .context("deferred graph build claim must publish")?;
        repositories::upsert_runtime_graph_snapshot(
            &pool,
            repositories::RuntimeGraphSnapshotInput {
                library_id: fixture.library_id,
                graph_status: "ready",
                projection_version: 1,
                build_epoch: epoch,
                node_count: 1,
                edge_count: 0,
                provenance_coverage_percent: Some(100.0),
                last_error_message: None,
            },
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
    pool.close().await;
    temp_database.drop().await?;
    result
}
