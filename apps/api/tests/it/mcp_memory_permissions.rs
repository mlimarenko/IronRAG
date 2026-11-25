use crate::{iam_token_support, mcp_tool_call_support, temp_postgres_database};

use anyhow::Context;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;

use temp_postgres_database::TempPostgresDatabase;

use ironrag_backend::{
    app::{config::Settings, seed_clean_v1_ai_catalog, state::AppState},
    infra::repositories::{
        catalog_repository, content_document_repository, content_revision_repository,
        iam_repository,
    },
    interfaces::http::{auth::hash_token, router},
    shared::auth_tokens::mint_plaintext_api_token,
};

struct McpPermissionsFixture {
    state: AppState,
    temp_database: TempPostgresDatabase,
    workspace_id: Uuid,
    foreign_workspace_id: Uuid,
    foreign_library_id: Uuid,
    foreign_library_ref: String,
}

impl McpPermissionsFixture {
    async fn create(mut settings: Settings) -> anyhow::Result<Self> {
        settings.credential_master_key =
            Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
        // This suite used to run straight against the configured database and
        // never migrated it, so it only worked when an earlier suite had
        // already created the schema — an ordering dependency the Makefile
        // never promised.
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, "mcp_memory_permissions").await?;
        settings.database_url = temp_database.database_url.clone();
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect mcp_memory_permissions postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply mcp_memory_permissions migrations")?;
        seed_clean_v1_ai_catalog(&postgres)
            .await
            .context("failed to seed mcp_memory_permissions AI catalog")?;
        postgres.close().await;
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
            temp_database,
            workspace_id: workspace.id,
            foreign_workspace_id: foreign_workspace.id,
            foreign_library_id: foreign_library.id,
            foreign_library_ref: format!("{}/{}", foreign_workspace.slug, foreign_library.slug),
        })
    }

    async fn cleanup(self) -> anyhow::Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
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

    async fn admin_session_cookie(&self, label: &str) -> anyhow::Result<String> {
        mcp_tool_call_support::create_admin_session_cookie(
            &self.state,
            label,
            "MCP Permissions Administrator",
        )
        .await
    }

    async fn mcp_capabilities(&self, token: &str) -> anyhow::Result<Value> {
        self.mcp_capabilities_at(token, "/v1/mcp/capabilities").await
    }

    async fn mcp_diagnostics_capabilities_with_cookie(
        &self,
        cookie: &str,
    ) -> anyhow::Result<Value> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/mcp/diagnostics/capabilities")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .context("failed to build MCP diagnostics capabilities request")?,
            )
            .await
            .context("MCP diagnostics capabilities request failed")?;

        if response.status() != StatusCode::OK {
            anyhow::bail!("unexpected status {} for diagnostics capabilities", response.status());
        }

        let bytes = response
            .into_body()
            .collect()
            .await
            .context("failed to collect diagnostics capabilities body")?
            .to_bytes();
        serde_json::from_slice(&bytes).context("failed to decode diagnostics capabilities json")
    }

    async fn mcp_capabilities_at(&self, token: &str, uri: &str) -> anyhow::Result<Value> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .context("failed to build MCP capabilities request")?,
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
        self.mcp_tools_list_at(token, "/v1/mcp").await
    }

    async fn mcp_diagnostics_tools_list(&self, token: &str) -> anyhow::Result<Value> {
        self.mcp_tools_list_at(token, "/v1/mcp/diagnostics").await
    }

    async fn mcp_tools_list_with_cookie(&self, cookie: &str, uri: &str) -> anyhow::Result<Value> {
        mcp_tool_call_support::call_rpc_with_cookie(
            self.app(),
            uri,
            cookie,
            "permissions-tools-list-cookie",
            "tools/list",
            json!({}),
        )
        .await
    }

    async fn mcp_tools_list_at(&self, token: &str, uri: &str) -> anyhow::Result<Value> {
        mcp_tool_call_support::call_rpc(
            self.app(),
            uri,
            token,
            "permissions-tools-list",
            "tools/list",
            json!({}),
        )
        .await
    }

    async fn mcp_tool_call(
        &self,
        token: &str,
        tool_name: &str,
        arguments: Value,
    ) -> anyhow::Result<Value> {
        self.mcp_tool_call_at(token, "/v1/mcp", tool_name, arguments).await
    }

    async fn mcp_tool_call_with_cookie(
        &self,
        cookie: &str,
        uri: &str,
        tool_name: &str,
        arguments: Value,
    ) -> anyhow::Result<Value> {
        mcp_tool_call_support::call_tool_with_cookie(
            self.app(),
            uri,
            cookie,
            "permissions-test-cookie",
            tool_name,
            arguments,
        )
        .await
    }

    async fn mcp_tool_call_at(
        &self,
        token: &str,
        uri: &str,
        tool_name: &str,
        arguments: Value,
    ) -> anyhow::Result<Value> {
        mcp_tool_call_support::call_tool(
            self.app(),
            uri,
            token,
            "permissions-test",
            tool_name,
            arguments,
        )
        .await
    }

    async fn create_foreign_document(&self, external_key: &str) -> anyhow::Result<Uuid> {
        let document = content_document_repository::create_document(
            &self.state.persistence.postgres,
            &content_document_repository::NewContentDocument {
                workspace_id: self.foreign_workspace_id,
                library_id: self.foreign_library_id,
                external_key,
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await
        .with_context(|| format!("failed to create foreign document {external_key}"))?;
        let revision = crate::content_repository::create_imported_fixture_revision(
            &self.state.persistence.postgres,
            &content_revision_repository::NewContentRevision {
                admission_identity: None,
                document_id: document.id,
                workspace_id: self.foreign_workspace_id,
                library_id: self.foreign_library_id,
                revision_number: 1,
                parent_revision_id: None,
                content_source_kind: "upload",
                checksum: &format!("sha256:{}", "8".repeat(64)),
                mime_type: "text/plain",
                byte_size: 64,
                title: Some(external_key),
                language_code: None,
                source_uri: Some(&format!("{external_key}.txt")),
                document_hint: None,
                storage_key: None,
                created_by_principal_id: None,
            },
            &format!("{external_key}.txt"),
        )
        .await
        .with_context(|| format!("failed to create foreign revision for {external_key}"))?;
        crate::content_lifecycle_support::seed_readable_revision_projection(
            &self.state.persistence.postgres,
            document.id,
            revision.id,
            &format!("{external_key}.txt"),
        )
        .await
        .context("failed to upsert foreign document head")?;
        Ok(document.id)
    }
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn workspace_scoped_tokens_only_see_their_workspace_in_capabilities_and_discovery()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp permissions test")?;
    let fixture = Box::pin(McpPermissionsFixture::create(settings)).await?;

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
#[ignore = "requires local postgres and redis services"]
async fn read_only_tokens_do_not_receive_writable_tool_descriptors() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp permissions test")?;
    let fixture = Box::pin(McpPermissionsFixture::create(settings)).await?;

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
        assert!(!tool_names.contains(&"create_documents"));
        assert!(!tool_names.contains(&"create_document_revision"));
        assert!(!tool_names.contains(&"delete_document"));
        assert!(!tool_names.contains(&"get_operation"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn system_admin_sessions_can_discover_all_visible_workspaces_and_libraries()
-> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp permissions test")?;
    let fixture = Box::pin(McpPermissionsFixture::create(settings)).await?;

    let result = async {
        let admin_cookie = fixture.admin_session_cookie("system-admin-discovery").await?;

        let capabilities = fixture.mcp_diagnostics_capabilities_with_cookie(&admin_cookie).await?;
        assert!(capabilities["workspaceScope"].is_null());
        assert!(
            capabilities["visibleWorkspaceCount"].as_u64().unwrap_or_default() >= 2,
            "system-admin capabilities must include both fixture workspaces"
        );
        assert!(
            capabilities["visibleLibraryCount"].as_u64().unwrap_or_default() >= 2,
            "system-admin capabilities must include both fixture libraries"
        );

        let tools =
            fixture.mcp_tools_list_with_cookie(&admin_cookie, "/v1/mcp/diagnostics").await?;
        let tool_names = tools["result"]["tools"]
            .as_array()
            .context("tools/list result must be an array")?
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect::<Vec<_>>();
        assert!(tool_names.contains(&"create_workspace"));
        assert!(tool_names.contains(&"create_library"));

        let public_tools = fixture.mcp_tools_list_with_cookie(&admin_cookie, "/v1/mcp").await?;
        let public_tool_names = public_tools["result"]["tools"]
            .as_array()
            .context("public tools/list result must be an array")?
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect::<Vec<_>>();
        assert!(!public_tool_names.contains(&"create_workspace"));
        assert!(!public_tool_names.contains(&"create_library"));

        let public_create = fixture
            .mcp_tool_call_with_cookie(
                &admin_cookie,
                "/v1/mcp",
                "create_workspace",
                json!({ "workspace": format!("public-admin-{}", Uuid::now_v7().simple()) }),
            )
            .await?;
        assert_eq!(public_create["result"]["isError"], json!(true));
        assert_eq!(
            public_create["result"]["structuredContent"]["errorKind"],
            json!("invalid_mcp_tool_call")
        );

        let workspaces = fixture
            .mcp_tool_call_with_cookie(
                &admin_cookie,
                "/v1/mcp/diagnostics",
                "list_workspaces",
                json!({}),
            )
            .await?;
        let workspace_items = workspaces["result"]["structuredContent"]["workspaces"]
            .as_array()
            .context("workspaces payload must be an array")?;
        assert!(
            workspace_items.iter().any(|item| item["workspaceId"] == json!(fixture.workspace_id))
        );
        assert!(
            workspace_items
                .iter()
                .any(|item| item["workspaceId"] == json!(fixture.foreign_workspace_id))
        );

        let libraries = fixture
            .mcp_tool_call_with_cookie(
                &admin_cookie,
                "/v1/mcp/diagnostics",
                "list_libraries",
                json!({}),
            )
            .await?;
        let library_items = libraries["result"]["structuredContent"]["libraries"]
            .as_array()
            .context("libraries payload must be an array")?;
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
#[ignore = "requires local postgres and redis services"]
async fn foreign_scope_rejections_do_not_leak_library_or_document_metadata() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp permissions test")?;
    let fixture = Box::pin(McpPermissionsFixture::create(settings)).await?;

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
        let missing = fixture
            .mcp_tool_call(
                &token,
                "read_document",
                json!({ "documentId": Uuid::now_v7(), "mode": "full" }),
            )
            .await?;
        assert_eq!(read["result"]["isError"], json!(true));
        assert_eq!(read["result"]["structuredContent"]["errorKind"], json!("not_found"));
        assert_eq!(read["result"]["isError"], missing["result"]["isError"]);
        assert_eq!(read["result"]["structuredContent"], missing["result"]["structuredContent"]);
        assert!(!read_body.contains(&foreign_document_id.to_string()));
        assert!(!read_body.contains(&fixture.foreign_workspace_id.to_string()));
        assert!(!read_body.contains(&fixture.foreign_library_id.to_string()));
        assert!(!read_body.contains("foreign-secret-memory"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn catalog_counts_and_pages_respect_document_grants() -> anyhow::Result<()> {
    let settings = Settings::from_env().context("failed to load settings")?;
    let fixture = Box::pin(McpPermissionsFixture::create(settings)).await?;
    let result = async {
        let readable = fixture.create_foreign_document("permitted-readable").await?;
        let mut empty_ids = Vec::new();
        for key in ["permitted-empty", "hidden-catalog-title"] {
            let doc = content_document_repository::create_document(
                &fixture.state.persistence.postgres,
                &content_document_repository::NewContentDocument {
                    workspace_id: fixture.foreign_workspace_id,
                    library_id: fixture.foreign_library_id,
                    external_key: key,
                    document_state: "active",
                    created_by_principal_id: None,
                    parent_external_key: None,
                    parent_document_id: None,
                    document_role: "primary",
                },
            )
            .await?;
            empty_ids.push(doc.id);
        }

        let pool = &fixture.state.persistence.postgres;
        let plaintext = mint_plaintext_api_token();
        let token = iam_repository::create_api_token(
            pool,
            Some(fixture.foreign_workspace_id),
            "document-catalog",
            "test-token",
            None,
            None,
        )
        .await?;
        iam_repository::create_api_token_secret(pool, token.principal_id, &hash_token(&plaintext))
            .await?;
        iam_token_support::create_principal_grants(
            pool,
            token.principal_id,
            "document-catalog",
            &[readable, empty_ids[0]].map(|id| iam_token_support::GrantSpec {
                resource_kind: "document",
                resource_id: id,
                permission_kind: "document_read".to_string(),
            }),
        )
        .await?;
        let broad = fixture
            .create_token(
                Some(fixture.foreign_workspace_id),
                "workspace",
                &["documents:read"],
                "broad-catalog",
            )
            .await?;
        let denied = fixture.bearer_token(&["documents:read"], "denied-catalog").await?;

        for (token, expected, readable_count) in [(&plaintext, 2, 1), (&broad, 3, 1)] {
            let response = fixture.mcp_tool_call(token, "list_libraries", json!({})).await?;
            let libraries = response["result"]["structuredContent"]["libraries"]
                .as_array()
                .context("library list")?;
            let library = libraries
                .iter()
                .find(|item| item["libraryId"] == json!(fixture.foreign_library_id))
                .context("authorized library missing")?;
            assert_eq!(library["documentCount"], json!(expected));
            assert_eq!(library["readableDocumentCount"], json!(readable_count));
            let buckets =
                library["documentCountsByStatus"].as_object().context("status buckets")?;
            assert_eq!(buckets.values().map(|v| v.as_u64().unwrap()).sum::<u64>(), expected);

            let mut ids = std::collections::BTreeSet::new();
            let mut continuation = Value::Null;
            loop {
                let mut arguments = json!({
                    "library": fixture.foreign_library_ref,
                    "limit": 1,
                });
                if !continuation.is_null() {
                    arguments["continuationToken"] = continuation.clone();
                }
                let response = fixture
                    .mcp_tool_call_at(token, "/v1/mcp/diagnostics", "list_documents", arguments)
                    .await?;
                anyhow::ensure!(
                    response["result"]["isError"] != json!(true),
                    "document inventory failed: {} ({})",
                    response["result"]["structuredContent"]["errorKind"],
                    response["result"]["structuredContent"]["message"],
                );
                if expected == 2 {
                    let body = serde_json::to_string(&response)?;
                    assert!(!body.contains(&empty_ids[1].to_string()));
                    assert!(!body.contains("hidden-catalog-title"));
                }
                let page = &response["result"]["structuredContent"];
                let documents = page["documents"].as_array().context("document page")?;
                assert_eq!(page["totalCount"], json!(expected));
                assert_eq!(page["count"], json!(documents.len()));
                for document in documents {
                    assert!(document["status"].is_string());
                    assert!(document["readinessKind"].is_string());
                    assert!(
                        ids.insert(
                            document["documentId"].as_str().context("document id")?.to_owned()
                        ),
                        "duplicate document"
                    );
                }
                if page["hasMore"] == json!(false) {
                    break;
                }
                assert_eq!(page["hasMore"], json!(true));
                continuation = page["continuationToken"].clone();
                assert!(continuation.is_string());
                assert!(ids.len() < 4, "pagination did not terminate");
            }
            assert_eq!(ids.len(), expected as usize);
            assert!(ids.contains(&readable.to_string()));
            assert!(ids.contains(&empty_ids[0].to_string()));
            assert_eq!(ids.contains(&empty_ids[1].to_string()), expected == 3);

            let response = fixture
                .mcp_tool_call_at(
                    token,
                    "/v1/mcp/diagnostics",
                    "list_documents",
                    json!({
                        "library": fixture.foreign_library_ref, "statusFilter": "empty", "limit": 10
                    }),
                )
                .await?;
            let page = &response["result"]["structuredContent"];
            // The pill's total is the filtered set it returns: every authorized
            // document except the single readable one is empty.
            assert_eq!(page["totalCount"], json!(expected - 1));
            assert_eq!(page["count"], json!(expected - 1));
            assert_eq!(page["hasMore"], json!(false));
            assert!(
                page["documents"]
                    .as_array()
                    .context("empty documents")?
                    .iter()
                    .all(|doc| doc["status"] == "empty")
            );
            let response = fixture
                .mcp_tool_call_at(
                    token,
                    "/v1/mcp/diagnostics",
                    "list_documents",
                    json!({
                        "library": fixture.foreign_library_ref, "readableOnly": true
                    }),
                )
                .await?;
            let page = &response["result"]["structuredContent"];
            assert_eq!(page["totalCount"], json!(1));
            assert_eq!(page["count"], json!(1));
            assert_eq!(page["hasMore"], json!(false));
            assert_eq!(page["documents"][0]["documentId"], json!(readable));
        }

        let broad_first = fixture
            .mcp_tool_call_at(
                &broad,
                "/v1/mcp/diagnostics",
                "list_documents",
                json!({ "library": fixture.foreign_library_ref, "limit": 1 }),
            )
            .await?;
        let broad_page = &broad_first["result"]["structuredContent"];
        assert_eq!(broad_page["hasMore"], json!(true));
        let broad_cursor = broad_page["continuationToken"].as_str().context("broad cursor")?;

        let scoped_first = fixture
            .mcp_tool_call_at(
                &plaintext,
                "/v1/mcp/diagnostics",
                "list_documents",
                json!({ "library": fixture.foreign_library_ref, "limit": 1 }),
            )
            .await?;
        let scoped_page = &scoped_first["result"]["structuredContent"];
        assert_eq!(scoped_page["hasMore"], json!(true));
        let scoped_cursor = scoped_page["continuationToken"].as_str().context("scoped cursor")?;
        let first_id = scoped_page["documents"][0]["documentId"].as_str().context("first id")?;

        for (token, args) in [
            (&plaintext, json!({ "library": fixture.foreign_library_ref, "limit": 1, "continuationToken": broad_cursor })),
            (&broad, json!({ "library": fixture.foreign_library_ref, "limit": 1, "continuationToken": broad_cursor, "statusFilter": "empty" })),
        ] {
            let response = fixture
                .mcp_tool_call_at(token, "/v1/mcp/diagnostics", "list_documents", args)
                .await?;
            assert_eq!(response["result"]["isError"], json!(true));
            assert_eq!(
                response["result"]["structuredContent"]["errorKind"],
                json!("invalid_continuation_token")
            );
        }

        let added = content_document_repository::create_document(
            pool,
            &content_document_repository::NewContentDocument {
                workspace_id: fixture.foreign_workspace_id,
                library_id: fixture.foreign_library_id,
                external_key: "added-hidden-catalog-title",
                document_state: "active",
                created_by_principal_id: None,
                parent_external_key: None,
                parent_document_id: None,
                document_role: "primary",
            },
        )
        .await?;
        let changed = fixture
            .mcp_tool_call_at(
                &broad,
                "/v1/mcp/diagnostics",
                "list_documents",
                json!({
                    "library": fixture.foreign_library_ref,
                    "limit": 1,
                    "continuationToken": broad_cursor
                }),
            )
            .await?;
        assert_eq!(changed["result"]["isError"], json!(true));
        assert_eq!(
            changed["result"]["structuredContent"]["errorKind"],
            json!("knowledge_not_ready")
        );

        let scoped_second = fixture
            .mcp_tool_call_at(
                &plaintext,
                "/v1/mcp/diagnostics",
                "list_documents",
                json!({
                    "library": fixture.foreign_library_ref,
                    "limit": 1,
                    "continuationToken": scoped_cursor
                }),
            )
            .await?;
        let page = &scoped_second["result"]["structuredContent"];
        assert_eq!(page["totalCount"], json!(2));
        assert_eq!(page["count"], json!(1));
        assert_eq!(page["hasMore"], json!(false));
        let second_id = page["documents"][0]["documentId"].as_str().context("second id")?;
        assert_ne!(first_id, second_id);
        let ids = [first_id.to_owned(), second_id.to_owned()]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids, [readable.to_string(), empty_ids[0].to_string()].into_iter().collect());
        let scoped_body = serde_json::to_string(&scoped_second)?;
        assert!(!scoped_body.contains(&added.id.to_string()));
        assert!(!scoped_body.contains("added-hidden-catalog-title"));

        let denied_libraries = fixture.mcp_tool_call(&denied, "list_libraries", json!({})).await?;
        let denied_listing = fixture
            .mcp_tool_call_at(
                &denied,
                "/v1/mcp/diagnostics",
                "list_documents",
                json!({
                    "library": fixture.foreign_library_ref
                }),
            )
            .await?;
        assert_eq!(denied_listing["result"]["isError"], json!(true));
        for response in [&denied_libraries, &denied_listing] {
            let body = serde_json::to_string(response)?;
            for secret in [
                readable.to_string(),
                empty_ids[0].to_string(),
                empty_ids[1].to_string(),
                "hidden-catalog-title".to_owned(),
            ] {
                assert!(!body.contains(&secret));
            }
            assert!(!body.contains(&fixture.foreign_library_id.to_string()));
        }
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn mcp_tool_visibility_matches_token_scope() -> anyhow::Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for mcp permissions test")?;
    let fixture = Box::pin(McpPermissionsFixture::create(settings)).await?;

    let result = async {
        let token =
            fixture.bearer_token(&["documents:read", "documents:write"], "tool-visibility").await?;
        let tools = fixture.mcp_diagnostics_tools_list(&token).await?;
        let tool_names = tools["result"]["tools"]
            .as_array()
            .context("tools/list result must be an array")?
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect::<Vec<_>>();

        for expected in [
            "search_documents",
            "create_documents",
            "create_document_revision",
            "delete_document",
            "get_operation",
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
