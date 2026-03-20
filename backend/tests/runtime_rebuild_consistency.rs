use anyhow::Context;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::config::Settings,
    domains::runtime_graph::RuntimeNodeType,
    infra::repositories::{self, ChunkRow, DocumentRow, ProjectRow, WorkspaceRow},
    services::{
        graph_extract::{
            GraphEntityCandidate, GraphExtractionCandidateSet, GraphRelationCandidate,
        },
        graph_merge::{GraphMergeScope, merge_chunk_graph_candidates},
        graph_quality_guard::GraphQualityGuardService,
    },
};

struct RuntimeRebuildFixture {
    workspace: WorkspaceRow,
    project: ProjectRow,
}

impl RuntimeRebuildFixture {
    async fn create(pool: &PgPool) -> anyhow::Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::create_workspace(
            pool,
            &format!("runtime-rebuild-{suffix}"),
            "Runtime Rebuild Test",
        )
        .await
        .context("failed to create rebuild test workspace")?;
        let project = repositories::create_project(
            pool,
            workspace.id,
            &format!("runtime-rebuild-library-{suffix}"),
            "Runtime Rebuild Library",
            Some("runtime rebuild consistency test fixture"),
        )
        .await
        .context("failed to create rebuild test library")?;

        Ok(Self { workspace, project })
    }

    async fn create_document_with_chunk(
        &self,
        pool: &PgPool,
        external_key: &str,
        body: &str,
    ) -> anyhow::Result<(DocumentRow, ChunkRow)> {
        let document = repositories::create_document(
            pool,
            self.project.id,
            None,
            external_key,
            Some(external_key),
            Some("text/markdown"),
            Some("fixture-checksum"),
        )
        .await
        .with_context(|| format!("failed to create fixture document {external_key}"))?;
        let chunk = repositories::create_chunk(
            pool,
            document.id,
            self.project.id,
            0,
            body,
            Some(i32::try_from(body.split_whitespace().count()).unwrap_or(i32::MAX)),
            serde_json::json!({ "pageRefs": ["p1"] }),
        )
        .await
        .with_context(|| format!("failed to create fixture chunk for {external_key}"))?;
        Ok((document, chunk))
    }

    async fn cleanup(&self, pool: &PgPool) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = $1")
            .bind(self.workspace.id)
            .execute(pool)
            .await
            .context("failed to delete rebuild test workspace")?;
        Ok(())
    }
}

async fn connect_postgres(settings: &Settings) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&settings.database_url)
        .await
        .context("failed to connect rebuild test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for rebuild test")?;
    Ok(pool)
}

fn shared_graph_candidates() -> GraphExtractionCandidateSet {
    GraphExtractionCandidateSet {
        entities: vec![
            GraphEntityCandidate {
                label: "OpenAI".to_string(),
                node_type: RuntimeNodeType::Entity,
                aliases: vec!["Open AI".to_string()],
                summary: Some("Shared provider".to_string()),
            },
            GraphEntityCandidate {
                label: "Knowledge Graph".to_string(),
                node_type: RuntimeNodeType::Topic,
                aliases: vec![],
                summary: Some("Shared topic".to_string()),
            },
        ],
        relations: vec![GraphRelationCandidate {
            source_label: "OpenAI".to_string(),
            target_label: "Knowledge Graph".to_string(),
            relation_type: "builds on".to_string(),
            summary: Some("Shared runtime relation".to_string()),
        }],
    }
}

