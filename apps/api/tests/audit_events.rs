#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::Utc;
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        arangodb::client::ArangoClient,
        persistence::Persistence,
        repositories::{
            ai_repository, audit_repository, billing_repository, catalog_repository,
            iam_repository, query_repository, runtime_repository,
        },
    },
    interfaces::http::{auth::hash_token, router},
    services::{
        iam::audit::{
            AppendAuditEventCommand, AppendAuditEventSubjectCommand,
            AppendQueryExecutionAuditCommand,
        },
        query::service::CreateConversationCommand,
    },
};

const TEST_TOKEN_PREFIX: &str = "audit-events";
const TEST_PROVIDER_CREDENTIAL_LABEL: &str = "audit-events-provider-credential";
const TEST_MODEL_PRESET_NAME: &str = "audit-events-model-preset";
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
        let database_name = format!("audit_events_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect audit events admin postgres")?;

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
            .context("failed to reconnect audit events admin postgres for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct AuditEventsFixture {
    state: AppState,
    temp_database: TempDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
    provider_catalog_id: Uuid,
    model_catalog_id: Uuid,
    provider_kind: String,
    model_name: String,
}

impl AuditEventsFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for audit events test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect audit events postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply audit events migrations")?;

        let state = build_test_state(settings, postgres)?;
        let mut fixture = Self {
            state,
            temp_database,
            workspace_id: Uuid::nil(),
            library_id: Uuid::nil(),
            provider_catalog_id: Uuid::nil(),
            model_catalog_id: Uuid::nil(),
            provider_kind: String::new(),
            model_name: String::new(),
        };
        fixture.seed().await?;
        Ok(fixture)
    }

    async fn seed(&mut self) -> Result<()> {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &self.state.persistence.postgres,
            &format!("audit-events-workspace-{suffix}"),
            "Audit Events Workspace",
            None,
        )
        .await
        .context("failed to create audit events workspace")?;
        let library = catalog_repository::create_library(
            &self.state.persistence.postgres,
            workspace.id,
            &format!("audit-events-library-{suffix}"),
            "Audit Events Library",
            Some("audit events library"),
            None,
        )
        .await
        .context("failed to create audit events library")?;

        let provider_catalog =
            ai_repository::list_provider_catalog(&self.state.persistence.postgres)
                .await
                .context("failed to load provider catalog")?
                .into_iter()
                .next()
                .context("expected seeded provider catalog")?;
        let model_catalog = ai_repository::list_model_catalog(
            &self.state.persistence.postgres,
            Some(provider_catalog.id),
        )
        .await
        .context("failed to load model catalog")?
        .into_iter()
        .next()
        .context("expected seeded model catalog")?;

        self.workspace_id = workspace.id;
        self.library_id = library.id;
        self.provider_catalog_id = provider_catalog.id;
        self.model_catalog_id = model_catalog.id;
        self.provider_kind = provider_catalog.provider_kind;
        self.model_name = model_catalog.model_name;
        Ok(())
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    const fn pool(&self) -> &PgPool {
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
            "audit",
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

    async fn mint_workspace_admin_token(&self, label: &str) -> Result<String> {
        self.mint_token_with_grants(
            Some(self.workspace_id),
            label,
            &[GrantSpec {
                resource_kind: "workspace",
                resource_id: self.workspace_id,
                permission_kind: "workspace_admin".to_string(),
            }],
        )
        .await
    }

    async fn mint_read_only_workspace_token(&self, label: &str) -> Result<String> {
        self.mint_token_with_grants(
            Some(self.workspace_id),
            label,
            &[GrantSpec {
                resource_kind: "workspace",
                resource_id: self.workspace_id,
                permission_kind: "workspace_read".to_string(),
            }],
        )
        .await
    }

    async fn mint_audit_read_workspace_token(&self, label: &str) -> Result<String> {
        self.mint_token_with_grants(
            Some(self.workspace_id),
            label,
            &[GrantSpec {
                resource_kind: "workspace",
                resource_id: self.workspace_id,
                permission_kind: "audit_read".to_string(),
            }],
        )
        .await
    }

    async fn rest_post(
        &self,
        token: &str,
        path: &str,
        payload: Value,
    ) -> Result<(StatusCode, Value)> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("build audit events POST request"),
            )
            .await
            .with_context(|| format!("POST {path} failed"))?;
        let status = response.status();
        Ok((status, response_json(response).await?))
    }

    async fn rest_get(&self, token: &str, path: &str) -> Result<(StatusCode, Value)> {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("build audit events GET request"),
            )
            .await
            .with_context(|| format!("GET {path} failed"))?;
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
                    "id": format!("audit-{}", method.replace('/', "-")),
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

    async fn append_audit_event(
        &self,
        action_kind: &str,
        subjects: Vec<AppendAuditEventSubjectCommand>,
    ) -> Result<Uuid> {
        let event = self
            .state
            .canonical_services
            .audit
            .append_event(
                &self.state,
                AppendAuditEventCommand {
                    actor_principal_id: None,
                    surface_kind: "rest".to_string(),
                    action_kind: action_kind.to_string(),
                    request_id: Some(format!("audit-events-{action_kind}")),
                    trace_id: None,
                    result_kind: "succeeded".to_string(),
                    redacted_message: Some("canonical audit subject proof".to_string()),
                    internal_message: Some("canonical audit subject proof".to_string()),
                    subjects,
                },
            )
            .await
            .context("failed to append audit event")?;
        Ok(event.id)
    }

    async fn seed_assistant_call_audit(
        &self,
        actor_principal_id: Uuid,
        surface_kind: &str,
        request_id: Option<&str>,
        provider_kind: &str,
        model_name: &str,
        total_cost: Decimal,
    ) -> Result<Uuid> {
        let conversation = self
            .state
            .canonical_services
            .query
            .create_conversation(
                &self.state,
                CreateConversationCommand {
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    created_by_principal_id: Some(actor_principal_id),
                    title: Some(format!("[{}] audit assistant call", surface_kind)),
                },
            )
            .await
            .context("failed to create assistant conversation")?;

        let execution_id = Uuid::now_v7();
        let runtime_execution_id = Uuid::now_v7();
        let context_bundle_id = Uuid::now_v7();

        runtime_repository::create_runtime_execution(
            self.pool(),
            &runtime_repository::NewRuntimeExecution {
                id: runtime_execution_id,
                owner_kind: "query_execution",
                owner_id: execution_id,
                task_kind: "query_answer",
                surface_kind,
                contract_name: "audit-events",
                contract_version: "test",
                lifecycle_state: "completed",
                active_stage: None,
                turn_budget: 4,
                turn_count: 1,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
            },
        )
        .await
        .context("failed to create runtime execution")?;

        query_repository::create_execution(
            self.pool(),
            &query_repository::NewQueryExecution {
                execution_id,
                context_bundle_id,
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                conversation_id: conversation.id,
                request_turn_id: None,
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id,
                query_text: "Which model handled this assistant call?",
                failure_code: None,
            },
        )
        .await
        .context("failed to create query execution")?;

        let provider_catalog =
            ai_repository::get_provider_catalog_by_kind(self.pool(), provider_kind)
                .await
                .context("failed to resolve provider catalog")?
                .with_context(|| format!("missing provider catalog for {provider_kind}"))?;
        let model_catalog = ai_repository::get_model_catalog_by_provider_and_name(
            self.pool(),
            provider_kind,
            model_name,
        )
        .await
        .context("failed to resolve model catalog")?
        .with_context(|| format!("missing model catalog for {provider_kind}/{model_name}"))?;

        billing_repository::create_provider_call(
            self.pool(),
            &billing_repository::NewBillingProviderCall {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                binding_id: None,
                owning_execution_kind: "query_execution",
                owning_execution_id: execution_id,
                runtime_execution_id: Some(runtime_execution_id),
                runtime_task_kind: Some("query_answer"),
                provider_catalog_id: provider_catalog.id,
                model_catalog_id: model_catalog.id,
                call_kind: "chat_completion",
                call_state: "completed",
                completed_at: Some(Utc::now()),
            },
        )
        .await
        .context("failed to create provider call")?;

        billing_repository::upsert_execution_cost(
            self.pool(),
            &billing_repository::UpsertBillingExecutionCost {
                owning_execution_kind: "query_execution",
                owning_execution_id: execution_id,
                total_cost,
                currency_code: "USD",
                provider_call_count: 1,
            },
        )
        .await
        .context("failed to upsert execution cost")?;

        self.state
            .canonical_services
            .audit
            .append_query_execution_event(
                &self.state,
                AppendQueryExecutionAuditCommand {
                    actor_principal_id,
                    surface_kind: surface_kind.to_string(),
                    request_id: request_id.map(ToString::to_string),
                    query_session_id: conversation.id,
                    query_execution_id: execution_id,
                    runtime_execution_id: Some(runtime_execution_id),
                    context_bundle_id,
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                },
            )
            .await
            .context("failed to append assistant query audit event")?;

        Ok(execution_id)
    }
}

fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let redis = redis::Client::open(settings.redis_url.clone())
        .context("failed to create redis client for audit events state")?;
    let persistence = Persistence::for_tests(postgres, redis);
    let arango_client = Arc::new(ArangoClient::from_settings(&settings)?);
    AppState::from_dependencies(settings, persistence, arango_client)
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

async fn latest_audit_event_for_action(
    postgres: &PgPool,
    action_kind: &str,
) -> Result<audit_repository::AuditEventRow> {
    sqlx::query_as::<_, audit_repository::AuditEventRow>(
        "select
            id,
            actor_principal_id,
            surface_kind::text as surface_kind,
            action_kind,
            request_id,
            trace_id,
            result_kind::text as result_kind,
            created_at,
            redacted_message,
            internal_message
         from audit_event
         where action_kind = $1
         order by created_at desc
         limit 1",
    )
    .bind(action_kind)
    .fetch_one(postgres)
    .await
    .with_context(|| format!("failed to load latest audit event for {action_kind}"))
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn token_mint_with_library_ids_creates_library_grants() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let system_admin = fixture.mint_system_admin_token("system-admin").await?;
        let sibling_library = catalog_repository::create_library(
            fixture.pool(),
            fixture.workspace_id,
            "audit-events-sibling-library",
            "Audit Events Sibling Library",
            Some("audit events sibling library"),
            None,
        )
        .await
        .context("failed to create sibling library")?;
        let label = format!("mint-multi-lib-{}", Uuid::now_v7());

        let (status, body) = fixture
            .rest_post(
                &system_admin,
                "/v1/iam/tokens",
                json!({
                    "workspaceId": fixture.workspace_id,
                    "label": label,
                    "libraryIds": [fixture.library_id, sibling_library.id],
                    "permissionKinds": ["library_read", "document_read"]
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        let token_principal_id =
            body["apiToken"]["principalId"].as_str().context("expected token principal id")?;
        let token_principal_id = Uuid::parse_str(token_principal_id)?;

        let grants =
            iam_repository::list_grants_by_principal(fixture.pool(), token_principal_id).await?;
        assert_eq!(grants.len(), 4);
        assert!(grants.iter().all(|grant| grant.resource_kind == "library"));
        let permission_kinds: Vec<String> =
            grants.iter().map(|grant| grant.permission_kind.clone()).collect();
        assert!(permission_kinds.iter().any(|value| value == "library_read"));
        assert!(permission_kinds.iter().any(|value| value == "document_read"));
        let library_ids: Vec<Uuid> = grants.iter().map(|grant| grant.resource_id).collect();
        assert!(library_ids.contains(&fixture.library_id));
        assert!(library_ids.contains(&sibling_library.id));

        let mint_subjects = audit_repository::list_audit_event_subjects(
            fixture.pool(),
            latest_audit_event_for_action(fixture.pool(), "iam.api_token.mint").await?.id,
        )
        .await?;
        assert_eq!(mint_subjects.len(), 1);
        assert_eq!(mint_subjects[0].subject_id, token_principal_id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn token_mint_with_library_ids_rejects_cross_workspace_library_ids() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let system_admin = fixture.mint_system_admin_token("system-admin").await?;
        let foreign_workspace = catalog_repository::create_workspace(
            fixture.pool(),
            "audit-events-foreign-workspace",
            "Audit Events Foreign Workspace",
            None,
        )
        .await
        .context("failed to create foreign workspace")?;
        let foreign_library = catalog_repository::create_library(
            fixture.pool(),
            foreign_workspace.id,
            "audit-events-foreign-library",
            "Audit Events Foreign Library",
            Some("audit events foreign library"),
            None,
        )
        .await
        .context("failed to create foreign library")?;

        let label = format!("mint-multi-lib-fail-{}", Uuid::now_v7());
        let before_count: i64 =
            sqlx::query_scalar::<_, i64>("select count(*) from iam_api_token where label = $1")
                .bind(&label)
                .fetch_one(fixture.pool())
                .await?;
        let (status, body) = fixture
            .rest_post(
                &system_admin,
                "/v1/iam/tokens",
                json!({
                    "workspaceId": fixture.workspace_id,
                    "label": label,
                    "libraryIds": [fixture.library_id, foreign_library.id],
                    "permissionKinds": ["library_read"]
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["errorKind"].as_str().context("expected api error kind")?, "bad_request");

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
#[ignore = "requires local postgres, redis, and arango services"]
async fn token_mint_with_library_ids_requires_permissions() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let system_admin = fixture.mint_system_admin_token("system-admin").await?;
        let label = format!("mint-multi-lib-missing-permissions-{}", Uuid::now_v7());

        let before_count: i64 =
            sqlx::query_scalar::<_, i64>("select count(*) from iam_api_token where label = $1")
                .bind(&label)
                .fetch_one(fixture.pool())
                .await?;
        let (status, body) = fixture
            .rest_post(
                &system_admin,
                "/v1/iam/tokens",
                json!({
                    "workspaceId": fixture.workspace_id,
                    "label": label,
                    "libraryIds": [fixture.library_id],
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["errorKind"].as_str().context("expected api error kind")?, "bad_request");

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
#[ignore = "requires local postgres, redis, and arango services"]
async fn token_mint_with_library_ids_rejects_invalid_permissions() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let system_admin = fixture.mint_system_admin_token("system-admin").await?;
        let label = format!("mint-multi-lib-invalid-permission-{uuid}", uuid = Uuid::now_v7());

        let before_count: i64 =
            sqlx::query_scalar::<_, i64>("select count(*) from iam_api_token where label = $1")
                .bind(&label)
                .fetch_one(fixture.pool())
                .await?;
        let (status, body) = fixture
            .rest_post(
                &system_admin,
                "/v1/iam/tokens",
                json!({
                    "workspaceId": fixture.workspace_id,
                    "label": label,
                    "libraryIds": [fixture.library_id],
                    "permissionKinds": ["iam_admin"]
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["errorKind"].as_str().context("expected api error kind")?, "bad_request");

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
#[ignore = "requires local postgres, redis, and arango services"]
async fn token_mint_and_revoke_append_audit_events_with_api_token_subjects() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let system_admin = fixture.mint_system_admin_token("system-admin").await?;

        let (status, body) = fixture
            .rest_post(
                &system_admin,
                "/v1/iam/tokens",
                json!({
                    "workspaceId": fixture.workspace_id,
                    "label": "minted-audit-token"
                }),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        let token_principal_id =
            body["apiToken"]["principalId"].as_str().context("expected token principal id")?;
        let token_principal_id = Uuid::parse_str(token_principal_id)?;

        let mint_event =
            latest_audit_event_for_action(fixture.pool(), "iam.api_token.mint").await?;
        assert_eq!(mint_event.result_kind, "succeeded");
        let mint_subjects =
            audit_repository::list_audit_event_subjects(fixture.pool(), mint_event.id).await?;
        assert_eq!(mint_subjects.len(), 1);
        assert_eq!(mint_subjects[0].subject_kind, "api_token");
        assert_eq!(mint_subjects[0].subject_id, token_principal_id);
        assert_eq!(mint_subjects[0].workspace_id, Some(fixture.workspace_id));

        let (status, _) = fixture
            .rest_post(
                &system_admin,
                &format!("/v1/iam/tokens/{token_principal_id}/revoke"),
                json!({}),
            )
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let revoke_event =
            latest_audit_event_for_action(fixture.pool(), "iam.api_token.revoke").await?;
        assert_eq!(revoke_event.result_kind, "succeeded");
        let revoke_subjects =
            audit_repository::list_audit_event_subjects(fixture.pool(), revoke_event.id).await?;
        assert_eq!(revoke_subjects.len(), 1);
        assert_eq!(revoke_subjects[0].subject_kind, "api_token");
        assert_eq!(revoke_subjects[0].subject_id, token_principal_id);
        assert_eq!(revoke_subjects[0].workspace_id, Some(fixture.workspace_id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn governance_actions_and_denials_append_expected_audit_subjects() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let workspace_admin = fixture.mint_workspace_admin_token("workspace-admin").await?;
        let read_only = fixture.mint_read_only_workspace_token("workspace-readonly").await?;

        let credential_response = fixture
            .rest_post(
                &workspace_admin,
                "/v1/ai/credentials",
                json!({
                    "workspaceId": fixture.workspace_id,
                    "providerCatalogId": fixture.provider_catalog_id,
                    "label": TEST_PROVIDER_CREDENTIAL_LABEL,
                    "apiKey": "audit-events-provider-key"
                }),
            )
            .await?;
        assert_eq!(credential_response.0, StatusCode::OK);
        let credential_id = Uuid::parse_str(
            credential_response.1["id"].as_str().context("expected provider credential id")?,
        )?;
        let credential_event =
            latest_audit_event_for_action(fixture.pool(), "ai.provider_credential.create").await?;
        assert_eq!(credential_event.result_kind, "succeeded");
        let credential_subjects =
            audit_repository::list_audit_event_subjects(fixture.pool(), credential_event.id)
                .await?;
        assert_eq!(credential_subjects.len(), 1);
        assert_eq!(credential_subjects[0].subject_kind, "provider_credential");
        assert_eq!(credential_subjects[0].subject_id, credential_id);

        let preset = ai_repository::create_model_preset(
            fixture.pool(),
            "workspace",
            Some(fixture.workspace_id),
            None,
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
        .context("failed to create model preset for audit events test")?;

        let binding_response = fixture
            .rest_post(
                &workspace_admin,
                "/v1/ai/library-bindings",
                json!({
                    "workspaceId": fixture.workspace_id,
                    "libraryId": fixture.library_id,
                    "bindingPurpose": TEST_BINDING_PURPOSE,
                    "providerCredentialId": credential_id,
                    "modelPresetId": preset.id
                }),
            )
            .await?;
        assert_eq!(binding_response.0, StatusCode::OK);
        let binding_id = Uuid::parse_str(
            binding_response.1["id"].as_str().context("expected library binding id")?,
        )?;
        let binding_event =
            latest_audit_event_for_action(fixture.pool(), "ai.library_binding.create").await?;
        assert_eq!(binding_event.result_kind, "succeeded");
        let binding_subjects =
            audit_repository::list_audit_event_subjects(fixture.pool(), binding_event.id).await?;
        assert_eq!(binding_subjects.len(), 1);
        assert_eq!(binding_subjects[0].subject_kind, "library_binding");
        assert_eq!(binding_subjects[0].subject_id, binding_id);
        assert_eq!(binding_subjects[0].library_id, Some(fixture.library_id));

        let create_library_response = fixture
            .mcp_call(
                &workspace_admin,
                "tools/call",
                Some(json!({
                    "name": "create_library",
                    "arguments": {
                        "workspaceId": fixture.workspace_id,
                        "name": "Audit Events MCP Library"
                    }
                })),
            )
            .await?;
        assert_eq!(create_library_response["result"]["isError"], json!(false));
        let created_library_id = Uuid::parse_str(
            create_library_response["result"]["structuredContent"]["library"]["libraryId"]
                .as_str()
                .context("expected created library id")?,
        )?;
        let library_event =
            latest_audit_event_for_action(fixture.pool(), "catalog.library.create").await?;
        assert_eq!(library_event.result_kind, "succeeded");
        let library_subjects =
            audit_repository::list_audit_event_subjects(fixture.pool(), library_event.id).await?;
        assert_eq!(library_subjects.len(), 1);
        assert_eq!(library_subjects[0].subject_kind, "library");
        assert_eq!(library_subjects[0].subject_id, created_library_id);
        assert_eq!(library_subjects[0].workspace_id, Some(fixture.workspace_id));

        let (status, body) = fixture
            .rest_get(&workspace_admin, &format!("/v1/audit/events?libraryId={created_library_id}"))
            .await?;
        assert_eq!(status, StatusCode::OK);
        let events =
            body["items"].as_array().context("audit events response must include items")?;
        let library_event_response = events
            .iter()
            .find(|event| event["id"] == json!(library_event.id))
            .context("expected MCP library create event in audit feed")?;
        let subjects = library_event_response["subjects"]
            .as_array()
            .context("audit event subjects must be an array")?;
        let subject = subjects
            .iter()
            .find(|subject| subject["subjectKind"] == json!("library"))
            .context("expected library subject in MCP audit response")?;
        assert_eq!(subject["subjectId"], json!(created_library_id));
        assert_eq!(subject["libraryId"], json!(created_library_id));
        assert_eq!(subject["workspaceId"], json!(fixture.workspace_id));

        let denied_response = fixture
            .rest_post(
                &read_only,
                "/v1/ai/credentials",
                json!({
                    "workspaceId": fixture.workspace_id,
                    "providerCatalogId": fixture.provider_catalog_id,
                    "label": "denied-credential",
                    "apiKey": "audit-events-denied-key"
                }),
            )
            .await?;
        assert_eq!(denied_response.0, StatusCode::UNAUTHORIZED);
        let denied_event =
            latest_audit_event_for_action(fixture.pool(), "ai.provider_credential.create").await?;
        assert_eq!(denied_event.result_kind, "rejected");
        let denied_subjects =
            audit_repository::list_audit_event_subjects(fixture.pool(), denied_event.id).await?;
        assert_eq!(denied_subjects.len(), 1);
        assert_eq!(denied_subjects[0].subject_kind, "workspace");
        assert_eq!(denied_subjects[0].subject_id, fixture.workspace_id);
        assert_eq!(denied_subjects[0].workspace_id, Some(fixture.workspace_id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn audit_events_surface_assistant_models_cost_and_runtime_subjects() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let workspace_admin = fixture.mint_workspace_admin_token("workspace-admin").await?;
        let actor_principal_id = Uuid::now_v7();
        let request_id = "assistant-mcp-audit";
        let execution_id = fixture
            .seed_assistant_call_audit(
                actor_principal_id,
                "mcp",
                Some(request_id),
                &fixture.provider_kind,
                &fixture.model_name,
                Decimal::new(123, 4),
            )
            .await?;

        let (status, body) = fixture
            .rest_get(
                &workspace_admin,
                &format!("/v1/audit/events?libraryId={}&includeAssistant=true", fixture.library_id),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);

        let events =
            body["items"].as_array().context("audit events response must include items")?;
        let event = events
            .iter()
            .find(|event| event["requestId"] == json!(request_id))
            .context("expected assistant audit event in feed")?;

        assert_eq!(event["actionKind"], json!("query.execution.run"));
        assert_eq!(event["surfaceKind"], json!("mcp"));
        assert_eq!(event["actorPrincipalId"], json!(actor_principal_id));
        assert_eq!(event["assistantCall"]["queryExecutionId"], json!(execution_id));
        assert_eq!(event["assistantCall"]["totalCost"], json!("0.0123"));
        assert_eq!(event["assistantCall"]["currencyCode"], json!("USD"));
        assert_eq!(event["assistantCall"]["providerCallCount"], json!(1));
        assert_eq!(
            event["assistantCall"]["models"][0]["providerKind"],
            json!(fixture.provider_kind.as_str())
        );
        assert_eq!(
            event["assistantCall"]["models"][0]["modelName"],
            json!(fixture.model_name.as_str())
        );

        let subjects =
            event["subjects"].as_array().context("audit event subjects must be an array")?;
        assert!(
            subjects.iter().any(|subject| subject["subjectKind"] == json!("runtime_execution")),
            "assistant audit event must include runtime_execution subject"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn audit_events_hide_assistant_costs_without_usage_read() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let audit_reader = fixture.mint_audit_read_workspace_token("audit-reader").await?;
        let request_id = "assistant-audit-redacted";
        fixture
            .seed_assistant_call_audit(
                Uuid::now_v7(),
                "rest",
                Some(request_id),
                &fixture.provider_kind,
                &fixture.model_name,
                Decimal::new(55, 4),
            )
            .await?;

        let (status, body) = fixture
            .rest_get(
                &audit_reader,
                &format!("/v1/audit/events?libraryId={}&includeAssistant=true", fixture.library_id),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);

        let events =
            body["items"].as_array().context("audit events response must include items")?;
        let event = events
            .iter()
            .find(|event| event["requestId"] == json!(request_id))
            .context("expected assistant audit event in feed")?;

        assert!(
            event.get("assistantCall").is_none(),
            "assistantCall must be hidden without usage_read"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn canonical_audit_subjects_surface_query_and_knowledge_ids_through_http() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let system_admin = fixture.mint_system_admin_token("canonical-subjects").await?;

        let query_session_id = Uuid::now_v7();
        let query_execution_id = Uuid::now_v7();
        let knowledge_document_id = Uuid::now_v7();
        let knowledge_bundle_id = Uuid::now_v7();
        let async_operation_id = Uuid::now_v7();

        let audit_event_id = fixture
            .append_audit_event(
                "governance.canonical_subjects.proof",
                vec![
                    AppendAuditEventSubjectCommand {
                        subject_kind: "query_session".to_string(),
                        subject_id: query_session_id,
                        workspace_id: Some(fixture.workspace_id),
                        library_id: Some(fixture.library_id),
                        document_id: None,
                    },
                    AppendAuditEventSubjectCommand {
                        subject_kind: "query_execution".to_string(),
                        subject_id: query_execution_id,
                        workspace_id: Some(fixture.workspace_id),
                        library_id: Some(fixture.library_id),
                        document_id: None,
                    },
                    AppendAuditEventSubjectCommand {
                        subject_kind: "knowledge_document".to_string(),
                        subject_id: knowledge_document_id,
                        workspace_id: Some(fixture.workspace_id),
                        library_id: Some(fixture.library_id),
                        document_id: Some(knowledge_document_id),
                    },
                    AppendAuditEventSubjectCommand {
                        subject_kind: "knowledge_bundle".to_string(),
                        subject_id: knowledge_bundle_id,
                        workspace_id: Some(fixture.workspace_id),
                        library_id: Some(fixture.library_id),
                        document_id: None,
                    },
                    AppendAuditEventSubjectCommand {
                        subject_kind: "async_operation".to_string(),
                        subject_id: async_operation_id,
                        workspace_id: Some(fixture.workspace_id),
                        library_id: Some(fixture.library_id),
                        document_id: None,
                    },
                ],
            )
            .await?;

        let raw_subjects =
            audit_repository::list_audit_event_subjects(fixture.pool(), audit_event_id).await?;
        assert_eq!(raw_subjects.len(), 5);
        assert!(raw_subjects.iter().any(|subject| subject.subject_kind == "query_session"));
        assert!(raw_subjects.iter().any(|subject| subject.subject_kind == "query_execution"));
        assert!(raw_subjects.iter().any(|subject| subject.subject_kind == "knowledge_document"));
        assert!(raw_subjects.iter().any(|subject| subject.subject_kind == "knowledge_bundle"));
        assert!(raw_subjects.iter().any(|subject| subject.subject_kind == "async_operation"));

        let filters = [
            ("querySessionId", query_session_id, "query_session", "querySessionId"),
            ("queryExecutionId", query_execution_id, "query_execution", "queryExecutionId"),
            (
                "knowledgeDocumentId",
                knowledge_document_id,
                "knowledge_document",
                "knowledgeDocumentId",
            ),
            ("contextBundleId", knowledge_bundle_id, "knowledge_bundle", "contextBundleId"),
            ("asyncOperationId", async_operation_id, "async_operation", "asyncOperationId"),
        ];

        for (query_param, subject_id, subject_kind, canonical_field) in filters {
            let (status, body) = fixture
                .rest_get(&system_admin, &format!("/v1/audit/events?{query_param}={subject_id}"))
                .await?;
            assert_eq!(status, StatusCode::OK);
            let events =
                body["items"].as_array().context("audit events response must include items")?;
            assert_eq!(events.len(), 1, "expected one audit event for {query_param}");

            let event = &events[0];
            assert_eq!(event["id"], json!(audit_event_id));
            let subjects =
                event["subjects"].as_array().context("audit event subjects must be an array")?;
            let subject = subjects
                .iter()
                .find(|subject| subject["subjectKind"] == json!(subject_kind))
                .with_context(|| format!("missing {subject_kind} subject in audit response"))?;
            assert_eq!(subject["subjectId"], json!(subject_id));
            assert_eq!(subject[canonical_field], json!(subject_id));
            assert_eq!(subject["workspaceId"], json!(fixture.workspace_id));
            assert_eq!(subject["libraryId"], json!(fixture.library_id));
        }

        let (status, body) = fixture.rest_get(&system_admin, "/v1/audit/events").await?;
        assert_eq!(status, StatusCode::OK);
        let events =
            body["items"].as_array().context("audit events response must include items")?;
        assert!(
            events.iter().any(|event| event["id"] == json!(audit_event_id)),
            "canonical audit proof event must be visible in the audit feed"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn canonical_agent_memory_audit_subjects_surface_knowledge_and_async_operation_ids()
-> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let system_admin = fixture.mint_system_admin_token("canonical-agent-memory").await?;
        let knowledge_document_id = Uuid::now_v7();
        let async_operation_id = Uuid::now_v7();

        let audit_event_id = fixture
            .append_audit_event(
                "agent.memory.upload",
                vec![
                    AppendAuditEventSubjectCommand {
                        subject_kind: "knowledge_document".to_string(),
                        subject_id: knowledge_document_id,
                        workspace_id: Some(fixture.workspace_id),
                        library_id: Some(fixture.library_id),
                        document_id: Some(knowledge_document_id),
                    },
                    AppendAuditEventSubjectCommand {
                        subject_kind: "async_operation".to_string(),
                        subject_id: async_operation_id,
                        workspace_id: Some(fixture.workspace_id),
                        library_id: Some(fixture.library_id),
                        document_id: None,
                    },
                ],
            )
            .await?;

        let raw_subjects =
            audit_repository::list_audit_event_subjects(fixture.pool(), audit_event_id).await?;
        assert_eq!(raw_subjects.len(), 2);
        assert!(raw_subjects.iter().any(|subject| subject.subject_kind == "knowledge_document"));
        assert!(raw_subjects.iter().any(|subject| subject.subject_kind == "async_operation"));

        let (status, body) = fixture
            .rest_get(
                &system_admin,
                &format!("/v1/audit/events?knowledgeDocumentId={knowledge_document_id}"),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        let events =
            body["items"].as_array().context("audit events response must include items")?;
        let event = events
            .iter()
            .find(|event| event["id"] == json!(audit_event_id))
            .context("expected agent.memory.upload event in audit feed by knowledgeDocumentId")?;
        let subjects =
            event["subjects"].as_array().context("audit event subjects must be an array")?;
        let knowledge_document_subject = subjects
            .iter()
            .find(|subject| subject["subjectKind"] == json!("knowledge_document"))
            .context("expected knowledge_document subject in audit response")?;
        assert_eq!(knowledge_document_subject["knowledgeDocumentId"], json!(knowledge_document_id));
        assert_eq!(knowledge_document_subject["documentId"], json!(knowledge_document_id));

        let (status, body) = fixture
            .rest_get(
                &system_admin,
                &format!("/v1/audit/events?asyncOperationId={async_operation_id}"),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        let events =
            body["items"].as_array().context("audit events response must include items")?;
        let event = events
            .iter()
            .find(|event| event["id"] == json!(audit_event_id))
            .context("expected agent.memory.upload event in audit feed by asyncOperationId")?;
        let subjects =
            event["subjects"].as_array().context("audit event subjects must be an array")?;
        let async_operation_subject = subjects
            .iter()
            .find(|subject| subject["subjectKind"] == json!("async_operation"))
            .context("expected async_operation subject in audit response")?;
        assert_eq!(async_operation_subject["asyncOperationId"], json!(async_operation_id));
        assert_eq!(async_operation_subject["workspaceId"], json!(fixture.workspace_id));
        assert_eq!(async_operation_subject["libraryId"], json!(fixture.library_id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
