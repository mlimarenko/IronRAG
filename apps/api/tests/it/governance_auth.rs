use crate::{http_response_support, temp_postgres_database};

use http_response_support::response_json;
use temp_postgres_database::TempPostgresDatabase;

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode, header},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, seed_clean_v1_ai_catalog, state::AppState},
    infra::{
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
        mcp::{MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION, MCP_SESSION_HEADER},
        router,
    },
    shared::{auth_tokens::mint_plaintext_api_token, secret_encryption::SecretPurpose},
};

const TEST_PROVIDER_CREDENTIAL_LABEL: &str = "governance-provider-credential";
const TEST_BINDING_PURPOSE: &str = "query_answer";

fn test_credential_master_key() -> String {
    STANDARD.encode([37_u8; 32])
}

use crate::iam_token_support::{GrantSpec, create_principal_grants};

struct GovernanceAuthFixture {
    state: AppState,
    temp_database: TempPostgresDatabase,
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
        let temp_database =
            TempPostgresDatabase::create(&settings.database_url, "governance_auth").await?;
        settings.database_url = temp_database.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;
        settings.credential_master_key = Some(test_credential_master_key());

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect governance auth postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply governance auth migrations")?;
        seed_clean_v1_ai_catalog(&postgres)
            .await
            .context("failed to seed governance auth AI catalog")?;

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

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }

    const fn pool(&self) -> &PgPool {
        &self.state.persistence.postgres
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

    async fn mint_token_with_grants(
        &self,
        token_workspace_id: Option<Uuid>,
        label: &str,
        grants: &[GrantSpec],
    ) -> Result<String> {
        let plaintext = mint_plaintext_api_token();
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
        create_principal_grants(self.pool(), token.principal_id, label, grants).await?;
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

    /// Asserts the token sees exactly its own workspace, and returns the ids
    /// of the libraries it can list inside it.
    async fn visible_workspace_and_library_ids(&self, token: &str) -> Result<Vec<String>> {
        let (status, body) = self.rest_get(token, "/v1/catalog/workspaces").await?;
        assert_eq!(status, StatusCode::OK);
        let workspaces = body.as_array().context("/v1/catalog/workspaces must return an array")?;
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0]["id"], json!(self.workspace_id));
        self.visible_library_ids(token).await
    }

    /// Lists the workspace's libraries and returns the ids the token can see.
    ///
    /// The listing is a bounded page — an `items` array plus its total — so
    /// every caller reads `items` rather than treating the body as an array.
    async fn visible_library_ids(&self, token: &str) -> Result<Vec<String>> {
        let path = format!("/v1/catalog/workspaces/{}/libraries", self.workspace_id);
        let (status, body) = self.rest_get(token, &path).await?;
        assert_eq!(status, StatusCode::OK);
        let items = body["items"]
            .as_array()
            .context("/v1/catalog/workspaces/{id}/libraries must return a page of items")?;
        Ok(items.iter().filter_map(|entry| entry["id"].as_str()).map(ToString::to_string).collect())
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
            .oneshot(
                request
                    .body(Body::empty())
                    .context("failed to build governance auth GET request")?,
            )
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
            .oneshot(
                request
                    .body(Body::empty())
                    .context("failed to build governance auth GET status request")?,
            )
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
                    .context("failed to build governance auth POST request")?,
            )
            .await
            .with_context(|| format!("POST {path} failed"))?;
        let status = response.status();
        Ok((status, response_json(response).await?))
    }

    async fn mcp_diagnostics_tools_list(&self, token: &str) -> Result<Value> {
        self.mcp_call_at("/v1/mcp/diagnostics", token, "tools/list", None).await
    }

    async fn mcp_call(&self, token: &str, method: &str, params: Option<Value>) -> Result<Value> {
        self.mcp_call_at("/v1/mcp", token, method, params).await
    }

    async fn mcp_call_at(
        &self,
        uri: &str,
        token: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value> {
        let initialize = self
            .mcp_transport_request_at(
                uri,
                token,
                "POST",
                None,
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": format!("governance-initialize-{}", method.replace('/', "-")),
                    "method": "initialize",
                    "params": {
                        "protocolVersion": MCP_PROTOCOL_VERSION,
                        "capabilities": {},
                        "clientInfo": { "name": "ironrag-governance-test", "version": "1" },
                    },
                })),
            )
            .await?;
        if initialize.status() != StatusCode::OK {
            anyhow::bail!("unexpected MCP initialize status {}", initialize.status());
        }
        let session_id = initialize
            .headers()
            .get(MCP_SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .context("MCP initialize omitted its session id")?
            .to_string();
        let response = self
            .mcp_transport_request_at(
                uri,
                token,
                "POST",
                Some(&session_id),
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": format!("governance-{}", method.replace('/', "-")),
                    "method": method,
                    "params": params,
                })),
            )
            .await?;
        let status = response.status();
        let json = response_json(response).await?;
        let cleanup =
            self.mcp_transport_request_at(uri, token, "DELETE", Some(&session_id), None).await?;
        if cleanup.status() != StatusCode::OK {
            anyhow::bail!("unexpected MCP session cleanup status {}", cleanup.status());
        }
        if status != StatusCode::OK && status != StatusCode::ACCEPTED {
            anyhow::bail!("unexpected status {status} for MCP {method}");
        }
        Ok(json)
    }

    async fn mcp_tools_list(&self, token: &str) -> Result<Value> {
        self.mcp_call(token, "tools/list", None).await
    }

    async fn mcp_transport_request(
        &self,
        token: &str,
        method: &str,
        session_id: Option<&str>,
        payload: Option<Value>,
    ) -> Result<axum::response::Response> {
        self.mcp_transport_request_at("/v1/mcp", token, method, session_id, payload).await
    }

    async fn mcp_transport_request_at(
        &self,
        uri: &str,
        token: &str,
        method: &str,
        session_id: Option<&str>,
        payload: Option<Value>,
    ) -> Result<axum::response::Response> {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        if let Some(session_id) = session_id {
            request = request
                .header(MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION)
                .header(MCP_SESSION_HEADER, session_id);
        }
        let body = match payload {
            Some(payload) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(payload.to_string())
            }
            None => Body::empty(),
        };
        self.app()
            .oneshot(request.body(body).context("failed to build MCP transport request")?)
            .await
            .context("MCP transport request failed")
    }
}

fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let bootstrap_settings = settings.bootstrap_settings();
    let redis = redis::Client::open(settings.redis_url.clone())
        .context("failed to create redis client for governance auth test state")?;
    let persistence = Persistence::for_tests(postgres, redis);
    AppState::from_dependencies(
        Settings {
            ui_bootstrap_admin_login: bootstrap_settings
                .ui_bootstrap_admin
                .as_ref()
                .map(|admin| admin.login.clone()),
            ui_bootstrap_admin_email: bootstrap_settings
                .ui_bootstrap_admin
                .as_ref()
                .map(|admin| admin.email.clone()),
            ui_bootstrap_admin_name: bootstrap_settings
                .ui_bootstrap_admin
                .as_ref()
                .map(|admin| admin.display_name.clone()),
            ui_bootstrap_admin_password: bootstrap_settings
                .ui_bootstrap_admin
                .as_ref()
                .map(|admin| admin.password.clone()),
            ..settings
        },
        persistence,
    )
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

/// One seeded content revision row for the governance chunk-read proof.
struct ContentRevisionSeed<'a> {
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    revision_number: i32,
    checksum: &'a str,
    title: &'a str,
}

async fn insert_content_revision(postgres: &PgPool, seed: ContentRevisionSeed<'_>) -> Result<Uuid> {
    let checksum = crate::content_lifecycle_support::fixture_checksum(seed.checksum);
    let admission = crate::content_repository::synthetic_import_admission(
        ironrag_contracts::documents::ReplacementIdentity {
            checksum: checksum.clone(),
            byte_size: 128,
            mime_type: "text/markdown".to_owned(),
            file_name: "governance-source.txt".to_owned(),
            title: Some(seed.title.to_owned()),
            language_code: Some("ru".to_owned()),
            source_uri: None,
            document_hint: None,
        },
    )?;
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
            created_by_principal_id, admission_identity
        )
        values (
            $1, $2, $3, $4, $5, null, 'upload', $6, 'text/markdown', 128, $7, 'ru', null, null, null, $8
        )
        returning id",
    )
    .bind(Uuid::now_v7())
    .bind(seed.document_id)
    .bind(seed.workspace_id)
    .bind(seed.library_id)
    .bind(seed.revision_number)
    .bind(checksum)
    .bind(seed.title)
    .bind(serde_json::to_value(admission)?)
    .fetch_one(postgres)
    .await
    .context("failed to insert content_revision row")
}

