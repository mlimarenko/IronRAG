use anyhow::Context;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::config::Settings,
    infra::repositories::{
        self,
        catalog_repository::{self, CatalogLibraryRow, CatalogWorkspaceRow},
    },
};

struct GraphProjectionRepositoryFixture {
    workspace: CatalogWorkspaceRow,
    library: CatalogLibraryRow,
}

impl GraphProjectionRepositoryFixture {
    async fn create(pool: &PgPool) -> anyhow::Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            pool,
            &format!("graph-projection-repo-{suffix}"),
            "Graph Projection Repository",
            None,
        )
        .await
        .context("failed to create graph projection repository test workspace")?;
        let library = catalog_repository::create_library(
            pool,
            workspace.id,
            &format!("graph-projection-library-{suffix}"),
            "Graph Projection Repository Library",
            Some("graph projection repository regression fixture"),
            None,
        )
        .await
        .context("failed to create graph projection repository test library")?;

        Ok(Self { workspace, library })
    }

    async fn cleanup(&self, pool: &PgPool) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = $1")
            .bind(self.workspace.id)
            .execute(pool)
            .await
            .context("failed to delete graph projection repository test workspace")?;
        Ok(())
    }
}

async fn connect_postgres(settings: &Settings) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&settings.database_url)
        .await
        .context("failed to connect graph projection repository test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for graph projection repository test")?;
    Ok(pool)
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn admitted_projection_queries_include_only_documents_and_connected_nodes()
-> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for graph projection repository test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = GraphProjectionRepositoryFixture::create(&pool).await?;

    let result = async {
        let projection_version = 1_i64;
        let document_node = repositories::upsert_runtime_graph_node(
            &pool,
            fixture.library.id,
            "document:alpha",
            "alpha.txt",
            "document",
            serde_json::json!([]),
            Some("Document node"),
            serde_json::json!({}),
            1,
            projection_version,
        )
        .await
        .context("failed to create document node")?;
        let connected_entity = repositories::upsert_runtime_graph_node(
            &pool,
            fixture.library.id,
            "entity:connected",
            "Connected Entity",
            "entity",
            serde_json::json!([]),
            Some("Connected entity"),
            serde_json::json!({}),
            1,
            projection_version,
        )
        .await
        .context("failed to create connected entity node")?;
        let isolated_entity = repositories::upsert_runtime_graph_node(
            &pool,
            fixture.library.id,
            "entity:isolated",
            "Isolated Entity",
            "entity",
            serde_json::json!([]),
            Some("Isolated entity"),
            serde_json::json!({}),
            1,
            projection_version,
        )
        .await
        .context("failed to create isolated entity node")?;

        repositories::upsert_runtime_graph_edge(
            &pool,
            fixture.library.id,
            document_node.id,
            connected_entity.id,
            "mentions",
            "edge:document-connected",
            Some("Valid admitted edge"),
            Some(1.0),
            1,
            serde_json::json!({}),
            projection_version,
        )
        .await
        .context("failed to create admitted edge")?;
        repositories::upsert_runtime_graph_edge(
            &pool,
            fixture.library.id,
            isolated_entity.id,
            isolated_entity.id,
            "self_loop",
            "edge:isolated-self-loop",
            Some("Degenerate loop"),
            Some(0.2),
            1,
            serde_json::json!({}),
            projection_version,
        )
        .await
        .context("failed to create degenerate loop edge")?;
        repositories::upsert_runtime_graph_edge(
            &pool,
            fixture.library.id,
            isolated_entity.id,
            document_node.id,
            "   ",
            "edge:isolated-blank",
            Some("Blank relation edge"),
            Some(0.1),
            1,
            serde_json::json!({}),
            projection_version,
        )
        .await
        .context("failed to create blank relation edge")?;

        let future_entity = repositories::upsert_runtime_graph_node(
            &pool,
            fixture.library.id,
            "entity:future",
            "Future Entity",
            "entity",
            serde_json::json!([]),
            Some("Future projection entity"),
            serde_json::json!({}),
            1,
            2,
        )
        .await
        .context("failed to create future projection entity")?;
        repositories::upsert_runtime_graph_edge(
            &pool,
            fixture.library.id,
            document_node.id,
            future_entity.id,
            "future",
            "edge:document-future",
            Some("Future projection edge"),
            Some(1.0),
            1,
            serde_json::json!({}),
            2,
        )
        .await
        .context("failed to create future projection edge")?;

        let admitted_nodes = repositories::list_admitted_runtime_graph_nodes_by_library(
            &pool,
            fixture.library.id,
            projection_version,
        )
        .await
        .context("failed to load admitted nodes by projection")?;
        let admitted_by_id = repositories::list_admitted_runtime_graph_nodes_by_ids(
            &pool,
            fixture.library.id,
            projection_version,
            &[document_node.id, connected_entity.id, isolated_entity.id, future_entity.id],
        )
        .await
        .context("failed to load admitted nodes by ids")?;
        let counts = repositories::count_admitted_runtime_graph_projection(
            &pool,
            fixture.library.id,
            projection_version,
        )
        .await
        .context("failed to count admitted graph projection rows")?;

        let admitted_keys =
            admitted_nodes.iter().map(|row| row.canonical_key.as_str()).collect::<Vec<_>>();
        assert_eq!(admitted_nodes.len(), 2);
        assert!(admitted_keys.contains(&"document:alpha"));
        assert!(admitted_keys.contains(&"entity:connected"));
        assert!(!admitted_keys.contains(&"entity:isolated"));
        assert!(!admitted_keys.contains(&"entity:future"));

        let admitted_id_keys =
            admitted_by_id.iter().map(|row| row.canonical_key.as_str()).collect::<Vec<_>>();
        assert_eq!(admitted_by_id.len(), 2);
        assert!(admitted_id_keys.contains(&"document:alpha"));
        assert!(admitted_id_keys.contains(&"entity:connected"));
        assert_eq!(counts.node_count, 2);
        assert_eq!(counts.edge_count, 1);

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}
