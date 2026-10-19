use crate::temp_postgres_database;

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
        let outcome = catalog_repository::delete_workspace(pool, self.workspace.id)
            .await
            .context("failed to delete graph projection repository test workspace")?;
        anyhow::ensure!(
            matches!(outcome, catalog_repository::CatalogWorkspaceDeleteOutcome::Deleted),
            "graph projection repository workspace cleanup did not delete its owner: {outcome:?}"
        );
        Ok(())
    }
}

async fn connect_postgres(
    settings: &Settings,
) -> anyhow::Result<(PgPool, temp_postgres_database::TempPostgresDatabase)> {
    let temp_database = temp_postgres_database::TempPostgresDatabase::create(
        &settings.database_url,
        "graph_projection_repository",
    )
    .await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect graph projection repository test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("failed to apply migrations for graph projection repository test")?;
    Ok((pool, temp_database))
}

const EVIDENCE_PRUNE_PROJECTION_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy)]
enum EvidencePrunePath {
    StandaloneSupportSynchronizer,
    TransactionalSupportSynchronizer,
    TransactionalTargetedSupportPrune,
    StandaloneExplicitPrune,
    TransactionalExplicitPrune,
}

impl EvidencePrunePath {
    const fn label(self) -> &'static str {
        match self {
            Self::StandaloneSupportSynchronizer => "standalone-support-sync",
            Self::TransactionalSupportSynchronizer => "transactional-support-sync",
            Self::TransactionalTargetedSupportPrune => "transactional-targeted-support-prune",
            Self::StandaloneExplicitPrune => "standalone-explicit-prune",
            Self::TransactionalExplicitPrune => "transactional-explicit-prune",
        }
    }
}

struct EvidencePruneCase {
    victim_node_id: Uuid,
    survivor_node_id: Uuid,
    incident_edge_id: Uuid,
    search_marker: String,
}

