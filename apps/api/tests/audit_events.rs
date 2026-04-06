#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
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
    interfaces::http::{auth::hash_token, router},
    services::audit_service::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
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
}

impl AuditEventsFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for audit events test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.bootstrap_token = Some("audit-events-bootstrap".to_string());
        settings.bootstrap_claim_enabled = true;
        settings.legacy_ui_bootstrap_enabled = false;
        settings.legacy_bootstrap_token_endpoint_enabled = false;
        settings.destructive_fresh_bootstrap_required = true;
        settings.destructive_allow_legacy_startup_side_effects = false;

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
}

fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let persistence = Persistence {
        postgres,
        redis: redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for audit events state")?,
    };
    let arango_client = Arc::new(ArangoClient::from_settings(&settings)?);
    Ok(AppState::from_dependencies(settings, persistence, arango_client))
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
        let events = body.as_array().context("audit events response must be an array")?;
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
            let events = body.as_array().context("audit events response must be an array")?;
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
        let events = body.as_array().context("audit events response must be an array")?;
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
        let events = body.as_array().context("audit events response must be an array")?;
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
        let events = body.as_array().context("audit events response must be an array")?;
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
