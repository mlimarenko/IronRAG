use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode, header},
    routing::{get, post},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        arangodb::client::ArangoClient,
        persistence::Persistence,
        repositories::{ai_repository, audit_repository, catalog_repository, iam_repository},
    },
    interfaces::http::{
        auth::{AuthContext, hash_token},
        authorization::{
            POLICY_DOCUMENTS_READ, POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ,
            POLICY_LIBRARY_WRITE, POLICY_WORKSPACE_ADMIN, load_content_document_and_authorize,
            load_library_and_authorize, load_workspace_and_authorize,
        },
        router,
    },
};

const TEST_TOKEN_PREFIX: &str = "governance";
const TEST_PROVIDER_CREDENTIAL_LABEL: &str = "governance-provider-credential";
const TEST_MODEL_PRESET_NAME: &str = "governance-model-preset";
const TEST_BINDING_PURPOSE: &str = "query_answer";

#[derive(Clone)]
struct GrantSpec {
    resource_kind: &'static str,
    resource_id: Uuid,
    permission_kind: String,
}

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("governance_auth_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect governance auth admin postgres")?;

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
            .context("failed to reconnect governance auth admin postgres for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct GovernanceAuthFixture {
    state: AppState,
    temp_database: TempDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
    sibling_library_id: Uuid,
    foreign_workspace_id: Uuid,
    foreign_library_id: Uuid,
    document_id: Uuid,
    sibling_document_id: Uuid,
    foreign_document_id: Uuid,
    provider_catalog_id: Uuid,
    model_catalog_id: Uuid,
}