async fn seed_evidence_prune_case(
    pool: &PgPool,
    fixture: &GraphProjectionRepositoryFixture,
    path: EvidencePrunePath,
) -> anyhow::Result<EvidencePruneCase> {
    let suffix = Uuid::now_v7().simple().to_string();
    let source_document_id = sqlx::query_scalar::<_, Uuid>(
        "insert into content_document (
            workspace_id,
            library_id,
            external_key,
            document_state,
            created_at
        )
        values ($1, $2, $3, 'active', now())
        returning id",
    )
    .bind(fixture.workspace.id)
    .bind(fixture.library.id)
    .bind(format!("evidence-prune-{}-{suffix}", path.label()))
    .fetch_one(pool)
    .await
    .context("failed to insert evidence-prune source document")?;
    let victim_node = repositories::upsert_runtime_graph_node(
        pool,
        repositories::RuntimeGraphNodeInput {
            library_id: fixture.library.id,
            identity_key: &format!("entity:evidence-prune-victim:{suffix}"),
            label: "Pruned Entity",
            node_type: "entity",
            document_id: None,
            aliases_json: serde_json::json!([]),
            summary: Some("Entity whose stale support is pruned"),
            metadata_json: serde_json::json!({}),
            support_count: 1,
            projection_version: EVIDENCE_PRUNE_PROJECTION_VERSION,
        },
    )
    .await
    .context("failed to create evidence-prune victim node")?;
    let survivor_node = repositories::upsert_runtime_graph_node(
        pool,
        repositories::RuntimeGraphNodeInput {
            library_id: fixture.library.id,
            identity_key: &format!("entity:evidence-prune-survivor:{suffix}"),
            label: "Retained Entity",
            node_type: "entity",
            document_id: None,
            aliases_json: serde_json::json!([]),
            summary: Some("Entity with surviving active support"),
            metadata_json: serde_json::json!({}),
            support_count: 1,
            projection_version: EVIDENCE_PRUNE_PROJECTION_VERSION,
        },
    )
    .await
    .context("failed to create evidence-prune survivor node")?;
    let incident_edge = repositories::upsert_runtime_graph_edge(
        pool,
        repositories::RuntimeGraphEdgeInput {
            library_id: fixture.library.id,
            from_node_id: victim_node.id,
            to_node_id: survivor_node.id,
            relation_type: "linked_to",
            identity_key: &format!("edge:evidence-prune-incident:{suffix}"),
            summary: Some("Cross-projection edge removed only by node cascade"),
            weight: Some(1.0),
            support_count: 1,
            metadata_json: serde_json::json!({}),
            projection_version: EVIDENCE_PRUNE_PROJECTION_VERSION + 1,
        },
    )
    .await
    .context("failed to create evidence-prune incident edge")?;

    let search_marker = format!("pruneproof{suffix}");
    let victim_evidence_text = format!("{search_marker} stale node evidence");
    repositories::create_runtime_graph_evidence(
        pool,
        repositories::CreateRuntimeGraphEvidenceInput {
            library_id: fixture.library.id,
            target_kind: "node",
            target_id: victim_node.id,
            document_id: None,
            revision_id: None,
            activated_by_attempt_id: None,
            chunk_id: None,
            source_file_name: Some("evidence-prune.txt"),
            page_ref: None,
            evidence_text: &victim_evidence_text,
            confidence_score: Some(0.9),
            evidence_context_key: "victim-node",
        },
    )
    .await
    .context("failed to create stale victim-node evidence")?;
    let incident_edge_evidence_text = format!("{search_marker} incident edge evidence");
    repositories::create_runtime_graph_evidence(
        pool,
        repositories::CreateRuntimeGraphEvidenceInput {
            library_id: fixture.library.id,
            target_kind: "edge",
            target_id: incident_edge.id,
            document_id: Some(source_document_id),
            revision_id: None,
            activated_by_attempt_id: None,
            chunk_id: None,
            source_file_name: Some("evidence-prune.txt"),
            page_ref: None,
            evidence_text: &incident_edge_evidence_text,
            confidence_score: Some(0.9),
            evidence_context_key: "incident-edge",
        },
    )
    .await
    .context("failed to create incident-edge evidence")?;
    repositories::create_runtime_graph_evidence(
        pool,
        repositories::CreateRuntimeGraphEvidenceInput {
            library_id: fixture.library.id,
            target_kind: "node",
            target_id: survivor_node.id,
            document_id: Some(source_document_id),
            revision_id: None,
            activated_by_attempt_id: None,
            chunk_id: None,
            source_file_name: Some("evidence-prune.txt"),
            page_ref: None,
            evidence_text: "surviving active node evidence",
            confidence_score: Some(0.9),
            evidence_context_key: "survivor-node",
        },
    )
    .await
    .context("failed to create survivor-node evidence")?;

    Ok(EvidencePruneCase {
        victim_node_id: victim_node.id,
        survivor_node_id: survivor_node.id,
        incident_edge_id: incident_edge.id,
        search_marker,
    })
}

async fn search_evidence_prune_case(
    pool: &PgPool,
    library_id: Uuid,
    case: &EvidencePruneCase,
) -> anyhow::Result<usize> {
    let queries = [repositories::RuntimeGraphEvidenceSearchQuery::LiteralOrFormal(
        case.search_marker.clone(),
    )];
    let evidence =
        repositories::search_runtime_graph_evidence_by_text(pool, library_id, &queries, &[], 10)
            .await
            .context("failed to search evidence-prune marker")?;
    Ok(evidence.len())
}

async fn assert_evidence_prune_case_ready(
    pool: &PgPool,
    library_id: Uuid,
    case: &EvidencePruneCase,
) -> anyhow::Result<()> {
    let target_evidence_count = sqlx::query_scalar::<_, i64>(
        "select count(*)
         from runtime_graph_evidence
         where library_id = $1
           and (
             (target_kind = 'node' and target_id = $2)
             or (target_kind = 'edge' and target_id = $3)
           )",
    )
    .bind(library_id)
    .bind(case.victim_node_id)
    .bind(case.incident_edge_id)
    .fetch_one(pool)
    .await
    .context("failed to count seeded evidence-prune targets")?;
    assert_eq!(target_evidence_count, 2);
    assert_eq!(search_evidence_prune_case(pool, library_id, case).await?, 2);
    Ok(())
}

