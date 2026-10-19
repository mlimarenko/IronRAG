use crate::{mcp_tool_call_support, temp_postgres_database};

use anyhow::{Context, Result, anyhow};
use axum::Router;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use tokio::{
    sync::broadcast,
    time::{Duration, sleep},
};
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

use ironrag_backend::{
    app::{config::Settings, seed_clean_v1_ai_catalog, state::AppState},
    infra::{
        knowledge_rows::KnowledgeDocumentRow,
        repositories::{self, content_document_repository, iam_repository},
    },
    interfaces::http::{
        auth::hash_token,
        authorization::{PERMISSION_LIBRARY_READ, PERMISSION_LIBRARY_WRITE},
        router,
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        ingest::worker,
    },
    shared::auth_tokens::mint_plaintext_api_token,
};

#[derive(Clone)]
struct GrantSpec {
    resource_kind: &'static str,
    resource_id: Uuid,
    permission_kind: &'static str,
}

struct McpKnowledgeFixture {
    state: AppState,
    temp_database: TempPostgresDatabase,
    _content_storage_root: tempfile::TempDir,
    workspace_id: Uuid,
    library_id: Uuid,
    library_ref: String,
}

struct SeededGraphFixture {
    top_entity_id: Uuid,
    second_entity_id: Uuid,
    hidden_entity_id: Uuid,
    top_relation_id: Uuid,
    second_relation_id: Uuid,
    visible_document_id: Uuid,
    secondary_visible_document_id: Uuid,
}

