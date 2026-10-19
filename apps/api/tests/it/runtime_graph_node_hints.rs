//! PostgreSQL integration coverage for the vocabulary-aware extraction hint
//! aggregation ordering
//! (`infra::repositories::runtime_graph_repository::node_operations`).
//!
//! Run with:
//! `cargo test -p ironrag-backend --test it -- --ignored runtime_graph_node_hints::`

use crate::temp_postgres_database;

use anyhow::{Context, Result};
use sqlx::PgPool;
use uuid::Uuid;

use ironrag_backend::infra::repositories::{
    catalog_repository,
    runtime_graph_repository::{self},
};
use temp_postgres_database::TempPostgresDatabase;

const PROJECTION_VERSION: i64 = 7;

struct Fixture {
    database: TempPostgresDatabase,
    pool: PgPool,
    library_id: Uuid,
}

impl Fixture {
    async fn create() -> Result<Self> {
        let database_url = std::env::var("IRONRAG_DATABASE_URL")
            .context("IRONRAG_DATABASE_URL must be configured")?;
        let database =
            TempPostgresDatabase::create(&database_url, "runtime_graph_node_hints").await?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&database.database_url)
            .await
            .context("failed to connect runtime graph node hints postgres")?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply runtime graph node hints migrations")?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("runtime-graph-node-hints-workspace-{suffix}"),
            "Runtime Graph Node Hints Workspace",
            None,
        )
        .await
        .context("failed to create runtime graph node hints workspace")?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("runtime-graph-node-hints-library-{suffix}"),
            "Runtime Graph Node Hints Library",
            None,
            None,
        )
        .await
        .context("failed to create runtime graph node hints library")?;

        Ok(Self { database, pool, library_id: library.id })
    }

    /// Seeds `occurrences` distinct graph nodes that all carry `sub_type`, so
    /// the aggregation sees exactly that many observed occurrences.
    async fn seed_sub_type(
        &self,
        node_type: &str,
        sub_type: &str,
        occurrences: usize,
    ) -> Result<()> {
        for index in 0..occurrences {
            runtime_graph_repository::upsert_runtime_graph_node(
                &self.pool,
                runtime_graph_repository::RuntimeGraphNodeInput {
                    library_id: self.library_id,
                    identity_key: &format!("{node_type}:{sub_type}:{index}"),
                    label: &format!("{sub_type}-{index}"),
                    node_type,
                    document_id: None,
                    aliases_json: serde_json::json!([]),
                    summary: Some("runtime graph node hint fixture"),
                    metadata_json: serde_json::json!({ "sub_type": sub_type }),
                    support_count: 1,
                    projection_version: PROJECTION_VERSION,
                },
            )
            .await
            .context("failed to seed runtime graph node hint")?;
        }
        Ok(())
    }

    async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
        self.database.drop().await
    }
}

fn observed(
    rows: &[runtime_graph_repository::RuntimeGraphSubTypeHintRow],
) -> Vec<(&str, &str, i64)> {
    rows.iter()
        .map(|row| (row.node_type.as_str(), row.sub_type.as_str(), row.occurrences))
        .collect()
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn hint_aggregation_ranks_by_frequency_and_breaks_ties_by_sub_type() -> Result<()> {
    let fixture = Fixture::create().await?;
    fixture.seed_sub_type("artifact", "service", 3).await?;
    fixture.seed_sub_type("artifact", "api", 2).await?;
    fixture.seed_sub_type("artifact", "database", 2).await?;
    fixture.seed_sub_type("concept", "alpha", 1).await?;

    let rows = runtime_graph_repository::list_observed_sub_type_hints(
        &fixture.pool,
        fixture.library_id,
        PROJECTION_VERSION,
    )
    .await
    .context("failed to list observed sub type hints")?;

    // `database` and `api` tie on occurrences; the sub_type tiebreak puts `api`
    // first. This is the total order the caller trims its top-N from.
    assert_eq!(
        observed(&rows),
        vec![
            ("artifact", "service", 3),
            ("artifact", "api", 2),
            ("artifact", "database", 2),
            ("concept", "alpha", 1),
        ],
        "rows must be ordered node_type asc, occurrences desc, sub_type asc"
    );

    // The order is total, so a second read of the same graph snapshot is
    // identical: the top-N selection it feeds is reproducible.
    let again = runtime_graph_repository::list_observed_sub_type_hints(
        &fixture.pool,
        fixture.library_id,
        PROJECTION_VERSION,
    )
    .await
    .context("failed to list observed sub type hints a second time")?;
    assert_eq!(observed(&rows), observed(&again), "the aggregation order must be deterministic");

    fixture.cleanup().await
}
