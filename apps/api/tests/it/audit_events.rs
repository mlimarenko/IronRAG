use crate::{database_url, http_response_support, mcp_tool_call_support};

use database_url::database_url_with_name;
use http_response_support::response_json;

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::Engine as _;
use chrono::Utc;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, seed_clean_v1_ai_catalog, state::AppState},
    infra::{
        persistence::Persistence,
        repositories::{
            ai_repository, audit_repository, billing_repository, catalog_repository,
            iam_repository::{self, SystemRole},
            ops_repository, query_repository, runtime_repository,
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
    shared::auth_tokens::{
        build_session_cookie_value, hash_session_secret, mint_plaintext_api_token,
        mint_plaintext_session_secret,
    },
};

const TEST_PROVIDER_CREDENTIAL_LABEL: &str = "audit-events-provider-credential";

use crate::iam_token_support::{GrantSpec, create_principal_grants};

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = database_url_with_name(base_database_url, "postgres")?;
        let database_name = format!("audit_events_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect audit events admin postgres")?;

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
            database_url: database_url_with_name(base_database_url, &database_name)?,
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
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
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

/// The actor, surface and provider call identifying one seeded assistant audit
/// event.
struct AssistantCallAuditSeed<'a> {
    actor_principal_id: Uuid,
    surface_kind: &'a str,
    request_id: Option<&'a str>,
    provider_kind: &'a str,
    model_name: &'a str,
    total_cost: Decimal,
}