async fn execute_evidence_prune_path(
    pool: &PgPool,
    library_id: Uuid,
    case: &EvidencePruneCase,
    path: EvidencePrunePath,
) -> anyhow::Result<repositories::RuntimeGraphProjectionPruneOutcome> {
    match path {
        EvidencePrunePath::StandaloneSupportSynchronizer => {
            repositories::synchronize_runtime_graph_support_and_prune(
                pool,
                library_id,
                EVIDENCE_PRUNE_PROJECTION_VERSION,
            )
            .await
            .context("standalone support synchronizer failed")
        }
        EvidencePrunePath::TransactionalSupportSynchronizer => {
            let mut transaction =
                pool.begin().await.context("failed to begin support-synchronizer transaction")?;
            let outcome =
                repositories::synchronize_runtime_graph_support_and_prune_with_connection(
                    &mut transaction,
                    library_id,
                    EVIDENCE_PRUNE_PROJECTION_VERSION,
                )
                .await
                .context("transactional support synchronizer failed")?;
            transaction
                .commit()
                .await
                .context("failed to commit support-synchronizer transaction")?;
            Ok(outcome)
        }
        EvidencePrunePath::TransactionalTargetedSupportPrune => {
            let mut transaction =
                pool.begin().await.context("failed to begin targeted-support-prune transaction")?;
            repositories::recalculate_runtime_graph_support_counts_by_ids_with_executor(
                &mut transaction,
                library_id,
                EVIDENCE_PRUNE_PROJECTION_VERSION,
                &[case.victim_node_id],
                &[],
            )
            .await
            .context("failed to recalculate targeted support")?;
            let outcome =
                repositories::prune_runtime_graph_projection_rows_without_support_with_connection(
                    &mut transaction,
                    library_id,
                    EVIDENCE_PRUNE_PROJECTION_VERSION,
                    &[case.victim_node_id],
                    &[],
                )
                .await
                .context("transactional targeted support prune failed")?;
            transaction
                .commit()
                .await
                .context("failed to commit targeted-support-prune transaction")?;
            Ok(outcome)
        }
        EvidencePrunePath::StandaloneExplicitPrune => {
            repositories::prune_runtime_graph_projection_rows(
                pool,
                library_id,
                EVIDENCE_PRUNE_PROJECTION_VERSION,
                &[case.victim_node_id],
                &[],
            )
            .await
            .context("standalone explicit projection prune failed")
        }
        EvidencePrunePath::TransactionalExplicitPrune => {
            let mut transaction =
                pool.begin().await.context("failed to begin explicit-prune transaction")?;
            let outcome = repositories::prune_runtime_graph_projection_rows_with_connection(
                &mut transaction,
                library_id,
                EVIDENCE_PRUNE_PROJECTION_VERSION,
                &[case.victim_node_id],
                &[],
            )
            .await
            .context("transactional explicit projection prune failed")?;
            transaction.commit().await.context("failed to commit explicit-prune transaction")?;
            Ok(outcome)
        }
    }
}

