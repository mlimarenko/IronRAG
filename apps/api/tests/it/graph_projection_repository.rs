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

async fn seed_admission_source(
    pool: &PgPool,
    fixture: &GraphProjectionRepositoryFixture,
    key: &str,
) -> anyhow::Result<(Uuid, Uuid)> {
    let document_id = sqlx::query_scalar::<_, Uuid>(
        "insert into content_document (workspace_id, library_id, external_key, document_state)
         values ($1, $2, $3, 'active') returning id",
    )
    .bind(fixture.workspace.id)
    .bind(fixture.library.id)
    .bind(key)
    .fetch_one(pool)
    .await?;
    let revision_id = crate::content_lifecycle_support::seed_graph_document_revision(
        pool,
        document_id,
        "admission.txt",
        "Admission source",
    )
    .await?;
    Ok((document_id, revision_id))
}

async fn seed_admission_evidence(
    pool: &PgPool,
    library_id: Uuid,
    kind: &str,
    target_id: Uuid,
    source: (Option<Uuid>, Option<Uuid>),
) -> anyhow::Result<()> {
    repositories::create_runtime_graph_evidence(
        pool,
        repositories::CreateRuntimeGraphEvidenceInput {
            library_id,
            target_kind: kind,
            target_id,
            document_id: source.0,
            revision_id: source.1,
            activated_by_attempt_id: None,
            chunk_id: None,
            source_file_name: Some("admission.txt"),
            page_ref: None,
            evidence_text: "Admission evidence",
            confidence_score: Some(1.0),
            evidence_context_key: &Uuid::now_v7().to_string(),
        },
    )
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn targeted_admission_requires_every_source_and_scopes_incident_edges() -> anyhow::Result<()>
{
    let settings = Settings::from_env()?;
    let (pool, temp_database) = connect_postgres(&settings).await?;
    let fixture = GraphProjectionRepositoryFixture::create(&pool).await?;
    let other = GraphProjectionRepositoryFixture::create(&pool).await?;
    let result = async {
        let current = seed_admission_source(&pool, &fixture, "current").await?;
        let stale = seed_admission_source(&pool, &fixture, "stale").await?;
        let suppressed = seed_admission_source(&pool, &fixture, "suppressed").await?;
        let foreign_source = seed_admission_source(&pool, &other, "foreign").await?;
        // A readable head alone is insufficient: remove the knowledge closure.
        sqlx::query("update knowledge_revision set text_readable_at = null where revision_id = $1")
            .bind(stale.1)
            .execute(&pool)
            .await?;
        sqlx::query("update content_document set deleted_at = now() where id = $1")
            .bind(suppressed.0)
            .execute(&pool)
            .await?;
        let current_source = (Some(current.0), Some(current.1));
        let document_key = format!("document:{}", current.0);
        let document_id = sqlx::query_scalar::<_, Uuid>(
            "insert into runtime_graph_node
             (library_id, canonical_key, label, node_type, document_id, projection_version)
             values ($1, $2, 'Unconnected document', 'document', $3, 1) returning id",
        )
        .bind(fixture.library.id)
        .bind(&document_key)
        .bind(current.0)
        .fetch_one(&pool)
        .await?;
        seed_admission_evidence(&pool, fixture.library.id, "node", document_id, current_source)
            .await?;

        let mut requested_nodes = vec![document_id];
        let mut requested_edges = Vec::new();
        let mut expected_nodes = vec![document_id];
        let mut expected_edges = Vec::new();
        let mut expected_current_nodes = vec![document_id];
        for source in [stale, suppressed] {
            let rejected_document_id = sqlx::query_scalar::<_, Uuid>(
                "insert into runtime_graph_node
                 (library_id, canonical_key, label, node_type, document_id, projection_version)
                 values ($1, $2, 'Rejected document', 'document', $3, 1) returning id",
            )
            .bind(fixture.library.id)
            .bind(format!("document:{}", source.0))
            .bind(source.0)
            .fetch_one(&pool)
            .await?;
            // Even one valid source cannot rescue an aggregate with stale support.
            for evidence_source in [current_source, (Some(source.0), Some(source.1))] {
                seed_admission_evidence(
                    &pool, fixture.library.id, "node", rejected_document_id, evidence_source,
                ).await?;
            }
            requested_nodes.push(rejected_document_id);
        }
        let cases = [
            ("incoming", true, None, "related", false, 1),
            ("outgoing", true, None, "related", false, 1),
            ("mixed-stale", true, Some((Some(stale.0), Some(stale.1))), "related", false, 1),
            ("null-lineage", true, Some((None, None)), "related", false, 1),
            ("missing-document", true, Some((None, Some(current.1))), "related", false, 1),
            ("missing-revision", true, Some((Some(current.0), None)), "related", false, 1),
            ("mismatched-lineage", true, Some((Some(current.0), Some(stale.1))), "related", false, 1),
            ("foreign-lineage", true, Some((Some(foreign_source.0), Some(foreign_source.1))), "related", false, 1),
            ("suppressed", true, Some((Some(suppressed.0), Some(suppressed.1))), "related", false, 1),
            ("no-evidence", false, None, "related", false, 1),
            ("blank", true, None, "   ", false, 1),
            ("self", true, None, "related", true, 1),
            ("projection", true, None, "related", false, 2),
        ];
        for (name, has_current, extra_source, relation, self_edge, version) in cases {
            let node_id = sqlx::query_scalar::<_, Uuid>(
                "insert into runtime_graph_node
                 (library_id, canonical_key, label, node_type, projection_version)
                 values ($1, $2, $2, 'entity', 1) returning id",
            )
            .bind(fixture.library.id)
            .bind(format!("entity:{name}"))
            .fetch_one(&pool)
            .await?;
            // Separate entity endpoints ensure both incident directions are tested.
            let peer_id = sqlx::query_scalar::<_, Uuid>(
                "insert into runtime_graph_node
                 (library_id, canonical_key, label, node_type, projection_version)
                 values ($1, $2, $2, 'entity', 1) returning id",
            )
            .bind(fixture.library.id)
            .bind(format!("entity:peer:{name}"))
            .fetch_one(&pool)
            .await?;
            let (from_id, to_id) = if self_edge {
                (node_id, node_id)
            } else if name == "outgoing" {
                (node_id, peer_id)
            } else {
                (peer_id, node_id)
            };
            let edge_id = sqlx::query_scalar::<_, Uuid>(
                "insert into runtime_graph_edge
                 (library_id, canonical_key, from_node_id, to_node_id, relation_type, projection_version)
                 values ($1, $2, $3, $4, $5, $6) returning id",
            )
            .bind(fixture.library.id)
            .bind(format!("edge:{name}"))
            .bind(from_id)
            .bind(to_id)
            .bind(relation)
            .bind(version)
            .fetch_one(&pool)
            .await?;
            for (kind, id) in [("node", node_id), ("node", peer_id), ("edge", edge_id)] {
                if has_current {
                    seed_admission_evidence(&pool, fixture.library.id, kind, id, current_source).await?;
                }
                if let Some(source) = extra_source.filter(|_| id != peer_id) {
                    seed_admission_evidence(&pool, fixture.library.id, kind, id, source).await?;
                }
            }
            if name == "no-evidence" {
                // Neither a different kind nor a different library can support this edge.
                seed_admission_evidence(&pool, fixture.library.id, "node", edge_id, current_source).await?;
                seed_admission_evidence(&pool, other.library.id, "edge", edge_id,
                    (Some(foreign_source.0), Some(foreign_source.1))).await?;
            }
            requested_nodes.extend([node_id, peer_id]);
            requested_edges.push(edge_id);
            if has_current {
                expected_current_nodes.push(peer_id);
                if extra_source.is_none() {
                    expected_current_nodes.push(node_id);
                }
            }
            if matches!(name, "incoming" | "outgoing") {
                expected_nodes.extend([node_id, peer_id]);
                expected_edges.push(edge_id);
            }
        }

        // Fully supported foreign candidates test library isolation independently
        // of the source admission fence.
        sqlx::query(
            "insert into runtime_graph_node
             (library_id, canonical_key, label, node_type, projection_version)
             select $1, 'unrelated:' || n, 'Unrelated', 'entity', 1
             from generate_series(1, 2) n",
        )
        .bind(other.library.id)
        .execute(&pool)
        .await?;
        let foreign_ids: Vec<Uuid> = sqlx::query_scalar(
            "select id from runtime_graph_node where library_id = $1 order by canonical_key",
        )
        .bind(other.library.id)
        .fetch_all(&pool)
        .await?;
        let foreign_edge_id = sqlx::query_scalar::<_, Uuid>(
            "insert into runtime_graph_edge
             (library_id, canonical_key, from_node_id, to_node_id, relation_type, projection_version)
             values ($1, 'foreign-edge', $2, $3, 'related', 1) returning id",
        )
        .bind(other.library.id)
        .bind(foreign_ids[0])
        .bind(foreign_ids[1])
        .fetch_one(&pool)
        .await?;
        for (kind, id) in [("node", foreign_ids[0]), ("node", foreign_ids[1]), ("edge", foreign_edge_id)] {
            seed_admission_evidence(
                &pool, other.library.id, kind, id,
                (Some(foreign_source.0), Some(foreign_source.1)),
            ).await?;
        }
        let foreign_edges = repositories::list_admitted_runtime_graph_edges_by_ids(
            &pool, other.library.id, 1, &[foreign_edge_id],
        ).await?;
        assert_eq!(foreign_edges.len(), 1);
        requested_nodes.push(foreign_ids[0]);
        requested_edges.push(foreign_edge_id);
        let nodes = repositories::list_admitted_runtime_graph_nodes_by_ids(
            &pool, fixture.library.id, 1, &requested_nodes,
        ).await?;
        let mut actual_nodes = nodes.iter().map(|node| node.id).collect::<Vec<_>>();
        actual_nodes.sort_unstable();
        expected_nodes.sort_unstable();
        assert_eq!(actual_nodes, expected_nodes);
        let keys = repositories::list_admitted_runtime_graph_document_keys_with_executor(
            &pool, fixture.library.id, 1,
        ).await?;
        assert_eq!(keys, vec![document_key]);
        let edge_lists = [
            repositories::list_admitted_runtime_graph_edges_by_ids(
                &pool, fixture.library.id, 1, &requested_edges,
            ).await?,
            repositories::list_admitted_runtime_graph_edges_by_node_ids(
                &pool, fixture.library.id, 1, &requested_nodes,
            ).await?,
            repositories::list_admitted_runtime_graph_edges_by_library(
                &pool, fixture.library.id, 1,
            ).await?,
            repositories::list_top_admitted_runtime_graph_relations_by_library(
                &pool, fixture.library.id, 1, 100,
            ).await?,
        ];
        expected_edges.sort_unstable();
        for edges in edge_lists {
            let mut ids = edges.iter().map(|edge| edge.id).collect::<Vec<_>>();
            ids.sort_unstable();
            assert_eq!(ids, expected_edges);
        }

        let slim_edges = repositories::list_admitted_runtime_graph_query_edges_by_library(
            &pool, fixture.library.id, 1,
        ).await?;
        let mut slim_edge_ids = slim_edges.iter().map(|edge| edge.id).collect::<Vec<_>>();
        slim_edge_ids.sort_unstable();
        assert_eq!(slim_edge_ids, expected_edges);
        let compact_edges = repositories::list_admitted_runtime_graph_edges_compact_by_library(
            &pool, fixture.library.id, 1,
        ).await?;
        let mut compact = compact_edges.iter().map(|edge| {
            (edge.from_node_id, edge.to_node_id, edge.relation_type.clone(), edge.support_count)
        }).collect::<Vec<_>>();
        let mut slim = slim_edges.iter().map(|edge| {
            (edge.from_node_id, edge.to_node_id, edge.relation_type.clone(), edge.support_count)
        }).collect::<Vec<_>>();
        compact.sort_unstable();
        slim.sort_unstable();
        assert_eq!(compact, slim, "streamed compact topology must retain full-row admission");

        // These readers trust supplied incident ids, but must independently
        // fence node evidence and the automatic document fallback.
        let slim_nodes = repositories::list_runtime_graph_query_nodes_by_ids_or_document_type(
            &pool, fixture.library.id, 1, &requested_nodes,
        ).await?;
        sqlx::query("update runtime_graph_node set metadata_json=metadata_json || jsonb_build_object(
            'sub_type','kind','confidence',0.75,'entity_state',42,'relation_state','inactive',
            'unrelated_payload',repeat('x',262144)) where id=$1")
            .bind(expected_current_nodes[0]).execute(&pool).await?;
        let stream_nodes = repositories::list_runtime_graph_topology_nodes_by_ids_or_document_type(
            &pool, fixture.library.id, 1, &requested_nodes,
        ).await?;
        for row in &stream_nodes {
            let full = repositories::get_runtime_graph_node_by_id(&pool, fixture.library.id, row.id)
                .await?.context("topology node must retain its source row")?;
            assert_eq!(row.identity_key, full.identity_key);
            assert_eq!(row.label, full.label);
            assert_eq!(row.node_type, full.node_type);
            assert_eq!(row.aliases_json, full.aliases_json);
            assert_eq!(row.summary, full.summary);
            assert_eq!(row.support_count, full.support_count);
            assert_eq!(row.document_id_json.as_ref(), full.metadata_json.get("document_id"));
            assert_eq!(row.sub_type_json.as_ref(), full.metadata_json.get("sub_type"));
            assert_eq!(row.confidence_json.as_ref(), full.metadata_json.get("confidence"));
            assert_eq!(row.entity_state_json.as_ref(), full.metadata_json.get("entity_state"));
            assert_eq!(row.relation_state_json.as_ref(), full.metadata_json.get("relation_state"));
        }
        let mut slim_node_ids = slim_nodes.iter().map(|node| node.id).collect::<Vec<_>>();
        let mut stream_node_ids = stream_nodes.iter().map(|node| node.id).collect::<Vec<_>>();
        expected_current_nodes.sort_unstable();
        slim_node_ids.sort_unstable();
        stream_node_ids.sort_unstable();
        assert_eq!(slim_node_ids, expected_current_nodes);
        assert_eq!(stream_node_ids, expected_current_nodes);
        let entity_count = repositories::count_admitted_runtime_graph_entities_by_library(
            &pool, fixture.library.id, 1,
        ).await?;
        let entities = repositories::list_top_admitted_runtime_graph_entities_by_library(
            &pool, fixture.library.id, 1, 100,
        ).await?;
        let mut entity_ids = entities.iter().map(|node| node.id).collect::<Vec<_>>();
        entity_ids.sort_unstable();
        let expected_entities = expected_current_nodes.iter().copied()
            .filter(|id| *id != document_id).collect::<Vec<_>>();
        assert_eq!(entity_ids, expected_entities);
        assert_eq!(entity_count, i64::try_from(expected_entities.len())?);
        let relation_count = repositories::count_admitted_runtime_graph_relations_by_library(
            &pool, fixture.library.id, 1,
        ).await?;
        assert_eq!(relation_count, i64::try_from(expected_edges.len())?);

        // The pre-admitted identity reader remains raw. Its caller must prove
        // node membership at the owning relation selector, not only edge support.
        let raw_nodes = repositories::list_runtime_graph_nodes_by_ids(
            &pool, fixture.library.id, 1, &requested_nodes,
        ).await?;
        assert_eq!(raw_nodes.len(), requested_nodes.len() - 1, "only the foreign-library id is excluded");
        assert!(raw_nodes.len() > stream_nodes.len());

        let endpoint = slim_edges[0].from_node_id;
        seed_admission_evidence(&pool, fixture.library.id, "node", endpoint, (None, None)).await?;
        let relations = repositories::list_top_admitted_runtime_graph_relations_by_library(
            &pool, fixture.library.id, 1, 100,
        ).await?;
        assert_eq!(relations.len(), expected_edges.len() - 1,
            "the raw endpoint consumer must receive only independently admitted nodes");
        assert!(relations.iter().all(|edge| edge.from_node_id != endpoint && edge.to_node_id != endpoint));
        let relations_count = repositories::count_admitted_runtime_graph_relations_by_library(
            &pool, fixture.library.id, 1,
        ).await?;
        assert_eq!(relations_count, i64::try_from(relations.len())?);
        let raw = repositories::list_runtime_graph_nodes_by_ids(
            &pool, fixture.library.id, 1, &[endpoint],
        ).await?;
        assert_eq!(raw.len(), 1);
        sqlx::query("delete from runtime_graph_evidence where target_kind = 'node'
                     and target_id = $1 and document_id is null and revision_id is null")
            .bind(endpoint).execute(&pool).await?;

        // Add a large, genuinely admitted, unrelated graph in the same bucket.
        // The small explicit-id read must still return only its own exact targets.
        sqlx::query(
            "insert into runtime_graph_node
             (library_id, canonical_key, label, node_type, projection_version)
             select $1, 'bulk:' || n, 'Bulk entity', 'entity', 1
             from generate_series(1, 2048) n",
        )
        .bind(fixture.library.id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into runtime_graph_edge
             (library_id, canonical_key, from_node_id, to_node_id, relation_type, projection_version)
             select $1, 'bulk-edge:' || id::text, id, next_id, 'related', 1
             from (select id, lead(id) over (order by id) as next_id
                   from runtime_graph_node where library_id = $1 and canonical_key like 'bulk:%') n
             where next_id is not null",
        )
        .bind(fixture.library.id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into runtime_graph_evidence
             (library_id, evidence_identity_key, target_kind, target_id, document_id, revision_id, evidence_text)
             select $1, 'bulk:' || kind || ':' || id::text, kind, id, $2, $3, 'Bulk evidence'
             from (select 'node'::text as kind, id from runtime_graph_node
                   where library_id = $1 and canonical_key like 'bulk:%'
                   union all select 'edge'::text, id from runtime_graph_edge
                   where library_id = $1 and canonical_key like 'bulk-edge:%') targets",
        )
        .bind(fixture.library.id)
        .bind(current.0)
        .bind(current.1)
        .execute(&pool)
        .await?;
        let nodes = repositories::list_admitted_runtime_graph_nodes_by_ids(
            &pool, fixture.library.id, 1, &requested_nodes,
        ).await?;
        let mut actual_nodes = nodes.iter().map(|node| node.id).collect::<Vec<_>>();
        actual_nodes.sort_unstable();
        assert_eq!(actual_nodes, expected_nodes);
        let edges = repositories::list_admitted_runtime_graph_edges_by_ids(
            &pool, fixture.library.id, 1, &requested_edges,
        ).await?;
        let mut actual_edges = edges.iter().map(|edge| edge.id).collect::<Vec<_>>();
        actual_edges.sort_unstable();
        assert_eq!(actual_edges, expected_edges);
        Ok(())
    }.await;
    other.cleanup(&pool).await?;
    fixture.cleanup(&pool).await?;
    pool.close().await;
    temp_database.drop().await?;
    result
}

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
