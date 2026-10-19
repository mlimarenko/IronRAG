//! PostgreSQL integration coverage for graph-summary executor fencing.

use crate::temp_postgres_database;

use std::time::Duration;

use anyhow::{Context, Result};
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        persistence::Persistence,
        repositories::{self, catalog_repository},
    },
    services::graph::{PendingGraphSummaryRefresh, PublishedGraphSummaryRefreshOutcome},
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use temp_postgres_database::TempPostgresDatabase;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn published_summary_refresh_completes_with_one_main_pool_connection() -> Result<()> {
    let mut settings = Settings::from_env().context("load graph summary executor settings")?;
    let database =
        TempPostgresDatabase::create(&settings.database_url, "graph_summary_executor").await?;
    settings.database_url = database.database_url.clone();
    settings.credential_master_key =
        Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());

    // The generation guard occupies this sole connection. Every query in the
    // guarded refresh must therefore use the transaction executor it already owns.
    let postgres = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(30))
        .connect(&settings.database_url)
        .await
        .context("connect one-slot graph summary postgres pool")?;
    sqlx::migrate!("./migrations")
        .run(&postgres)
        .await
        .context("apply graph summary executor migrations")?;
    let redis = redis::Client::open(settings.redis_url.clone())
        .context("create graph summary executor redis client")?;
    let state = AppState::from_dependencies(settings, Persistence::for_tests(postgres, redis))?;

    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("graph-summary-workspace-{suffix}"),
            "Graph Summary Workspace",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("graph-summary-library-{suffix}"),
            "Graph Summary Library",
            None,
            None,
        )
        .await?;
        let source_document_id: Uuid = sqlx::query_scalar(
            "insert into content_document (workspace_id, library_id, external_key, document_state)
             values ($1, $2, $3, 'active') returning id",
        ).bind(workspace.id).bind(library.id).bind(format!("graph-summary:{suffix}"))
        .fetch_one(&state.persistence.postgres).await?;
        let source_revision_id = crate::content_lifecycle_support::seed_graph_document_revision(
            &state.persistence.postgres, source_document_id, "summary.txt", "Summary source",
        ).await?;
        let nodes = repositories::bulk_upsert_runtime_graph_nodes(
            &state.persistence.postgres,
            library.id,
            1,
            &[
                repositories::RuntimeGraphNodeUpsertInput {
                    identity_key: format!("entity:source:{suffix}"),
                    label: "Neutral source".to_string(),
                    node_type: "entity".to_string(),
                    aliases_json: json!([]),
                    summary: Some("A reusable source record".to_string()),
                    metadata_json: json!({}),
                    support_count: 1,
                },
                repositories::RuntimeGraphNodeUpsertInput {
                    identity_key: format!("entity:target:{suffix}"),
                    label: "Neutral target".to_string(),
                    node_type: "entity".to_string(),
                    aliases_json: json!([]),
                    summary: Some("A reusable target record".to_string()),
                    metadata_json: json!({}),
                    support_count: 1,
                },
            ],
        )
        .await?;
        let source_node = nodes
            .iter()
            .find(|node| node.label == "Neutral source")
            .context("graph summary fixture did not create its source node")?;
        let target_node = nodes
            .iter()
            .find(|node| node.label == "Neutral target")
            .context("graph summary fixture did not create its target node")?;
        let edge = repositories::upsert_runtime_graph_edge(&state.persistence.postgres, repositories::RuntimeGraphEdgeInput {
    library_id: library.id,
    from_node_id: source_node.id,
    to_node_id: target_node.id,
    relation_type: "relates_to",
    identity_key: &format!("entity:source:{suffix}--relates_to--entity:target:{suffix}"),
    summary: Some("The source relates to the target"),
    weight: Some(1.0),
    support_count: 1,
    metadata_json: json!({}),
    projection_version: 1,
})
        .await?;
        for (kind, target_id) in [("node", source_node.id), ("node", target_node.id), ("edge", edge.id)] {
            repositories::create_runtime_graph_evidence(&state.persistence.postgres, repositories::CreateRuntimeGraphEvidenceInput {
                library_id: library.id, target_kind: kind, target_id,
                document_id: Some(source_document_id), revision_id: Some(source_revision_id),
                activated_by_attempt_id: None, chunk_id: None, source_file_name: Some("summary.txt"),
                page_ref: None, evidence_text: "The source relates to the target", confidence_score: Some(1.0),
                evidence_context_key: &format!("summary:{kind}:{target_id}"),
            }).await?;
        }
        let source_truth_version = catalog_repository::get_library_source_truth_version(
            &state.persistence.postgres,
            library.id,
        )
        .await?;
        let pending = PendingGraphSummaryRefresh::targeted(vec![source_node.id], Vec::new());

        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            state.application_services.graph.apply_published_summary_refresh(
                &state,
                library.id,
                source_truth_version,
                &pending,
            ),
        )
        .await
        .context(
            "graph summary refresh tried to acquire a second connection while its generation guard held the only slot",
        )??;

        assert_eq!(
            outcome,
            PublishedGraphSummaryRefreshOutcome::Applied { invalidated: 0, refreshed: 1 }
        );
        let summary = repositories::get_active_runtime_graph_summary_by_target(
            &state.persistence.postgres,
            library.id,
            "node",
            source_node.id,
        )
        .await?
        .context("graph summary refresh did not persist the target summary")?;
        assert_eq!(summary.workspace_id, workspace.id);
        assert_eq!(summary.source_truth_version, source_truth_version);
        Ok(())
    }
    .await;

    state.persistence.postgres.close().await;
    database.drop().await?;
    result
}