async fn upsert_content_document_head(
    postgres: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<()> {
    crate::content_lifecycle_support::seed_readable_revision_head(
        postgres,
        document_id,
        revision_id,
        "governance-source.txt",
    )
    .await
}

/// One seeded content chunk row for the governance chunk-read proof.
struct ContentChunkSeed<'a> {
    revision_id: Uuid,
    chunk_index: i32,
    start_offset: i32,
    end_offset: i32,
    token_count: Option<i32>,
    normalized_text: &'a str,
    text_checksum: &'a str,
}

async fn insert_content_chunk(postgres: &PgPool, seed: ContentChunkSeed<'_>) -> Result<Uuid> {
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
    .bind(seed.revision_id)
    .bind(seed.chunk_index)
    .bind(seed.start_offset)
    .bind(seed.end_offset)
    .bind(seed.token_count)
    .bind(seed.normalized_text)
    .bind(seed.text_checksum)
    .fetch_one(postgres)
    .await
    .context("failed to insert content_chunk row")
}

async fn publish_prepared_chunk_metadata(
    postgres: &PgPool,
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    revision_id: Uuid,
    chunk_count: i64,
) -> Result<()> {
    let mut transaction =
        postgres.begin().await.context("failed to begin prepared chunk metadata publication")?;
    let projected_document = sqlx::query(
        "insert into knowledge_document (
            document_id, workspace_id, library_id, external_key, file_name, title,
            document_state, active_revision_id, readable_revision_id, latest_revision_no,
            parent_document_id, document_role, created_at, updated_at, deleted_at
         )
         select
            document.id, document.workspace_id, document.library_id, document.external_key,
            document.source_file_name, revision.title, document.document_state::text,
            revision.id, revision.id, revision.revision_number, document.parent_document_id,
            document.document_role, document.created_at, now(), document.deleted_at
         from content_document as document
         join content_revision as revision on revision.document_id = document.id
         where document.id = $1
           and document.workspace_id = $2
           and document.library_id = $3
           and revision.id = $4
           and revision.workspace_id = document.workspace_id
           and revision.library_id = document.library_id",
    )
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(revision_id)
    .execute(&mut *transaction)
    .await
    .context("failed to publish prepared chunk document projection")?;
    anyhow::ensure!(
        projected_document.rows_affected() == 1,
        "prepared chunk document projection did not match its exact source scope"
    );

    let projected_revision = sqlx::query(
        "insert into knowledge_revision (
            revision_id, workspace_id, library_id, document_id, revision_number,
            revision_state, revision_kind, storage_ref, source_uri, document_hint,
            mime_type, checksum, title, byte_size, normalized_text, text_checksum,
            image_checksum, text_state, vector_state, graph_state, text_readable_at,
            vector_ready_at, graph_ready_at, superseded_by_revision_id, created_at
         )
         select
            revision.id, revision.workspace_id, revision.library_id, revision.document_id,
            revision.revision_number, 'ready', revision.content_source_kind::text,
            revision.storage_key, revision.source_uri, revision.document_hint,
            revision.mime_type, revision.checksum, revision.title, revision.byte_size,
            null, revision.checksum, null, 'text_readable', 'accepted', 'accepted',
            now(), null, null, null, revision.created_at
         from content_revision as revision
         where revision.id = $1
           and revision.document_id = $2
           and revision.workspace_id = $3
           and revision.library_id = $4",
    )
    .bind(revision_id)
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .execute(&mut *transaction)
    .await
    .context("failed to publish prepared chunk revision projection")?;
    anyhow::ensure!(
        projected_revision.rows_affected() == 1,
        "prepared chunk revision projection did not match its exact source scope"
    );

    sqlx::query(
        "insert into knowledge_structured_revision (
            revision_id, workspace_id, library_id, document_id,
            preparation_state, normalization_profile, source_format, language_code,
            block_count, chunk_count, typed_fact_count, outline_json,
            prepared_at, updated_at
         ) values (
            $1, $2, $3, $4, 'prepared', 'governance-auth-test-v1', 'text', null,
            0, $5, 0, '{}'::jsonb, now(), now()
         )",
    )
    .bind(revision_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(document_id)
    .bind(chunk_count)
    .execute(&mut *transaction)
    .await
    .context("failed to publish prepared structured revision metadata")?;

    transaction.commit().await.context("failed to commit prepared chunk metadata publication")?;
    upsert_content_document_head(postgres, document_id, revision_id).await?;
    crate::content_lifecycle_support::assert_current_revision(postgres, document_id, revision_id)
        .await
}

