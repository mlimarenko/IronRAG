#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/iam_token_support.rs"]
mod iam_token_support;

use anyhow::Context;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::{catalog_repository, content_repository},
    interfaces::http::router,
};

struct McpPermissionsFixture {
    state: AppState,
    workspace_id: Uuid,
    foreign_workspace_id: Uuid,
    foreign_library_id: Uuid,
    foreign_library_ref: String,
}

impl McpPermissionsFixture {
    async fn create(settings: Settings) -> anyhow::Result<Self> {
        let state = AppState::new(settings).await?;
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-permissions-{suffix}"),
            "MCP Permissions Test",
            None,
        )
        .await
        .context("failed to create mcp permissions workspace")?;
        let _library = catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("mcp-permissions-library-{suffix}"),
            "MCP Permissions Library",
            Some("mcp permissions test library"),
            None,
        )
        .await
        .context("failed to create mcp permissions library")?;

        let foreign_workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("mcp-permissions-foreign-{suffix}"),
            "MCP Permissions Foreign Test",
            None,
        )
        .await
        .context("failed to create foreign permissions workspace")?;
        let foreign_library = catalog_repository::create_library(
            &state.persistence.postgres,
            foreign_workspace.id,
            &format!("mcp-permissions-foreign-library-{suffix}"),
            "MCP Permissions Foreign Library",
            Some("mcp permissions foreign library"),
            None,
        )
        .await
        .context("failed to create foreign permissions library")?;

        Ok(Self {
            state,
            workspace_id: workspace.id,
            foreign_workspace_id: foreign_workspace.id,
            foreign_library_id: foreign_library.id,
            foreign_library_ref: format!("{}/{}", foreign_workspace.slug, foreign_library.slug),
        })
    }

    async fn cleanup(&self) -> anyhow::Result<()> {
        sqlx::query("delete from workspace where id = any($1)")
            .bind([self.workspace_id, self.foreign_workspace_id].as_slice())
            .execute(&self.state.persistence.postgres)
            .await
            .context("failed to delete permissions test workspaces")?;
        Ok(())
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn create_token(
        &self,
        workspace_id: Option<Uuid>,
        token_kind: &str,
        _scopes: &[&str],
        label: &str,
    ) -> anyhow::Result<String> {
        iam_token_support::mint_api_token(
            &self.state.persistence.postgres,
            workspace_id,
            token_kind,
            label,
            _scopes,
        )
        .await
        .map(|token| token.plaintext)
        .with_context(|| format!("failed to create mcp permissions token for {label}"))
    }

    async fn bearer_token(&self, scopes: &[&str], label: &str) -> anyhow::Result<String> {
        self.create_token(Some(self.workspace_id), "workspace", scopes, label).await
    }

    async fn instance_admin_token(&self, scopes: &[&str], label: &str) -> anyhow::Result<String> {
        self.create_token(None, "instance_admin", scopes, label).await
    }

    async fn mcp_capabilities(&self, token: &str) -> anyhow::Result<Value> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/mcp/capabilities")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("build mcp capabilities request"),
            )
            .await
            .context("MCP capabilities request failed")?;

        if response.status() != StatusCode::OK {
            anyhow::bail!("unexpected status {} for capabilities", response.status());
        }

        let bytes = response
            .into_body()
            .collect()
            .await
            .context("failed to collect capabilities body")?
            .to_bytes();
        serde_json::from_slice(&bytes).context("failed to decode capabilities json")
    }

    async fn mcp_tools_list(&self, token: &str) -> anyhow::Result<Value> {
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
                            "id": "permissions-tools-list",
                            "method": "tools/list",
                        })
                        .to_string(),
                    ))
                    .expect("build mcp tools/list request"),
            )
            .await
            .context("MCP tools/list request failed")?;

        if response.status() != StatusCode::OK {
            anyhow::bail!("unexpected status {} for tools/list", response.status());
        }

        let bytes = response
            .into_body()
            .collect()
            .await
            .context("failed to collect tools/list response body")?
            .to_bytes();
        serde_json::from_slice(&bytes).context("failed to decode tools/list response json")
    }

    async fn mcp_tool_call(
        &self,
        token: &str,
        tool_name: &str,
        arguments: Value,
    ) -> anyhow::Result<Value> {
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
                            "id": "permissions-test",
                            "method": "tools/call",
                            "params": {
                                "name": tool_name,
                                "arguments": arguments,
                            },
                        })
                        .to_string(),
                    ))
                    .expect("build mcp permissions request"),
            )
            .await
            .with_context(|| format!("MCP permissions tool call {tool_name} failed"))?;

        if response.status() != StatusCode::OK {
            anyhow::bail!("unexpected status {} for tool {tool_name}", response.status());
        }

        let bytes = response
            .into_body()
            .collect()
            .await
            .context("failed to collect permissions response body")?
            .to_bytes();
        serde_json::from_slice(&bytes).context("failed to decode mcp permissions response json")
    }

    async fn create_foreign_document(&self, external_key: &str) -> anyhow::Result<Uuid> {
        let document = content_repository::create_document(
            &self.state.persistence.postgres,
            &content_repository::NewContentDocument {
                workspace_id: self.workspace_id,
                library_id: self.foreign_library_id,
                external_key,
                document_state: "active",
                created_by_principal_id: None,
            },
        )
        .await
        .with_context(|| format!("failed to create foreign document {external_key}"))?;
        let revision = content_repository::create_revision(
            &self.state.persistence.postgres,
            &content_repository::NewContentRevision {
                document_id: document.id,
                workspace_id: self.workspace_id,
                library_id: self.foreign_library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "initial_upload",
                checksum: "mcp-permissions-revision",
                mime_type: "text/plain",
                byte_size: 64,
                title: Some(external_key),
                language_code: None,
                source_uri: Some(&format!("{external_key}.txt")),
                storage_key: None,
                created_by_principal_id: None,
            },
        )
        .await
        .with_context(|| format!("failed to create foreign revision for {external_key}"))?;
        content_repository::upsert_document_head(
            &self.state.persistence.postgres,
            &content_repository::NewContentDocumentHead {
                document_id: document.id,
                active_revision_id: Some(revision.id),
                readable_revision_id: Some(revision.id),
                latest_mutation_id: None,
                latest_successful_attempt_id: None,
            },
        )
        .await
        .context("failed to upsert foreign document head")?;
        Ok(document.id)
    }
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn workspace_scoped_tokens_only_see_their_workspace_in_capabilities_and_discovery()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp permissions test")?;
    let fixture = McpPermissionsFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "workspace-discovery").await?;

        let capabilities = fixture.mcp_capabilities(&token).await?;
        assert_eq!(capabilities["workspaceScope"], json!(fixture.workspace_id));
        assert_eq!(capabilities["visibleWorkspaceCount"], json!(1));
        assert_eq!(capabilities["visibleLibraryCount"], json!(1));

        let workspaces = fixture.mcp_tool_call(&token, "list_workspaces", json!({})).await?;
        let workspace_items = workspaces["result"]["structuredContent"]["workspaces"]
            .as_array()
            .context("workspaces payload must be an array")?;
        assert_eq!(workspace_items.len(), 1);
        assert_eq!(workspace_items[0]["workspaceId"], json!(fixture.workspace_id));
        assert_ne!(workspace_items[0]["workspaceId"], json!(fixture.foreign_workspace_id));

        let libraries = fixture.mcp_tool_call(&token, "list_libraries", json!({})).await?;
        let library_items = libraries["result"]["structuredContent"]["libraries"]
            .as_array()
            .context("libraries payload must be an array")?;
        assert_eq!(library_items.len(), 1);
        assert_eq!(library_items[0]["workspaceId"], json!(fixture.workspace_id));
        assert_ne!(library_items[0]["libraryId"], json!(fixture.foreign_library_id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn read_only_tokens_do_not_receive_writable_tool_descriptors() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp permissions test")?;
    let fixture = McpPermissionsFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "readonly-tools").await?;
        let tools = fixture.mcp_tools_list(&token).await?;
        let tool_names = tools["result"]["tools"]
            .as_array()
            .context("tools/list result must be an array")?
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect::<Vec<_>>();

        assert!(tool_names.contains(&"list_workspaces"));
        assert!(tool_names.contains(&"list_libraries"));
        assert!(tool_names.contains(&"search_documents"));
        assert!(tool_names.contains(&"read_document"));
        assert!(tool_names.contains(&"get_runtime_execution"));
        assert!(tool_names.contains(&"get_runtime_execution_trace"));
        assert!(!tool_names.contains(&"create_workspace"));
        assert!(!tool_names.contains(&"create_library"));
        assert!(!tool_names.contains(&"upload_documents"));
        assert!(!tool_names.contains(&"update_document"));
        assert!(!tool_names.contains(&"delete_document"));
        assert!(!tool_names.contains(&"get_mutation_status"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn instance_admin_tokens_can_discover_all_visible_workspaces_and_libraries()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp permissions test")?;
    let fixture = McpPermissionsFixture::create(settings).await?;

    let result = async {
        let token = fixture
            .instance_admin_token(
                &["workspace:admin", "projects:write", "documents:read"],
                "instance-admin-discovery",
            )
            .await?;

        let capabilities = fixture.mcp_capabilities(&token).await?;
        assert!(capabilities["workspaceScope"].is_null());
        assert_eq!(capabilities["visibleWorkspaceCount"], json!(2));
        assert_eq!(capabilities["visibleLibraryCount"], json!(2));

        let tools = fixture.mcp_tools_list(&token).await?;
        let tool_names = tools["result"]["tools"]
            .as_array()
            .context("tools/list result must be an array")?
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect::<Vec<_>>();
        assert!(tool_names.contains(&"create_workspace"));
        assert!(tool_names.contains(&"create_library"));

        let workspaces = fixture.mcp_tool_call(&token, "list_workspaces", json!({})).await?;
        let workspace_items = workspaces["result"]["structuredContent"]["workspaces"]
            .as_array()
            .context("workspaces payload must be an array")?;
        assert_eq!(workspace_items.len(), 2);
        assert!(
            workspace_items.iter().any(|item| item["workspaceId"] == json!(fixture.workspace_id))
        );
        assert!(
            workspace_items
                .iter()
                .any(|item| item["workspaceId"] == json!(fixture.foreign_workspace_id))
        );

        let libraries = fixture.mcp_tool_call(&token, "list_libraries", json!({})).await?;
        let library_items = libraries["result"]["structuredContent"]["libraries"]
            .as_array()
            .context("libraries payload must be an array")?;
        assert_eq!(library_items.len(), 2);
        assert!(
            library_items.iter().any(|item| item["workspaceId"] == json!(fixture.workspace_id))
        );
        assert!(
            library_items
                .iter()
                .any(|item| item["workspaceId"] == json!(fixture.foreign_workspace_id))
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn foreign_scope_rejections_do_not_leak_library_or_document_metadata() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp permissions test")?;
    let fixture = McpPermissionsFixture::create(settings).await?;

    let result = async {
        let token = fixture.bearer_token(&["documents:read"], "permissions-read").await?;
        let foreign_document_id = fixture.create_foreign_document("foreign-secret-memory").await?;

        let search = fixture
            .mcp_tool_call(
                &token,
                "search_documents",
                json!({
                    "query": "secret",
                    "libraries": [fixture.foreign_library_ref.clone()]
                }),
            )
            .await?;
        let search_body =
            serde_json::to_string(&search).context("failed to stringify search body")?;
        assert_eq!(search["result"]["isError"], json!(true));
        assert_eq!(search["result"]["structuredContent"]["errorKind"], json!("unauthorized"));
        assert!(!search_body.contains(&fixture.foreign_workspace_id.to_string()));
        assert!(!search_body.contains(&fixture.foreign_library_id.to_string()));

        let read = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": foreign_document_id, "mode": "full" }),
            )
            .await?;
        let read_body = serde_json::to_string(&read).context("failed to stringify read body")?;
        assert_eq!(read["result"]["isError"], json!(true));
        assert_eq!(read["result"]["structuredContent"]["errorKind"], json!("unauthorized"));
        assert!(!read_body.contains(&fixture.foreign_workspace_id.to_string()));
        assert!(!read_body.contains(&fixture.foreign_library_id.to_string()));
        assert!(!read_body.contains(&foreign_document_id.to_string()));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango"]
async fn mcp_tool_visibility_matches_token_scope() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp permissions test")?;
    let fixture = McpPermissionsFixture::create(settings).await?;

    let result = async {
        let token =
            fixture.bearer_token(&["documents:read", "documents:write"], "tool-visibility").await?;
        let tools = fixture.mcp_tools_list(&token).await?;
        let tool_names = tools["result"]["tools"]
            .as_array()
            .context("tools/list result must be an array")?
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect::<Vec<_>>();

        for expected in [
            "search_documents",
            "upload_documents",
            "update_document",
            "delete_document",
            "get_mutation_status",
            "read_document",
        ] {
            assert!(tool_names.contains(&expected));
        }

        assert!(!tool_names.contains(&"create_workspace"));
        assert!(!tool_names.contains(&"create_library"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