fn unique_graph_candidates() -> GraphExtractionCandidateSet {
    GraphExtractionCandidateSet {
        entities: vec![GraphEntityCandidate {
            label: "RustRAG".to_string(),
            node_type: RuntimeNodeType::Entity,
            aliases: vec!["Rust RAG".to_string()],
            summary: Some("Document-specific entity".to_string()),
        }],
        relations: Vec::new(),
    }
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn delete_and_reprocess_cleanup_keep_runtime_graph_consistent() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings for rebuild test")?;
    let pool = connect_postgres(&settings).await?;
    let fixture = RuntimeRebuildFixture::create(&pool).await?;
    let graph_quality_guard = GraphQualityGuardService::default();

    let result = async {
        let (document_a, chunk_a) = fixture
            .create_document_with_chunk(
                &pool,
                "alpha.md",
                "Alpha document mentions OpenAI, RustRAG, and a knowledge graph runtime.",
            )
            .await?;
        let (document_b, chunk_b) = fixture
            .create_document_with_chunk(
                &pool,
                "beta.md",
                "Beta document mentions OpenAI and the knowledge graph runtime.",
            )
            .await?;

        let scope_v1 = GraphMergeScope::new(fixture.project.id, 1);
        merge_chunk_graph_candidates(&pool, &graph_quality_guard, &scope_v1, &document_a, &chunk_a, &shared_graph_candidates(), None)
            .await
            .context("failed to merge shared candidates for document A")?;
        merge_chunk_graph_candidates(&pool, &graph_quality_guard, &scope_v1, &document_a, &chunk_a, &unique_graph_candidates(), None)
            .await
            .context("failed to merge unique candidates for document A")?;
        merge_chunk_graph_candidates(&pool, &graph_quality_guard, &scope_v1, &document_b, &chunk_b, &shared_graph_candidates(), None)
            .await
            .context("failed to merge shared candidates for document B")?;

        let nodes_v1_before =
            repositories::list_runtime_graph_nodes_by_projection(&pool, fixture.project.id, 1)
                .await
                .context("failed to load v1 graph nodes before cleanup")?;
        let edges_v1_before =
            repositories::list_runtime_graph_edges_by_projection(&pool, fixture.project.id, 1)
                .await
                .context("failed to load v1 graph edges before cleanup")?;
        let openai_node = nodes_v1_before
            .iter()
            .find(|row| row.canonical_key == "entity:openai")
            .context("missing shared OpenAI node in v1")?;
        nodes_v1_before
            .iter()
            .find(|row| row.canonical_key == "entity:rustrag")
            .context("missing unique RustRAG node in v1")?;
        let shared_edge = edges_v1_before
            .iter()
            .find(|row| row.canonical_key.contains("builds_on"))
            .context("missing shared graph relation in v1")?;

        let query_execution = repositories::create_runtime_query_execution(
            &pool,
            fixture.project.id,
            "hybrid",
            "What builds on the knowledge graph runtime?",
            "completed",
            Some("OpenAI builds on the knowledge graph runtime."),
            "grounded",
            "openai",
            "gpt-5.4",
            serde_json::json!({}),
        )
        .await
        .context("failed to persist runtime query execution")?;
        repositories::create_runtime_query_reference(
            &pool,
            query_execution.id,
            "chunk",
            chunk_a.id,
            Some("Alpha document mentions OpenAI"),
            1,
            Some(0.99),
            serde_json::json!({}),
        )
        .await
        .context("failed to persist chunk reference")?;
        repositories::create_runtime_query_reference(
            &pool,
            query_execution.id,
            "node",
            openai_node.id,
            Some("OpenAI"),
            2,
            Some(0.98),
            serde_json::json!({}),
        )
        .await
        .context("failed to persist node reference")?;
        repositories::create_runtime_query_reference(
            &pool,
            query_execution.id,
            "edge",
            shared_edge.id,
            Some("OpenAI builds on Knowledge Graph"),
            3,
            Some(0.97),
            serde_json::json!({}),
        )
        .await
        .context("failed to persist edge reference")?;

        repositories::delete_runtime_query_references_by_document(
            &pool,
            fixture.project.id,
            document_a.id,
        )
        .await
        .context("failed to clean persisted query references for document A")?;
        repositories::deactivate_runtime_graph_evidence_by_document(
            &pool,
            fixture.project.id,
            document_a.id,
        )
        .await
        .context("failed to deactivate document A evidence")?;
        repositories::recalculate_runtime_graph_support_counts(&pool, fixture.project.id, 1)
            .await
            .context("failed to recalculate support counts for v1")?;

        let remaining_references = repositories::list_runtime_query_references_by_execution(
            &pool,
            query_execution.id,
        )
        .await
        .context("failed to load remaining query references")?;
        assert!(remaining_references.is_empty());

        let contributions_after_cleanup = repositories::count_runtime_graph_contributions_by_document(
            &pool,
            fixture.project.id,
            document_a.id,
        )
        .await
        .context("failed to count contributions after cleanup")?;
        assert_eq!(contributions_after_cleanup.evidence_count, 0);

        let nodes_v1_after =
            repositories::list_runtime_graph_nodes_by_projection(&pool, fixture.project.id, 1)
                .await
                .context("failed to load v1 graph nodes after cleanup")?;
        let edges_v1_after =
            repositories::list_runtime_graph_edges_by_projection(&pool, fixture.project.id, 1)
                .await
                .context("failed to load v1 graph edges after cleanup")?;
        let openai_after_cleanup = nodes_v1_after
            .iter()
            .find(|row| row.canonical_key == "entity:openai")
            .context("missing OpenAI node after cleanup")?;
        let rust_rag_after_cleanup = nodes_v1_after
            .iter()
            .find(|row| row.canonical_key == "entity:rustrag")
            .context("missing RustRAG node after cleanup")?;
        let shared_edge_after_cleanup = edges_v1_after
            .iter()
            .find(|row| row.canonical_key == shared_edge.canonical_key)
            .context("missing shared relation after cleanup")?;
        assert_eq!(openai_after_cleanup.support_count, 2);
        assert_eq!(rust_rag_after_cleanup.support_count, 0);
        assert_eq!(shared_edge_after_cleanup.support_count, 1);

        repositories::delete_document_by_id(&pool, document_a.id)
            .await
            .context("failed to delete document A")?;

        let scope_v2 = GraphMergeScope::new(fixture.project.id, 2);
        merge_chunk_graph_candidates(&pool, &graph_quality_guard, &scope_v2, &document_b, &chunk_b, &shared_graph_candidates(), None)
            .await
            .context("failed to rebuild surviving graph into v2")?;

        let nodes_v2_after_delete =
            repositories::list_runtime_graph_nodes_by_projection(&pool, fixture.project.id, 2)
                .await
                .context("failed to load v2 graph nodes after delete rebuild")?;
        assert!(nodes_v2_after_delete.iter().all(|row| row.canonical_key != "entity:rustrag"));
        assert_eq!(
            nodes_v2_after_delete
                .iter()
                .filter(|row| row.canonical_key == "entity:openai")
                .count(),
            1
        );

        let (document_a_reprocessed, chunk_a_reprocessed) = fixture
            .create_document_with_chunk(
                &pool,
                "alpha.md",
                "Alpha document was reprocessed and still mentions OpenAI, RustRAG, and a knowledge graph runtime.",
            )
            .await?;
        merge_chunk_graph_candidates(
            &pool,
            &graph_quality_guard,
            &scope_v2,
            &document_a_reprocessed,
            &chunk_a_reprocessed,
            &shared_graph_candidates(),
            None,
        )
        .await
        .context("failed to merge shared reprocessed document knowledge")?;
        merge_chunk_graph_candidates(
            &pool,
            &graph_quality_guard,
            &scope_v2,
            &document_a_reprocessed,
            &chunk_a_reprocessed,
            &unique_graph_candidates(),
            None,
        )
        .await
        .context("failed to merge unique reprocessed document knowledge")?;

        let nodes_v2_after_reprocess =
            repositories::list_runtime_graph_nodes_by_projection(&pool, fixture.project.id, 2)
                .await
                .context("failed to load v2 graph nodes after reprocess")?;
        let edges_v2_after_reprocess =
            repositories::list_runtime_graph_edges_by_projection(&pool, fixture.project.id, 2)
                .await
                .context("failed to load v2 graph edges after reprocess")?;
        assert_eq!(
            nodes_v2_after_reprocess
                .iter()
                .filter(|row| row.canonical_key == "entity:openai")
                .count(),
            1
        );
        assert_eq!(
            nodes_v2_after_reprocess
                .iter()
                .filter(|row| row.canonical_key == "entity:rustrag")
                .count(),
            1
        );
        assert_eq!(
            edges_v2_after_reprocess
                .iter()
                .filter(|row| row.canonical_key.contains("builds_on"))
                .count(),
            1
        );

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    result
}