impl GovernanceAuthFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for governance auth test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.bootstrap_token = Some("governance-auth-bootstrap".to_string());
        settings.bootstrap_claim_enabled = true;
        settings.legacy_ui_bootstrap_enabled = false;
        settings.legacy_bootstrap_token_endpoint_enabled = false;
        settings.destructive_fresh_bootstrap_required = true;
        settings.destructive_allow_legacy_startup_side_effects = false;

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect governance auth postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply governance auth migrations")?;

        let state = build_test_state(settings, postgres)?;
        let fixture = Self {
            state,
            temp_database,
            workspace_id: Uuid::nil(),
            library_id: Uuid::nil(),
            sibling_library_id: Uuid::nil(),
            foreign_workspace_id: Uuid::nil(),
            foreign_library_id: Uuid::nil(),
            document_id: Uuid::nil(),
            sibling_document_id: Uuid::nil(),
            foreign_document_id: Uuid::nil(),
            provider_catalog_id: Uuid::nil(),
            model_catalog_id: Uuid::nil(),
        };
        fixture.seed().await
    }

    async fn seed(mut self) -> Result<Self> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &self.state.persistence.postgres,
            &format!("governance-auth-workspace-{suffix}"),
            "Governance Auth Workspace",
            None,
        )
        .await
        .context("failed to create governance auth workspace")?;
        let library = catalog_repository::create_library(
            &self.state.persistence.postgres,
            workspace.id,
            &format!("governance-auth-library-{suffix}"),
            "Governance Auth Library",
            Some("governance auth test library"),
            None,
        )
        .await
        .context("failed to create governance auth library")?;
        let sibling_library = catalog_repository::create_library(
            &self.state.persistence.postgres,
            workspace.id,
            &format!("governance-auth-library-sibling-{suffix}"),
            "Governance Auth Sibling Library",
            Some("governance auth sibling library"),
            None,
        )
        .await
        .context("failed to create governance auth sibling library")?;

        let foreign_workspace = catalog_repository::create_workspace(
            &self.state.persistence.postgres,
            &format!("governance-auth-foreign-{suffix}"),
            "Governance Auth Foreign Workspace",
            None,
        )
        .await
        .context("failed to create governance auth foreign workspace")?;
        let foreign_library = catalog_repository::create_library(
            &self.state.persistence.postgres,
            foreign_workspace.id,
            &format!("governance-auth-foreign-library-{suffix}"),
            "Governance Auth Foreign Library",
            Some("governance auth foreign test library"),
            None,
        )
        .await
        .context("failed to create governance auth foreign library")?;
        let document_id = insert_content_document(
            self.pool(),
            workspace.id,
            library.id,
            &format!("governance-auth-doc-{suffix}"),
        )
        .await
        .context("failed to create governance auth primary document")?;
        let sibling_document_id = insert_content_document(
            self.pool(),
            workspace.id,
            sibling_library.id,
            &format!("governance-auth-doc-sibling-{suffix}"),
        )
        .await
        .context("failed to create governance auth sibling document")?;
        let foreign_document_id = insert_content_document(
            self.pool(),
            foreign_workspace.id,
            foreign_library.id,
            &format!("governance-auth-doc-foreign-{suffix}"),
        )
        .await
        .context("failed to create governance auth foreign document")?;

        let provider_catalog =
            ai_repository::list_provider_catalog(&self.state.persistence.postgres)
                .await
                .context("failed to load provider catalog for governance auth test")?
                .into_iter()
                .next()
                .context("expected seeded provider catalog to be present")?;
        let model_catalog = ai_repository::list_model_catalog(
            &self.state.persistence.postgres,
            Some(provider_catalog.id),
        )
        .await
        .context("failed to load model catalog for governance auth test")?
        .into_iter()
        .next()
        .context("expected seeded model catalog to be present")?;

        self.workspace_id = workspace.id;
        self.library_id = library.id;
        self.sibling_library_id = sibling_library.id;
        self.foreign_workspace_id = foreign_workspace.id;
        self.foreign_library_id = foreign_library.id;
        self.document_id = document_id;
        self.sibling_document_id = sibling_document_id;
        self.foreign_document_id = foreign_document_id;
        self.provider_catalog_id = provider_catalog.id;
        self.model_catalog_id = model_catalog.id;
        Ok(self)
    }

    fn app(&self) -> Router {
        Router::new()
            .nest("/v1", router())
            .route("/_test/authz/workspaces/{workspace_id}/admin", post(probe_workspace_admin))
            .route("/_test/authz/libraries/{library_id}/read", get(probe_library_read))
            .route("/_test/authz/libraries/{library_id}/write", post(probe_library_write))
            .route("/_test/authz/documents/{document_id}/read", get(probe_document_read))
            .route("/_test/authz/documents/{document_id}/write", post(probe_document_write))
            .with_state(self.state.clone())
    }

    fn pool(&self) -> &PgPool {
        &self.state.persistence.postgres
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }

    async fn mint_token_with_grants(
        &self,
        token_workspace_id: Option<Uuid>,
        label: &str,
        grants: &[GrantSpec],
    ) -> Result<String> {
        let plaintext = format!("{TEST_TOKEN_PREFIX}-{label}-{}", Uuid::now_v7());
        let token = iam_repository::create_api_token(
            self.pool(),
            token_workspace_id,
            label,
            "governance",
            None,
            None,
        )
        .await
        .with_context(|| format!("failed to create token {label}"))?;
        iam_repository::create_api_token_secret(
            self.pool(),
            token.principal_id,
            &hash_token(&plaintext),
        )
        .await
        .with_context(|| format!("failed to create token secret for {label}"))?;
        for grant in grants {
            iam_repository::create_grant(
                self.pool(),
                token.principal_id,
                grant.resource_kind,
                grant.resource_id,
                &grant.permission_kind,
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

    async fn mint_workspace_token(
        &self,
        workspace_id: Uuid,
        label: &str,
        permissions: &[&str],
    ) -> Result<String> {
        let grants = permissions
            .iter()
            .map(|permission| GrantSpec {
                resource_kind: "workspace",
                resource_id: workspace_id,
                permission_kind: (*permission).to_string(),
            })
            .collect::<Vec<_>>();
        self.mint_token_with_grants(Some(workspace_id), label, &grants).await
    }

    async fn mint_system_admin_token(&self, label: &str) -> Result<String> {
        self.mint_token_with_grants(
            None,
            label,
            &[GrantSpec {
                resource_kind: "system",
                resource_id: Uuid::nil(),
                permission_kind: "iam_admin".to_string(),
            }],
        )
        .await
    }

    async fn rest_get_optional(
        &self,
        token: Option<&str>,
        path: &str,
    ) -> Result<(StatusCode, Value)> {
        let mut request = Request::builder().method("GET").uri(path);
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = self
            .app()
            .oneshot(request.body(Body::empty()).expect("build governance auth GET request"))
            .await
            .with_context(|| format!("GET {path} failed"))?;
        let status = response.status();
        Ok((status, response_json(response).await?))
    }

    async fn rest_get_status_optional(
        &self,
        token: Option<&str>,
        path: &str,
    ) -> Result<StatusCode> {
        let mut request = Request::builder().method("GET").uri(path);
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = self
            .app()
            .oneshot(request.body(Body::empty()).expect("build governance auth GET status request"))
            .await
            .with_context(|| format!("GET {path} failed"))?;
        Ok(response.status())
    }

    async fn rest_get(&self, token: &str, path: &str) -> Result<(StatusCode, Value)> {
        self.rest_get_optional(Some(token), path).await
    }

    async fn rest_post(
        &self,
        token: &str,
        path: &str,
        payload: Value,
    ) -> Result<(StatusCode, Value)> {
        self.rest_post_optional(Some(token), path, payload).await
    }

    async fn rest_post_optional(
        &self,
        token: Option<&str>,
        path: &str,
        payload: Value,
    ) -> Result<(StatusCode, Value)> {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = self
            .app()
            .oneshot(
                request
                    .body(Body::from(payload.to_string()))
                    .expect("build governance auth POST request"),
            )
            .await
            .with_context(|| format!("POST {path} failed"))?;
        let status = response.status();
        Ok((status, response_json(response).await?))
    }

    async fn mcp_call(&self, token: &str, method: &str, params: Option<Value>) -> Result<Value> {
        let (status, json) = self
            .rest_post(
                token,
                "/v1/mcp",
                json!({
                    "jsonrpc": "2.0",
                    "id": format!("governance-{}", method.replace('/', "-")),
                    "method": method,
                    "params": params,
                }),
            )
            .await?;
        if status != StatusCode::OK && status != StatusCode::ACCEPTED {
            anyhow::bail!("unexpected status {status} for MCP {method}");
        }
        Ok(json)
    }

    async fn mcp_tools_list(&self, token: &str) -> Result<Value> {
        self.mcp_call(token, "tools/list", None).await
    }
}

fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let bootstrap_settings = settings.bootstrap_settings();
    let persistence = Persistence {
        postgres,
        redis: redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for governance auth test state")?,
    };
    let arango_client = Arc::new(ArangoClient::from_settings(&settings)?);

    Ok(AppState::from_dependencies(
        Settings {
            ui_bootstrap_admin_login: bootstrap_settings
                .legacy_ui_bootstrap_admin
                .as_ref()
                .map(|admin| admin.login.clone()),
            ui_bootstrap_admin_email: bootstrap_settings
                .legacy_ui_bootstrap_admin
                .as_ref()
                .map(|admin| admin.email.clone()),
            ui_bootstrap_admin_name: bootstrap_settings
                .legacy_ui_bootstrap_admin
                .as_ref()
                .map(|admin| admin.display_name.clone()),
            ui_bootstrap_admin_password: bootstrap_settings
                .legacy_ui_bootstrap_admin
                .as_ref()
                .map(|admin| admin.password.clone()),
            ..settings
        },
        persistence,
        arango_client,
    ))
}

fn replace_database_name(database_url: &str, new_database: &str) -> Result<String> {
    let (without_query, query_suffix) = database_url
        .split_once('?')
        .map_or((database_url, None), |(prefix, suffix)| (prefix, Some(suffix)));
    let slash_index = without_query
        .rfind('/')
        .with_context(|| format!("database url is missing database name: {database_url}"))?;
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

async fn response_json(response: axum::response::Response) -> Result<Value> {
    let bytes =
        response.into_body().collect().await.context("failed to collect response body")?.to_bytes();
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).context("failed to decode response json")
}

async fn insert_content_document(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    external_key: &str,
) -> Result<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "insert into content_document (
            id,
            workspace_id,
            library_id,
            external_key,
            document_state,
            created_at
        )
        values ($1, $2, $3, $4, 'active', now())
        returning id",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(library_id)
    .bind(external_key)
    .fetch_one(postgres)
    .await
    .context("failed to insert content_document row")
}