async fn probe_workspace_admin(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<StatusCode, ironrag_backend::interfaces::http::router_support::ApiError> {
    let _ =
        load_workspace_and_authorize(&auth, &state, workspace_id, POLICY_WORKSPACE_ADMIN).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn probe_library_read(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<StatusCode, ironrag_backend::interfaces::http::router_support::ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn probe_library_write(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<StatusCode, ironrag_backend::interfaces::http::router_support::ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn probe_document_read(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<StatusCode, ironrag_backend::interfaces::http::router_support::ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn probe_document_write(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<StatusCode, ironrag_backend::interfaces::http::router_support::ApiError> {
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
#[ignore = "requires local postgres and redis services"]
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
        // The listing is a bounded page: an `items` array plus its total.
        let libraries = body["items"]
            .as_array()
            .context("/v1/catalog/workspaces/{id}/libraries must return a page of items")?;
        // workspace_read grants catalog visibility over the whole workspace:
        // both same-workspace libraries are listed, the foreign one is not.
        assert_eq!(libraries.len(), 2);
        let listed: Vec<_> = libraries.iter().map(|l| l["id"].clone()).collect();
        assert!(listed.contains(&json!(fixture.library_id)));
        assert!(listed.contains(&json!(fixture.sibling_library_id)));
        assert!(!listed.contains(&json!(fixture.foreign_library_id)));
        let primary = libraries
            .iter()
            .find(|l| l["id"] == json!(fixture.library_id))
            .context("granted library must be listed")?;
        assert_eq!(primary["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(primary["ingestionReadiness"]["ready"], json!(false));
        assert_eq!(
            primary["ingestionReadiness"]["missingBindingPurposes"],
            json!(["extract_graph", "embed_chunk"])
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
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

        let library_ids = fixture.visible_workspace_and_library_ids(&token).await?;
        assert_eq!(library_ids.len(), 2);
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
#[ignore = "requires local postgres and redis services"]
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

        let library_ids = fixture.visible_workspace_and_library_ids(&token).await?;
        assert_eq!(library_ids, vec![fixture.library_id.to_string()]);

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
#[ignore = "requires local postgres and redis services"]
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

        let library_ids = fixture.visible_workspace_and_library_ids(&token).await?;
        assert_eq!(library_ids, vec![fixture.library_id.to_string()]);

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
#[ignore = "requires local postgres and redis services"]
async fn document_scoped_chunk_reads_use_authoritative_content_chunks() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let revision_id = insert_content_revision(
            fixture.pool(),
            ContentRevisionSeed {
                document_id: fixture.document_id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                revision_number: 1,
                checksum: "sha256:governance-chunks",
                title: "Governance chunk revision",
            },
        )
        .await?;
        let first_chunk_id = insert_content_chunk(
            fixture.pool(),
            ContentChunkSeed {
                revision_id,
                chunk_index: 0,
                start_offset: 0,
                end_offset: 14,
                token_count: Some(3),
                normalized_text: "first chunk text",
                text_checksum: "sha256:governance-chunks-0",
            },
        )
        .await?;
        let second_chunk_id = insert_content_chunk(
            fixture.pool(),
            ContentChunkSeed {
                revision_id,
                chunk_index: 1,
                start_offset: 15,
                end_offset: 32,
                token_count: Some(4),
                normalized_text: "second chunk text",
                text_checksum: "sha256:governance-chunks-1",
            },
        )
        .await?;
        publish_prepared_chunk_metadata(
            fixture.pool(),
            fixture.document_id,
            fixture.workspace_id,
            fixture.library_id,
            revision_id,
            2,
        )
        .await?;

        let projected_chunk_count = sqlx::query_scalar::<_, i64>(
            "select count(*) from knowledge_chunk where revision_id = $1",
        )
        .bind(revision_id)
        .fetch_one(fixture.pool())
        .await
        .context("failed to count projected chunks for governance chunk read")?;
        assert_eq!(
            projected_chunk_count, 0,
            "the endpoint proof must not be kept alive by the query-facing chunk projection",
        );

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
        sqlx::query(
            "update knowledge_structured_revision
             set preparation_state = 'preparing'
             where revision_id = $1",
        )
        .bind(revision_id)
        .execute(fixture.pool())
        .await
        .context("failed to mark the governance chunk artifact as preparing")?;
        let (status, preparing_body) = fixture.rest_get(&token, &path).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            preparing_body.as_array().map(Vec::len),
            Some(0),
            "an unfinished exact revision must not expose authoritative source chunks",
        );
        sqlx::query(
            "update knowledge_structured_revision
             set preparation_state = 'prepared'
             where revision_id = $1",
        )
        .bind(revision_id)
        .execute(fixture.pool())
        .await
        .context("failed to finalize the governance chunk artifact")?;
        let (status, body) = fixture.rest_get(&token, &path).await?;
        assert_eq!(status, StatusCode::OK);

        let chunks = body.as_array().context("/v1/chunks must return an array")?;
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0]["id"], json!(first_chunk_id));
        assert_eq!(chunks[0]["documentId"], json!(fixture.document_id));
        assert_eq!(chunks[0]["libraryId"], json!(fixture.library_id));
        assert_eq!(chunks[0]["ordinal"], json!(0));
        assert_eq!(chunks[0]["content"], json!("first chunk text"));
        assert_eq!(chunks[0]["tokenCount"], json!(3));
        assert_eq!(chunks[1]["id"], json!(second_chunk_id));
        assert_eq!(chunks[1]["documentId"], json!(fixture.document_id));
        assert_eq!(chunks[1]["libraryId"], json!(fixture.library_id));
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
#[ignore = "requires local postgres and redis services"]
async fn library_scoped_binding_admin_can_create_library_binding() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let token = fixture
            .mint_workspace_token(fixture.workspace_id, "library-binding-admin", &["binding_admin"])
            .await?;

        let account_id = Uuid::now_v7();
        let encrypted_api_key = fixture.state.credential_cipher.encrypt(
            SecretPurpose::AiAccountApiKey,
            account_id,
            "secret://governance/provider-credential",
        )?;
        let account = ai_repository::create_account(
            fixture.pool(),
            ai_repository::NewAiAccount {
                account_id,
                scope_kind: "workspace",
                workspace_id: Some(fixture.workspace_id),
                library_id: None,
                provider_catalog_id: fixture.provider_catalog_id,
                label: TEST_PROVIDER_CREDENTIAL_LABEL,
                base_url: None,
                created_by_principal_id: None,
            },
            Some(&encrypted_api_key),
        )
        .await
        .context("failed to create AI account for governance auth test")?;

        let (status, body) = fixture
            .rest_post(
                &token,
                "/v1/ai/bindings",
                json!({
                    "scopeKind": "library",
                    "workspaceId": fixture.workspace_id,
                    "libraryId": fixture.library_id,
                    "bindingPurpose": TEST_BINDING_PURPOSE,
                    "accountId": account.id,
                    "modelCatalogId": fixture.model_catalog_id,
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(body["libraryId"], json!(fixture.library_id));
        assert_eq!(body["bindingPurpose"], json!(TEST_BINDING_PURPOSE));
        assert_eq!(body["accountId"], json!(account.id));
        assert_eq!(body["modelCatalogId"], json!(fixture.model_catalog_id));
        assert_eq!(body["bindingState"], json!("active"));

        let library_bindings = ai_repository::list_bindings_exact(
            fixture.pool(),
            "library",
            Some(fixture.workspace_id),
            Some(fixture.library_id),
        )
        .await
        .context("failed to reload library bindings after create")?;
        assert_eq!(library_bindings.len(), 1);
        assert_eq!(library_bindings[0].workspace_id, Some(fixture.workspace_id));
        assert_eq!(library_bindings[0].library_id, Some(fixture.library_id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn workspace_scoped_iam_admin_cannot_mint_foreign_workspace_token() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let token = fixture
            .mint_workspace_token(fixture.workspace_id, "workspace-iam-admin", &["iam_admin"])
            .await?;

        let label = format!("foreign-workspace-token-{}", Uuid::now_v7());
        let before_count: i64 =
            sqlx::query_scalar::<_, i64>("select count(*) from iam_api_token where label = $1")
                .bind(&label)
                .fetch_one(fixture.pool())
                .await?;

        let (status, _) = fixture
            .rest_post(
                &token,
                "/v1/iam/tokens",
                json!({
                    "workspaceId": fixture.foreign_workspace_id,
                    "label": label,
                    "permissionKinds": ["workspace_read"]
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let after_count: i64 =
            sqlx::query_scalar::<_, i64>("select count(*) from iam_api_token where label = $1")
                .bind(&label)
                .fetch_one(fixture.pool())
                .await?;
        assert_eq!(before_count, after_count);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
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
        assert!(!names.contains(&"create_documents".to_string()));
        assert!(!names.contains(&"create_document_revision".to_string()));
        assert!(!names.contains(&"get_operation".to_string()));
        assert!(!names.contains(&"submit_web_run".to_string()));
        assert!(!names.contains(&"get_web_run".to_string()));
        assert!(!names.contains(&"list_web_run_pages".to_string()));
        assert!(!names.contains(&"cancel_web_run".to_string()));
        assert!(!names.contains(&"list_audit_events".to_string()));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn mcp_tools_list_respects_workspace_library_and_document_grants() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
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

        // The answer mount is read-only. Mutation and administration tools live
        // exclusively on the authoritative diagnostics mount.
        const DIAGNOSTICS_ONLY_TOOLS: &[&str] = &[
            "create_workspace",
            "create_library",
            "update_workspace",
            "update_library",
            "create_documents",
            "create_document_revision",
            "delete_document",
            "get_operation",
            "submit_web_run",
            "cancel_web_run",
        ];

        // Diagnostics mount: the mutation matrix follows the grant scopes.

        let workspace_diag =
            tool_names(&fixture.mcp_diagnostics_tools_list(&workspace_admin).await?)?;
        assert!(!workspace_diag.contains(&"create_workspace".to_string()));
        assert!(workspace_diag.contains(&"create_library".to_string()));
        assert!(workspace_diag.contains(&"create_documents".to_string()));
        assert!(workspace_diag.contains(&"create_document_revision".to_string()));
        assert!(workspace_diag.contains(&"get_operation".to_string()));
        assert!(workspace_diag.contains(&"submit_web_run".to_string()));
        assert!(workspace_diag.contains(&"cancel_web_run".to_string()));

        let library_diag = tool_names(&fixture.mcp_diagnostics_tools_list(&library_writer).await?)?;
        assert!(!library_diag.contains(&"create_workspace".to_string()));
        assert!(!library_diag.contains(&"create_library".to_string()));
        assert!(library_diag.contains(&"create_documents".to_string()));
        assert!(library_diag.contains(&"create_document_revision".to_string()));
        assert!(library_diag.contains(&"get_operation".to_string()));
        assert!(library_diag.contains(&"submit_web_run".to_string()));
        assert!(library_diag.contains(&"cancel_web_run".to_string()));

        let library_tools = tool_names(&fixture.mcp_tools_list(&library_writer).await?)?;
        assert!(library_tools.contains(&"search_documents".to_string()));
        assert!(library_tools.contains(&"read_document".to_string()));
        assert!(library_tools.contains(&"get_runtime_execution".to_string()));
        for tool in DIAGNOSTICS_ONLY_TOOLS {
            assert!(!library_tools.contains(&(*tool).to_string()), "main mount leaked {tool}");
        }

        let document_tools = tool_names(&fixture.mcp_tools_list(&document_writer).await?)?;
        assert!(!document_tools.contains(&"search_documents".to_string()));
        assert!(document_tools.contains(&"read_document".to_string()));
        assert!(document_tools.contains(&"get_runtime_execution".to_string()));
        assert!(!document_tools.contains(&"list_audit_events".to_string()));

        let document_diag =
            tool_names(&fixture.mcp_diagnostics_tools_list(&document_writer).await?)?;
        // Registry semantics: any document-memory write grant lists the write
        // tools; per-call authorization still scopes actual creation.
        assert!(document_diag.contains(&"create_documents".to_string()));
        assert!(document_diag.contains(&"create_document_revision".to_string()));
        assert!(document_diag.contains(&"get_operation".to_string()));
        assert!(!document_diag.contains(&"submit_web_run".to_string()));
        assert!(!document_diag.contains(&"cancel_web_run".to_string()));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
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
            fixture.rest_get_status_optional(None, "/v1/catalog/workspaces").await?,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            fixture.rest_get_status_optional(None, "/v1/mcp/capabilities").await?,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            fixture.rest_get_status_optional(None, "/v1/mcp").await?,
            StatusCode::UNAUTHORIZED
        );
        let anonymous_delete = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/mcp")
                    .body(Body::empty())
                    .context("failed to build anonymous MCP DELETE request")?,
            )
            .await
            .context("anonymous DELETE /v1/mcp failed")?;
        assert_eq!(anonymous_delete.status(), StatusCode::UNAUTHORIZED);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn mcp_streamable_http_session_is_owned_bounded_and_idempotently_terminated() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let token = fixture
            .mint_workspace_token(fixture.workspace_id, "mcp-session-owner", &["workspace_read"])
            .await?;
        let initialize = fixture
            .mcp_transport_request(
                &token,
                "POST",
                None,
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": "session-initialize",
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": {},
                        "clientInfo": { "name": "ironrag-session-test", "version": "1" }
                    }
                })),
            )
            .await?;
        assert_eq!(initialize.status(), StatusCode::OK);
        let session_id = initialize
            .headers()
            .get(MCP_SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .context("initialize must issue an MCP session id")?
            .to_string();
        let initialize_payload = response_json(initialize).await?;
        assert_eq!(
            initialize_payload["result"]["protocolVersion"],
            json!("2025-11-25"),
            "initialize must negotiate every valid request to the one authoritative server version"
        );

        for protocol_version in [None, Some("2025-06-18")] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/v1/mcp")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(MCP_SESSION_HEADER, &session_id);
            if let Some(protocol_version) = protocol_version {
                request = request.header(MCP_PROTOCOL_HEADER, protocol_version);
            }
            let response = fixture
                .app()
                .oneshot(
                    request
                        .body(Body::from(
                            json!({
                                "jsonrpc": "2.0",
                                "id": "strict-protocol-tools-list",
                                "method": "tools/list",
                                "params": {},
                            })
                            .to_string(),
                        ))
                        .context("failed to build strict MCP protocol request")?,
                )
                .await
                .context("strict MCP protocol request failed")?;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }

        assert_eq!(
            fixture.mcp_transport_request(&token, "GET", None, None).await?.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            fixture.mcp_transport_request(&token, "DELETE", None, None).await?.status(),
            StatusCode::NOT_FOUND
        );
        let foreign_session = Uuid::now_v7().as_hyphenated().to_string();
        assert_eq!(
            fixture
                .mcp_transport_request(&token, "GET", Some(&foreign_session), None)
                .await?
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            fixture
                .mcp_transport_request(&token, "DELETE", Some(&foreign_session), None)
                .await?
                .status(),
            StatusCode::NOT_FOUND
        );
        let foreign_token = fixture
            .mint_workspace_token(
                fixture.workspace_id,
                "mcp-session-foreign-owner",
                &["workspace_read"],
            )
            .await?;
        assert_eq!(
            fixture
                .mcp_transport_request(&foreign_token, "GET", Some(&session_id), None)
                .await?
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            fixture
                .mcp_transport_request(&foreign_token, "DELETE", Some(&session_id), None)
                .await?
                .status(),
            StatusCode::NOT_FOUND
        );

        let stream = fixture.mcp_transport_request(&token, "GET", Some(&session_id), None).await?;
        assert_eq!(stream.status(), StatusCode::OK);
        let mut stream_body = stream.into_body();
        let ready = tokio::time::timeout(std::time::Duration::from_secs(1), stream_body.frame())
            .await
            .context("MCP GET stream did not emit ready promptly")?
            .context("MCP GET stream closed before ready")?
            .context("MCP GET stream ready frame failed")?
            .into_data()
            .map_err(|_| anyhow::anyhow!("MCP GET stream ready frame was not data"))?;
        assert_eq!(ready.as_ref(), b": ready\n\n");
        let tools = fixture
            .mcp_transport_request(
                &token,
                "POST",
                Some(&session_id),
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": "session-tools-list",
                    "method": "tools/list"
                })),
            )
            .await?;
        assert_eq!(tools.status(), StatusCode::OK);

        assert_eq!(
            fixture
                .mcp_transport_request(&token, "DELETE", Some(&session_id), None)
                .await?
                .status(),
            StatusCode::OK
        );
        let stream_end =
            tokio::time::timeout(std::time::Duration::from_secs(1), stream_body.frame())
                .await
                .context("owned MCP DELETE did not cancel the local GET stream")?;
        assert!(stream_end.is_none(), "terminated MCP GET stream must close without a data frame");
        assert_eq!(
            fixture
                .mcp_transport_request(&token, "DELETE", Some(&session_id), None)
                .await?
                .status(),
            StatusCode::OK,
            "owned session teardown must be idempotent while the tombstone is alive"
        );
        assert_eq!(
            fixture
                .mcp_transport_request(
                    &token,
                    "POST",
                    Some(&session_id),
                    Some(json!({
                        "jsonrpc": "2.0",
                        "id": "session-after-delete",
                        "method": "tools/list"
                    })),
                )
                .await?
                .status(),
            StatusCode::NOT_FOUND
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
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
        let events = body["items"].as_array().context("/v1/audit/events must return items")?;
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
        assert_eq!(body["code"], json!("forbidden"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn document_writer_token_polls_its_async_operation_but_reader_cannot() -> Result<()> {
    let fixture = GovernanceAuthFixture::create().await?;

    let result = async {
        let operation_id = Uuid::now_v7();
        sqlx::query(
            "insert into ops_async_operation
                (id, workspace_id, library_id, operation_kind, surface_kind, subject_kind)
             values ($1, $2, $3, 'replace_document', 'rest', 'content_mutation')",
        )
        .bind(operation_id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .execute(fixture.pool())
        .await?;

        // The 202 + Location contract: a principal allowed to admit the
        // mutation must be able to poll the resulting operation without the
        // ops-observability permission.
        let writer = fixture
            .mint_token_with_grants(
                Some(fixture.workspace_id),
                "operation-poll-writer",
                &[GrantSpec {
                    resource_kind: "library",
                    resource_id: fixture.library_id,
                    permission_kind: "document_write".to_string(),
                }],
            )
            .await?;
        let path = format!("/v1/ops/operations/{operation_id}");
        let (status, body) = fixture.rest_get(&writer, &path).await?;
        assert_eq!(status, StatusCode::OK, "document_write must poll its operation: {body}");
        assert_eq!(body["id"], json!(operation_id));

        // Query-only access is not part of the write contract and stays out.
        let reader = fixture
            .mint_token_with_grants(
                Some(fixture.workspace_id),
                "operation-poll-reader",
                &[GrantSpec {
                    resource_kind: "library",
                    resource_id: fixture.library_id,
                    permission_kind: "query_run".to_string(),
                }],
            )
            .await?;
        let (status, _body) = fixture.rest_get(&reader, &path).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