impl AuditEventsFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for audit events test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;
        // Credentialed AI-account writes fail closed without an encryption
        // key; use the same deterministic test key as governance_auth.
        settings.credential_master_key =
            Some(base64::engine::general_purpose::STANDARD.encode([37_u8; 32]));

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect audit events postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply audit events migrations")?;
        // Migrations create the catalog tables but leave them empty; startup
        // seeds them separately. The fixture below resolves a provider from
        // that catalog, so it has to run the same seed.
        seed_clean_v1_ai_catalog(&postgres)
            .await
            .context("failed to seed audit events AI catalog")?;

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
        // `is_well_formed_api_token` rejects anything that is not
        // `inrg_` + 32 hex digits, and it runs before the secret is ever
        // hashed — so a hand-built plaintext returns 401 no matter which
        // grants the principal holds.
        let plaintext = mint_plaintext_api_token();
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
        // Grants alone are not enough: a workspace-scoped request also checks
        // that the principal belongs to the workspace, and answers 401 when it
        // does not.
        if let Some(workspace_id) = token_workspace_id {
            iam_repository::upsert_workspace_membership(
                self.pool(),
                workspace_id,
                token.principal_id,
                "active",
            )
            .await
            .with_context(|| format!("failed to create workspace membership for {label}"))?;
        }
        create_principal_grants(self.pool(), token.principal_id, label, grants).await?;
        Ok(plaintext)
    }

    /// A token that may review this workspace's audit feed.
    ///
    /// It used to be minted as a system administrator, which no API token can
    /// be: `is_system_admin` comes from the *user* row behind the principal,
    /// and `iam_api_token_secret` can only reference an `iam_api_token`
    /// principal, so the grant was unreachable and every request 401'd. A
    /// caller with `audit_read` on its single visible workspace reaches the
    /// same feed through `authorize_mcp_audit_review`.
    async fn mint_audit_review_token(&self, label: &str) -> Result<String> {
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

    /// Signs in a real user with the `admin` system role and returns its
    /// session cookie.
    ///
    /// `is_system_admin` is read from the user row behind the principal, and
    /// `iam_api_token_secret` can only reference an `iam_api_token` principal,
    /// so IAM administration is reachable only through a session — never
    /// through a bearer token, whatever grants it holds.
    async fn mint_admin_session(&self, label: &str) -> Result<String> {
        let suffix = Uuid::now_v7().simple();
        let user = iam_repository::create_user(
            self.pool(),
            &format!("{label}-{suffix}"),
            &format!("{label}-{suffix}@example.invalid"),
            "Audit Events Administrator",
            &hash_token(&format!("password-{suffix}")),
            SystemRole::Admin,
        )
        .await
        .with_context(|| format!("failed to create admin user for {label}"))?;
        let secret = mint_plaintext_session_secret();
        let session = iam_repository::create_session(
            self.pool(),
            user.principal_id,
            &hash_session_secret(&secret),
            Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .with_context(|| format!("failed to create admin session for {label}"))?;
        Ok(format!(
            "{}={}",
            self.state.ui_session_cookie.name,
            build_session_cookie_value(session.id, &secret)
        ))
    }

    /// Creates the durable issuer-scoped owner shape delivered by IRG-79,
    /// while keeping this test independent of the browser OIDC callback.
    async fn mint_oidc_admin_session(
        &self,
        label: &str,
        issuer: &str,
        subject: &str,
    ) -> Result<(Uuid, String)> {
        let suffix = Uuid::now_v7().simple();
        let user = iam_repository::create_user(
            self.pool(),
            &format!("{label}-{suffix}"),
            &format!("{label}-{suffix}@example.invalid"),
            "OIDC Audit Events Administrator",
            &hash_token(&format!("password-{suffix}")),
            SystemRole::Admin,
        )
        .await
        .with_context(|| format!("failed to create OIDC admin user for {label}"))?;
        sqlx::query(
            "update iam_user
             set auth_provider_kind = $2,
                 external_subject = $3
             where principal_id = $1",
        )
        .bind(user.principal_id)
        .bind(issuer)
        .bind(subject)
        .execute(self.pool())
        .await
        .with_context(|| format!("failed to attach OIDC identity for {label}"))?;
        let secret = mint_plaintext_session_secret();
        let session = iam_repository::create_session(
            self.pool(),
            user.principal_id,
            &hash_session_secret(&secret),
            Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .with_context(|| format!("failed to create OIDC admin session for {label}"))?;
        Ok((
            user.principal_id,
            format!(
                "{}={}",
                self.state.ui_session_cookie.name,
                build_session_cookie_value(session.id, &secret)
            ),
        ))
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        credential: (header::HeaderName, String),
        payload: Option<Value>,
    ) -> Result<(StatusCode, Value)> {
        let (credential_header, credential_value) = credential;
        let mut builder =
            Request::builder().method(method).uri(path).header(credential_header, credential_value);
        let body = match payload {
            Some(payload) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(payload.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .app()
            .oneshot(builder.body(body).context("failed to build audit events request")?)
            .await
            .with_context(|| format!("{method} {path} failed"))?;
        let status = response.status();
        Ok((status, response_json(response).await?))
    }

    fn bearer(token: &str) -> (header::HeaderName, String) {
        (header::AUTHORIZATION, format!("Bearer {token}"))
    }

    fn cookie(session_cookie: &str) -> (header::HeaderName, String) {
        (header::COOKIE, session_cookie.to_string())
    }

    async fn rest_post(
        &self,
        token: &str,
        path: &str,
        payload: Value,
    ) -> Result<(StatusCode, Value)> {
        self.send("POST", path, Self::bearer(token), Some(payload)).await
    }

    async fn rest_get(&self, token: &str, path: &str) -> Result<(StatusCode, Value)> {
        self.send("GET", path, Self::bearer(token), None).await
    }

    async fn admin_post(
        &self,
        session_cookie: &str,
        path: &str,
        payload: Value,
    ) -> Result<(StatusCode, Value)> {
        self.send("POST", path, Self::cookie(session_cookie), Some(payload)).await
    }

    async fn admin_get(&self, session_cookie: &str, path: &str) -> Result<(StatusCode, Value)> {
        self.send("GET", path, Self::cookie(session_cookie), None).await
    }

    async fn admin_patch(
        &self,
        session_cookie: &str,
        path: &str,
        payload: Value,
    ) -> Result<(StatusCode, Value)> {
        self.send("PATCH", path, Self::cookie(session_cookie), Some(payload)).await
    }

    async fn rest_put(
        &self,
        token: &str,
        path: &str,
        payload: Value,
    ) -> Result<(StatusCode, Value)> {
        self.send("PUT", path, Self::bearer(token), Some(payload)).await
    }

    async fn admin_delete(&self, session_cookie: &str, path: &str) -> Result<StatusCode> {
        Ok(self.send("DELETE", path, Self::cookie(session_cookie), None).await?.0)
    }

    async fn mcp_diagnostics_call(
        &self,
        token: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value> {
        mcp_tool_call_support::call_rpc(
            self.app(),
            "/v1/mcp/diagnostics",
            token,
            &format!("audit-diag-{}", method.replace('/', "-")),
            method,
            params.unwrap_or(Value::Null),
        )
        .await
    }

    async fn append_audit_event(
        &self,
        action_kind: &str,
        subjects: Vec<AppendAuditEventSubjectCommand>,
    ) -> Result<Uuid> {
        let event = self
            .state
            .application_services
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
                    redacted_message: Some("authoritative audit subject proof".to_string()),
                    internal_message: Some("authoritative audit subject proof".to_string()),
                    subjects,
                },
            )
            .await
            .context("failed to append audit event")?;
        Ok(event.id)
    }

    async fn seed_assistant_call_audit(&self, seed: AssistantCallAuditSeed<'_>) -> Result<Uuid> {
        let conversation = self
            .state
            .application_services
            .query
            .create_conversation(
                &self.state,
                CreateConversationCommand {
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    created_by_principal_id: Some(seed.actor_principal_id),
                    title: Some(format!("[{}] audit assistant call", seed.surface_kind)),
                    request_surface: if seed.surface_kind == "mcp" {
                        "mcp".to_string()
                    } else {
                        "ui".to_string()
                    },
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
                surface_kind: seed.surface_kind,
                contract_name: "audit-events",
                contract_version: "test",
                lifecycle_state: "completed",
                active_stage: None,
                turn_budget: 4,
                turn_count: 1,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
                parent_execution_id: None,
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
            ai_repository::get_provider_catalog_by_kind(self.pool(), seed.provider_kind)
                .await
                .context("failed to resolve provider catalog")?
                .with_context(|| format!("missing provider catalog for {}", seed.provider_kind))?;
        let model_catalog = ai_repository::get_model_catalog_by_provider_and_name(
            self.pool(),
            seed.provider_kind,
            seed.model_name,
        )
        .await
        .context("failed to resolve model catalog")?
        .with_context(|| {
            format!("missing model catalog for {}/{}", seed.provider_kind, seed.model_name)
        })?;

        billing_repository::create_provider_call(
            self.pool(),
            &billing_repository::NewBillingProviderCall {
                id: Uuid::now_v7(),
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
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                knowledge_document_id: None,
                total_cost: seed.total_cost,
                currency_code: "USD",
                provider_call_count: 1,
                unpriced_provider_call_count: 0,
            },
        )
        .await
        .context("failed to upsert execution cost")?;

        // The audit cost surface only shows settled rollups
        // (applied_generation == dirty_generation); mark the seeded cost
        // clean the way the rollup worker would.
        sqlx::query(
            "insert into billing_execution_cost_rollup_state (
                owning_execution_kind, owning_execution_id, workspace_id,
                library_id, dirty_generation, applied_generation, dirty_at,
                repair_attempts, next_attempt_at, last_error
             ) values (
                'query_execution'::billing_owning_execution_kind, $1, $2, $3,
                1, 1, now(), 0, now(), null
             )
             on conflict (owning_execution_kind, owning_execution_id)
             do update set dirty_generation = 1, applied_generation = 1",
        )
        .bind(execution_id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .execute(self.pool())
        .await
        .context("failed to settle execution cost rollup state")?;

        self.state
            .application_services
            .audit
            .append_query_execution_event(
                &self.state,
                AppendQueryExecutionAuditCommand {
                    actor_principal_id: seed.actor_principal_id,
                    surface_kind: seed.surface_kind.to_string(),
                    request_id: seed.request_id.map(ToString::to_string),
                    query_session_id: conversation.id,
                    query_execution_id: execution_id,
                    runtime_execution_id: Some(runtime_execution_id),
                    context_bundle_id,
                    workspace_id: self.workspace_id,
                    library_id: self.library_id,
                    question_preview: None,
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
    AppState::from_dependencies(settings, persistence)
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
#[ignore = "requires local postgres and redis services"]
async fn service_identity_create_requires_oidc_owner() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let password_admin = fixture.mint_admin_session("password-service-owner").await?;
        let (status, body) = fixture
            .admin_post(
                &password_admin,
                "/v1/iam/service-identities",
                json!({
                    "idempotencyKey": "password-owner-must-be-rejected",
                    "workspaceId": fixture.workspace_id,
                    "label": "forbidden-password-owned-service",
                    "libraryIds": [fixture.library_id],
                    "permissionKinds": ["library_read", "query_run"]
                }),
            )
            .await?;

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
async fn integration_provisioner_owns_catalog_and_mints_only_bounded_child_keys() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let owner_issuer = format!("oidc:{}", "d".repeat(64));
        let (owner_principal_id, owner_session) = fixture
            .mint_oidc_admin_session(
                "integration-provisioner-owner",
                &owner_issuer,
                "integration-owner-subject",
            )
            .await?;
        let (bootstrap_status, bootstrap_body) = fixture
            .admin_post(
                &owner_session,
                "/v1/iam/service-identities",
                json!({
                    "idempotencyKey": "bootstrap-integration-provisioner",
                    "label": "integration-provisioner",
                    "provisioningAuthority": "integration"
                }),
            )
            .await?;
        assert_eq!(bootstrap_status, StatusCode::OK);
        let provisioner_credential =
            bootstrap_body["credential"].as_str().context("expected provisioning credential")?;
        let provisioner_principal_id = Uuid::parse_str(
            bootstrap_body["serviceIdentity"]["principalId"]
                .as_str()
                .context("expected provisioning principal id")?,
        )?;
        assert_eq!(bootstrap_body["serviceIdentity"]["workspaceId"], Value::Null);
        let foreign_owner_issuer = format!("oidc:{}", "e".repeat(64));
        let (_, foreign_owner_session) = fixture
            .mint_oidc_admin_session(
                "foreign-integration-provisioner-owner",
                &foreign_owner_issuer,
                "foreign-integration-owner-subject",
            )
            .await?;
        let (foreign_bootstrap_status, foreign_bootstrap_body) = fixture
            .admin_post(
                &foreign_owner_session,
                "/v1/iam/service-identities",
                json!({
                    "idempotencyKey": "bootstrap-foreign-integration-provisioner",
                    "label": "foreign-integration-provisioner",
                    "provisioningAuthority": "integration"
                }),
            )
            .await?;
        assert_eq!(foreign_bootstrap_status, StatusCode::OK);
        let foreign_provisioner_credential = foreign_bootstrap_body["credential"]
            .as_str()
            .context("expected foreign provisioning credential")?;

        let legacy_workspace = catalog_repository::create_workspace(
            fixture.pool(),
            "legacy-botron-workspace",
            "Legacy Botron Workspace",
            Some(owner_principal_id),
        )
        .await?;
        let legacy_library = catalog_repository::create_library(
            fixture.pool(),
            legacy_workspace.id,
            "legacy-botron-library",
            "Legacy Botron Library",
            None,
            Some(owner_principal_id),
        )
        .await?;
        let legacy_sibling_library = catalog_repository::create_library(
            fixture.pool(),
            legacy_workspace.id,
            "legacy-botron-sibling-library",
            "Legacy Botron Sibling Library",
            None,
            Some(owner_principal_id),
        )
        .await?;
        let omitted_library = ironrag_backend::services::maintenance::service_identity_schema::assign_integration_provisioning_ownership(
            fixture.pool(),
            false,
            provisioner_principal_id,
            legacy_workspace.id,
            vec![legacy_library.id],
        )
        .await;
        assert!(omitted_library.is_err(), "an omitted workspace library must block ownership transfer");
        let dry_run = ironrag_backend::services::maintenance::service_identity_schema::assign_integration_provisioning_ownership(
            fixture.pool(),
            false,
            provisioner_principal_id,
            legacy_workspace.id,
            vec![legacy_library.id, legacy_sibling_library.id],
        )
        .await?;
        assert_eq!(dry_run.assigned_workspace_count, 0);
        assert_eq!(dry_run.assigned_library_count, 0);
        let assignment = ironrag_backend::services::maintenance::service_identity_schema::assign_integration_provisioning_ownership(
            fixture.pool(),
            true,
            provisioner_principal_id,
            legacy_workspace.id,
            vec![legacy_library.id, legacy_sibling_library.id],
        )
        .await?;
        assert_eq!(assignment.assigned_workspace_count, 1);
        assert_eq!(assignment.assigned_library_count, 2);
        let reassignment = ironrag_backend::services::maintenance::service_identity_schema::assign_integration_provisioning_ownership(
            fixture.pool(),
            true,
            provisioner_principal_id,
            legacy_workspace.id,
            vec![legacy_library.id, legacy_sibling_library.id],
        )
        .await;
        assert!(reassignment.is_err(), "an owned mapping must never be reassigned");

        let foreign_workspace = catalog_repository::create_workspace(
            fixture.pool(),
            "foreign-admin-workspace",
            "Foreign Admin Workspace",
            Some(owner_principal_id),
        )
        .await?;
        iam_repository::create_grant(fixture.pool(), &iam_repository::NewGrant {
    principal_id: provisioner_principal_id,
    resource_kind: "workspace",
    resource_id: foreign_workspace.id,
    permission_kind: "workspace_admin",
    granted_by_principal_id: Some(owner_principal_id),
    expires_at: None,
})
        .await?;
        let foreign_library = catalog_repository::create_library(
            fixture.pool(),
            foreign_workspace.id,
            "foreign-admin-library",
            "Foreign Admin Library",
            None,
            Some(owner_principal_id),
        )
        .await?;
        let (foreign_admin_create_status, _) = fixture
            .rest_post(
                provisioner_credential,
                &format!("/v1/provisioning/workspaces/{}/libraries", foreign_workspace.id),
                json!({"displayName": "Must Not Be Created"}),
            )
            .await?;
        assert_eq!(foreign_admin_create_status, StatusCode::NOT_FOUND);
        let (foreign_admin_key_status, _) = fixture
            .rest_post(
                provisioner_credential,
                "/v1/provisioning/keys",
                json!({
                    "idempotencyKey": "foreign-admin-child",
                    "workspaceId": foreign_workspace.id,
                    "libraryIds": [foreign_library.id],
                    "permissionKinds": ["library_read"],
                    "label": "must-not-exist"
                }),
            )
            .await?;
        assert_eq!(foreign_admin_key_status, StatusCode::NOT_FOUND);
        let (foreign_admin_delete_status, _) = fixture
            .send(
                "DELETE",
                &format!("/v1/provisioning/workspaces/{}", foreign_workspace.id),
                AuditEventsFixture::bearer(provisioner_credential),
                None,
            )
            .await?;
        assert_eq!(foreign_admin_delete_status, StatusCode::NOT_FOUND);
        ops_repository::create_async_operation(
            fixture.pool(),
            &ops_repository::NewOpsAsyncOperation {
                workspace_id: foreign_workspace.id,
                library_id: Some(foreign_library.id),
                operation_kind: "delete_library",
                surface_kind: "rest",
                requested_by_principal_id: Some(owner_principal_id),
                status: "ready",
                subject_kind: "catalog_library",
                subject_id: Some(foreign_library.id),
                parent_async_operation_id: None,
                completed_at: Some(Utc::now()),
                failure_code: None,
            },
        )
        .await?;
        sqlx::query("delete from catalog_library where id = $1")
            .bind(foreign_library.id)
            .execute(fixture.pool())
            .await?;
        let (foreign_replay_status, _) = fixture
            .send(
                "DELETE",
                &format!(
                    "/v1/provisioning/workspaces/{}/libraries/{}",
                    foreign_workspace.id, foreign_library.id
                ),
                AuditEventsFixture::bearer(provisioner_credential),
                None,
            )
            .await?;
        assert_eq!(foreign_replay_status, StatusCode::NOT_FOUND);

        let replay_library_id = legacy_library.id;
        let replay_operation = ops_repository::create_async_operation(
            fixture.pool(),
            &ops_repository::NewOpsAsyncOperation {
                workspace_id: legacy_workspace.id,
                library_id: Some(replay_library_id),
                operation_kind: "delete_library",
                surface_kind: "rest",
                requested_by_principal_id: Some(provisioner_principal_id),
                status: "ready",
                subject_kind: "catalog_library",
                subject_id: Some(replay_library_id),
                parent_async_operation_id: None,
                completed_at: Some(Utc::now()),
                failure_code: None,
            },
        )
        .await?;
        sqlx::query("delete from catalog_library where id = $1")
            .bind(replay_library_id)
            .execute(fixture.pool())
            .await?;
        let mut replay_probe = fixture.pool().begin().await?;
        let replay_probe_row = ops_repository::get_provisioning_deletion_replay_with_transaction(
            replay_probe.as_mut(),
            ops_repository::ProvisioningDeletionReplayQuery {
                actor_principal_id: provisioner_principal_id,
                operation_kind: "delete_library",
                subject_kind: "catalog_library",
                subject_id: replay_library_id,
                workspace_id: legacy_workspace.id,
                library_id: Some(replay_library_id),
            },
        )
        .await?;
        replay_probe.rollback().await?;
        assert_eq!(replay_probe_row.map(|row| row.id), Some(replay_operation.id));
        let operation_path = format!("/v1/provisioning/operations/{}", replay_operation.id);
        let (owned_operation_status, owned_operation_body) =
            fixture.rest_get(provisioner_credential, &operation_path).await?;
        assert_eq!(owned_operation_status, StatusCode::OK, "{owned_operation_body}");
        assert_eq!(owned_operation_body["status"], json!("ready"));
        let (foreign_operation_status, _) = fixture
            .rest_get(foreign_provisioner_credential, &operation_path)
            .await?;
        assert_eq!(foreign_operation_status, StatusCode::NOT_FOUND);
        for _ in 0..2 {
            let (replay_status, replay_body) = fixture
                .send(
                    "DELETE",
                    &format!(
                        "/v1/provisioning/workspaces/{}/libraries/{replay_library_id}",
                        legacy_workspace.id
                    ),
                    AuditEventsFixture::bearer(provisioner_credential),
                    None,
                )
                .await?;
            assert_eq!(replay_status, StatusCode::ACCEPTED, "{replay_body}");
            assert_eq!(replay_body["operationId"], json!(replay_operation.id));
        }

        let (retry_library_status, retry_library_body) = fixture
            .rest_post(
                provisioner_credential,
                &format!("/v1/provisioning/workspaces/{}/libraries", legacy_workspace.id),
                json!({
                    "slug": "failed-delete-retry-library",
                    "displayName": "Failed Delete Retry Library"
                }),
            )
            .await?;
        assert_eq!(retry_library_status, StatusCode::CREATED, "{retry_library_body}");
        let retry_library_id = Uuid::parse_str(
            retry_library_body["id"].as_str().context("expected retry library id")?,
        )?;
        let failed_operation = ops_repository::create_async_operation(
            fixture.pool(),
            &ops_repository::NewOpsAsyncOperation {
                workspace_id: legacy_workspace.id,
                library_id: Some(retry_library_id),
                operation_kind: "delete_library",
                surface_kind: "rest",
                requested_by_principal_id: Some(provisioner_principal_id),
                status: "failed",
                subject_kind: "catalog_library",
                subject_id: Some(retry_library_id),
                parent_async_operation_id: None,
                completed_at: Some(Utc::now()),
                failure_code: Some("planted_delete_failure"),
            },
        )
        .await?;
        let (retry_status, retry_body) = fixture
            .send(
                "DELETE",
                &format!(
                    "/v1/provisioning/workspaces/{}/libraries/{}",
                    legacy_workspace.id, retry_library_id
                ),
                AuditEventsFixture::bearer(provisioner_credential),
                None,
            )
            .await?;
        assert_eq!(retry_status, StatusCode::ACCEPTED, "{retry_body}");
        let retry_operation_id = Uuid::parse_str(
            retry_body["operationId"].as_str().context("expected retry operation id")?,
        )?;
        assert_ne!(retry_operation_id, failed_operation.id);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let status: String = sqlx::query_scalar(
                    "select status::text from ops_async_operation where id = $1",
                )
                .bind(retry_operation_id)
                .fetch_one(fixture.pool())
                .await?;
                if status == "ready" {
                    return Ok::<(), anyhow::Error>(());
                }
                if matches!(status.as_str(), "failed" | "canceled" | "superseded") {
                    anyhow::bail!("retried deletion entered terminal status {status}");
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("retried deletion worker did not finish")??;
        assert!(
            catalog_repository::get_library_by_id(fixture.pool(), retry_library_id)
                .await?
                .is_none()
        );
        let retry_operations: i64 = sqlx::query_scalar(
            "select count(*) from ops_async_operation where subject_kind = 'catalog_library' and subject_id = $1",
        )
        .bind(retry_library_id)
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(retry_operations, 2);
        let (owned_workspace_delete_status, owned_workspace_delete_body) = fixture
            .send(
                "DELETE",
                &format!("/v1/provisioning/workspaces/{}", legacy_workspace.id),
                AuditEventsFixture::bearer(provisioner_credential),
                None,
            )
            .await?;
        assert_eq!(owned_workspace_delete_status, StatusCode::ACCEPTED);
        let owned_workspace_delete_operation = Uuid::parse_str(
            owned_workspace_delete_body["operationId"]
                .as_str()
                .context("expected workspace deletion operation id")?,
        )?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let status: String = sqlx::query_scalar(
                    "select status::text from ops_async_operation where id = $1",
                )
                .bind(owned_workspace_delete_operation)
                .fetch_one(fixture.pool())
                .await?;
                if status == "ready" {
                    return Ok::<(), anyhow::Error>(());
                }
                if matches!(status.as_str(), "failed" | "canceled" | "superseded") {
                    anyhow::bail!("workspace deletion entered terminal status {status}");
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("owned workspace deletion worker did not finish")??;
        assert!(
            catalog_repository::get_workspace_by_id(fixture.pool(), legacy_workspace.id)
                .await?
                .is_none(),
            "a workspace whose complete child set is owned must be deleted"
        );
        let (foreign_adoption_status, _) = fixture
            .rest_post(
                provisioner_credential,
                "/v1/provisioning/workspaces",
                json!({
                    "slug": "foreign-admin-workspace",
                    "displayName": "Foreign Admin Workspace"
                }),
            )
            .await?;
        assert_eq!(foreign_adoption_status, StatusCode::CONFLICT);

        let workspace_payload = json!({
            "slug": "provisioned-workspace",
            "displayName": "Provisioned Workspace"
        });
        let (human_status, _) = fixture
            .admin_post(&owner_session, "/v1/provisioning/workspaces", workspace_payload.clone())
            .await?;
        assert_eq!(human_status, StatusCode::FORBIDDEN);

        let (workspace_status, workspace_body) = fixture
            .rest_post(
                provisioner_credential,
                "/v1/provisioning/workspaces",
                workspace_payload.clone(),
            )
            .await?;
        assert_eq!(workspace_status, StatusCode::CREATED);
        assert_eq!(workspace_body["adopted"], json!(false));
        let workspace_id =
            Uuid::parse_str(workspace_body["id"].as_str().context("expected workspace id")?)?;
        let (adopted_workspace_status, adopted_workspace_body) = fixture
            .rest_post(provisioner_credential, "/v1/provisioning/workspaces", workspace_payload)
            .await?;
        assert_eq!(adopted_workspace_status, StatusCode::OK);
        assert_eq!(adopted_workspace_body["id"], json!(workspace_id));
        assert_eq!(adopted_workspace_body["adopted"], json!(true));

        let library_path = format!("/v1/provisioning/workspaces/{workspace_id}/libraries");
        catalog_repository::create_library(
            fixture.pool(),
            workspace_id,
            "ordinary-library",
            "Ordinary Library",
            None,
            Some(owner_principal_id),
        )
        .await?;
        let (unowned_child_delete_status, _) = fixture
            .send(
                "DELETE",
                &format!("/v1/provisioning/workspaces/{workspace_id}"),
                AuditEventsFixture::bearer(provisioner_credential),
                None,
            )
            .await?;
        assert_eq!(unowned_child_delete_status, StatusCode::NOT_FOUND);
        assert!(
            catalog_repository::get_workspace_by_id(fixture.pool(), workspace_id)
                .await?
                .is_some(),
            "a workspace with an unowned child must remain present"
        );
        let (ordinary_library_adoption_status, _) = fixture
            .rest_post(
                provisioner_credential,
                &library_path,
                json!({"slug": "ordinary-library", "displayName": "Ordinary Library"}),
            )
            .await?;
        assert_eq!(ordinary_library_adoption_status, StatusCode::CONFLICT);
        let library_payload = json!({
            "slug": "owned-library",
            "displayName": "Owned Library",
            "description": "Provisioned by the integration identity"
        });
        let (library_status, library_body) = fixture
            .rest_post(provisioner_credential, &library_path, library_payload.clone())
            .await?;
        assert_eq!(library_status, StatusCode::CREATED);
        let library_id =
            Uuid::parse_str(library_body["id"].as_str().context("expected library id")?)?;
        let (adopted_library_status, adopted_library_body) =
            fixture.rest_post(provisioner_credential, &library_path, library_payload).await?;
        assert_eq!(adopted_library_status, StatusCode::OK);
        assert_eq!(adopted_library_body["id"], json!(library_id));
        assert_eq!(adopted_library_body["adopted"], json!(true));
        let (catalog_read_status, _) = fixture
            .rest_get(
                provisioner_credential,
                &format!("/v1/catalog/workspaces/{workspace_id}/libraries"),
            )
            .await?;
        assert_eq!(catalog_read_status, StatusCode::OK);
        let (generic_content_create_status, _) = fixture
            .rest_post(
                provisioner_credential,
                &format!("/v1/catalog/workspaces/{workspace_id}/libraries"),
                json!({"displayName": "Generic API Must Stay Read Only"}),
            )
            .await?;
        assert_eq!(generic_content_create_status, StatusCode::UNAUTHORIZED);

        let invalid_key_payload = json!({
            "idempotencyKey": "invalid-child-permission",
            "workspaceId": workspace_id,
            "libraryIds": [library_id],
            "permissionKinds": ["workspace_admin"],
            "label": "invalid-child"
        });
        let (invalid_key_status, _) = fixture
            .rest_post(provisioner_credential, "/v1/provisioning/keys", invalid_key_payload)
            .await?;
        assert_eq!(invalid_key_status, StatusCode::BAD_REQUEST);

        let key_payload = json!({
            "idempotencyKey": "owned-library-child",
            "workspaceId": workspace_id,
            "libraryIds": [library_id],
            "permissionKinds": ["library_read", "library_write", "query_run"],
            "label": "owned-library-child"
        });
        let (key_status, key_body) = fixture
            .rest_post(provisioner_credential, "/v1/provisioning/keys", key_payload.clone())
            .await?;
        assert_eq!(key_status, StatusCode::OK);
        assert_eq!(key_body["replayed"], json!(false));
        let child_principal_id = Uuid::parse_str(
            key_body["serviceIdentity"]["principalId"]
                .as_str()
                .context("expected child principal id")?,
        )?;
        let child_credential =
            key_body["credential"].as_str().context("expected child credential")?;
        let (child_cannot_provision_status, _) = fixture
            .rest_post(
                child_credential,
                "/v1/provisioning/workspaces",
                json!({"displayName": "Forbidden Grandchild Workspace"}),
            )
            .await?;
        assert_eq!(child_cannot_provision_status, StatusCode::FORBIDDEN);
        let (replay_status, replay_body) =
            fixture.rest_post(provisioner_credential, "/v1/provisioning/keys", key_payload).await?;
        assert_eq!(replay_status, StatusCode::OK);
        assert_eq!(replay_body["replayed"], json!(true));
        assert_eq!(replay_body["credential"], key_body["credential"]);

        sqlx::query("update iam_api_token set expires_at = now() - interval '1 second' where principal_id = $1")
            .bind(child_principal_id)
            .execute(fixture.pool())
            .await?;
        let mut count_transaction = fixture.pool().begin().await?;
        let active_child_count = iam_repository::count_active_child_keys_with_transaction(
            count_transaction.as_mut(),
            provisioner_principal_id,
            workspace_id,
        )
        .await?;
        count_transaction.rollback().await?;
        assert_eq!(active_child_count, 0);

        let (revoke_child_status, _) = fixture
            .rest_post(
                provisioner_credential,
                &format!("/v1/provisioning/keys/{child_principal_id}/revoke"),
                json!({"idempotencyKey": "revoke-expired-child"}),
            )
            .await?;
        assert_eq!(revoke_child_status, StatusCode::NO_CONTENT);

        for action_kind in [
            "provisioning.workspace.create",
            "provisioning.library.create",
            "provisioning.key.create",
            "provisioning.key.revoke",
        ] {
            let provisioning_audit =
                latest_audit_event_for_action(fixture.pool(), action_kind).await?;
            assert_eq!(provisioning_audit.actor_principal_id, Some(provisioner_principal_id));
        }

        let immutable_owner_update = sqlx::query(
            "update catalog_workspace set provisioned_by_principal_id = $2 where id = $1",
        )
        .bind(workspace_id)
        .bind(owner_principal_id)
        .execute(fixture.pool())
        .await;
        assert!(immutable_owner_update.is_err());

        sqlx::query("update catalog_library set lifecycle_state = 'archived' where id = $1")
            .bind(library_id)
            .execute(fixture.pool())
            .await?;
        let (archived_library_adoption_status, _) = fixture
            .rest_post(
                provisioner_credential,
                &library_path,
                json!({
                    "slug": "owned-library",
                    "displayName": "Owned Library",
                    "description": "Provisioned by the integration identity"
                }),
            )
            .await?;
        assert_eq!(archived_library_adoption_status, StatusCode::CONFLICT);

        let deleting_library_payload = json!({
            "slug": "deleting-library",
            "displayName": "Deleting Library"
        });
        let (deleting_library_status, deleting_library_body) = fixture
            .rest_post(
                provisioner_credential,
                &library_path,
                deleting_library_payload.clone(),
            )
            .await?;
        assert_eq!(deleting_library_status, StatusCode::CREATED);
        let deleting_library_id = Uuid::parse_str(
            deleting_library_body["id"].as_str().context("expected deleting library id")?,
        )?;
        ops_repository::create_async_operation(
            fixture.pool(),
            &ops_repository::NewOpsAsyncOperation {
                workspace_id,
                library_id: Some(deleting_library_id),
                operation_kind: "delete_library",
                surface_kind: "rest",
                requested_by_principal_id: Some(provisioner_principal_id),
                status: "processing",
                subject_kind: "catalog_library",
                subject_id: Some(deleting_library_id),
                parent_async_operation_id: None,
                completed_at: None,
                failure_code: None,
            },
        )
        .await?;
        let (deleting_library_adoption_status, _) = fixture
            .rest_post(provisioner_credential, &library_path, deleting_library_payload)
            .await?;
        assert_eq!(deleting_library_adoption_status, StatusCode::CONFLICT);

        sqlx::query("update catalog_workspace set lifecycle_state = 'archived' where id = $1")
            .bind(workspace_id)
            .execute(fixture.pool())
            .await?;
        let (archived_workspace_adoption_status, _) = fixture
            .rest_post(
                provisioner_credential,
                "/v1/provisioning/workspaces",
                json!({
                    "slug": "provisioned-workspace",
                    "displayName": "Provisioned Workspace"
                }),
            )
            .await?;
        assert_eq!(archived_workspace_adoption_status, StatusCode::CONFLICT);

        let mut revoke_transaction = fixture.pool().begin().await?;
        sqlx::query(
            "update iam_api_token
             set status = 'revoked', revoked_at = now()
             where principal_id = $1",
        )
        .bind(provisioner_principal_id)
        .execute(&mut *revoke_transaction)
        .await?;
        let race_request = Request::builder()
            .method("POST")
            .uri("/v1/provisioning/workspaces")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {provisioner_credential}"),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"displayName": "Revoked Parent Race"}).to_string(),
            ))?;
        let race_app = fixture.app();
        let race_request_task = tokio::spawn(async move { race_app.oneshot(race_request).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let waiting = sqlx::query_scalar::<_, bool>(
                    "select exists (
                         select 1
                         from pg_stat_activity
                         where datname = current_database()
                           and wait_event_type = 'Lock'
                           and query like '%for update of identity%'
                     )",
                )
                .fetch_one(fixture.pool())
                .await?;
                if waiting {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("provisioning request did not wait for the parent revocation")??;
        revoke_transaction.commit().await?;
        let race_response = race_request_task.await??;
        assert_eq!(race_response.status(), StatusCode::FORBIDDEN);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn service_identity_lifecycle_is_idempotent_owner_only_and_token_isolated() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let owner_issuer = format!("oidc:{}", "a".repeat(64));
        let other_issuer = format!("oidc:{}", "b".repeat(64));
        let (owner_principal_id, owner_session) = fixture
            .mint_oidc_admin_session("service-owner", &owner_issuer, "owner-subject")
            .await?;
        let (_other_principal_id, other_session) = fixture
            .mint_oidc_admin_session("other-service-admin", &other_issuer, "other-subject")
            .await?;
        let sibling_library = catalog_repository::create_library(
            fixture.pool(),
            fixture.workspace_id,
            "service-identity-sibling-library",
            "Service Identity Sibling Library",
            Some("service identity lifecycle test"),
            None,
        )
        .await?;

        let create_key = format!("create-{}", Uuid::now_v7());
        let create_payload = json!({
            "idempotencyKey": create_key,
            "workspaceId": fixture.workspace_id,
            "label": "harnessd-owner-only",
            "libraryIds": [fixture.library_id, sibling_library.id],
            "permissionKinds": ["library_read", "query_run"]
        });
        let (create_status, create_body) = fixture
            .admin_post(&owner_session, "/v1/iam/service-identities", create_payload.clone())
            .await?;
        assert_eq!(create_status, StatusCode::OK);
        assert_eq!(create_body["replayed"], json!(false));
        let principal_id = Uuid::parse_str(
            create_body["serviceIdentity"]["principalId"]
                .as_str()
                .context("expected service identity principal id")?,
        )?;
        let original_credential = create_body["credential"]
            .as_str()
            .context("expected one-time service identity credential")?
            .to_string();
        assert_eq!(
            create_body["serviceIdentity"]["owner"],
            json!({
                "issuer": owner_issuer,
                "subject": "owner-subject"
            })
        );

        let (replay_status, replay_body) = fixture
            .admin_post(&owner_session, "/v1/iam/service-identities", create_payload.clone())
            .await?;
        assert_eq!(replay_status, StatusCode::OK);
        assert_eq!(replay_body["replayed"], json!(true));
        assert_eq!(replay_body["serviceIdentity"]["principalId"], json!(principal_id));
        assert_eq!(replay_body["credential"], json!(original_credential));

        let (conflict_status, conflict_body) = fixture
            .admin_post(
                &owner_session,
                "/v1/iam/service-identities",
                json!({
                    "idempotencyKey": create_key,
                    "workspaceId": fixture.workspace_id,
                    "label": "different-request",
                    "libraryIds": [fixture.library_id],
                    "permissionKinds": ["library_read"]
                }),
            )
            .await?;
        assert_eq!(conflict_status, StatusCode::CONFLICT);
        assert_eq!(conflict_body["code"], json!("conflict"));

        sqlx::query(
            "update iam_user
             set auth_provider_kind = $2,
                 external_subject = 'retired-owner-subject'
             where principal_id = $1",
        )
        .bind(owner_principal_id)
        .bind(format!("oidc:{}", "c".repeat(64)))
        .execute(fixture.pool())
        .await?;
        let (replacement_owner_principal_id, replacement_owner_session) = fixture
            .mint_oidc_admin_session("replacement-service-owner", &owner_issuer, "owner-subject")
            .await?;
        assert_ne!(replacement_owner_principal_id, owner_principal_id);
        let (retired_owner_list_status, retired_owner_list) =
            fixture.admin_get(&owner_session, "/v1/iam/service-identities").await?;
        assert_eq!(retired_owner_list_status, StatusCode::OK);
        assert_eq!(retired_owner_list, json!([]));
        let (replacement_replay_status, replacement_replay_body) = fixture
            .admin_post(
                &replacement_owner_session,
                "/v1/iam/service-identities",
                create_payload.clone(),
            )
            .await?;
        assert_eq!(replacement_replay_status, StatusCode::CONFLICT);
        assert_eq!(replacement_replay_body["code"], json!("conflict"));
        assert!(replacement_replay_body.get("credential").is_none());
        let owner_session = replacement_owner_session;

        let identity_count: i64 = sqlx::query_scalar("select count(*) from iam_service_identity")
            .fetch_one(fixture.pool())
            .await?;
        assert_eq!(identity_count, 1);
        let operation_count: i64 = sqlx::query_scalar(
            "select count(*) from iam_service_identity_operation where operation_kind = 'create'",
        )
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(operation_count, 1);
        let principal_kind: String =
            sqlx::query_scalar("select principal_kind::text from iam_principal where id = $1")
                .bind(principal_id)
                .fetch_one(fixture.pool())
                .await?;
        assert_eq!(principal_kind, "worker");
        let parent_principal_id: Option<Uuid> =
            sqlx::query_scalar("select parent_principal_id from iam_principal where id = $1")
                .bind(principal_id)
                .fetch_one(fixture.pool())
                .await?;
        assert_eq!(parent_principal_id, None, "service identity must not inherit a parent");
        let memberships: i64 = sqlx::query_scalar(
            "select count(*) from iam_workspace_membership where principal_id = $1",
        )
        .bind(principal_id)
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(memberships, 0, "service identity must not inherit workspace membership");

        let grants = iam_repository::list_grants_by_principal(fixture.pool(), principal_id).await?;
        assert_eq!(grants.len(), 4);
        assert!(grants.iter().all(|grant| grant.resource_kind == "library"));
        let exact_grants = grants
            .iter()
            .map(|grant| (grant.resource_id, grant.permission_kind.as_str()))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            exact_grants,
            std::collections::BTreeSet::from([
                (fixture.library_id, "library_read"),
                (fixture.library_id, "query_run"),
                (sibling_library.id, "library_read"),
                (sibling_library.id, "query_run"),
            ])
        );

        let (ordinary_get_status, _) =
            fixture.admin_get(&owner_session, &format!("/v1/iam/tokens/{principal_id}")).await?;
        assert_eq!(ordinary_get_status, StatusCode::NOT_FOUND);
        let (ordinary_list_status, ordinary_list_body) =
            fixture.admin_get(&owner_session, "/v1/iam/tokens").await?;
        assert_eq!(ordinary_list_status, StatusCode::OK);
        assert!(
            ordinary_list_body
                .as_array()
                .context("expected ordinary token array")?
                .iter()
                .all(|token| token["principalId"] != json!(principal_id))
        );

        let identity_path = format!("/v1/iam/service-identities/{principal_id}");
        let (other_list_status, other_list) =
            fixture.admin_get(&other_session, "/v1/iam/service-identities").await?;
        assert_eq!(other_list_status, StatusCode::OK);
        assert_eq!(other_list, json!([]));
        let (other_get_status, _) = fixture.admin_get(&other_session, &identity_path).await?;
        assert_eq!(other_get_status, StatusCode::NOT_FOUND);
        let (other_patch_status, _) = fixture
            .admin_patch(
                &other_session,
                &identity_path,
                json!({
                    "label": "must-not-change",
                    "libraryIds": [fixture.library_id],
                    "permissionKinds": ["library_read", "query_run"]
                }),
            )
            .await?;
        assert_eq!(other_patch_status, StatusCode::NOT_FOUND);
        let (other_rotate_status, _) = fixture
            .admin_post(
                &other_session,
                &format!("{identity_path}/rotate"),
                json!({"idempotencyKey": format!("foreign-rotate-{}", Uuid::now_v7())}),
            )
            .await?;
        assert_eq!(other_rotate_status, StatusCode::NOT_FOUND);
        let (other_revoke_status, _) = fixture
            .admin_post(
                &other_session,
                &format!("{identity_path}/revoke"),
                json!({"idempotencyKey": format!("foreign-revoke-{}", Uuid::now_v7())}),
            )
            .await?;
        assert_eq!(other_revoke_status, StatusCode::NOT_FOUND);
        assert_eq!(
            fixture.admin_delete(&other_session, &identity_path).await?,
            StatusCode::NOT_FOUND
        );

        let (patch_status, patch_body) = fixture
            .admin_patch(
                &owner_session,
                &identity_path,
                json!({
                    "label": "harnessd-reconciled",
                    "libraryIds": [fixture.library_id],
                    "permissionKinds": ["library_read", "query_run"]
                }),
            )
            .await?;
        assert_eq!(patch_status, StatusCode::OK);
        assert_eq!(patch_body["label"], json!("harnessd-reconciled"));
        let reconciled_grants =
            iam_repository::list_grants_by_principal(fixture.pool(), principal_id).await?;
        assert_eq!(reconciled_grants.len(), 2);
        assert!(reconciled_grants.iter().all(|grant| grant.resource_id == fixture.library_id));

        let (query_status, _) = fixture
            .rest_post(
                &original_credential,
                &format!("/v1/query/libraries/{}/sessions", fixture.library_id),
                json!({"title": "service identity permission proof"}),
            )
            .await?;
        assert_eq!(query_status, StatusCode::CREATED);
        let (write_status, _) = fixture
            .rest_put(
                &original_credential,
                &format!("/v1/catalog/libraries/{}", fixture.library_id),
                json!({
                    "displayName": "must not be writable",
                    "description": null,
                    "extractionPrompt": null
                }),
            )
            .await?;
        assert_eq!(write_status, StatusCode::UNAUTHORIZED);

        let rotate_key = format!("rotate-{}", Uuid::now_v7());
        let rotate_payload = json!({"idempotencyKey": rotate_key});
        let (rotate_status, rotate_body) = fixture
            .admin_post(&owner_session, &format!("{identity_path}/rotate"), rotate_payload.clone())
            .await?;
        assert_eq!(rotate_status, StatusCode::OK);
        assert_eq!(rotate_body["replayed"], json!(false));
        let rotated_credential =
            rotate_body["credential"].as_str().context("expected rotated credential")?.to_string();
        assert_ne!(rotated_credential, original_credential);
        let (rotate_replay_status, rotate_replay_body) = fixture
            .admin_post(&owner_session, &format!("{identity_path}/rotate"), rotate_payload)
            .await?;
        assert_eq!(rotate_replay_status, StatusCode::OK);
        assert_eq!(rotate_replay_body["replayed"], json!(true));
        assert_eq!(rotate_replay_body["credential"], json!(rotated_credential));
        let (old_credential_status, _) = fixture
            .rest_post(
                &original_credential,
                &format!("/v1/query/libraries/{}/sessions", fixture.library_id),
                json!({"title": "revoked old credential"}),
            )
            .await?;
        assert_eq!(old_credential_status, StatusCode::UNAUTHORIZED);
        let (rotated_credential_status, _) = fixture
            .rest_post(
                &rotated_credential,
                &format!("/v1/query/libraries/{}/sessions", fixture.library_id),
                json!({"title": "rotated credential proof"}),
            )
            .await?;
        assert_eq!(rotated_credential_status, StatusCode::CREATED);

        let revoke_key = format!("revoke-{}", Uuid::now_v7());
        let revoke_payload = json!({"idempotencyKey": revoke_key});
        let (revoke_status, revoke_body) = fixture
            .admin_post(&owner_session, &format!("{identity_path}/revoke"), revoke_payload.clone())
            .await?;
        assert_eq!(revoke_status, StatusCode::OK);
        assert_eq!(revoke_body["replayed"], json!(false));
        assert_eq!(revoke_body["serviceIdentity"]["status"], json!("revoked"));
        let (revoke_replay_status, revoke_replay_body) = fixture
            .admin_post(&owner_session, &format!("{identity_path}/revoke"), revoke_payload)
            .await?;
        assert_eq!(revoke_replay_status, StatusCode::OK);
        assert_eq!(revoke_replay_body["replayed"], json!(true));
        let (revoked_credential_status, _) = fixture
            .rest_post(
                &rotated_credential,
                &format!("/v1/query/libraries/{}/sessions", fixture.library_id),
                json!({"title": "revoked credential"}),
            )
            .await?;
        assert_eq!(revoked_credential_status, StatusCode::UNAUTHORIZED);

        assert_eq!(
            fixture.admin_delete(&owner_session, &identity_path).await?,
            StatusCode::NO_CONTENT
        );
        let (deleted_get_status, _) = fixture.admin_get(&owner_session, &identity_path).await?;
        assert_eq!(deleted_get_status, StatusCode::NOT_FOUND);
        let (deleted_create_replay_status, _) = fixture
            .admin_post(&owner_session, "/v1/iam/service-identities", create_payload)
            .await?;
        assert_eq!(deleted_create_replay_status, StatusCode::CONFLICT);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn token_mint_with_library_ids_creates_library_grants() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let admin_session = fixture.mint_admin_session("audit-events-admin").await?;
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
            .admin_post(
                &admin_session,
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

/// Asserts a `POST /v1/iam/tokens` mint request carrying `body` is rejected
/// with `400 bad_request` and leaves the `iam_api_token` row count for
/// `label` unchanged — the shared assertion behind every
/// `token_mint_with_library_ids_rejects_*`/`_requires_*` negative case below,
/// which differ only in which validation rule they exercise.
async fn assert_token_mint_rejected_with_unchanged_count(
    fixture: &AuditEventsFixture,
    admin_session: &str,
    label: &str,
    body: Value,
) -> Result<()> {
    let before_count: i64 =
        sqlx::query_scalar::<_, i64>("select count(*) from iam_api_token where label = $1")
            .bind(label)
            .fetch_one(fixture.pool())
            .await?;
    let (status, response_body) = fixture.admin_post(admin_session, "/v1/iam/tokens", body).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(response_body["code"].as_str().context("expected api error kind")?, "bad_request");

    let after_count: i64 =
        sqlx::query_scalar::<_, i64>("select count(*) from iam_api_token where label = $1")
            .bind(label)
            .fetch_one(fixture.pool())
            .await?;
    assert_eq!(before_count, after_count);
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn token_mint_with_library_ids_rejects_cross_workspace_library_ids() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let admin_session = fixture.mint_admin_session("audit-events-admin").await?;
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
        assert_token_mint_rejected_with_unchanged_count(
            &fixture,
            &admin_session,
            &label,
            json!({
                "workspaceId": fixture.workspace_id,
                "label": label,
                "libraryIds": [fixture.library_id, foreign_library.id],
                "permissionKinds": ["library_read"]
            }),
        )
        .await
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn token_mint_with_library_ids_requires_permissions() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let admin_session = fixture.mint_admin_session("audit-events-admin").await?;
        let label = format!("mint-multi-lib-missing-permissions-{}", Uuid::now_v7());
        assert_token_mint_rejected_with_unchanged_count(
            &fixture,
            &admin_session,
            &label,
            json!({
                "workspaceId": fixture.workspace_id,
                "label": label,
                "libraryIds": [fixture.library_id],
            }),
        )
        .await
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn token_mint_with_library_ids_rejects_invalid_permissions() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let admin_session = fixture.mint_admin_session("audit-events-admin").await?;
        let label = format!("mint-multi-lib-invalid-permission-{uuid}", uuid = Uuid::now_v7());
        assert_token_mint_rejected_with_unchanged_count(
            &fixture,
            &admin_session,
            &label,
            json!({
                "workspaceId": fixture.workspace_id,
                "label": label,
                "libraryIds": [fixture.library_id],
                "permissionKinds": ["iam_admin"]
            }),
        )
        .await
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn token_mint_and_delete_append_audit_events_with_api_token_subjects() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let admin_session = fixture.mint_admin_session("audit-events-admin").await?;

        let (status, body) = fixture
            .admin_post(
                &admin_session,
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

        let status = fixture
            .admin_delete(&admin_session, &format!("/v1/iam/tokens/{token_principal_id}"))
            .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // `DELETE /v1/iam/tokens/{id}` emits one `iam.api_token.delete` event.
        // The separate `iam.api_token.revoke` event this test also expected is
        // not written by any handler — revoke-then-delete became a single
        // deletion, and nothing noticed because the suite never ran.
        let delete_event =
            latest_audit_event_for_action(fixture.pool(), "iam.api_token.delete").await?;
        assert_eq!(delete_event.result_kind, "succeeded");
        let delete_subjects =
            audit_repository::list_audit_event_subjects(fixture.pool(), delete_event.id).await?;
        assert_eq!(delete_subjects.len(), 1);
        assert_eq!(delete_subjects[0].subject_kind, "api_token");
        assert_eq!(delete_subjects[0].subject_id, token_principal_id);
        assert_eq!(delete_subjects[0].workspace_id, Some(fixture.workspace_id));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn governance_actions_and_denials_append_expected_audit_subjects() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let workspace_admin = fixture.mint_workspace_admin_token("workspace-admin").await?;
        let read_only = fixture.mint_read_only_workspace_token("workspace-readonly").await?;

        // Account creation validates against the live provider. Use the
        // ollama catalog entry (loopback base urls allowed, no api key
        // required) pointed at a hermetic stub that answers both the tag
        // listing and an OpenAI-style chat round trip.
        let ollama_catalog =
            ai_repository::list_provider_catalog(&fixture.state.persistence.postgres)
                .await?
                .into_iter()
                .find(|provider| provider.provider_kind == "ollama")
                .context("expected seeded ollama provider catalog")?;
        // Ollama ships no seeded models on purpose — they are discovered from
        // the running daemon's tag listing. Register the one the stub below
        // advertises, the same way discovery would.
        let ollama_model = ai_repository::create_model_catalog(
            &fixture.state.persistence.postgres,
            ai_repository::NewModelCatalog {
                provider_catalog_id: ollama_catalog.id,
                model_name: "audit-events-embed",
                context_window: Some(8192),
                max_output_tokens: None,
                lifecycle_state: "active",
                metadata_json: json!({ "modalities": ["text"] }),
                pricing: &ai_repository::ModelPricingBinds {
                    currency_code: "USD".to_string(),
                    input_price_per_1m: Decimal::ZERO,
                    cached_input_price_per_1m: Decimal::ZERO,
                    cache_write_input_price_per_1m: Decimal::ZERO,
                    output_price_per_1m: Decimal::ZERO,
                    long_context_threshold_tokens: None,
                    long_context_input_price_per_1m: None,
                    long_context_cached_input_price_per_1m: None,
                    long_context_output_price_per_1m: None,
                },
                allowed_binding_purposes: &["embed_chunk".to_string()],
            },
        )
        .await
        .context("failed to register the stubbed ollama model")?;
        let stub_model_name = ollama_model.model_name.clone();
        let stub_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let stub_base_url = format!("http://{}", stub_listener.local_addr()?);
        let tags_model_name = stub_model_name.clone();
        let models_model_name = stub_model_name.clone();
        let stub_app = Router::new()
            .route(
                "/api/tags",
                axum::routing::get(move || {
                    let name = tags_model_name.clone();
                    async move { axum::Json(json!({ "models": [{ "name": name }] })) }
                }),
            )
            .route(
                "/models",
                axum::routing::get(move || {
                    let name = models_model_name.clone();
                    async move {
                        axum::Json(json!({
                            "object": "list",
                            "data": [{ "id": name, "object": "model" }]
                        }))
                    }
                }),
            )
            .route(
                "/api/chat",
                axum::routing::post(move || {
                    let name = stub_model_name.clone();
                    async move {
                        axum::Json(json!({
                            "model": name,
                            "message": { "role": "assistant", "content": "OK" },
                            "done": true
                        }))
                    }
                }),
            );
        let stub_server = tokio::spawn(async move {
            let _ = axum::serve(stub_listener, stub_app).await;
        });

        let credential_response = fixture
            .rest_post(
                &workspace_admin,
                "/v1/ai/accounts",
                json!({
                    "scopeKind": "workspace",
                    "workspaceId": fixture.workspace_id,
                    "providerCatalogId": ollama_catalog.id,
                    "label": TEST_PROVIDER_CREDENTIAL_LABEL,
                    "baseUrl": stub_base_url
                }),
            )
            .await?;
        stub_server.abort();
        assert_eq!(credential_response.0, StatusCode::CREATED, "{}", credential_response.1);
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

        let binding_response = fixture
            .rest_post(
                &workspace_admin,
                "/v1/ai/bindings",
                json!({
                    "scopeKind": "library",
                    "workspaceId": fixture.workspace_id,
                    "libraryId": fixture.library_id,
                    // The stubbed ollama account carries an embedding model.
                    "bindingPurpose": "embed_chunk",
                    "accountId": credential_id,
                    "modelCatalogId": ollama_model.id
                }),
            )
            .await?;
        assert_eq!(binding_response.0, StatusCode::CREATED, "{}", binding_response.1);
        let binding_id = Uuid::parse_str(
            binding_response.1["id"].as_str().context("expected library binding id")?,
        )?;
        let binding_event =
            latest_audit_event_for_action(fixture.pool(), "ai.binding_assignment.create").await?;
        assert_eq!(binding_event.result_kind, "succeeded");
        let binding_subjects =
            audit_repository::list_audit_event_subjects(fixture.pool(), binding_event.id).await?;
        assert_eq!(binding_subjects.len(), 1);
        assert_eq!(binding_subjects[0].subject_kind, "binding_assignment");
        assert_eq!(binding_subjects[0].subject_id, binding_id);
        assert_eq!(binding_subjects[0].library_id, Some(fixture.library_id));

        let workspace_slug =
            sqlx::query_scalar::<_, String>("select slug from catalog_workspace where id = $1")
                .bind(fixture.workspace_id)
                .fetch_one(fixture.pool())
                .await?;
        let create_library_response = fixture
            .mcp_diagnostics_call(
                &workspace_admin,
                "tools/call",
                Some(json!({
                    "name": "create_library",
                    "arguments": {
                        "library": format!("{workspace_slug}/audit-events-mcp-library"),
                        "title": "Audit Events MCP Library"
                    }
                })),
            )
            .await?;
        assert_eq!(
            create_library_response["result"]["isError"],
            json!(false),
            "{create_library_response}"
        );
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
                "/v1/ai/accounts",
                json!({
                    "scopeKind": "workspace",
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
#[ignore = "requires local postgres and redis services"]
async fn audit_events_surface_assistant_models_cost_and_runtime_subjects() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let workspace_admin = fixture.mint_workspace_admin_token("workspace-admin").await?;
        // query_conversation.created_by_principal_id is FK-enforced; mint a
        // real principal instead of a random id.
        let actor_principal_id = iam_repository::create_api_token(
            fixture.pool(),
            Some(fixture.workspace_id),
            "audit-cost-actor",
            "audit",
            None,
            None,
        )
        .await?
        .principal_id;
        let request_id = "assistant-mcp-audit";
        let execution_id = fixture
            .seed_assistant_call_audit(AssistantCallAuditSeed {
                actor_principal_id,
                surface_kind: "mcp",
                request_id: Some(request_id),
                provider_kind: &fixture.provider_kind,
                model_name: &fixture.model_name,
                total_cost: Decimal::new(123, 4),
            })
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
        let total_cost: Decimal = event["assistantCall"]["totalCost"]
            .as_str()
            .context("assistant call total cost must be a string")?
            .parse()?;
        assert_eq!(total_cost, Decimal::new(123, 4));
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
#[ignore = "requires local postgres and redis services"]
async fn audit_events_hide_assistant_costs_without_usage_read() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let audit_reader = fixture.mint_audit_read_workspace_token("audit-reader").await?;
        let request_id = "assistant-audit-redacted";
        // created_by_principal_id is FK-enforced; use a real principal.
        let redacted_actor = iam_repository::create_api_token(
            fixture.pool(),
            Some(fixture.workspace_id),
            "audit-redacted-actor",
            "audit",
            None,
            None,
        )
        .await?
        .principal_id;
        fixture
            .seed_assistant_call_audit(AssistantCallAuditSeed {
                actor_principal_id: redacted_actor,
                surface_kind: "rest",
                request_id: Some(request_id),
                provider_kind: &fixture.provider_kind,
                model_name: &fixture.model_name,
                total_cost: Decimal::new(55, 4),
            })
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
#[ignore = "requires local postgres and redis services"]
async fn authoritative_audit_subjects_surface_query_and_knowledge_ids_through_http() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let audit_reviewer = fixture.mint_audit_review_token("authoritative-subjects").await?;

        let query_session_id = Uuid::now_v7();
        let query_execution_id = Uuid::now_v7();
        let knowledge_document_id = Uuid::now_v7();
        let knowledge_bundle_id = Uuid::now_v7();
        let async_operation_id = Uuid::now_v7();

        let audit_event_id = fixture
            .append_audit_event(
                "governance.authoritative_subjects.proof",
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

        for (query_param, subject_id, subject_kind, authoritative_field) in filters {
            let (status, body) = fixture
                .rest_get(&audit_reviewer, &format!("/v1/audit/events?{query_param}={subject_id}"))
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
            assert_eq!(subject[authoritative_field], json!(subject_id));
            assert_eq!(subject["workspaceId"], json!(fixture.workspace_id));
            assert_eq!(subject["libraryId"], json!(fixture.library_id));
        }

        let (status, body) = fixture.rest_get(&audit_reviewer, "/v1/audit/events").await?;
        assert_eq!(status, StatusCode::OK);
        let events =
            body["items"].as_array().context("audit events response must include items")?;
        assert!(
            events.iter().any(|event| event["id"] == json!(audit_event_id)),
            "authoritative audit proof event must be visible in the audit feed"
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn authoritative_agent_memory_audit_subjects_surface_knowledge_and_async_operation_ids()
-> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let audit_reviewer = fixture.mint_audit_review_token("authoritative-agent-memory").await?;
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
                &audit_reviewer,
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
                &audit_reviewer,
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

/// IRG-227: append one audit event directly, with an explicit library scope, so
/// a repository-level check can run without the HTTP layer.
async fn append_irg227_event_in(
    fixture: &AuditEventsFixture,
    action_kind: &str,
    workspace_id: Uuid,
    library_id: Uuid,
    result_kind: &str,
) -> Result<Uuid> {
    let event = audit_repository::append_audit_event(
        fixture.pool(),
        audit_repository::NewAuditEvent {
            actor_principal_id: None,
            surface_kind: "rest".to_string(),
            action_kind: action_kind.to_string(),
            request_id: Some(format!("irg227-{action_kind}")),
            trace_id: None,
            result_kind: result_kind.to_string(),
            redacted_message: Some("irg227 fullset proof".to_string()),
            internal_message: None,
        },
        &[audit_repository::NewAuditEventSubject {
            subject_kind: "library".to_string(),
            subject_id: library_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        }],
    )
    .await
    .context("failed to append irg227 audit event")?;
    Ok(event.id)
}

async fn append_irg227_event(
    fixture: &AuditEventsFixture,
    action_kind: &str,
    result_kind: &str,
) -> Result<Uuid> {
    append_irg227_event_in(
        fixture,
        action_kind,
        fixture.workspace_id,
        fixture.library_id,
        result_kind,
    )
    .await
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn audit_displayed_dynamic_action_search_matches_scoped_rows_counts_and_facets() -> Result<()>
{
    let fixture = AuditEventsFixture::create().await?;
    let result = async {
        let mut expected_ids = Vec::new();
        for action in ["query_execution.run", "query-execution.run", "..query___execution..run.."] {
            expected_ids.push(append_irg227_event(&fixture, action, "succeeded").await?);
        }
        let suffix = Uuid::now_v7().simple().to_string();
        let foreign_workspace = catalog_repository::create_workspace(
            fixture.pool(),
            &format!("foreign-audit-{suffix}"),
            "Foreign Audit",
            None,
        )
        .await?;
        let foreign_library = catalog_repository::create_library(
            fixture.pool(),
            foreign_workspace.id,
            &format!("foreign-audit-{suffix}"),
            "Foreign Audit",
            None,
            None,
        )
        .await?;
        let foreign_id = append_irg227_event_in(
            &fixture,
            "query_execution.run",
            foreign_workspace.id,
            foreign_library.id,
            "failed",
        )
        .await?;
        let mut query = audit_repository::ListAuditEventsQuery {
            workspace_id: Some(fixture.workspace_id),
            library_id: Some(fixture.library_id),
            subject_filter: audit_repository::AuditEventSubjectFilter::default(),
            filters: audit_repository::AuditEventMultiFilters::default(),
            search: Some("Query Execution RUN".to_string()),
            internal: false,
            search_hints: audit_repository::AuditSearchHints::default(),
            sort: audit_repository::AuditSort {
                key: audit_repository::AuditSortKey::Action,
                order: audit_repository::AuditSortOrder::Asc,
            },
            limit: 1,
            cursor: None,
        };
        let first = audit_repository::list_audit_events(fixture.pool(), &query).await?;
        assert_eq!(first.total, 3, "displayed action search must count the entire authorized set");
        assert_eq!(first.items.len(), 1);
        let mut actual_ids = vec![first.items[0].id];
        query.cursor = first.next_cursor;
        while query.cursor.is_some() {
            let next = audit_repository::list_audit_events(fixture.pool(), &query).await?;
            assert_eq!(next.total, 3);
            actual_ids.extend(next.items.iter().map(|row| row.id));
            query.cursor = next.next_cursor;
        }
        actual_ids.sort_unstable();
        expected_ids.sort_unstable();
        assert_eq!(actual_ids, expected_ids);
        assert!(!actual_ids.contains(&foreign_id));
        let mut facet_query = audit_repository::AuditFacetQuery {
            workspace_id: query.workspace_id,
            library_id: query.library_id,
            subject_filter: query.subject_filter.clone(),
            filters: query.filters.clone(),
            search: None,
            internal: false,
            search_hints: audit_repository::AuditSearchHints::default(),
            facet_column: audit_repository::AuditFacetColumn::Action,
            facet_search: Some("Query Execution RUN".to_string()),
            facet_search_hints: audit_repository::AuditSearchHints::default(),
            selected_values: vec![],
            limit: 50,
            offset: 0,
        };
        let facets =
            audit_repository::list_audit_event_facets(fixture.pool(), &facet_query).await?;
        assert_eq!(
            facets.len(),
            3,
            "the action facet must search its displayed labels, not just raw keys"
        );
        assert!(facets.iter().all(|row| row.count == 1));
        facet_query.search = query.search.clone();
        facet_query.facet_search = None;
        let global_facets =
            audit_repository::list_audit_event_facets(fixture.pool(), &facet_query).await?;
        assert_eq!(global_facets.len(), 3);
        query.cursor = None;
        query.filters.result_kinds = vec!["failed".to_string()];
        assert_eq!(audit_repository::list_audit_events(fixture.pool(), &query).await?.total, 0);
        facet_query.filters = query.filters.clone();
        assert!(
            audit_repository::list_audit_event_facets(fixture.pool(), &facet_query)
                .await?
                .is_empty()
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// IRG-227: the list is one whole-set query. Server sort, the multi-value OR
/// filter, the facet aggregation and the keyset cursor must all agree on the
/// same scoped set.
#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn audit_list_sorts_filters_facets_and_pages_over_the_whole_set() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        for (action, result_kind) in
            [("aaa.one", "succeeded"), ("bbb.two", "failed"), ("ccc.three", "succeeded")]
        {
            append_irg227_event(&fixture, action, result_kind).await?;
        }

        let mut query = audit_repository::ListAuditEventsQuery {
            workspace_id: Some(fixture.workspace_id),
            library_id: Some(fixture.library_id),
            subject_filter: audit_repository::AuditEventSubjectFilter::default(),
            filters: audit_repository::AuditEventMultiFilters::default(),
            search: None,
            internal: false,
            search_hints: audit_repository::AuditSearchHints::default(),
            sort: audit_repository::AuditSort {
                key: audit_repository::AuditSortKey::Action,
                order: audit_repository::AuditSortOrder::Asc,
            },
            limit: 50,
            cursor: None,
        };

        let page = audit_repository::list_audit_events(fixture.pool(), &query).await?;
        let actions: Vec<String> = page.items.iter().map(|row| row.action_kind.clone()).collect();
        let positions: Vec<usize> = ["aaa.one", "bbb.two", "ccc.three"]
            .iter()
            .map(|wanted| {
                actions
                    .iter()
                    .position(|action| action == wanted)
                    .with_context(|| format!("{wanted} missing from the audit page"))
            })
            .collect::<Result<_>>()?;
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "server sort by action ascending must order the page: {actions:?}"
        );

        // Multi-value filter is OR within the column.
        query.filters.action_kinds = vec!["aaa.one".to_string(), "ccc.three".to_string()];
        let filtered = audit_repository::list_audit_events(fixture.pool(), &query).await?;
        assert_eq!(filtered.total, 2);
        assert!(filtered.items.iter().all(|row| row.action_kind != "bbb.two"));
        assert!(filtered.items.iter().all(|row| row.action_kind != "other.action"));

        // NULL is another selected actor value, not an extra AND constraint.
        let suffix = Uuid::now_v7().simple();
        let actor = iam_repository::create_user(
            fixture.pool(),
            &format!("audit-grid-actor-{suffix}"),
            &format!("audit-grid-actor-{suffix}@example.invalid"),
            "Audit grid actor",
            &hash_token(&format!("fixture-password-{suffix}")),
            SystemRole::Admin,
        ).await?;
        sqlx::query("update audit_event set actor_principal_id = $1 where action_kind = 'aaa.one' and id in (select audit_event_id from audit_event_subject where workspace_id = $2)")
            .bind(actor.principal_id)
            .bind(fixture.workspace_id)
            .execute(fixture.pool())
            .await?;
        for (actor_ids, include_null, expected) in [
            (vec![actor.principal_id.to_string()], false, 1),
            (vec![], true, 1),
            (vec![actor.principal_id.to_string()], true, 2),
        ] {
            query.filters.actor_principal_ids = actor_ids;
            query.filters.actor_include_null = include_null;
            let selected = audit_repository::list_audit_events(fixture.pool(), &query).await?;
            assert_eq!(selected.total, expected, "actor IDs and the NULL bucket must OR within the column");
        }

        // Facets aggregate the whole filtered set (here: the default, unfiltered
        // scope) and include every value, not only the current page.
        query.filters = audit_repository::AuditEventMultiFilters::default();
        let facets = audit_repository::list_audit_event_facets(
            fixture.pool(),
            &audit_repository::AuditFacetQuery {
                workspace_id: Some(fixture.workspace_id),
                library_id: Some(fixture.library_id),
                subject_filter: audit_repository::AuditEventSubjectFilter::default(),
                filters: query.filters.clone(),
                search: None,
                internal: false,
                search_hints: audit_repository::AuditSearchHints::default(),
                facet_column: audit_repository::AuditFacetColumn::Action,
                facet_search: None,
                facet_search_hints: audit_repository::AuditSearchHints::default(),
                selected_values: vec![],
                limit: 50,
                offset: 0,
            },
        )
        .await?;
        for expected in ["aaa.one", "bbb.two", "ccc.three"] {
            assert!(
                facets.iter().any(|row| row.value.as_deref() == Some(expected)),
                "facet must list {expected}: {facets:?}"
            );
        }

        // The keyset cursor steps to a different row and does not repeat.
        let mut cursor_query = query.clone();
        cursor_query.sort = audit_repository::AuditSort {
            key: audit_repository::AuditSortKey::CreatedAt,
            order: audit_repository::AuditSortOrder::Desc,
        };
        cursor_query.limit = 1;
        let first = audit_repository::list_audit_events(fixture.pool(), &cursor_query).await?;
        assert_eq!(first.items.len(), 1);
        let first_id = first.items[0].id;
        assert!(first.next_cursor.is_some());
        cursor_query.cursor = first.next_cursor.clone();
        let second = audit_repository::list_audit_events(fixture.pool(), &cursor_query).await?;
        assert_eq!(second.items.len(), 1);
        assert_ne!(second.items[0].id, first_id);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// IRG-227: HTTP validation, comma-separated multi-filter forwarding, facet
/// self-exclusion and workspace permission scoping.
#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn audit_http_validates_sort_forwards_filters_and_scopes_facets() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        let reviewer = fixture.mint_audit_review_token("audit-http-contract").await?;
        append_irg227_event(&fixture, "aaa.one", "succeeded").await?;
        append_irg227_event(&fixture, "bbb.two", "failed").await?;

        // An unknown sort column or direction is rejected, not defaulted.
        let (status, _) = fixture.rest_get(&reviewer, "/v1/audit/events?sortBy=bogus").await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) =
            fixture.rest_get(&reviewer, "/v1/audit/events?sortOrder=sideways").await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Comma-separated multi-values are forwarded and OR together.
        let (status, body) =
            fixture.rest_get(&reviewer, "/v1/audit/events?actionKind=aaa.one,bbb.two").await?;
        assert_eq!(status, StatusCode::OK);
        let events = body["items"].as_array().context("audit items must be an array")?;
        assert!(!events.is_empty());
        assert!(
            events.iter().all(|event| {
                matches!(event["actionKind"].as_str(), Some("aaa.one" | "bbb.two"))
            })
        );

        // An unknown facet column is rejected.
        let (status, _) =
            fixture.rest_get(&reviewer, "/v1/audit/events/facets?facetColumn=bogus").await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Self-exclusion: selecting aaa.one still lists bbb.two, because the
        // action column's own selection is dropped for its facet.
        let (status, body) = fixture
            .rest_get(&reviewer, "/v1/audit/events/facets?facetColumn=action&actionKind=aaa.one")
            .await?;
        assert_eq!(status, StatusCode::OK);
        let values = body["values"].as_array().context("facet values must be an array")?;
        assert!(values.iter().any(|value| value["value"] == json!("bbb.two")));
        assert_eq!(body["hasMore"], json!(false));

        let query_string = |pairs: &[(&str, String)]| {
            url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs.iter().map(|(key, value)| (*key, value.as_str()))).finish()
        };
        let hints = json!({"actionKinds":["aaa.one"],"resultKinds":["failed"]}).to_string();
        let search_query = query_string(&[("search", "localized term absent from raw fields".into()), ("searchHints", hints)]);
        let (status, body) = fixture.rest_get(&reviewer, &format!("/v1/audit/events?{search_query}")).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], json!(2), "global typed hints OR across displayed columns and raw text");

        let facet_query = query_string(&[
            ("facetColumn", "result".into()),
            ("search", "localized global label".into()),
            ("searchHints", json!({"actionKinds":["aaa.one"]}).to_string()),
            ("facetSearch", "localized value label".into()),
            ("facetSearchHints", json!({"resultKinds":["succeeded"]}).to_string()),
        ]);
        let (status, body) = fixture.rest_get(&reviewer, &format!("/v1/audit/events/facets?{facet_query}")).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["values"].as_array().context("facet values")?.len(), 1);
        assert_eq!(body["values"][0]["value"], json!("succeeded"));
        assert_eq!(body["values"][0]["count"], json!(1), "local hints cannot overwrite global scope");

        sqlx::query("update audit_event set created_at = $1::timestamptz where action_kind = 'aaa.one' and id in (select audit_event_id from audit_event_subject where workspace_id = $2)")
            .bind("2026-08-02T10:00:00.123Z")
            .bind(fixture.workspace_id)
            .execute(fixture.pool()).await?;
        let range = json!({"createdAtRange":{"min":"2026-08-02T10:00:00Z","max":"2026-08-02T10:00:01Z"}});
        for (path, pairs) in [
            ("/v1/audit/events", vec![("search", "localized second label".into()), ("searchHints", range.to_string())]),
            ("/v1/audit/events/facets", vec![("facetColumn", "created_at".into()), ("facetSearch", "localized second label".into()), ("facetSearchHints", range.to_string())]),
        ] {
            let encoded = query_string(&pairs);
            let (status, body) = fixture.rest_get(&reviewer, &format!("{path}?{encoded}")).await?;
            assert_eq!(status, StatusCode::OK);
            if path.ends_with("facets") {
                assert_eq!(body["values"].as_array().context("date facet values")?.len(), 1);
                assert_eq!(body["values"][0]["count"], json!(1));
            } else {
                assert_eq!(body["total"], json!(1));
            }
        }
        let null_search = query_string(&[("search", "localized NULL label".into()), ("searchHints", json!({"actorNull":true}).to_string())]);
        let (status, body) = fixture.rest_get(&reviewer, &format!("/v1/audit/events?{null_search}")).await?;
        assert_eq!(status, StatusCode::OK);
        assert!(body["items"].as_array().context("NULL actor rows")?.len() >= 2);
        assert!(body["items"].as_array().context("NULL actor rows")?.iter().all(|event| event["actorPrincipalId"].is_null()));

        for hints in [
            json!({"unknown":true}),
            json!({"createdAtRange":{"min":"not-a-date","max":"2026-08-02T10:00:01Z"}}),
            json!({"createdAtRange":{"min":"2026-08-02T10:00:01Z","max":"2026-08-02T10:00:00Z"}}),
        ] {
            let invalid = query_string(&[("searchHints", hints.to_string())]);
            assert_eq!(fixture.rest_get(&reviewer, &format!("/v1/audit/events?{invalid}")).await?.0, StatusCode::BAD_REQUEST);
        }
        let wrong_column = query_string(&[("facetColumn", "actor".into()), ("facetSearchHints", json!({"resultKinds":["failed"]}).to_string())]);
        assert_eq!(fixture.rest_get(&reviewer, &format!("/v1/audit/events/facets?{wrong_column}")).await?.0, StatusCode::BAD_REQUEST);

        // A second workspace's event is invisible to this workspace reviewer.
        let other_workspace = catalog_repository::create_workspace(
            fixture.pool(),
            &format!("irg227-other-{}", Uuid::now_v7().simple()),
            "IRG227 Other Workspace",
            None,
        )
        .await?;
        let other_library = catalog_repository::create_library(
            fixture.pool(),
            other_workspace.id,
            &format!("irg227-other-lib-{}", Uuid::now_v7().simple()),
            "IRG227 Other Library",
            None,
            None,
        )
        .await?;
        append_irg227_event_in(
            &fixture,
            "other.action",
            other_workspace.id,
            other_library.id,
            "succeeded",
        )
        .await?;
        let (status, body) =
            fixture.rest_get(&reviewer, "/v1/audit/events?actionKind=other.action").await?;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body["items"].as_array().context("audit items must be an array")?.is_empty(),
            "a reviewer must not see another workspace's audit events"
        );

        let mut selected_actors = Vec::new();
        for (action, workspace) in [("aaa.one", fixture.workspace_id), ("other.action", other_workspace.id)] {
            let suffix = Uuid::now_v7().simple();
            let actor = iam_repository::create_user(fixture.pool(), &format!("selected-{suffix}"), &format!("selected-{suffix}@example.invalid"), "Selected audit actor", &hash_token(&format!("fixture-{suffix}")), SystemRole::Admin).await?;
            sqlx::query("update audit_event set actor_principal_id = $1 where action_kind = $2 and id in (select audit_event_id from audit_event_subject where workspace_id = $3)")
                .bind(actor.principal_id).bind(action).bind(workspace).execute(fixture.pool()).await?;
            selected_actors.push(actor.principal_id.to_string());
        }
        let lookup = query_string(&[("facetColumn", "actor".into()), ("selectedValueColumn", "actor,actor".into()), ("selectedValue", selected_actors.join(","))]);
        let (status, body) = fixture.rest_get(&reviewer, &format!("/v1/audit/events/facets?{lookup}")).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["values"].as_array().context("scoped selected actors")?.len(), 1);
        assert_eq!(body["values"][0]["value"], json!(selected_actors[0]));
        assert_eq!(body["values"][0]["label"], json!("Selected audit actor"));

        let filtered_lookup = query_string(&[("facetColumn", "actor".into()), ("selectedValueColumn", "actor".into()), ("selectedValue", selected_actors[0].clone()), ("search", "localized failed result".into()), ("searchHints", json!({"resultKinds":["failed"]}).to_string())]);
        let (status, body) = fixture.rest_get(&reviewer, &format!("/v1/audit/events/facets?{filtered_lookup}")).await?;
        assert_eq!(status, StatusCode::OK);
        assert!(body["values"].as_array().context("filtered selected actors")?.is_empty(), "selected labels must retain global hints and permissions");

        let mut many_actors = Vec::new();
        for index in 0..101 {
            let suffix = Uuid::now_v7().simple();
            let actor = iam_repository::create_user(fixture.pool(), &format!("many-selected-{suffix}"), &format!("many-selected-{suffix}@example.invalid"), &format!("Selected actor {index}"), &hash_token(&format!("fixture-{suffix}")), SystemRole::Admin).await?;
            let event = fixture.append_audit_event(&format!("selected.actor.{index}"), vec![AppendAuditEventSubjectCommand {
                subject_kind: "library".to_string(), subject_id: fixture.library_id,
                workspace_id: Some(fixture.workspace_id), library_id: Some(fixture.library_id), document_id: None,
            }]).await?;
            sqlx::query("update audit_event set actor_principal_id = $1 where id = $2").bind(actor.principal_id).bind(event).execute(fixture.pool()).await?;
            many_actors.push(actor.principal_id.to_string());
        }
        let lookup = query_string(&[("facetColumn", "actor".into()), ("selectedValueColumn", vec!["actor"; many_actors.len()].join(",")), ("selectedValue", many_actors.join(",")), ("facetLimit", "100".into())]);
        let (status, body) = fixture.rest_get(&reviewer, &format!("/v1/audit/events/facets?{lookup}")).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["values"].as_array().context("complete bounded selected actors")?.len(), 101, "selected actor lookup must not truncate at the facet menu's page limit");
        assert_eq!(body["hasMore"], json!(false));

        // High-cardinality facets must exhaust at the offset budget rather
        // than advertise a next page that silently replays an earlier page.
        sqlx::query(
            "with events as (
                insert into audit_event (surface_kind, action_kind, result_kind, created_at)
                select 'rest', 'audit.facet.boundary', 'succeeded',
                       '2026-01-01T00:00:00Z'::timestamptz + n * interval '1 second'
                from generate_series(1, 10052) n returning id
            )
            insert into audit_event_subject
                (audit_event_id, subject_kind, subject_id, workspace_id, library_id)
            select id, 'library', $1, $2, $1 from events",
        )
        .bind(fixture.library_id)
        .bind(fixture.workspace_id)
        .execute(fixture.pool())
        .await?;
        let boundary_path = "/v1/audit/events/facets?facetColumn=created_at&actionKind=audit.facet.boundary&facetLimit=50";
        let (status, body) = fixture
            .rest_get(&reviewer, &format!("{boundary_path}&facetOffset=9950"))
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["values"].as_array().context("penultimate facet page")?.len(), 50);
        assert_eq!(body["hasMore"], json!(true));
        let (status, body) = fixture
            .rest_get(&reviewer, &format!("{boundary_path}&facetOffset=10000"))
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["values"].as_array().context("terminal bounded facet page")?.len(), 50);
        assert_eq!(body["hasMore"], json!(false));
        let (status, _) = fixture
            .rest_get(&reviewer, &format!("{boundary_path}&facetOffset=10050"))
            .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "never clamp and replay an earlier page");
        let narrowed = query_string(&[
            ("facetColumn", "created_at".into()),
            ("actionKind", "audit.facet.boundary".into()),
            ("facetSearch", "last two timestamps".into()),
            ("facetSearchHints", json!({"createdAtRange": {
                "min": "2026-01-01T02:47:31Z", "max": "2026-01-01T02:47:33Z"
            }}).to_string()),
        ]);
        let (status, body) = fixture
            .rest_get(&reviewer, &format!("/v1/audit/events/facets?{narrowed}"))
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["values"].as_array().context("search beyond facet offset budget")?.len(), 2);
        assert_eq!(body["hasMore"], json!(false));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// IRG-227: every exact-subject filter must stay inside the same authorized
/// subject scope as raw search and response serialization. Plant one subject
/// row that names a fresh hidden id under `foreign` for each exact-filter kind,
/// then require both the list and the facet surface to select nothing under
/// either of the fixture's own scopes.
async fn assert_exact_subject_filters_stay_within_scope(
    fixture: &AuditEventsFixture,
    reviewer: &str,
    event_id: Uuid,
    foreign_workspace_id: Uuid,
    foreign_library_id: Uuid,
) -> Result<()> {
    const HIDDEN_SUBJECT_FILTERS: &[(&str, &str)] = &[
        ("knowledge_document", "knowledgeDocumentId"),
        ("knowledge_revision", "knowledgeRevisionId"),
        ("knowledge_bundle", "contextBundleId"),
        ("query_session", "querySessionId"),
        ("query_execution", "queryExecutionId"),
        ("runtime_execution", "runtimeExecutionId"),
        ("async_operation", "asyncOperationId"),
    ];
    for &(kind, parameter) in HIDDEN_SUBJECT_FILTERS {
        let hidden_id = Uuid::now_v7();
        sqlx::query("insert into audit_event_subject (audit_event_id, subject_kind, subject_id, workspace_id, library_id, document_id) values ($1, $2, $3, $4, $5, $6)")
            .bind(event_id).bind(kind).bind(hidden_id).bind(foreign_workspace_id).bind(foreign_library_id)
            .bind((kind == "knowledge_document").then_some(hidden_id)).execute(fixture.pool()).await?;
        for scope in [
            format!("libraryId={}", fixture.library_id),
            format!("workspaceId={}", fixture.workspace_id),
        ] {
            let (status, body) = fixture
                .rest_get(reviewer, &format!("/v1/audit/events?{scope}&{parameter}={hidden_id}"))
                .await?;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                body["total"],
                json!(0),
                "hidden {kind} must not select a visible event through an exact filter"
            );
            let (status, body) = fixture
                .rest_get(
                    reviewer,
                    &format!(
                        "/v1/audit/events/facets?{scope}&{parameter}={hidden_id}&facetColumn=action"
                    ),
                )
                .await?;
            assert_eq!(status, StatusCode::OK);
            assert!(
                body["values"].as_array().context("scoped exact subject facets")?.is_empty(),
                "hidden {kind} must not select facet values"
            );
        }
    }
    Ok(())
}

/// IRG-227: the whole-table search must never become a hidden-field oracle. A
/// private `internal_message` is matched only on the authorized internal view,
/// and no view selects a visible event through a subject row that lies outside
/// the caller's scope.
#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn audit_search_hides_internal_metadata_and_out_of_scope_subject_rows() -> Result<()> {
    let fixture = AuditEventsFixture::create().await?;

    let result = async {
        const PRIVATE_NEEDLE: &str = "hiddenprivatenoodle";
        let admin_session = fixture.mint_admin_session("audit-oracle-admin").await?;
        let reviewer = fixture.mint_audit_review_token("audit-oracle-reviewer").await?;

        // A visible, authorized (REST) event whose internal message holds the
        // private word. The non-internal mapper never serializes it.
        let seed = fixture
            .append_audit_event(
                "rest.sensitive",
                vec![AppendAuditEventSubjectCommand {
                    subject_kind: "library".to_string(),
                    subject_id: fixture.library_id,
                    workspace_id: Some(fixture.workspace_id),
                    library_id: Some(fixture.library_id),
                    document_id: None,
                }],
            )
            .await?;
        sqlx::query("update audit_event set internal_message = $2 where id = $1")
            .bind(seed)
            .bind(PRIVATE_NEEDLE)
            .execute(fixture.pool())
            .await?;

        // A visible event carrying an out-of-scope subject row (with a foreign
        // library id) alongside an in-scope subject row.
        let foreign_workspace = catalog_repository::create_workspace(
            fixture.pool(),
            &format!("irg227-oracle-fw-{}", Uuid::now_v7().simple()),
            "IRG227 Foreign Workspace",
            None,
        )
        .await?;
        let foreign_library = catalog_repository::create_library(
            fixture.pool(),
            foreign_workspace.id,
            &format!("irg227-oracle-fl-{}", Uuid::now_v7().simple()),
            "IRG227 Foreign Library",
            None,
            None,
        )
        .await?;
        let mixed = audit_repository::append_audit_event(
            fixture.pool(),
            audit_repository::NewAuditEvent {
                actor_principal_id: None,
                surface_kind: "rest".to_string(),
                action_kind: "rest.mixed_subject".to_string(),
                request_id: None,
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some("mixed subject event".to_string()),
                internal_message: None,
            },
            &[
                audit_repository::NewAuditEventSubject {
                    subject_kind: "library".to_string(),
                    subject_id: fixture.library_id,
                    workspace_id: Some(fixture.workspace_id),
                    library_id: Some(fixture.library_id),
                    document_id: None,
                },
                audit_repository::NewAuditEventSubject {
                    subject_kind: "library".to_string(),
                    subject_id: foreign_library.id,
                    workspace_id: Some(foreign_workspace.id),
                    library_id: Some(foreign_library.id),
                    document_id: None,
                },
            ],
        )
        .await?;

        // Every exact-subject filter must stay inside the same authorized
        // subject scope as raw search and response serialization.
        assert_exact_subject_filters_stay_within_scope(
            &fixture,
            &reviewer,
            mixed.id,
            foreign_workspace.id,
            foreign_library.id,
        )
        .await?;

        // The private word matches the internal view (system admin) ...
        let (status, body) = fixture
            .admin_get(
                &admin_session,
                &format!("/v1/audit/events?internal=true&search={PRIVATE_NEEDLE}"),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["total"],
            json!(1),
            "the authorized internal view matches its own internal metadata"
        );

        // ... and does not match the non-internal view, where the field is not
        // even serialized.
        let (status, body) = fixture
            .rest_get(&reviewer, &format!("/v1/audit/events?search={PRIVATE_NEEDLE}"))
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], json!(0), "hidden internal metadata must not be a search oracle");
        assert!(body["items"].as_array().context("audit items")?.is_empty());
        let (status, body) =
            fixture.rest_get(&reviewer, "/v1/audit/events?actionKind=rest.sensitive").await?;
        assert_eq!(status, StatusCode::OK);
        let event = body["items"]
            .as_array()
            .context("audit items")?
            .iter()
            .find(|event| event["actionKind"] == json!("rest.sensitive"))
            .context("the visible event must be listed")?;
        assert!(
            event.get("internalMessage").is_none(),
            "non-internal view must not serialize internalMessage"
        );

        // A non-admin asking for the internal view is refused outright.
        let (status, _) = fixture.rest_get(&reviewer, "/v1/audit/events?internal=true").await?;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // The foreign subject id cannot select the visible event ...
        let (status, body) = fixture
            .rest_get(
                &reviewer,
                &format!(
                    "/v1/audit/events?libraryId={}&search={}",
                    fixture.library_id, foreign_library.id
                ),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["total"],
            json!(0),
            "an out-of-scope subject id must not select a visible event"
        );

        // ... while the in-scope event stays searchable through visible text.
        let (status, body) = fixture
            .rest_get(
                &reviewer,
                &format!("/v1/audit/events?libraryId={}&search=rest.mixed", fixture.library_id),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], json!(1), "the in-scope event remains searchable");
        assert!(
            body["items"]
                .as_array()
                .context("audit items")?
                .iter()
                .any(|event| event["id"] == json!(mixed.id))
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