async fn insert_content_revision(
    postgres: &PgPool,
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    revision_number: i32,
    checksum: &str,
    title: &str,
) -> Result<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "insert into content_revision (
            id,
            document_id,
            workspace_id,
            library_id,
            revision_number,
            parent_revision_id,
            content_source_kind,
            checksum,
            mime_type,
            byte_size,
            title,
            language_code,
            source_uri,
            storage_key,
            created_by_principal_id
        )
        values (
            $1, $2, $3, $4, $5, null, 'upload', $6, 'text/markdown', 128, $7, 'ru', null, null, null
        )
        returning id",
    )
    .bind(Uuid::now_v7())
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(revision_number)
    .bind(checksum)
    .bind(title)
    .fetch_one(postgres)
    .await
    .context("failed to insert content_revision row")
}

async fn upsert_content_document_head(
    postgres: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "insert into content_document_head (
            document_id,
            active_revision_id,
            readable_revision_id,
            latest_mutation_id,
            latest_successful_attempt_id,
            head_updated_at
        )
        values ($1, $2, $2, null, null, now())
        on conflict (document_id)
        do update set
            active_revision_id = excluded.active_revision_id,
            readable_revision_id = excluded.readable_revision_id,
            head_updated_at = excluded.head_updated_at",
    )
    .bind(document_id)
    .bind(revision_id)
    .execute(postgres)
    .await
    .context("failed to upsert content_document_head row")?;
    Ok(())
}

