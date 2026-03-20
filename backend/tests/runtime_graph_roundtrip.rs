use anyhow::Context;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::config::Settings,
    domains::runtime_graph::RuntimeNodeType,
    infra::{
        graph_store::{GraphProjectionEdgeWrite, GraphProjectionNodeWrite, GraphStore},
        neo4j_store::Neo4jStore,
        repositories::{self, ChunkRow, DocumentRow, ProjectRow, WorkspaceRow},
    },
    services::{
        graph_extract::{
            GraphEntityCandidate, GraphExtractionCandidateSet, GraphRelationCandidate,
        },
        graph_merge::{
            GraphMergeScope, merge_chunk_graph_candidates, reconcile_merge_support_counts,
        },
        graph_quality_guard::GraphQualityGuardService,
    },
};

struct RuntimeGraphFixture {
    workspace: WorkspaceRow,
    project: ProjectRow,
    document: DocumentRow,
    chunk: ChunkRow,
}

impl RuntimeGraphFixture {
    async fn create(pool: &PgPool) -> anyhow::Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::create_workspace(
            pool,
            &format!("runtime-graph-test-{suffix}"),
            "Runtime Graph Test",
        )
        .await
        .context("failed to create runtime graph test workspace")?;
        let project = repositories::create_project(
            pool,
            workspace.id,
            &format!("runtime-graph-library-{suffix}"),
            "Runtime Graph Library",
            Some("runtime graph integration test fixture"),
        )
        .await
        .context("failed to create runtime graph test library")?;
        let document = repositories::create_document(
            pool,
            project.id,
            None,
            "spec.md",
            Some("spec.md"),
            Some("text/markdown"),
            Some("fixture-checksum"),
        )
        .await
        .context("failed to create runtime graph test document")?;
        let chunk = repositories::create_chunk(
            pool,
            document.id,
            project.id,
            0,
            "OpenAI builds on graph retrieval and the spec document mentions OpenAI explicitly.",
            Some(16),
            serde_json::json!({ "pageRefs": ["p1"] }),
        )
        .await
        .context("failed to create runtime graph test chunk")?;

        Ok(Self { workspace, project, document, chunk })
    }

    async fn cleanup(&self, pool: &PgPool) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = $1")
            .bind(self.workspace.id)
            .execute(pool)
            .await
            .context("failed to delete runtime graph test workspace")?;
        Ok(())
    }
}

async fn connect_postgres(settings: &Settings) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&settings.database_url)
        .await
        .context("failed to connect runtime graph test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for runtime graph test")?;
    Ok(pool)
}

fn projectable_nodes(rows: &[repositories::RuntimeGraphNodeRow]) -> Vec<GraphProjectionNodeWrite> {
    rows.iter()
        .map(|row| GraphProjectionNodeWrite {
            node_id: row.id,
            canonical_key: row.canonical_key.clone(),
            label: row.label.clone(),
            node_type: row.node_type.clone(),
            support_count: row.support_count,
            summary: row.summary.clone(),
            aliases: serde_json::from_value(row.aliases_json.clone()).unwrap_or_default(),
            metadata_json: row.metadata_json.clone(),
        })
        .collect()
}

fn projectable_edges(rows: &[repositories::RuntimeGraphEdgeRow]) -> Vec<GraphProjectionEdgeWrite> {
    rows.iter()
        .map(|row| GraphProjectionEdgeWrite {
            edge_id: row.id,
            from_node_id: row.from_node_id,
            to_node_id: row.to_node_id,
            relation_type: row.relation_type.clone(),
            canonical_key: row.canonical_key.clone(),
            support_count: row.support_count,
            summary: row.summary.clone(),
            weight: row.weight,
            metadata_json: row.metadata_json.clone(),
        })
        .collect()
}