async fn assert_evidence_prune_case_cleaned(
    pool: &PgPool,
    library_id: Uuid,
    case: &EvidencePruneCase,
    outcome: repositories::RuntimeGraphProjectionPruneOutcome,
) -> anyhow::Result<()> {
    let victim_exists = sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from runtime_graph_node where library_id = $1 and id = $2)",
    )
    .bind(library_id)
    .bind(case.victim_node_id)
    .fetch_one(pool)
    .await
    .context("failed to check pruned victim node")?;
    let survivor_exists = sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from runtime_graph_node where library_id = $1 and id = $2)",
    )
    .bind(library_id)
    .bind(case.survivor_node_id)
    .fetch_one(pool)
    .await
    .context("failed to check retained survivor node")?;
    let incident_edge_exists = sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from runtime_graph_edge where library_id = $1 and id = $2)",
    )
    .bind(library_id)
    .bind(case.incident_edge_id)
    .fetch_one(pool)
    .await
    .context("failed to check cascade-pruned incident edge")?;
    let missing_target_evidence_count = sqlx::query_scalar::<_, i64>(
        "select count(*)
         from runtime_graph_evidence as evidence
         where evidence.library_id = $1
           and (
             (
               evidence.target_kind = 'node'
               and not exists (
                 select 1 from runtime_graph_node as node where node.id = evidence.target_id
               )
             )
             or (
               evidence.target_kind = 'edge'
               and not exists (
                 select 1 from runtime_graph_edge as edge where edge.id = evidence.target_id
               )
             )
           )",
    )
    .bind(library_id)
    .fetch_one(pool)
    .await
    .context("failed to count evidence with missing graph targets")?;

    assert!(!victim_exists);
    assert!(survivor_exists);
    assert!(!incident_edge_exists);
    assert_eq!(outcome.deleted_node_count, 1);
    assert_eq!(outcome.deleted_edge_count, 1);
    assert_eq!(missing_target_evidence_count, 0);
    assert_eq!(search_evidence_prune_case(pool, library_id, case).await?, 0);
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn every_projection_prune_path_removes_polymorphic_evidence_and_search_hits()
-> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for graph projection evidence-prune test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = GraphProjectionRepositoryFixture::create(&pool).await?;

    let result = async {
        for path in [
            EvidencePrunePath::StandaloneSupportSynchronizer,
            EvidencePrunePath::TransactionalSupportSynchronizer,
            EvidencePrunePath::TransactionalTargetedSupportPrune,
            EvidencePrunePath::StandaloneExplicitPrune,
            EvidencePrunePath::TransactionalExplicitPrune,
        ] {
            let case = seed_evidence_prune_case(&pool, &fixture, path).await?;
            assert_evidence_prune_case_ready(&pool, fixture.library.id, &case).await?;
            let outcome =
                execute_evidence_prune_path(&pool, fixture.library.id, &case, path).await?;
            assert_evidence_prune_case_cleaned(&pool, fixture.library.id, &case, outcome).await?;
        }
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
async fn admitted_projection_queries_include_only_documents_and_connected_nodes()
-> anyhow::Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for graph projection repository test")?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = GraphProjectionRepositoryFixture::create(&pool).await?;

    let result = async {
        let projection_version = 1_i64;
        let source_document_id = sqlx::query_scalar::<_, Uuid>(
            "insert into content_document (
                workspace_id,
                library_id,
                external_key,
                document_state,
                created_at
            )
            values ($1, $2, $3, 'active', now())
            returning id",
        )
        .bind(fixture.workspace.id)
        .bind(fixture.library.id)
        .bind("graph-projection-alpha")
        .fetch_one(&pool)
        .await
        .context("failed to insert projection source document")?;
        let source_revision_id = crate::content_lifecycle_support::seed_graph_document_revision(
            &pool,
            source_document_id,
            "alpha.txt",
            "Projection source",
        )
        .await?;
        let document_key = format!("document:{source_document_id}");
        let document_node = repositories::upsert_runtime_graph_node(
            &pool,
            repositories::RuntimeGraphNodeInput {
                library_id: fixture.library.id,
                identity_key: &document_key,
                label: "alpha.txt",
                node_type: "document",
                document_id: Some(source_document_id),
                aliases_json: serde_json::json!([]),
                summary: Some("Document node"),
                metadata_json: serde_json::json!({}),
                support_count: 1,
                projection_version,
            },
        )
        .await
        .context("failed to create document node")?;
        let connected_entity = repositories::upsert_runtime_graph_node(
            &pool,
            repositories::RuntimeGraphNodeInput {
                library_id: fixture.library.id,
                identity_key: "entity:connected",
                label: "Connected Entity",
                node_type: "entity",
                document_id: None,
                aliases_json: serde_json::json!([]),
                summary: Some("Connected entity"),
                metadata_json: serde_json::json!({}),
                support_count: 1,
                projection_version,
            },
        )
        .await
        .context("failed to create connected entity node")?;
        let isolated_entity = repositories::upsert_runtime_graph_node(
            &pool,
            repositories::RuntimeGraphNodeInput {
                library_id: fixture.library.id,
                identity_key: "entity:isolated",
                label: "Isolated Entity",
                node_type: "entity",
                document_id: None,
                aliases_json: serde_json::json!([]),
                summary: Some("Isolated entity"),
                metadata_json: serde_json::json!({}),
                support_count: 1,
                projection_version,
            },
        )
        .await
        .context("failed to create isolated entity node")?;

        repositories::upsert_runtime_graph_edge(
            &pool,
            repositories::RuntimeGraphEdgeInput {
                library_id: fixture.library.id,
                from_node_id: document_node.id,
                to_node_id: connected_entity.id,
                relation_type: "mentions",
                identity_key: "edge:document-connected",
                summary: Some("Valid admitted edge"),
                weight: Some(1.0),
                support_count: 1,
                metadata_json: serde_json::json!({}),
                projection_version,
            },
        )
        .await
        .context("failed to create admitted edge")?;
        repositories::upsert_runtime_graph_edge(
            &pool,
            repositories::RuntimeGraphEdgeInput {
                library_id: fixture.library.id,
                from_node_id: isolated_entity.id,
                to_node_id: isolated_entity.id,
                relation_type: "self_loop",
                identity_key: "edge:isolated-self-loop",
                summary: Some("Degenerate loop"),
                weight: Some(0.2),
                support_count: 1,
                metadata_json: serde_json::json!({}),
                projection_version,
            },
        )
        .await
        .context("failed to create degenerate loop edge")?;
        repositories::upsert_runtime_graph_edge(
            &pool,
            repositories::RuntimeGraphEdgeInput {
                library_id: fixture.library.id,
                from_node_id: isolated_entity.id,
                to_node_id: document_node.id,
                relation_type: "   ",
                identity_key: "edge:isolated-blank",
                summary: Some("Blank relation edge"),
                weight: Some(0.1),
                support_count: 1,
                metadata_json: serde_json::json!({}),
                projection_version,
            },
        )
        .await
        .context("failed to create blank relation edge")?;

        let future_entity = repositories::upsert_runtime_graph_node(
            &pool,
            repositories::RuntimeGraphNodeInput {
                library_id: fixture.library.id,
                identity_key: "entity:future",
                label: "Future Entity",
                node_type: "entity",
                document_id: None,
                aliases_json: serde_json::json!([]),
                summary: Some("Future projection entity"),
                metadata_json: serde_json::json!({}),
                support_count: 1,
                projection_version: 2_i64,
            },
        )
        .await
        .context("failed to create future projection entity")?;
        repositories::upsert_runtime_graph_edge(
            &pool,
            repositories::RuntimeGraphEdgeInput {
                library_id: fixture.library.id,
                from_node_id: document_node.id,
                to_node_id: future_entity.id,
                relation_type: "future",
                identity_key: "edge:document-future",
                summary: Some("Future projection edge"),
                weight: Some(1.0),
                support_count: 1,
                metadata_json: serde_json::json!({}),
                projection_version: 2,
            },
        )
        .await
        .context("failed to create future projection edge")?;

        // All candidates have current source evidence. The negative assertions
        // must test topology/version policy, not pass because evidence is absent.
        let targets: Vec<(String, Uuid)> = sqlx::query_as(
            "select 'node'::text, id from runtime_graph_node where library_id = $1
             union all select 'edge'::text, id from runtime_graph_edge where library_id = $1",
        )
        .bind(fixture.library.id)
        .fetch_all(&pool)
        .await?;
        for (kind, target_id) in targets {
            repositories::create_runtime_graph_evidence(
                &pool,
                repositories::CreateRuntimeGraphEvidenceInput {
                    library_id: fixture.library.id,
                    target_kind: &kind,
                    target_id,
                    document_id: Some(source_document_id),
                    revision_id: Some(source_revision_id),
                    activated_by_attempt_id: None,
                    chunk_id: None,
                    source_file_name: Some("alpha.txt"),
                    page_ref: None,
                    evidence_text: "Projection source evidence",
                    confidence_score: Some(1.0),
                    evidence_context_key: &format!("projection:{kind}:{target_id}"),
                },
            )
            .await?;
        }

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
            admitted_nodes.iter().map(|row| row.identity_key.as_str()).collect::<Vec<_>>();
        assert_eq!(admitted_nodes.len(), 2);
        assert!(admitted_keys.contains(&document_key.as_str()));
        assert!(admitted_keys.contains(&"entity:connected"));
        assert!(!admitted_keys.contains(&"entity:isolated"));
        assert!(!admitted_keys.contains(&"entity:future"));

        let admitted_id_keys =
            admitted_by_id.iter().map(|row| row.identity_key.as_str()).collect::<Vec<_>>();
        assert_eq!(admitted_by_id.len(), 2);
        assert!(admitted_id_keys.contains(&document_key.as_str()));
        assert!(admitted_id_keys.contains(&"entity:connected"));
        assert_eq!(counts.node_count, 2);
        assert_eq!(counts.edge_count, 1);

        Ok(())
    }
    .await;

    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}