async fn insert_content_chunk(
    postgres: &PgPool,
    revision_id: Uuid,
    chunk_index: i32,
    start_offset: i32,
    end_offset: i32,
    token_count: Option<i32>,
    normalized_text: &str,
    text_checksum: &str,
) -> Result<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "insert into content_chunk (
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8)
        returning id",
    )
    .bind(Uuid::now_v7())
    .bind(revision_id)
    .bind(chunk_index)
    .bind(start_offset)
    .bind(end_offset)
    .bind(token_count)
    .bind(normalized_text)
    .bind(text_checksum)
    .fetch_one(postgres)
    .await
    .context("failed to insert content_chunk row")
}

async fn probe_workspace_admin(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<StatusCode, rustrag_backend::interfaces::http::router_support::ApiError> {
    let _ =
        load_workspace_and_authorize(&auth, &state, workspace_id, POLICY_WORKSPACE_ADMIN).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn probe_library_read(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<StatusCode, rustrag_backend::interfaces::http::router_support::ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn probe_library_write(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<StatusCode, rustrag_backend::interfaces::http::router_support::ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn probe_document_read(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<StatusCode, rustrag_backend::interfaces::http::router_support::ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn probe_document_write(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<StatusCode, rustrag_backend::interfaces::http::router_support::ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_WRITE)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn tool_names(response: &Value) -> Result<Vec<String>> {
    response["result"]["tools"]
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
#[ignore = "requires local postgres, redis, and arango services"]
async fn workspace_scoped_discovery_only_returns_visible_workspace_and_libraries() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let token = fixture
            .mint_workspace_token(fixture.workspace_id, "workspace-discovery", &["workspace_read"])
            .await?;

        let (status, body) = fixture.rest_get(&token, "/v1/catalog/workspaces").await?;
        assert_eq!(status, StatusCode::OK);
        let workspaces = body.as_array().context("/v1/catalog/workspaces must return an array")?;
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0]["id"], json!(fixture.workspace_id));
        assert_ne!(workspaces[0]["id"], json!(fixture.foreign_workspace_id));

        let path = format!("/v1/catalog/workspaces/{}/libraries", fixture.workspace_id);
        let (status, body) = fixture.rest_get(&token, &path).await?;
        assert_eq!(status, StatusCode::OK);
        let libraries = body
            .as_array()
            .context("/v1/catalog/workspaces/{id}/libraries must return an array")?;
        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0]["id"], json!(fixture.library_id));
        assert_eq!(libraries[0]["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(libraries[0]["ingestionReadiness"]["ready"], json!(false));
        assert_eq!(
            libraries[0]["ingestionReadiness"]["missingBindingPurposes"],
            json!(["extract_graph"])
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn workspace_scoped_grants_match_between_discovery_and_mutation_probes() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let token = fixture
            .mint_workspace_token(
                fixture.workspace_id,
                "workspace-admin-matrix",
                &["workspace_read", "workspace_admin"],
            )
            .await?;

        let (status, body) = fixture.rest_get(&token, "/v1/catalog/workspaces").await?;
        assert_eq!(status, StatusCode::OK);
        let workspaces = body.as_array().context("/v1/catalog/workspaces must return an array")?;
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0]["id"], json!(fixture.workspace_id));

        let path = format!("/v1/catalog/workspaces/{}/libraries", fixture.workspace_id);
        let (status, body) = fixture.rest_get(&token, &path).await?;
        assert_eq!(status, StatusCode::OK);
        let libraries = body
            .as_array()
            .context("/v1/catalog/workspaces/{id}/libraries must return an array")?;
        assert_eq!(libraries.len(), 2);
        let library_ids = libraries
            .iter()
            .filter_map(|entry| entry["id"].as_str())
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(library_ids.contains(&fixture.library_id.to_string()));
        assert!(library_ids.contains(&fixture.sibling_library_id.to_string()));
        assert!(!library_ids.contains(&fixture.foreign_library_id.to_string()));

        let allowed_path = format!("/_test/authz/workspaces/{}/admin", fixture.workspace_id);
        let denied_path = format!("/_test/authz/workspaces/{}/admin", fixture.foreign_workspace_id);
        let (status, _) = fixture.rest_post(&token, &allowed_path, json!({})).await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = fixture.rest_post(&token, &denied_path, json!({})).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn library_scoped_grants_match_between_discovery_and_mutation_probes() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let token = fixture
            .mint_token_with_grants(
                Some(fixture.workspace_id),
                "library-scope",
                &[
                    GrantSpec {
                        resource_kind: "library",
                        resource_id: fixture.library_id,
                        permission_kind: "library_read".to_string(),
                    },
                    GrantSpec {
                        resource_kind: "library",
                        resource_id: fixture.library_id,
                        permission_kind: "library_write".to_string(),
                    },
                ],
            )
            .await?;

        let (status, body) = fixture.rest_get(&token, "/v1/catalog/workspaces").await?;
        assert_eq!(status, StatusCode::OK);
        let workspaces = body.as_array().context("/v1/catalog/workspaces must return an array")?;
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0]["id"], json!(fixture.workspace_id));

        let path = format!("/v1/catalog/workspaces/{}/libraries", fixture.workspace_id);
        let (status, body) = fixture.rest_get(&token, &path).await?;
        assert_eq!(status, StatusCode::OK);
        let libraries = body
            .as_array()
            .context("/v1/catalog/workspaces/{id}/libraries must return an array")?;
        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0]["id"], json!(fixture.library_id));

        let allowed_get = format!("/v1/catalog/libraries/{}", fixture.library_id);
        let denied_get = format!("/v1/catalog/libraries/{}", fixture.sibling_library_id);
        let (status, _) = fixture.rest_get(&token, &allowed_get).await?;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = fixture.rest_get(&token, &denied_get).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let allowed_write = format!("/_test/authz/libraries/{}/write", fixture.library_id);
        let denied_write = format!("/_test/authz/libraries/{}/write", fixture.sibling_library_id);
        let (status, _) = fixture.rest_post(&token, &allowed_write, json!({})).await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = fixture.rest_post(&token, &denied_write, json!({})).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn document_scoped_grants_match_between_discovery_and_mutation_probes() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let token = fixture
            .mint_token_with_grants(
                Some(fixture.workspace_id),
                "document-scope",
                &[
                    GrantSpec {
                        resource_kind: "document",
                        resource_id: fixture.document_id,
                        permission_kind: "document_read".to_string(),
                    },
                    GrantSpec {
                        resource_kind: "document",
                        resource_id: fixture.document_id,
                        permission_kind: "document_write".to_string(),
                    },
                ],
            )
            .await?;

        let (status, body) = fixture.rest_get(&token, "/v1/catalog/workspaces").await?;
        assert_eq!(status, StatusCode::OK);
        let workspaces = body.as_array().context("/v1/catalog/workspaces must return an array")?;
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0]["id"], json!(fixture.workspace_id));

        let path = format!("/v1/catalog/workspaces/{}/libraries", fixture.workspace_id);
        let (status, body) = fixture.rest_get(&token, &path).await?;
        assert_eq!(status, StatusCode::OK);
        let libraries = body
            .as_array()
            .context("/v1/catalog/workspaces/{id}/libraries must return an array")?;
        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0]["id"], json!(fixture.library_id));

        let allowed_read = format!("/_test/authz/documents/{}/read", fixture.document_id);
        let allowed_write = format!("/_test/authz/documents/{}/write", fixture.document_id);
        let denied_write = format!("/_test/authz/documents/{}/write", fixture.sibling_document_id);
        let foreign_write = format!("/_test/authz/documents/{}/write", fixture.foreign_document_id);

        let (status, _) = fixture.rest_get(&token, &allowed_read).await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = fixture.rest_post(&token, &allowed_write, json!({})).await?;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = fixture.rest_post(&token, &denied_write, json!({})).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = fixture.rest_post(&token, &foreign_write, json!({})).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn document_scoped_chunk_reads_use_canonical_content_chunks() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let revision_id = insert_content_revision(
            fixture.pool(),
            fixture.document_id,
            fixture.workspace_id,
            fixture.library_id,
            1,
            "sha256:governance-chunks",
            "Governance chunk revision",
        )
        .await?;
        upsert_content_document_head(fixture.pool(), fixture.document_id, revision_id).await?;
        let first_chunk_id = insert_content_chunk(
            fixture.pool(),
            revision_id,
            0,
            0,
            14,
            Some(3),
            "first chunk text",
            "sha256:governance-chunks-0",
        )
        .await?;
        let second_chunk_id = insert_content_chunk(
            fixture.pool(),
            revision_id,
            1,
            15,
            32,
            Some(4),
            "second chunk text",
            "sha256:governance-chunks-1",
        )
        .await?;

        let token = fixture
            .mint_token_with_grants(
                Some(fixture.workspace_id),
                "document-chunks",
                &[GrantSpec {
                    resource_kind: "document",
                    resource_id: fixture.document_id,
                    permission_kind: "document_read".to_string(),
                }],
            )
            .await?;

        let path = format!("/v1/chunks?documentId={}", fixture.document_id);
        let (status, body) = fixture.rest_get(&token, &path).await?;
        assert_eq!(status, StatusCode::OK);

        let chunks = body.as_array().context("/v1/chunks must return an array")?;
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0]["id"], json!(first_chunk_id));
        assert_eq!(chunks[0]["documentId"], json!(fixture.document_id));
        assert_eq!(chunks[0]["projectId"], json!(fixture.library_id));
        assert_eq!(chunks[0]["ordinal"], json!(0));
        assert_eq!(chunks[0]["content"], json!("first chunk text"));
        assert_eq!(chunks[0]["tokenCount"], json!(3));
        assert_eq!(chunks[1]["id"], json!(second_chunk_id));
        assert_eq!(chunks[1]["documentId"], json!(fixture.document_id));
        assert_eq!(chunks[1]["projectId"], json!(fixture.library_id));
        assert_eq!(chunks[1]["ordinal"], json!(1));
        assert_eq!(chunks[1]["content"], json!("second chunk text"));
        assert_eq!(chunks[1]["tokenCount"], json!(4));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn library_scoped_binding_admin_can_create_library_binding() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let token = fixture
            .mint_workspace_token(fixture.workspace_id, "library-binding-admin", &["binding_admin"])
            .await?;

        let credential = ai_repository::create_provider_credential(
            fixture.pool(),
            fixture.workspace_id,
            fixture.provider_catalog_id,
            TEST_PROVIDER_CREDENTIAL_LABEL,
            "secret://governance/provider-credential",
            None,
        )
        .await
        .context("failed to create provider credential for governance auth test")?;

        let preset = ai_repository::create_model_preset(
            fixture.pool(),
            fixture.workspace_id,
            fixture.model_catalog_id,
            TEST_MODEL_PRESET_NAME,
            None,
            None,
            None,
            None,
            json!({}),
            None,
        )
        .await
        .context("failed to create model preset for governance auth test")?;

        let (status, body) = fixture
            .rest_post(
                &token,
                "/v1/ai/library-bindings",
                json!({
                    "workspaceId": fixture.workspace_id,
                    "libraryId": fixture.library_id,
                    "bindingPurpose": TEST_BINDING_PURPOSE,
                    "providerCredentialId": credential.id,
                    "modelPresetId": preset.id,
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(body["libraryId"], json!(fixture.library_id));
        assert_eq!(body["bindingPurpose"], json!(TEST_BINDING_PURPOSE));
        assert_eq!(body["providerCredentialId"], json!(credential.id));
        assert_eq!(body["modelPresetId"], json!(preset.id));
        assert_eq!(body["bindingState"], json!("active"));

        let library_bindings =
            ai_repository::list_library_bindings(fixture.pool(), fixture.library_id)
                .await
                .context("failed to reload library bindings after create")?;
        assert_eq!(library_bindings.len(), 1);
        assert_eq!(library_bindings[0].workspace_id, fixture.workspace_id);
        assert_eq!(library_bindings[0].library_id, fixture.library_id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn mcp_tools_list_hides_unauthorized_mutation_and_admin_tools() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let token = fixture
            .mint_workspace_token(fixture.workspace_id, "readonly-mcp", &["workspace_read"])
            .await?;

        let response = fixture.mcp_tools_list(&token).await?;
        let names = tool_names(&response)?;

        assert!(names.contains(&"list_workspaces".to_string()));
        assert!(names.contains(&"list_libraries".to_string()));
        assert!(!names.contains(&"create_workspace".to_string()));
        assert!(!names.contains(&"create_library".to_string()));
        assert!(!names.contains(&"upload_documents".to_string()));
        assert!(!names.contains(&"update_document".to_string()));
        assert!(!names.contains(&"get_mutation_status".to_string()));
        assert!(!names.contains(&"submit_web_ingest_run".to_string()));
        assert!(!names.contains(&"get_web_ingest_run".to_string()));
        assert!(!names.contains(&"list_web_ingest_run_pages".to_string()));
        assert!(!names.contains(&"cancel_web_ingest_run".to_string()));
        assert!(!names.contains(&"list_audit_events".to_string()));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn mcp_tools_list_respects_system_workspace_library_and_document_grants() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let system_admin = fixture.mint_system_admin_token("system-admin-mcp").await?;
        let workspace_admin = fixture
            .mint_workspace_token(
                fixture.workspace_id,
                "workspace-admin-mcp",
                &["workspace_read", "workspace_admin"],
            )
            .await?;
        let library_writer = fixture
            .mint_token_with_grants(
                Some(fixture.workspace_id),
                "library-writer-mcp",
                &[
                    GrantSpec {
                        resource_kind: "library",
                        resource_id: fixture.library_id,
                        permission_kind: "library_read".to_string(),
                    },
                    GrantSpec {
                        resource_kind: "library",
                        resource_id: fixture.library_id,
                        permission_kind: "library_write".to_string(),
                    },
                ],
            )
            .await?;
        let document_writer = fixture
            .mint_token_with_grants(
                Some(fixture.workspace_id),
                "document-writer-mcp",
                &[
                    GrantSpec {
                        resource_kind: "document",
                        resource_id: fixture.document_id,
                        permission_kind: "document_read".to_string(),
                    },
                    GrantSpec {
                        resource_kind: "document",
                        resource_id: fixture.document_id,
                        permission_kind: "document_write".to_string(),
                    },
                ],
            )
            .await?;

        let system_tools = tool_names(&fixture.mcp_tools_list(&system_admin).await?)?;
        assert!(system_tools.contains(&"create_workspace".to_string()));
        assert!(system_tools.contains(&"create_library".to_string()));
        assert!(system_tools.contains(&"search_documents".to_string()));
        assert!(system_tools.contains(&"read_document".to_string()));
        assert!(system_tools.contains(&"upload_documents".to_string()));
        assert!(system_tools.contains(&"update_document".to_string()));
        assert!(system_tools.contains(&"get_mutation_status".to_string()));
        assert!(system_tools.contains(&"get_runtime_execution".to_string()));
        assert!(system_tools.contains(&"get_runtime_execution_trace".to_string()));
        assert!(system_tools.contains(&"submit_web_ingest_run".to_string()));
        assert!(system_tools.contains(&"get_web_ingest_run".to_string()));
        assert!(system_tools.contains(&"list_web_ingest_run_pages".to_string()));
        assert!(system_tools.contains(&"cancel_web_ingest_run".to_string()));

        let workspace_tools = tool_names(&fixture.mcp_tools_list(&workspace_admin).await?)?;
        assert!(!workspace_tools.contains(&"create_workspace".to_string()));
        assert!(workspace_tools.contains(&"create_library".to_string()));
        assert!(workspace_tools.contains(&"search_documents".to_string()));
        assert!(workspace_tools.contains(&"read_document".to_string()));
        assert!(workspace_tools.contains(&"upload_documents".to_string()));
        assert!(workspace_tools.contains(&"update_document".to_string()));
        assert!(workspace_tools.contains(&"get_mutation_status".to_string()));
        assert!(workspace_tools.contains(&"get_runtime_execution".to_string()));
        assert!(workspace_tools.contains(&"get_runtime_execution_trace".to_string()));
        assert!(workspace_tools.contains(&"submit_web_ingest_run".to_string()));
        assert!(workspace_tools.contains(&"get_web_ingest_run".to_string()));
        assert!(workspace_tools.contains(&"list_web_ingest_run_pages".to_string()));
        assert!(workspace_tools.contains(&"cancel_web_ingest_run".to_string()));

        let library_tools = tool_names(&fixture.mcp_tools_list(&library_writer).await?)?;
        assert!(!library_tools.contains(&"create_workspace".to_string()));
        assert!(!library_tools.contains(&"create_library".to_string()));
        assert!(library_tools.contains(&"search_documents".to_string()));
        assert!(library_tools.contains(&"read_document".to_string()));
        assert!(library_tools.contains(&"upload_documents".to_string()));
        assert!(library_tools.contains(&"update_document".to_string()));
        assert!(library_tools.contains(&"get_mutation_status".to_string()));
        assert!(library_tools.contains(&"get_runtime_execution".to_string()));
        assert!(library_tools.contains(&"get_runtime_execution_trace".to_string()));
        assert!(library_tools.contains(&"submit_web_ingest_run".to_string()));
        assert!(library_tools.contains(&"get_web_ingest_run".to_string()));
        assert!(library_tools.contains(&"list_web_ingest_run_pages".to_string()));
        assert!(library_tools.contains(&"cancel_web_ingest_run".to_string()));

        let document_tools = tool_names(&fixture.mcp_tools_list(&document_writer).await?)?;
        assert!(!document_tools.contains(&"create_workspace".to_string()));
        assert!(!document_tools.contains(&"create_library".to_string()));
        assert!(!document_tools.contains(&"search_documents".to_string()));
        assert!(document_tools.contains(&"read_document".to_string()));
        assert!(!document_tools.contains(&"upload_documents".to_string()));
        assert!(document_tools.contains(&"update_document".to_string()));
        assert!(document_tools.contains(&"get_mutation_status".to_string()));
        assert!(document_tools.contains(&"get_runtime_execution".to_string()));
        assert!(document_tools.contains(&"get_runtime_execution_trace".to_string()));
        assert!(!document_tools.contains(&"submit_web_ingest_run".to_string()));
        assert!(!document_tools.contains(&"get_web_ingest_run".to_string()));
        assert!(!document_tools.contains(&"list_web_ingest_run_pages".to_string()));
        assert!(!document_tools.contains(&"cancel_web_ingest_run".to_string()));
        assert!(!document_tools.contains(&"list_audit_events".to_string()));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn anonymous_governance_and_operational_reads_are_rejected() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        assert_eq!(
            fixture.rest_get_status_optional(None, "/v1/ai/providers").await?,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            fixture.rest_get_status_optional(None, "/v1/ai/models").await?,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            fixture.rest_get_status_optional(None, "/v1/ai/prices").await?,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            fixture.rest_get_status_optional(None, "/v1/catalog/workspaces").await?,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            fixture.rest_get_status_optional(None, "/v1/mcp/capabilities").await?,
            StatusCode::UNAUTHORIZED
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn workspace_audit_reader_gets_redacted_visible_events_only() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        audit_repository::append_audit_event(
            fixture.pool(),
            audit_repository::NewAuditEvent {
                actor_principal_id: None,
                surface_kind: "rest".to_string(),
                action_kind: "catalog.library.create".to_string(),
                request_id: Some("governance-audit-visible".to_string()),
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some("visible workspace event".to_string()),
                internal_message: Some("visible internal detail".to_string()),
            },
            &[audit_repository::NewAuditEventSubject {
                subject_kind: "workspace".to_string(),
                subject_id: fixture.workspace_id,
                workspace_id: Some(fixture.workspace_id),
                library_id: None,
                document_id: None,
            }],
        )
        .await
        .context("failed to append visible audit event")?;

        audit_repository::append_audit_event(
            fixture.pool(),
            audit_repository::NewAuditEvent {
                actor_principal_id: None,
                surface_kind: "rest".to_string(),
                action_kind: "catalog.library.create".to_string(),
                request_id: Some("governance-audit-foreign".to_string()),
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some("foreign workspace event".to_string()),
                internal_message: Some("foreign internal detail".to_string()),
            },
            &[audit_repository::NewAuditEventSubject {
                subject_kind: "workspace".to_string(),
                subject_id: fixture.foreign_workspace_id,
                workspace_id: Some(fixture.foreign_workspace_id),
                library_id: None,
                document_id: None,
            }],
        )
        .await
        .context("failed to append foreign audit event")?;

        let token = fixture
            .mint_workspace_token(fixture.workspace_id, "audit-reader", &["audit_read"])
            .await?;

        let visible_path = format!("/v1/audit/events?workspaceId={}", fixture.workspace_id);
        let (status, body) = fixture.rest_get(&token, &visible_path).await?;
        assert_eq!(status, StatusCode::OK);
        let events = body.as_array().context("/v1/audit/events must return an array")?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["requestId"], json!("governance-audit-visible"));
        assert_eq!(events[0]["redactedMessage"], json!("visible workspace event"));
        assert!(events[0]["internalMessage"].is_null());
        let subjects =
            events[0]["subjects"].as_array().context("audit event subjects must exist")?;
        assert_eq!(subjects.len(), 1);
        assert_eq!(subjects[0]["workspaceId"], json!(fixture.workspace_id));

        let internal_path =
            format!("/v1/audit/events?workspaceId={}&internal=true", fixture.workspace_id);
        let (status, body) = fixture.rest_get(&token, &internal_path).await?;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["errorKind"], json!("forbidden"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