#[tokio::test]
#[ignore = "requires local postgres and neo4j services"]
async fn merge_and_projection_round_trip_preserves_graph_and_evidence() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for runtime graph test")?;
    let pool = connect_postgres(&settings).await?;
    let store = Neo4jStore::connect(&settings).context("failed to connect neo4j store")?;
    let fixture = RuntimeGraphFixture::create(&pool).await?;
    let scope = GraphMergeScope::new(fixture.project.id, 11);
    let graph_quality_guard = GraphQualityGuardService::default();

    let result = async {
        let candidates = GraphExtractionCandidateSet {
            entities: vec![GraphEntityCandidate {
                label: "OpenAI".to_string(),
                node_type: RuntimeNodeType::Entity,
                aliases: vec!["Open AI".to_string()],
                summary: Some("LLM provider".to_string()),
            }],
            relations: vec![GraphRelationCandidate {
                source_label: "OpenAI".to_string(),
                target_label: "Knowledge Graph".to_string(),
                relation_type: "builds on".to_string(),
                summary: Some("provider supports graph-aware retrieval".to_string()),
            }],
        };

        let merge = merge_chunk_graph_candidates(
            &pool,
            &graph_quality_guard,
            &scope,
            &fixture.document,
            &fixture.chunk,
            &candidates,
            None,
        )
        .await
        .context("failed to merge chunk graph candidates")?;

        assert_eq!(merge.evidence_count, 5);

        let nodes =
            repositories::list_runtime_graph_nodes_by_projection(&pool, fixture.project.id, 11)
                .await
                .context("failed to load merged graph nodes")?;
        let edges =
            repositories::list_runtime_graph_edges_by_projection(&pool, fixture.project.id, 11)
                .await
                .context("failed to load merged graph edges")?;
        assert_eq!(nodes.len(), 3);
        assert_eq!(edges.len(), 2);

        let openai_node = nodes
            .iter()
            .find(|row| row.canonical_key == "entity:openai")
            .context("missing merged OpenAI node")?;
        let openai_evidence = repositories::list_runtime_graph_evidence_by_target(
            &pool,
            fixture.project.id,
            "node",
            openai_node.id,
        )
        .await
        .context("failed to load OpenAI evidence")?;
        assert_eq!(openai_evidence.len(), 2);
        assert!(openai_evidence.iter().all(|row| row.document_id == Some(fixture.document.id)));
        assert!(openai_evidence.iter().all(|row| row.chunk_id == Some(fixture.chunk.id)));

        let relation_edge = edges
            .iter()
            .find(|row| row.canonical_key.contains("builds_on"))
            .context("missing merged relation edge")?;
        let relation_evidence = repositories::list_runtime_graph_evidence_by_target(
            &pool,
            fixture.project.id,
            "edge",
            relation_edge.id,
        )
        .await
        .context("failed to load relation evidence")?;
        assert_eq!(relation_evidence.len(), 1);
        assert_eq!(relation_evidence[0].document_id, Some(fixture.document.id));

        store
            .replace_library_projection(
                fixture.project.id,
                11,
                &projectable_nodes(&nodes),
                &projectable_edges(&edges),
            )
            .await
            .context("failed to replace Neo4j projection")?;
        let projection = store
            .load_library_projection(fixture.project.id, 11)
            .await
            .context("failed to load Neo4j projection")?;
        assert_eq!(projection.nodes.len(), nodes.len());
        assert_eq!(projection.edges.len(), edges.len());
        assert!(
            projection.edges.iter().any(|row| row.canonical_key == relation_edge.canonical_key)
        );

        store
            .replace_library_projection(fixture.project.id, 11, &[], &[])
            .await
            .context("failed to clean Neo4j projection")?;

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn repeated_merge_of_same_chunk_keeps_evidence_and_support_counts_stable()
-> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for runtime graph idempotency test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = RuntimeGraphFixture::create(&pool).await?;
    let scope = GraphMergeScope::new(fixture.project.id, 19);
    let graph_quality_guard = GraphQualityGuardService::default();

    let result = async {
        let candidates = GraphExtractionCandidateSet {
            entities: vec![GraphEntityCandidate {
                label: "OpenAI".to_string(),
                node_type: RuntimeNodeType::Entity,
                aliases: vec!["Open AI".to_string()],
                summary: Some("LLM provider".to_string()),
            }],
            relations: vec![GraphRelationCandidate {
                source_label: "OpenAI".to_string(),
                target_label: "Knowledge Graph".to_string(),
                relation_type: "builds on".to_string(),
                summary: Some("provider supports graph-aware retrieval".to_string()),
            }],
        };

        for _ in 0..2 {
            let merge = merge_chunk_graph_candidates(
                &pool,
                &graph_quality_guard,
                &scope,
                &fixture.document,
                &fixture.chunk,
                &candidates,
                None,
            )
            .await
            .context("failed to merge repeated chunk graph candidates")?;
            reconcile_merge_support_counts(
                &pool,
                &scope,
                &merge.changed_node_ids(),
                &merge.changed_edge_ids(),
            )
            .await
            .context("failed to reconcile support counts after repeated merge")?;
        }

        let nodes =
            repositories::list_runtime_graph_nodes_by_projection(&pool, fixture.project.id, 19)
                .await
                .context("failed to load repeated-merge graph nodes")?;
        let edges =
            repositories::list_runtime_graph_edges_by_projection(&pool, fixture.project.id, 19)
                .await
                .context("failed to load repeated-merge graph edges")?;

        let openai_node = nodes
            .iter()
            .find(|row| row.canonical_key == "entity:openai")
            .context("missing repeated-merge OpenAI node")?;
        let relation_edge = edges
            .iter()
            .find(|row| row.canonical_key.contains("builds_on"))
            .context("missing repeated-merge relation edge")?;

        let openai_evidence = repositories::list_runtime_graph_evidence_by_target(
            &pool,
            fixture.project.id,
            "node",
            openai_node.id,
        )
        .await
        .context("failed to load repeated-merge OpenAI evidence")?;
        let relation_evidence = repositories::list_runtime_graph_evidence_by_target(
            &pool,
            fixture.project.id,
            "edge",
            relation_edge.id,
        )
        .await
        .context("failed to load repeated-merge relation evidence")?;

        assert_eq!(openai_evidence.len(), 2);
        assert_eq!(relation_evidence.len(), 1);
        assert_eq!(openai_node.support_count, 2);
        assert_eq!(relation_edge.support_count, 1);

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}