impl McpKnowledgeFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for mcp knowledge test")?;
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, "mcp_knowledge").await?;
        settings.database_url = temp_database.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;
        let content_storage_root =
            tempfile::tempdir().context("failed to create MCP knowledge storage root")?;
        settings.dependency_object_storage_mode = "disabled".to_string();
        settings.content_storage_provider = "filesystem".to_string();
        settings.content_storage_topology = "single_node".to_string();
        settings.content_storage_key_prefix.clear();
        settings.content_storage_root = content_storage_root.path().to_string_lossy().into_owned();

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect to mcp knowledge postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply mcp knowledge migrations")?;
        seed_clean_v1_ai_catalog(&postgres)
            .await
            .context("failed to seed mcp knowledge AI catalog")?;

        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = state
            .application_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("mcp-knowledge-workspace-{suffix}")),
                    display_name: "MCP Knowledge Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create mcp knowledge workspace")?;
        let library = state
            .application_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("mcp-knowledge-library-{suffix}")),
                    display_name: "MCP Knowledge Library".to_string(),
                    description: Some("mcp knowledge proof fixture".to_string()),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create mcp knowledge library")?;

        Ok(Self {
            state,
            temp_database,
            _content_storage_root: content_storage_root,
            workspace_id: workspace.id,
            library_id: library.id,
            library_ref: format!("{}/{}", workspace.slug, library.slug),
        })
    }

    async fn configure_ingest_ai(&mut self) -> Result<()> {
        mcp_tool_call_support::configure_empty_graph_ingest_ai(
            &mut self.state,
            self.workspace_id,
            self.library_id,
            "MCP knowledge fixture",
            "secret://mcp-knowledge/ingest-provider",
            "MCP knowledge",
        )
        .await
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn mint_token_with_grants(&self, label: &str, grants: &[GrantSpec]) -> Result<String> {
        // `is_well_formed_api_token` rejects anything that is not `inrg_`
        // followed by 32 hex digits, and it runs before the secret is looked
        // up — a hand-built plaintext answers 401 whatever grants it holds.
        let plaintext = mint_plaintext_api_token();
        let token = iam_repository::create_api_token(
            &self.state.persistence.postgres,
            Some(self.workspace_id),
            label,
            "mcp-knowledge",
            None,
            None,
        )
        .await
        .with_context(|| format!("failed to create api token for {label}"))?;
        iam_repository::create_api_token_secret(
            &self.state.persistence.postgres,
            token.principal_id,
            &hash_token(&plaintext),
        )
        .await
        .with_context(|| format!("failed to create api token secret for {label}"))?;
        // Workspace-scoped requests also check membership, not just grants.
        iam_repository::upsert_workspace_membership(
            &self.state.persistence.postgres,
            self.workspace_id,
            token.principal_id,
            "active",
        )
        .await
        .with_context(|| format!("failed to create workspace membership for {label}"))?;

        for grant in grants {
            iam_repository::create_grant(
                &self.state.persistence.postgres,
                &iam_repository::NewGrant {
                    principal_id: token.principal_id,
                    resource_kind: grant.resource_kind,
                    resource_id: grant.resource_id,
                    permission_kind: grant.permission_kind,
                    granted_by_principal_id: None,
                    expires_at: None,
                },
            )
            .await
            .with_context(|| {
                format!(
                    "failed to create grant {}:{} for {label}",
                    grant.resource_kind, grant.permission_kind
                )
            })?;
        }

        Ok(plaintext)
    }

    async fn mcp_call(&self, token: &str, method: &str, params: Value) -> Result<Value> {
        mcp_tool_call_support::call_rpc(
            self.app(),
            "/v1/mcp",
            token,
            &format!("mcp-knowledge-{}", method.replace('/', "-")),
            method,
            params,
        )
        .await
    }

    async fn diagnostics_mcp_call(
        &self,
        token: &str,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        mcp_tool_call_support::call_rpc(
            self.app(),
            "/v1/mcp/diagnostics",
            token,
            &format!("mcp-knowledge-diagnostics-{}", method.replace('/', "-")),
            method,
            params,
        )
        .await
    }

    async fn tools_list(&self, token: &str) -> Result<Vec<String>> {
        let response = self.mcp_call(token, "tools/list", json!({})).await?;
        tool_names(&response)
    }

    /// The write-side tools live on the diagnostics surface only, so grant
    /// visibility has to be checked there as well as on `/v1/mcp`.
    async fn diagnostics_tools_list(&self, token: &str) -> Result<Vec<String>> {
        let response = mcp_tool_call_support::call_rpc(
            self.app(),
            "/v1/mcp/diagnostics",
            token,
            "mcp-knowledge-diagnostics-tools-list",
            "tools/list",
            json!({}),
        )
        .await?;
        tool_names(&response)
    }

    async fn insert_graph_document(
        &self,
        external_key: &str,
        title: &str,
    ) -> Result<KnowledgeDocumentRow> {
        let document = content_document_repository::create_document(
            &self.state.persistence.postgres,
            &content_document_repository::NewContentDocument {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                external_key,
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await
        .with_context(|| format!("failed to create content document {external_key}"))?;
        crate::content_lifecycle_support::seed_graph_document_revision(
            &self.state.persistence.postgres,
            document.id,
            &format!("{external_key}.md"),
            title,
        )
        .await?;
        self.state
            .document_store
            .get_document(document.id)
            .await?
            .context("published graph fixture document missing")
    }

    async fn seed_graph_quality_fixture(&self) -> Result<SeededGraphFixture> {
        let projection_version = 7_i64;
        let visible_document = self
            .insert_graph_document("mcp-graph-topology-visible", "Visible topology evidence")
            .await?;
        let secondary_visible_document = self
            .insert_graph_document(
                "mcp-graph-topology-visible-secondary",
                "Secondary visible topology evidence",
            )
            .await?;
        let hidden_document = self
            .insert_graph_document("mcp-graph-topology-hidden", "Hidden topology evidence")
            .await?;

        let nodes = self.create_graph_nodes(projection_version).await?;

        let edges = self.create_graph_edges(projection_version, &nodes).await?;

        self.create_graph_evidence(
            &GraphEvidenceDocuments {
                visible: visible_document.document_id,
                secondary_visible: secondary_visible_document.document_id,
                hidden: hidden_document.document_id,
            },
            &nodes,
            &edges,
        )
        .await?;

        self.upsert_graph_snapshot(projection_version).await?;

        Ok(SeededGraphFixture {
            top_entity_id: nodes.top,
            second_entity_id: nodes.second,
            hidden_entity_id: nodes.hidden,
            top_relation_id: edges.top,
            second_relation_id: edges.second,
            visible_document_id: visible_document.document_id,
            secondary_visible_document_id: secondary_visible_document.document_id,
        })
    }

    async fn create_graph_nodes(&self, projection_version: i64) -> Result<GraphNodeIds> {
        let top = repositories::upsert_runtime_graph_node(
            &self.state.persistence.postgres,
            repositories::RuntimeGraphNodeInput {
                library_id: self.library_id,
                identity_key: "entity:orion",
                label: "Orion",
                node_type: "entity",
                document_id: None,
                aliases_json: json!(["Orion Signal"]),
                summary: Some("Primary supported entity."),
                metadata_json: json!({}),
                support_count: 10,
                projection_version,
            },
        )
        .await
        .context("failed to create top graph entity")?;
        let second = repositories::upsert_runtime_graph_node(
            &self.state.persistence.postgres,
            repositories::RuntimeGraphNodeInput {
                library_id: self.library_id,
                identity_key: "entity:atlas",
                label: "Atlas",
                node_type: "entity",
                document_id: None,
                aliases_json: json!([]),
                summary: Some("Secondary supported entity."),
                metadata_json: json!({}),
                support_count: 8,
                projection_version,
            },
        )
        .await
        .context("failed to create second graph entity")?;
        let third = repositories::upsert_runtime_graph_node(
            &self.state.persistence.postgres,
            repositories::RuntimeGraphNodeInput {
                library_id: self.library_id,
                identity_key: "entity:zephyr",
                label: "Zephyr",
                node_type: "entity",
                document_id: None,
                aliases_json: json!([]),
                summary: Some("Tertiary supported entity."),
                metadata_json: json!({}),
                support_count: 7,
                projection_version,
            },
        )
        .await
        .context("failed to create third graph entity")?;
        let hidden = repositories::upsert_runtime_graph_node(
            &self.state.persistence.postgres,
            repositories::RuntimeGraphNodeInput {
                library_id: self.library_id,
                identity_key: "entity:noise",
                label: "Noise",
                node_type: "entity",
                document_id: None,
                aliases_json: json!([]),
                summary: Some("Low-value entity."),
                metadata_json: json!({}),
                support_count: 1,
                projection_version,
            },
        )
        .await
        .context("failed to create hidden graph entity")?;
        Ok(GraphNodeIds { top: top.id, second: second.id, third: third.id, hidden: hidden.id })
    }

    async fn create_graph_edges(
        &self,
        projection_version: i64,
        nodes: &GraphNodeIds,
    ) -> Result<GraphEdgeIds> {
        let top = repositories::upsert_runtime_graph_edge(
            &self.state.persistence.postgres,
            repositories::RuntimeGraphEdgeInput {
                library_id: self.library_id,
                from_node_id: nodes.top,
                to_node_id: nodes.second,
                relation_type: "depends_on",
                identity_key: "edge:orion:depends_on:atlas",
                summary: Some("Orion depends on Atlas."),
                weight: None,
                support_count: 9,
                metadata_json: json!({}),
                projection_version,
            },
        )
        .await
        .context("failed to create top graph relation")?;
        let second = repositories::upsert_runtime_graph_edge(
            &self.state.persistence.postgres,
            repositories::RuntimeGraphEdgeInput {
                library_id: self.library_id,
                from_node_id: nodes.second,
                to_node_id: nodes.third,
                relation_type: "feeds",
                identity_key: "edge:atlas:feeds:zephyr",
                summary: Some("Atlas feeds Zephyr."),
                weight: None,
                support_count: 5,
                metadata_json: json!({}),
                projection_version,
            },
        )
        .await
        .context("failed to create second graph relation")?;
        let hidden = repositories::upsert_runtime_graph_edge(
            &self.state.persistence.postgres,
            repositories::RuntimeGraphEdgeInput {
                library_id: self.library_id,
                from_node_id: nodes.top,
                to_node_id: nodes.hidden,
                relation_type: "mentions",
                identity_key: "edge:orion:mentions:noise",
                summary: Some("Orion mentions Noise."),
                weight: None,
                support_count: 1,
                metadata_json: json!({}),
                projection_version,
            },
        )
        .await
        .context("failed to create hidden graph relation")?;
        Ok(GraphEdgeIds { top: top.id, second: second.id, hidden: hidden.id })
    }

    async fn create_graph_evidence(
        &self,
        documents: &GraphEvidenceDocuments,
        nodes: &GraphNodeIds,
        edges: &GraphEdgeIds,
    ) -> Result<()> {
        let revisions: std::collections::HashMap<Uuid, Uuid> = sqlx::query_as::<_, (Uuid, Uuid)>(
            "select document_id, revision_id from content_current_revision where library_id = $1",
        )
        .bind(self.library_id)
        .fetch_all(&self.state.persistence.postgres)
        .await?
        .into_iter()
        .collect();
        repositories::create_runtime_graph_evidence(
            &self.state.persistence.postgres,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: self.library_id,
                target_kind: "node",
                target_id: nodes.top,
                document_id: Some(documents.visible),
                revision_id: Some(
                    *revisions.get(&documents.visible).context("visible source missing")?,
                ),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("visible.md"),
                page_ref: None,
                evidence_text: "Visible evidence for Orion.",
                confidence_score: Some(0.95),
                evidence_context_key: "visible-node",
            },
        )
        .await
        .context("failed to create visible node evidence")?;
        repositories::create_runtime_graph_evidence(
            &self.state.persistence.postgres,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: self.library_id,
                target_kind: "edge",
                target_id: edges.top,
                document_id: Some(documents.visible),
                revision_id: Some(
                    *revisions.get(&documents.visible).context("visible source missing")?,
                ),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("visible.md"),
                page_ref: None,
                evidence_text: "Visible evidence for Orion -> Atlas.",
                confidence_score: Some(0.9),
                evidence_context_key: "visible-edge",
            },
        )
        .await
        .context("failed to create visible edge evidence")?;
        repositories::create_runtime_graph_evidence(
            &self.state.persistence.postgres,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: self.library_id,
                target_kind: "node",
                target_id: nodes.second,
                document_id: Some(documents.secondary_visible),
                revision_id: Some(
                    *revisions
                        .get(&documents.secondary_visible)
                        .context("secondary source missing")?,
                ),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("visible-secondary.md"),
                page_ref: None,
                evidence_text: "Secondary visible evidence for Atlas.",
                confidence_score: Some(0.7),
                evidence_context_key: "visible-secondary-node",
            },
        )
        .await
        .context("failed to create secondary visible node evidence")?;
        for (kind, target_id) in [("node", nodes.third), ("edge", edges.second)] {
            repositories::create_runtime_graph_evidence(
                &self.state.persistence.postgres,
                repositories::CreateRuntimeGraphEvidenceInput {
                    library_id: self.library_id,
                    target_kind: kind,
                    target_id,
                    document_id: Some(documents.secondary_visible),
                    revision_id: Some(
                        *revisions
                            .get(&documents.secondary_visible)
                            .context("secondary source missing")?,
                    ),
                    activated_by_attempt_id: None,
                    chunk_id: None,
                    source_file_name: Some("visible-secondary.md"),
                    page_ref: None,
                    evidence_text: "Atlas feeds Zephyr.",
                    confidence_score: Some(0.7),
                    evidence_context_key: &format!("secondary:{kind}:{target_id}"),
                },
            )
            .await?;
        }
        repositories::create_runtime_graph_evidence(
            &self.state.persistence.postgres,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: self.library_id,
                target_kind: "node",
                target_id: nodes.hidden,
                document_id: Some(documents.hidden),
                revision_id: Some(
                    *revisions.get(&documents.hidden).context("hidden source missing")?,
                ),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("hidden.md"),
                page_ref: None,
                evidence_text: "Hidden evidence for Noise.",
                confidence_score: Some(0.2),
                evidence_context_key: "hidden-node",
            },
        )
        .await
        .context("failed to create hidden node evidence")?;
        repositories::create_runtime_graph_evidence(
            &self.state.persistence.postgres,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: self.library_id,
                target_kind: "edge",
                target_id: edges.hidden,
                document_id: Some(documents.hidden),
                revision_id: Some(
                    *revisions.get(&documents.hidden).context("hidden source missing")?,
                ),
                activated_by_attempt_id: None,
                chunk_id: None,
                source_file_name: Some("hidden.md"),
                page_ref: None,
                evidence_text: "Hidden evidence for Orion -> Noise.",
                confidence_score: Some(0.1),
                evidence_context_key: "hidden-edge",
            },
        )
        .await
        .context("failed to create hidden edge evidence")?;
        Ok(())
    }

    async fn upsert_graph_snapshot(&self, projection_version: i64) -> Result<()> {
        repositories::upsert_runtime_graph_snapshot(
            &self.state.persistence.postgres,
            repositories::RuntimeGraphSnapshotInput {
                library_id: self.library_id,
                graph_status: "ready",
                projection_version,
                build_epoch: Uuid::now_v7(),
                node_count: 4,
                edge_count: 3,
                provenance_coverage_percent: Some(100.0),
                last_error_message: None,
            },
            true,
        )
        .await
        .context("failed to upsert runtime graph snapshot")?;
        Ok(())
    }
}

