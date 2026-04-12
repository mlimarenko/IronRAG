#![allow(clippy::unwrap_used, clippy::expect_used)]

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::time::{Duration, sleep};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::iam_repository,
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
        sqlx::query(&format!("drop database if exists \"{database_name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {database_name}"))?;
        sqlx::query(&format!("create database \"{database_name}\""))
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
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct TempArangoDatabase {
    base_url: String,
    username: String,
    password: String,
    name: String,
    http: reqwest::Client,
}

impl TempArangoDatabase {
    async fn create(settings: &Settings) -> Result<Self> {
        let base_url = settings.arangodb_url.trim().trim_end_matches('/').to_string();
        let name = format!("mcp_knowledge_{}", Uuid::now_v7().simple());
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                settings.arangodb_request_timeout_seconds.max(1),
            ))
            .build()
            .context("failed to build ArangoDB admin http client for MCP knowledge test")?;
        let response = http
            .post(format!("{base_url}/_api/database"))
            .basic_auth(&settings.arangodb_username, Some(&settings.arangodb_password))
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .context("failed to create temp ArangoDB database for mcp_knowledge")?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "failed to create temp ArangoDB database {}: status {}",
                name,
                response.status()
            ));
        }

        Ok(Self {
            base_url,
            username: settings.arangodb_username.clone(),
            password: settings.arangodb_password.clone(),
            name,
            http,
        })
    }

    async fn drop(self) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/_api/database/{}", self.base_url, self.name))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to drop temp ArangoDB database for mcp_knowledge")?;
        if response.status() != reqwest::StatusCode::NOT_FOUND && !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "failed to drop temp ArangoDB database {}: status {}",
                self.name,
                response.status()
            ));
        }
        Ok(())
    }
}

struct McpKnowledgeFixture {
    state: AppState,
    temp_database: TempDatabase,
    temp_arango: TempArangoDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
}

impl McpKnowledgeFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for mcp knowledge test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        let temp_arango = TempArangoDatabase::create(&settings).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.arangodb_database = temp_arango.name.clone();
        settings.destructive_fresh_bootstrap_required = true;
        settings.arangodb_bootstrap_vector_indexes = false;

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
            temp_arango,
            workspace_id: workspace.id,
            library_id: library.id,
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_arango.drop().await?;
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
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/mcp")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "id": format!("mcp-knowledge-{}", method.replace('/', "-")),
                            "method": method,
                            "params": params,
                        })
                        .to_string(),
                    ))
                    .expect("build mcp knowledge request"),
            )
            .await
            .with_context(|| format!("MCP method {method} failed"))?;
        if response.status() != StatusCode::OK && response.status() != StatusCode::ACCEPTED {
            anyhow::bail!("unexpected status {} for MCP method {method}", response.status());
        }
        response_json(response).await
    }

    async fn tools_list(&self, token: &str) -> Result<Vec<String>> {
        let response = self.mcp_call(token, "tools/list", json!({})).await?;
        tool_names(&response)
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

async fn response_json(response: axum::response::Response) -> Result<Value> {
    let bytes =
        response.into_body().collect().await.context("failed to collect response body")?.to_bytes();
    serde_json::from_slice(&bytes).context("failed to decode response json")
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
#[ignore = "requires local postgres, redis, and arango services"]
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
        assert!(read_tools.contains(&"get_web_ingest_run".to_string()));
        assert!(read_tools.contains(&"list_web_ingest_run_pages".to_string()));
        assert!(!read_tools.contains(&"create_workspace".to_string()));
        assert!(!read_tools.contains(&"create_library".to_string()));
        assert!(!read_tools.contains(&"upload_documents".to_string()));
        assert!(!read_tools.contains(&"update_document".to_string()));
        assert!(!read_tools.contains(&"delete_document".to_string()));
        assert!(!read_tools.contains(&"get_mutation_status".to_string()));
        assert!(!read_tools.contains(&"submit_web_ingest_run".to_string()));
        assert!(!read_tools.contains(&"cancel_web_ingest_run".to_string()));

        let write_tools = fixture.tools_list(&write_token).await?;
        assert!(write_tools.contains(&"list_workspaces".to_string()));
        assert!(write_tools.contains(&"list_libraries".to_string()));
        assert!(write_tools.contains(&"search_documents".to_string()));
        assert!(write_tools.contains(&"read_document".to_string()));
        assert!(write_tools.contains(&"upload_documents".to_string()));
        assert!(write_tools.contains(&"update_document".to_string()));
        assert!(write_tools.contains(&"delete_document".to_string()));
        assert!(write_tools.contains(&"get_mutation_status".to_string()));
        assert!(write_tools.contains(&"get_runtime_execution".to_string()));
        assert!(write_tools.contains(&"get_runtime_execution_trace".to_string()));
        assert!(write_tools.contains(&"submit_web_ingest_run".to_string()));
        assert!(write_tools.contains(&"get_web_ingest_run".to_string()));
        assert!(write_tools.contains(&"list_web_ingest_run_pages".to_string()));
        assert!(write_tools.contains(&"cancel_web_ingest_run".to_string()));
        assert!(!write_tools.contains(&"create_workspace".to_string()));
        assert!(!write_tools.contains(&"create_library".to_string()));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
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
                    "name": "upload_documents",
                    "arguments": {
                        "libraryId": fixture.library_id,
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
        let receipt_id: Uuid =
            serde_json::from_value(receipt["receiptId"].clone()).context("missing receipt id")?;

        let status = fixture
            .mcp_call(
                &token,
                "tools/call",
                json!({
                    "name": "get_mutation_status",
                    "arguments": {
                        "receiptId": receipt_id,
                    },
                }),
            )
            .await?;
        assert_eq!(status["result"]["isError"], json!(false));
        assert_eq!(status["result"]["structuredContent"]["receiptId"], json!(receipt_id));
        assert_eq!(
            status["result"]["structuredContent"]["documentId"],
            json!(receipt_document_id)
        );
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
                            "libraryIds": [fixture.library_id],
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

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
