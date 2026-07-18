#[path = "support/mcp_tool_call_support.rs"]
mod mcp_tool_call_support;

use anyhow::{Context, Result};
use axum::Router;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::time::{Duration, sleep};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        knowledge_rows::KnowledgeDocumentRow,
        repositories::{self, content_repository, iam_repository},
    },
    interfaces::http::{
        auth::hash_token,
        authorization::{PERMISSION_LIBRARY_READ, PERMISSION_LIBRARY_WRITE},
        router,
    },
    services::catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
};

#[derive(Clone)]
struct GrantSpec {
    resource_kind: &'static str,
    resource_id: Uuid,
    permission_kind: &'static str,
}

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("mcp_knowledge_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect to postgres admin database")?;

        terminate_database_connections(&admin_pool, &database_name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {database_name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {database_name}"))?;
        admin_pool.close().await;

        Ok(Self {
            database_url: replace_database_name(base_database_url, &database_name)?,
            admin_url,
            name: database_name,
        })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect postgres admin database for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct McpKnowledgeFixture {
    state: AppState,
    temp_database: TempDatabase,
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
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect to mcp knowledge postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply mcp knowledge migrations")?;

        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = state
            .canonical_services
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
            .canonical_services
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
            workspace_id: workspace.id,
            library_id: library.id,
            library_ref: format!("{}/{}", workspace.slug, library.slug),
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn mint_token_with_grants(&self, label: &str, grants: &[GrantSpec]) -> Result<String> {
        let plaintext = format!("mcp-knowledge-{label}-{}", Uuid::now_v7());
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

        for grant in grants {
            iam_repository::create_grant(
                &self.state.persistence.postgres,
                token.principal_id,
                grant.resource_kind,
                grant.resource_id,
                grant.permission_kind,
                None,
                None,
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

    async fn tools_list(&self, token: &str) -> Result<Vec<String>> {
        let response = self.mcp_call(token, "tools/list", json!({})).await?;
        tool_names(&response)
    }

    async fn insert_graph_document(
        &self,
        external_key: &str,
        title: &str,
    ) -> Result<KnowledgeDocumentRow> {
        let document = content_repository::create_document(
            &self.state.persistence.postgres,
            &content_repository::NewContentDocument {
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
        let now = Utc::now();
        self.state
            .document_store
            .upsert_document(&KnowledgeDocumentRow {
                document_id: document.id,
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                external_key: external_key.to_string(),
                file_name: Some(format!("{external_key}.md")),
                title: Some(title.to_string()),
                source_uri: None,
                document_hint: None,
                document_state: "active".to_string(),
                active_revision_id: None,
                readable_revision_id: None,
                latest_revision_no: None,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                parent_document_id: None,
                document_role: "primary".to_string(),
            })
            .await
            .with_context(|| format!("failed to upsert knowledge document {external_key}"))
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

        let top_entity = repositories::upsert_runtime_graph_node(
            &self.state.persistence.postgres,
            self.library_id,
            "entity:orion",
            "Orion",
            "entity",
            None,
            json!(["Orion Signal"]),
            Some("Primary supported entity."),
            json!({}),
            10,
            projection_version,
        )
        .await
        .context("failed to create top graph entity")?;
        let second_entity = repositories::upsert_runtime_graph_node(
            &self.state.persistence.postgres,
            self.library_id,
            "entity:atlas",
            "Atlas",
            "entity",
            None,
            json!([]),
            Some("Secondary supported entity."),
            json!({}),
            8,
            projection_version,
        )
        .await
        .context("failed to create second graph entity")?;
        let third_entity = repositories::upsert_runtime_graph_node(
            &self.state.persistence.postgres,
            self.library_id,
            "entity:zephyr",
            "Zephyr",
            "entity",
            None,
            json!([]),
            Some("Tertiary supported entity."),
            json!({}),
            7,
            projection_version,
        )
        .await
        .context("failed to create third graph entity")?;
        let hidden_entity = repositories::upsert_runtime_graph_node(
            &self.state.persistence.postgres,
            self.library_id,
            "entity:noise",
            "Noise",
            "entity",
            None,
            json!([]),
            Some("Low-value entity."),
            json!({}),
            1,
            projection_version,
        )
        .await
        .context("failed to create hidden graph entity")?;

        let top_relation = repositories::upsert_runtime_graph_edge(
            &self.state.persistence.postgres,
            self.library_id,
            top_entity.id,
            second_entity.id,
            "depends_on",
            "edge:orion:depends_on:atlas",
            Some("Orion depends on Atlas."),
            None,
            9,
            json!({}),
            projection_version,
        )
        .await
        .context("failed to create top graph relation")?;
        let second_relation = repositories::upsert_runtime_graph_edge(
            &self.state.persistence.postgres,
            self.library_id,
            second_entity.id,
            third_entity.id,
            "feeds",
            "edge:atlas:feeds:zephyr",
            Some("Atlas feeds Zephyr."),
            None,
            5,
            json!({}),
            projection_version,
        )
        .await
        .context("failed to create second graph relation")?;
        let hidden_relation = repositories::upsert_runtime_graph_edge(
            &self.state.persistence.postgres,
            self.library_id,
            top_entity.id,
            hidden_entity.id,
            "mentions",
            "edge:orion:mentions:noise",
            Some("Orion mentions Noise."),
            None,
            1,
            json!({}),
            projection_version,
        )
        .await
        .context("failed to create hidden graph relation")?;

        repositories::create_runtime_graph_evidence(
            &self.state.persistence.postgres,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: self.library_id,
                target_kind: "node",
                target_id: top_entity.id,
                document_id: Some(visible_document.document_id),
                revision_id: None,
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
                target_id: top_relation.id,
                document_id: Some(visible_document.document_id),
                revision_id: None,
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
                target_id: second_entity.id,
                document_id: Some(secondary_visible_document.document_id),
                revision_id: None,
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
        repositories::create_runtime_graph_evidence(
            &self.state.persistence.postgres,
            repositories::CreateRuntimeGraphEvidenceInput {
                library_id: self.library_id,
                target_kind: "node",
                target_id: hidden_entity.id,
                document_id: Some(hidden_document.document_id),
                revision_id: None,
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
                target_id: hidden_relation.id,
                document_id: Some(hidden_document.document_id),
                revision_id: None,
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

        repositories::upsert_runtime_graph_snapshot(
            &self.state.persistence.postgres,
            self.library_id,
            "ready",
            projection_version,
            Uuid::now_v7(),
            4,
            3,
            Some(100.0),
            None,
            true,
        )
        .await
        .context("failed to upsert runtime graph snapshot")?;

        Ok(SeededGraphFixture {
            top_entity_id: top_entity.id,
            second_entity_id: second_entity.id,
            hidden_entity_id: hidden_entity.id,
            top_relation_id: top_relation.id,
            second_relation_id: second_relation.id,
            visible_document_id: visible_document.document_id,
            secondary_visible_document_id: secondary_visible_document.document_id,
        })
    }
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

fn replace_database_name(database_url: &str, new_database: &str) -> Result<String> {
    let (without_query, query_suffix) = database_url
        .split_once('?')
        .map_or((database_url, None), |(prefix, suffix)| (prefix, Some(suffix)));
    let slash_index = without_query.rfind('/').context("database url is missing database name")?;
    let mut rebuilt = format!("{}{new_database}", &without_query[..=slash_index]);
    if let Some(query) = query_suffix {
        rebuilt.push('?');
        rebuilt.push_str(query);
    }
    Ok(rebuilt)
}

async fn terminate_database_connections(postgres: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(postgres)
    .await
    .with_context(|| format!("failed to terminate connections for {database_name}"))?;
    Ok(())
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
        assert!(write_tools.contains(&"create_documents".to_string()));
        assert!(write_tools.contains(&"create_document_revision".to_string()));
        assert!(write_tools.contains(&"delete_document".to_string()));
        assert!(write_tools.contains(&"get_operation".to_string()));
        assert!(write_tools.contains(&"get_runtime_execution".to_string()));
        assert!(write_tools.contains(&"get_runtime_execution_trace".to_string()));
        assert!(write_tools.contains(&"submit_web_run".to_string()));
        assert!(write_tools.contains(&"get_web_run".to_string()));
        assert!(write_tools.contains(&"list_web_run_pages".to_string()));
        assert!(write_tools.contains(&"cancel_web_run".to_string()));
        assert!(write_tools.contains(&"search_entities".to_string()));
        assert!(write_tools.contains(&"get_graph_topology".to_string()));
        assert!(write_tools.contains(&"list_relations".to_string()));
        assert!(write_tools.contains(&"get_communities".to_string()));
        assert!(!write_tools.contains(&"create_workspace".to_string()));
        assert!(!write_tools.contains(&"create_library".to_string()));

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
        let relation_rows = list_relations["result"]["structuredContent"]
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
async fn upload_status_and_grounded_search_read_share_canonical_knowledge_truth() -> Result<()> {
    let fixture = McpKnowledgeFixture::create().await?;

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
            .mcp_call(
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
        assert_eq!(upload["result"]["isError"], json!(false));
        let receipt = &upload["result"]["structuredContent"]["receipts"][0];
        assert_eq!(receipt["operationKind"], json!("upload"));
        assert!(matches!(
            receipt["status"].as_str(),
            Some("accepted" | "processing" | "ready")
        ));
        let receipt_document_id: Uuid =
            serde_json::from_value(receipt["documentId"].clone()).context("missing document id")?;
        assert!(receipt.get("runtimeTrackingId").is_none());
        // `operationId` is the canonical async-operation id (renamed from
        // `receiptId` — plan §6.3/§6.4 convergence), pollable through
        // `get_operation`, the same canonical store `GET
        // /v1/ops/operations/{operationId}` reads.
        let operation_id: Uuid =
            serde_json::from_value(receipt["operationId"].clone()).context("missing operation id")?;

        let status = fixture
            .mcp_call(
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
        assert_eq!(status["result"]["isError"], json!(false));
        // `get_operation` reads the canonical `OpsAsyncOperation` row
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
        assert!(
            uploaded_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("Shared async operation proof"))
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
        assert!(
            filename_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("mcp-knowledge-upload.txt"))
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
        assert!(
            title_hit["excerpt"]
                .as_str()
                .is_some_and(|excerpt| excerpt.contains("Upload Proof"))
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