struct GraphNodeIds {
    top: Uuid,
    second: Uuid,
    third: Uuid,
    hidden: Uuid,
}

struct GraphEdgeIds {
    top: Uuid,
    second: Uuid,
    hidden: Uuid,
}

struct GraphEvidenceDocuments {
    visible: Uuid,
    secondary_visible: Uuid,
    hidden: Uuid,
}

fn tool_names(value: &Value) -> Result<Vec<String>> {
    value["result"]["tools"]
        .as_array()
        .context("tools/list result must be an array")?
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .map(ToString::to_string)
                .context("tool descriptor must contain a string name")
        })
        .collect()
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn mcp_tool_visibility_tracks_grants_without_legacy_fallbacks() -> Result<()> {
    let fixture = McpKnowledgeFixture::create().await?;

    let result = async {
        let read_token = fixture
            .mint_token_with_grants(
                "read-token",
                &[GrantSpec {
                    resource_kind: "library",
                    resource_id: fixture.library_id,
                    permission_kind: PERMISSION_LIBRARY_READ,
                }],
            )
            .await?;
        let write_token = fixture
            .mint_token_with_grants(
                "write-token",
                &[GrantSpec {
                    resource_kind: "library",
                    resource_id: fixture.library_id,
                    permission_kind: PERMISSION_LIBRARY_WRITE,
                }],
            )
            .await?;

        let read_tools = fixture.tools_list(&read_token).await?;
        assert!(read_tools.contains(&"list_workspaces".to_string()));
        assert!(read_tools.contains(&"list_libraries".to_string()));
        assert!(read_tools.contains(&"search_documents".to_string()));
        assert!(read_tools.contains(&"read_document".to_string()));
        assert!(read_tools.contains(&"get_runtime_execution".to_string()));
        assert!(read_tools.contains(&"get_runtime_execution_trace".to_string()));
        assert!(read_tools.contains(&"get_web_run".to_string()));
        assert!(read_tools.contains(&"list_web_run_pages".to_string()));
        assert!(read_tools.contains(&"search_entities".to_string()));
        assert!(read_tools.contains(&"get_graph_topology".to_string()));
        assert!(read_tools.contains(&"list_relations".to_string()));
        assert!(read_tools.contains(&"get_communities".to_string()));
        assert!(!read_tools.contains(&"create_workspace".to_string()));
        assert!(!read_tools.contains(&"create_library".to_string()));
        assert!(!read_tools.contains(&"create_documents".to_string()));
        assert!(!read_tools.contains(&"create_document_revision".to_string()));
        assert!(!read_tools.contains(&"delete_document".to_string()));
        assert!(!read_tools.contains(&"get_operation".to_string()));
        assert!(!read_tools.contains(&"submit_web_run".to_string()));
        assert!(!read_tools.contains(&"cancel_web_run".to_string()));

        let write_tools = fixture.tools_list(&write_token).await?;
        assert!(write_tools.contains(&"list_workspaces".to_string()));
        assert!(write_tools.contains(&"list_libraries".to_string()));
        assert!(write_tools.contains(&"search_documents".to_string()));
        assert!(write_tools.contains(&"read_document".to_string()));
        // Runtime-execution reads are not implied by a library write grant;
        // they need their own read permission, which this token does not hold.
        assert!(!write_tools.contains(&"get_runtime_execution".to_string()));
        assert!(!write_tools.contains(&"get_runtime_execution_trace".to_string()));
        assert!(write_tools.contains(&"get_web_run".to_string()));
        assert!(write_tools.contains(&"list_web_run_pages".to_string()));
        assert!(write_tools.contains(&"search_entities".to_string()));
        assert!(write_tools.contains(&"get_graph_topology".to_string()));
        assert!(write_tools.contains(&"list_relations".to_string()));
        assert!(write_tools.contains(&"get_communities".to_string()));
        // Every mutating tool moved to the diagnostics surface. A write grant
        // must not resurrect them on `/v1/mcp`, and must reach them on
        // `/v1/mcp/diagnostics`.
        for diagnostics_only in [
            "create_documents",
            "create_document_revision",
            "delete_document",
            "get_operation",
            "submit_web_run",
            "cancel_web_run",
        ] {
            assert!(
                !write_tools.contains(&diagnostics_only.to_string()),
                "{diagnostics_only} must not appear on the primary MCP surface"
            );
        }

        let write_diagnostics_tools = fixture.diagnostics_tools_list(&write_token).await?;
        for expected in [
            "create_documents",
            "create_document_revision",
            "delete_document",
            "get_operation",
            "submit_web_run",
            "cancel_web_run",
        ] {
            assert!(
                write_diagnostics_tools.contains(&expected.to_string()),
                "{expected} must be reachable with a document-write grant"
            );
        }
        // Catalog administration still needs more than a library write grant.
        assert!(!write_diagnostics_tools.contains(&"create_workspace".to_string()));
        assert!(!write_diagnostics_tools.contains(&"create_library".to_string()));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn graph_tools_return_ranked_coherent_subgraphs_instead_of_orphaned_slices() -> Result<()> {
    let fixture = McpKnowledgeFixture::create().await?;

    let result = async {
        let seeded = fixture.seed_graph_quality_fixture().await?;
        let token = fixture
            .mint_token_with_grants(
                "graph-read-token",
                &[GrantSpec {
                    resource_kind: "library",
                    resource_id: fixture.library_id,
                    permission_kind: PERMISSION_LIBRARY_READ,
                }],
            )
            .await?;

        let entity_search = fixture
            .mcp_call(
                &token,
                "tools/call",
                json!({
                    "name": "search_entities",
                    "arguments": {
                        "library": fixture.library_ref.clone(),
                        "query": "Orion",
                        "limit": 2,
                    },
                }),
            )
            .await?;
        assert_eq!(entity_search["result"]["isError"], json!(false));
        let entity_hits = entity_search["result"]["structuredContent"]["entities"]
            .as_array()
            .context("search_entities content must be an array")?;
        assert!(!entity_hits.is_empty());
        assert_eq!(entity_hits[0]["entityId"], json!(seeded.top_entity_id));

        let topology = fixture
            .mcp_call(
                &token,
                "tools/call",
                json!({
                    "name": "get_graph_topology",
                    "arguments": {
                        "library": fixture.library_ref.clone(),
                        "limit": 2,
                    },
                }),
            )
            .await?;
        assert_eq!(topology["result"]["isError"], json!(false));
        let entities = topology["result"]["structuredContent"]["entities"]
            .as_array()
            .context("graph topology entities must be an array")?;
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0]["entityId"], json!(seeded.top_entity_id));
        assert_eq!(entities[1]["entityId"], json!(seeded.second_entity_id));
        let relations = topology["result"]["structuredContent"]["relations"]
            .as_array()
            .context("graph topology relations must be an array")?;
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0]["relationId"], json!(seeded.top_relation_id));
        let links = topology["result"]["structuredContent"]["documentLinks"]
            .as_array()
            .context("graph topology links must be an array")?;
        assert!(!links.is_empty());
        let visible_document_ids =
            [seeded.visible_document_id, seeded.secondary_visible_document_id]
                .into_iter()
                .map(|id| json!(id))
                .collect::<Vec<_>>();
        assert!(links.iter().all(|row| visible_document_ids.contains(&row["documentId"])));
        assert!(
            links.iter().all(|row| row["targetNodeId"] != json!(seeded.hidden_entity_id)),
            "hidden target should not leak into the truncated subgraph"
        );
        let documents = topology["result"]["structuredContent"]["documents"]
            .as_array()
            .context("graph topology documents must be an array")?;
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0]["documentId"], json!(seeded.visible_document_id));
        assert_eq!(documents[1]["documentId"], json!(seeded.secondary_visible_document_id));
        let linked_document_ids = links
            .iter()
            .filter_map(|row| row["documentId"].as_str().map(ToString::to_string))
            .collect::<std::collections::BTreeSet<_>>();
        let topology_document_ids = documents
            .iter()
            .filter_map(|row| row["documentId"].as_str().map(ToString::to_string))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(linked_document_ids, topology_document_ids);
        assert_eq!(
            topology["result"]["structuredContent"]["truncation"]["totalEntities"],
            json!(4)
        );
        assert_eq!(
            topology["result"]["structuredContent"]["truncation"]["totalRelations"],
            json!(3)
        );

        let list_relations = fixture
            .mcp_call(
                &token,
                "tools/call",
                json!({
                    "name": "list_relations",
                    "arguments": {
                        "library": fixture.library_ref.clone(),
                        "limit": 2,
                    },
                }),
            )
            .await?;
        assert_eq!(list_relations["result"]["isError"], json!(false));
        let relation_rows = list_relations["result"]["structuredContent"]["relations"]
            .as_array()
            .context("list_relations content must be an array")?;
        assert_eq!(relation_rows.len(), 2);
        assert_eq!(relation_rows[0]["relationId"], json!(seeded.top_relation_id));
        assert_eq!(relation_rows[1]["relationId"], json!(seeded.second_relation_id));
        assert_eq!(relation_rows[0]["sourceLabel"], json!("Orion"));
        assert_eq!(relation_rows[0]["targetLabel"], json!("Atlas"));
        assert!(relation_rows.iter().all(|row| row["sourceLabel"] != json!("unknown")));
        assert!(relation_rows.iter().all(|row| row["targetLabel"] != json!("unknown")));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn upload_status_and_grounded_search_read_share_authoritative_knowledge_truth() -> Result<()>
{
    let mut fixture = McpKnowledgeFixture::create().await?;
    fixture.configure_ingest_ai().await?;
    let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
    let worker_handle = worker::spawn_ingestion_worker(fixture.state.clone(), shutdown_rx);

    let result = async {
        let token = fixture
            .mint_token_with_grants(
                "write-token",
                &[GrantSpec {
                    resource_kind: "library",
                    resource_id: fixture.library_id,
                    permission_kind: PERMISSION_LIBRARY_WRITE,
                }],
            )
            .await?;

        let upload = fixture
            .diagnostics_mcp_call(
                &token,
                "tools/call",
                json!({
                    "name": "create_documents",
                    "arguments": {
                        "library": fixture.library_ref.clone(),
                        "documents": [{
                            "fileName": "mcp-knowledge-upload.txt",
                            "mimeType": "text/plain",
                            "title": "Upload Proof",
                            "contentBase64": BASE64_STANDARD.encode("Shared async operation proof for MCP knowledge tests."),
                        }],
                    },
                }),
            )
            .await?;
        assert_eq!(upload["result"]["isError"], json!(false), "upload response: {upload:#}");
        let receipt = &upload["result"]["structuredContent"]["receipts"][0];
        assert_eq!(receipt["operationKind"], json!("upload"));
        assert!(matches!(
            receipt["status"].as_str(),
            Some("accepted" | "processing" | "ready")
        ));
        let receipt_document_id: Uuid =
            serde_json::from_value(receipt["documentId"].clone()).context("missing document id")?;
        assert!(receipt.get("runtimeTrackingId").is_none());
        // `operationId` is the authoritative async-operation id (renamed from
        // `receiptId` — plan §6.3/§6.4 convergence), pollable through
        // `get_operation`, the same authoritative store `GET
        // /v1/ops/operations/{operationId}` reads.
        let operation_id: Uuid =
            serde_json::from_value(receipt["operationId"].clone()).context("missing operation id")?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let status = loop {
            let status = fixture
                .diagnostics_mcp_call(
                    &token,
                    "tools/call",
                    json!({
                        "name": "get_operation",
                        "arguments": {
                            "operationId": operation_id,
                        },
                    }),
                )
                .await?;
            assert_eq!(status["result"]["isError"], json!(false), "status response: {status:#}");
            match status["result"]["structuredContent"]["status"].as_str() {
                Some("ready") => break status,
                Some("failed" | "canceled") => {
                    anyhow::bail!("MCP upload terminated before it became readable: {status:#}");
                }
                _ if tokio::time::Instant::now() >= deadline => {
                    anyhow::bail!("timed out waiting for MCP upload operation: {status:#}");
                }
                _ => sleep(Duration::from_millis(100)).await,
            }
        };
        // `get_operation` reads the authoritative `OpsAsyncOperation` row
        // (flattened), not a content-mutation-specific receipt, so it no
        // longer carries a `documentId` field — only the mutation receipt
        // itself (`receipt`, above) does.
        assert_eq!(status["result"]["structuredContent"]["id"], json!(operation_id));
        assert!(matches!(
            status["result"]["structuredContent"]["status"].as_str(),
            Some("accepted" | "processing" | "ready")
        ));

        let uploaded_read = fixture
            .mcp_call(
                &token,
                "tools/call",
                json!({
                    "name": "read_document",
                    "arguments": {
                        "documentId": receipt_document_id,
                        "mode": "full",
                        "includeReferences": true,
                    },
                }),
            )
            .await?;
        assert_eq!(uploaded_read["result"]["isError"], json!(false));
        assert_eq!(
            uploaded_read["result"]["structuredContent"]["documentId"],
            json!(receipt_document_id)
        );
        assert_eq!(uploaded_read["result"]["structuredContent"]["libraryId"], json!(fixture.library_id));
        assert_eq!(uploaded_read["result"]["structuredContent"]["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(uploaded_read["result"]["structuredContent"]["readabilityState"], json!("readable"));
        let readable_revision_id =
            uploaded_read["result"]["structuredContent"]["latestRevisionId"].clone();
        assert!(readable_revision_id.is_string());
        assert!(
            uploaded_read["result"]["structuredContent"]["content"]
                .as_str()
                .is_some_and(|content| content.contains("Shared async operation proof for MCP knowledge tests."))
        );
        let uploaded_chunk_refs = uploaded_read["result"]["structuredContent"]["chunkReferences"]
            .as_array()
            .context("uploaded read chunk references must be an array")?;
        assert!(!uploaded_chunk_refs.is_empty());

        let mut uploaded_search = json!({});
        let mut uploaded_hit = None;
        for _attempt in 0..60 {
            uploaded_search = fixture
                .mcp_call(
                    &token,
                    "tools/call",
                    json!({
                        "name": "search_documents",
                        "arguments": {
                            "query": "Shared async operation proof",
                            "libraries": [fixture.library_ref.clone()],
                            "limit": 5,
                            "includeReferences": true,
                        },
                    }),
                )
                .await?;
            assert_eq!(uploaded_search["result"]["isError"], json!(false));
            let uploaded_hits = uploaded_search["result"]["structuredContent"]["hits"]
                .as_array()
                .context("uploaded search hits must be an array")?;
            uploaded_hit =
                uploaded_hits.iter().find(|hit| hit["documentId"] == json!(receipt_document_id));
            if uploaded_hit.is_some() {
                break;
            }
            sleep(Duration::from_millis(250)).await;
        }
        let uploaded_hit = uploaded_hit.context(
            "uploaded search hit must include the uploaded document after search-view catch-up",
        )?;
        assert_eq!(uploaded_hit["libraryId"], json!(fixture.library_id));
        assert_eq!(uploaded_hit["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(uploaded_hit["readabilityState"], json!("readable"));
        assert_eq!(uploaded_hit["latestRevisionId"], readable_revision_id);
        assert!(
            uploaded_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("Shared async operation proof")),
            "uploaded search excerpt must be anchored to the matching body: {uploaded_hit:#}",
        );
        let uploaded_hit_chunk_refs = uploaded_hit["chunkReferences"]
            .as_array()
            .context("uploaded search chunk references must be an array")?;
        assert!(!uploaded_hit_chunk_refs.is_empty());

        let mut filename_search = json!({});
        let mut filename_hit = None;
        for _attempt in 0..20 {
            filename_search = fixture
                .mcp_call(
                    &token,
                    "tools/call",
                    json!({
                        "name": "search_documents",
                        "arguments": {
                            "query": "mcp-knowledge-upload.txt",
                            "libraries": [fixture.library_ref.clone()],
                            "limit": 5,
                        },
                    }),
                )
                .await?;
            assert_eq!(filename_search["result"]["isError"], json!(false));
            let filename_hits = filename_search["result"]["structuredContent"]["hits"]
                .as_array()
                .context("filename search hits must be an array")?;
            filename_hit =
                filename_hits.iter().find(|hit| hit["documentId"] == json!(receipt_document_id));
            if filename_hit.is_some() {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
        let filename_hit = filename_hit
            .context("metadata filename search must include the uploaded document")?;
        assert_eq!(filename_hit["libraryId"], json!(fixture.library_id));
        assert_eq!(filename_hit["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(filename_hit["readabilityState"], json!("readable"));
        assert_eq!(filename_hit["latestRevisionId"], readable_revision_id);
        assert!(
            filename_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("mcp-knowledge-upload.txt")),
            "filename search excerpt must expose the matching metadata: {filename_hit:#}",
        );

        let mut title_search = json!({});
        let mut title_hit = None;
        for _attempt in 0..20 {
            title_search = fixture
                .mcp_call(
                    &token,
                    "tools/call",
                    json!({
                        "name": "search_documents",
                        "arguments": {
                            "query": "Upload Proof",
                            "libraries": [fixture.library_ref.clone()],
                            "limit": 5,
                        },
                    }),
                )
                .await?;
            assert_eq!(title_search["result"]["isError"], json!(false));
            let title_hits = title_search["result"]["structuredContent"]["hits"]
                .as_array()
                .context("title search hits must be an array")?;
            title_hit = title_hits.iter().find(|hit| hit["documentId"] == json!(receipt_document_id));
            if title_hit.is_some() {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
        let title_hit =
            title_hit.context("metadata title search must include the uploaded document")?;
        assert_eq!(title_hit["libraryId"], json!(fixture.library_id));
        assert_eq!(title_hit["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(title_hit["readabilityState"], json!("readable"));
        assert_eq!(title_hit["latestRevisionId"], readable_revision_id);
        assert!(
            title_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("Upload Proof"))
        );

        Ok(())
    }
    .await;

    let _ = shutdown_tx.send(());
    let worker_result = match tokio::time::timeout(Duration::from_secs(5), worker_handle).await {
        Ok(join_result) => join_result.context("MCP ingestion worker task failed"),
        Err(error) => Err(anyhow!(error).context("timed out stopping MCP ingestion worker")),
    };
    let cleanup_result = fixture.cleanup().await;
    result?;
    worker_result?;
    cleanup_result
}
