//! Integration tests for the outbound webhook subsystem.
//!
//! All tests require real Postgres — they are gated by `#[ignore]`.
//! Run with:
//!   cargo test -p ironrag-backend --test webhook_outbound -- --include-ignored

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{Row, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    domains::webhook::{WebhookEvent, revision_ready_event_id},
    infra::{
        persistence::Persistence,
        repositories::{
            iam_repository, webhook_outbox_repository,
            webhook_repository::{self, NewWebhookDeliveryAttempt, NewWebhookSubscription},
        },
    },
    interfaces::http::{auth::hash_token, router},
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        webhook::{
            custom_headers, outbound::publish_webhook_event,
            outbox::drain_webhook_lifecycle_outbox_once,
        },
    },
    shared::secret_encryption::SecretPurpose,
};

fn test_credential_master_key() -> String {
    STANDARD.encode([47_u8; 32])
}

// ============================================================================
// Temp database (Postgres only — outbound tests don't need external services)
// ============================================================================

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("webhook_outbound_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect admin postgres for webhook_outbound test")?;
        terminate_connections(&admin_pool, &database_name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale db {database_name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create db {database_name}"))?;
        admin_pool.close().await;
        Ok(Self {
            name: database_name.clone(),
            admin_url,
            database_url: replace_database_name(base_database_url, &database_name)?,
        })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect admin postgres for webhook_outbound cleanup")?;
        terminate_connections(&admin_pool, &self.name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop db {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

// ============================================================================
// Fixture
// ============================================================================

struct WebhookOutboundFixture {
    pub state: AppState,
    temp_database: TempDatabase,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub admin_token: String,
}

impl WebhookOutboundFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for webhook_outbound test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;
        settings.credential_master_key = Some(test_credential_master_key());
        settings.credential_encryption_write_enabled = true;

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect webhook_outbound postgres")?;

        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply migrations for webhook_outbound test")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for webhook_outbound test")?;
        let persistence = Persistence::for_tests(postgres, redis);
        let state = AppState::from_dependencies(settings, persistence)?;

        let workspace = state
            .canonical_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("wh-outbound-ws-{}", Uuid::now_v7().simple())),
                    display_name: "Webhook Outbound Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create workspace for webhook_outbound")?;

        let library = state
            .canonical_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("wh-outbound-lib-{}", Uuid::now_v7().simple())),
                    display_name: "Webhook Outbound Library".to_string(),
                    description: None,
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create library for webhook_outbound")?;

        // Mint an admin token for HTTP requests
        let admin_token_plaintext = format!("wh-out-admin-{}", Uuid::now_v7().simple());
        let token_row = iam_repository::create_api_token(
            &state.persistence.postgres,
            Some(workspace.id),
            "webhook-outbound-test-admin",
            "rest",
            None,
            None,
        )
        .await
        .context("failed to create admin token for webhook_outbound")?;
        iam_repository::create_api_token_secret(
            &state.persistence.postgres,
            token_row.principal_id,
            &hash_token(&admin_token_plaintext),
        )
        .await
        .context("failed to create admin token secret for webhook_outbound")?;
        // Grant workspace admin
        iam_repository::create_grant(
            &state.persistence.postgres,
            token_row.principal_id,
            "workspace",
            workspace.id,
            "workspace_admin",
            None,
            None,
        )
        .await
        .context("failed to grant workspace_admin for webhook_outbound")?;

        Ok(Self {
            state,
            temp_database,
            workspace_id: workspace.id,
            library_id: library.id,
            admin_token: admin_token_plaintext,
        })
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    fn pool(&self) -> &sqlx::PgPool {
        &self.state.persistence.postgres
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }

    /// Create a subscription directly via the repository (bypasses HTTP auth for unit-like tests).
    async fn create_subscription_direct(
        &self,
        library_id: Option<Uuid>,
        event_types: &[&str],
        active: bool,
        target_url: &str,
    ) -> Result<webhook_repository::WebhookSubscriptionViewRow> {
        let plaintext_secret = format!("sub-secret-{}", Uuid::now_v7().simple());
        let subscription_id = Uuid::now_v7();
        let encrypted_secret = self.state.credential_cipher.encrypt(
            SecretPurpose::WebhookSigningSecret,
            subscription_id,
            &plaintext_secret,
        )?;
        let serialized_custom_headers = custom_headers::validate_and_serialize(&json!({}))?;
        let encrypted_custom_headers = self.state.credential_cipher.encrypt(
            SecretPurpose::WebhookCustomHeaders,
            subscription_id,
            serialized_custom_headers.as_str(),
        )?;
        let row = webhook_repository::create_webhook_subscription(
            self.pool(),
            &NewWebhookSubscription {
                id: subscription_id,
                workspace_id: self.workspace_id,
                library_id,
                display_name: "Test Subscription".to_string(),
                target_url: target_url.to_string(),
                secret: encrypted_secret,
                event_types: event_types.iter().map(|s| s.to_string()).collect(),
                custom_headers_json: encrypted_custom_headers,
                created_by_principal_id: None,
            },
        )
        .await
        .context("failed to create webhook subscription")?;

        if !active {
            webhook_repository::update_webhook_subscription(
                self.pool(),
                row.id,
                &webhook_repository::UpdateWebhookSubscription {
                    display_name: None,
                    target_url: None,
                    secret: None,
                    event_types: None,
                    custom_headers_json: None,
                    active: Some(false),
                },
            )
            .await
            .context("failed to deactivate subscription")?;
        }

        Ok(row)
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn replace_database_name(url: &str, new_db: &str) -> Result<String> {
    let (without_query, query_suffix) =
        url.split_once('?').map_or((url, None), |(p, s)| (p, Some(s)));
    let slash = without_query
        .rfind('/')
        .with_context(|| format!("database url missing database name: {url}"))?;
    let mut rebuilt = format!("{}{new_db}", &without_query[..=slash]);
    if let Some(q) = query_suffix {
        rebuilt.push('?');
        rebuilt.push_str(q);
    }
    Ok(rebuilt)
}

async fn terminate_connections(pool: &sqlx::PgPool, db_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid) from pg_stat_activity \
         where datname = $1 and pid <> pg_backend_pid()",
    )
    .bind(db_name)
    .execute(pool)
    .await
    .with_context(|| format!("failed to terminate connections for {db_name}"))?;
    Ok(())
}

async fn http_get_json(app: Router, uri: &str, token: &str) -> Result<(StatusCode, Value)> {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .context("failed to build GET request")?;
    let resp = app.oneshot(req).await.context("GET request failed")?;
    let status = resp.status();
    let body =
        resp.into_body().collect().await.context("failed to collect GET response body")?.to_bytes();
    let json: Value = serde_json::from_slice(&body).context("failed to decode GET response")?;
    Ok((status, json))
}

async fn http_post_json(
    app: Router,
    uri: &str,
    token: &str,
    body: Value,
) -> Result<(StatusCode, Value)> {
    let body_bytes = serde_json::to_vec(&body).context("failed to serialize POST body")?;
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body_bytes))
        .context("failed to build POST request")?;
    let resp = app.oneshot(req).await.context("POST request failed")?;
    let status = resp.status();
    let body = resp
        .into_body()
        .collect()
        .await
        .context("failed to collect POST response body")?
        .to_bytes();
    let json: Value = serde_json::from_slice(&body).context("failed to decode POST response")?;
    Ok((status, json))
}

async fn http_patch_json(
    app: Router,
    uri: &str,
    token: &str,
    body: Value,
) -> Result<(StatusCode, Value)> {
    let body_bytes = serde_json::to_vec(&body).context("failed to serialize PATCH body")?;
    let req = Request::builder()
        .method("PATCH")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body_bytes))
        .context("failed to build PATCH request")?;
    let resp = app.oneshot(req).await.context("PATCH request failed")?;
    let status = resp.status();
    let body = resp
        .into_body()
        .collect()
        .await
        .context("failed to collect PATCH response body")?
        .to_bytes();
    let json: Value = serde_json::from_slice(&body).context("failed to decode PATCH response")?;
    Ok((status, json))
}

async fn http_delete(app: Router, uri: &str, token: &str) -> Result<StatusCode> {
    let req = Request::builder()
        .method("DELETE")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .context("failed to build DELETE request")?;
    let resp = app.oneshot(req).await.context("DELETE request failed")?;
    Ok(resp.status())
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_subscription_crud_round_trip() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        let workspace_id = fixture.workspace_id;
        let token = &fixture.admin_token;

        // CREATE
        let create_body = json!({
            "displayName": "Alpha Suite Subscription",
            "targetUrl": "https://example.com/webhook",
            "secret": "x".repeat(48),
            "eventTypes": ["revision.ready", "document.deleted"],
            "customHeaders": {
                "Authorization": format!("Bearer {}", "x".repeat(32))
            }
        });
        let (create_status, create_resp) = http_post_json(
            fixture.app(),
            &format!("/v1/webhooks/workspaces/{workspace_id}/subscriptions"),
            token,
            create_body,
        )
        .await?;
        assert_eq!(
            create_status,
            StatusCode::CREATED,
            "create should return 201; body={create_resp}"
        );
        let sub_id = create_resp["id"].as_str().context("id missing in create response")?;
        let subscription_id = Uuid::parse_str(sub_id).context("created subscription id is UUID")?;
        assert_eq!(create_resp["displayName"], "Alpha Suite Subscription");
        assert_eq!(create_resp["active"], true);
        assert_eq!(create_resp["eventTypes"].as_array().map(|a| a.len()), Some(2));
        assert!(
            create_resp.get("customHeaders").is_none(),
            "secret-bearing custom headers must never be returned"
        );
        let stored_headers: Value = sqlx::query_scalar(
            "select custom_headers_json from webhook_subscription where id = $1",
        )
        .bind(subscription_id)
        .fetch_one(fixture.pool())
        .await
        .context("load custom headers at rest")?;
        let stored_envelope = stored_headers
            .as_str()
            .context("custom headers must be stored as an encrypted JSON string")?;
        assert!(stored_envelope.starts_with("ironrag:enc:v3:xchacha20poly1305:"));
        assert!(!stored_envelope.contains("custom-header-at-rest-regression"));
        let decrypted_headers = custom_headers::decrypt_and_validate_stored(
            &fixture.state.credential_cipher,
            subscription_id,
            &stored_headers,
        )?;
        assert!(decrypted_headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("authorization")
                && value == format!("Bearer {}", "x".repeat(32))
        }));

        // GET by id
        let (get_status, get_resp) =
            http_get_json(fixture.app(), &format!("/v1/webhooks/subscriptions/{sub_id}"), token)
                .await?;
        assert_eq!(get_status, StatusCode::OK, "GET should return 200; body={get_resp}");
        assert_eq!(get_resp["id"], sub_id);
        assert_eq!(get_resp["targetUrl"], "https://example.com/webhook");

        // LIST
        let (list_status, list_resp) = http_get_json(
            fixture.app(),
            &format!("/v1/webhooks/workspaces/{workspace_id}/subscriptions"),
            token,
        )
        .await?;
        assert_eq!(list_status, StatusCode::OK, "list should return 200; body={list_resp}");
        let items = list_resp["items"].as_array().context("list response should have items")?;
        assert!(
            items.iter().any(|s| s["id"] == sub_id),
            "created subscription should appear in list"
        );

        // PATCH — deactivate and rename
        let (patch_status, patch_resp) = http_patch_json(
            fixture.app(),
            &format!("/v1/webhooks/subscriptions/{sub_id}"),
            token,
            json!({ "displayName": "Alpha Suite Subscription Updated", "active": false }),
        )
        .await?;
        assert_eq!(patch_status, StatusCode::OK, "PATCH should return 200; body={patch_resp}");
        assert_eq!(patch_resp["displayName"], "Alpha Suite Subscription Updated");
        assert_eq!(patch_resp["active"], false);

        // DELETE
        let delete_status =
            http_delete(fixture.app(), &format!("/v1/webhooks/subscriptions/{sub_id}"), token)
                .await?;
        assert_eq!(delete_status, StatusCode::NO_CONTENT, "DELETE should return 204");

        // GET after delete — should 404
        let (get_after_status, _) =
            http_get_json(fixture.app(), &format!("/v1/webhooks/subscriptions/{sub_id}"), token)
                .await?;
        assert_eq!(get_after_status, StatusCode::NOT_FOUND, "GET after delete should return 404");

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_management_lists_use_bounded_keysets_and_minimal_projections() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        let mut subscription_ids = Vec::new();
        for suffix in 0..3 {
            let row = fixture
                .create_subscription_direct(
                    None,
                    &["revision.ready"],
                    true,
                    &format!("https://example.com/page-{suffix}?token=stored-url-value"),
                )
                .await?;
            subscription_ids.push(row.id);
        }

        let (first_status, first_body) = http_get_json(
            fixture.app(),
            &format!(
                "/v1/webhooks/workspaces/{}/subscriptions?limit=2",
                fixture.workspace_id
            ),
            &fixture.admin_token,
        )
        .await?;
        assert_eq!(first_status, StatusCode::OK, "body={first_body}");
        let first_items = first_body["items"].as_array().context("first page has items")?;
        assert_eq!(first_items.len(), 2, "repository must enforce requested page bound");
        for item in first_items {
            assert!(item.get("secret").is_none());
            assert!(item.get("customHeaders").is_none());
        }
        let next_cursor =
            first_body["nextCursor"].as_str().context("first page must carry a next cursor")?;
        let (second_status, second_body) = http_get_json(
            fixture.app(),
            &format!(
                "/v1/webhooks/workspaces/{}/subscriptions?limit=2&cursor={next_cursor}",
                fixture.workspace_id
            ),
            &fixture.admin_token,
        )
        .await?;
        assert_eq!(second_status, StatusCode::OK, "body={second_body}");
        let second_items = second_body["items"].as_array().context("second page has items")?;
        assert_eq!(second_items.len(), 1);
        assert_ne!(first_items[0]["id"], second_items[0]["id"]);
        assert_ne!(first_items[1]["id"], second_items[0]["id"]);
        assert!(
            second_body["nextCursor"].is_null(),
            "the exhausted page must not carry a further cursor"
        );

        let (invalid_cursor_status, _) = http_get_json(
            fixture.app(),
            &format!(
                "/v1/webhooks/workspaces/{}/subscriptions?cursor=not-a-valid-cursor",
                fixture.workspace_id
            ),
            &fixture.admin_token,
        )
        .await?;
        assert_eq!(invalid_cursor_status, StatusCode::BAD_REQUEST);

        let subscription_id = subscription_ids[0];
        let mut attempt_ids = Vec::new();
        for suffix in 0..3 {
            let attempt = webhook_repository::create_webhook_delivery_attempt(
                fixture.pool(),
                &NewWebhookDeliveryAttempt {
                    subscription_id,
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    event_type: "revision.ready".to_string(),
                    event_id: format!("pagination-event-{suffix}-{}", Uuid::now_v7()),
                    occurred_at: Utc::now(),
                    payload_json: json!({"privatePayload": "must-never-reach-management-api"}),
                    target_url: "https://example.com/attempt?token=compatibility-value".to_string(),
                },
            )
            .await?;
            attempt_ids.push(attempt.id);
        }
        sqlx::query(
            "update webhook_delivery_attempt
             set response_body_excerpt = 'must-never-reach-management-api'
             where id = $1",
        )
        .bind(attempt_ids[0])
        .execute(fixture.pool())
        .await?;

        let (attempt_status, attempt_body) = http_get_json(
            fixture.app(),
            &format!("/v1/webhooks/subscriptions/{subscription_id}/attempts?limit=2"),
            &fixture.admin_token,
        )
        .await?;
        assert_eq!(attempt_status, StatusCode::OK, "body={attempt_body}");
        let attempt_items = attempt_body["items"].as_array().context("attempt page has items")?;
        assert_eq!(attempt_items.len(), 2);
        let serialized_attempt_page = serde_json::to_string(attempt_items)?;
        assert!(!serialized_attempt_page.contains("must-never-reach-management-api"));
        for item in attempt_items {
            for forbidden_field in [
                "payloadJson",
                "responseBodyExcerpt",
                "jobId",
                "deliveryLeaseToken",
            ] {
                assert!(item.get(forbidden_field).is_none(), "exposed {forbidden_field}");
            }
        }
        let attempt_next_cursor = attempt_body["nextCursor"]
            .as_str()
            .context("attempt first page must carry a next cursor")?;
        let (attempt_second_status, attempt_second_body) = http_get_json(
            fixture.app(),
            &format!(
                "/v1/webhooks/subscriptions/{subscription_id}/attempts?limit=2&cursor={attempt_next_cursor}"
            ),
            &fixture.admin_token,
        )
        .await?;
        assert_eq!(attempt_second_status, StatusCode::OK, "body={attempt_second_body}");
        assert_eq!(attempt_second_body["items"].as_array().map(Vec::len), Some(1));

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn global_subscription_ids_are_uniformly_hidden_outside_the_tenant_scope() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        let other_workspace = fixture
            .state
            .canonical_services
            .catalog
            .create_workspace(
                &fixture.state,
                CreateWorkspaceCommand {
                    slug: Some(format!("wh-hidden-ws-{}", Uuid::now_v7().simple())),
                    display_name: "Hidden Webhook Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await?;
        let other_library = fixture
            .state
            .canonical_services
            .catalog
            .create_library(
                &fixture.state,
                CreateLibraryCommand {
                    workspace_id: other_workspace.id,
                    slug: Some(format!("wh-hidden-lib-{}", Uuid::now_v7().simple())),
                    display_name: "Hidden Webhook Library".to_string(),
                    description: None,
                    created_by_principal_id: None,
                },
            )
            .await?;
        let subscription_id = Uuid::now_v7();
        let secret = fixture.state.credential_cipher.encrypt(
            SecretPurpose::WebhookSigningSecret,
            subscription_id,
            "hidden-tenant-signing-secret-at-least-32-bytes",
        )?;
        let serialized_headers = custom_headers::validate_and_serialize(&json!({}))?;
        let encrypted_headers = fixture.state.credential_cipher.encrypt(
            SecretPurpose::WebhookCustomHeaders,
            subscription_id,
            serialized_headers.as_str(),
        )?;
        webhook_repository::create_webhook_subscription(
            fixture.pool(),
            &NewWebhookSubscription {
                id: subscription_id,
                workspace_id: other_workspace.id,
                library_id: Some(other_library.id),
                display_name: "Hidden tenant subscription".to_string(),
                target_url: "https://example.com/hidden".to_string(),
                secret,
                event_types: vec!["revision.ready".to_string()],
                custom_headers_json: encrypted_headers,
                created_by_principal_id: None,
            },
        )
        .await?;

        let path = format!("/v1/webhooks/subscriptions/{subscription_id}");
        let (get_status, _) = http_get_json(fixture.app(), &path, &fixture.admin_token).await?;
        let (patch_status, _) = http_patch_json(
            fixture.app(),
            &path,
            &fixture.admin_token,
            json!({"displayName": "must not update"}),
        )
        .await?;
        let delete_status = http_delete(fixture.app(), &path, &fixture.admin_token).await?;
        let (attempt_status, _) =
            http_get_json(fixture.app(), &format!("{path}/attempts"), &fixture.admin_token).await?;
        assert_eq!(get_status, StatusCode::NOT_FOUND);
        assert_eq!(patch_status, StatusCode::NOT_FOUND);
        assert_eq!(delete_status, StatusCode::NOT_FOUND);
        assert_eq!(attempt_status, StatusCode::NOT_FOUND);

        let still_present: bool = sqlx::query_scalar(
            "select exists(select 1 from webhook_subscription where id = $1 and active)",
        )
        .bind(subscription_id)
        .fetch_one(fixture.pool())
        .await?;
        assert!(still_present, "foreign-tenant mutation must not touch the row");

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_subscription_rejects_a_library_from_another_workspace() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        let other_workspace = fixture
            .state
            .canonical_services
            .catalog
            .create_workspace(
                &fixture.state,
                CreateWorkspaceCommand {
                    slug: Some(format!("wh-scope-ws-{}", Uuid::now_v7().simple())),
                    display_name: "Webhook Scope Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await?;
        let other_library = fixture
            .state
            .canonical_services
            .catalog
            .create_library(
                &fixture.state,
                CreateLibraryCommand {
                    workspace_id: other_workspace.id,
                    slug: Some(format!("wh-scope-lib-{}", Uuid::now_v7().simple())),
                    display_name: "Webhook Scope Library".to_string(),
                    description: None,
                    created_by_principal_id: None,
                },
            )
            .await?;

        let (status, response) = http_post_json(
            fixture.app(),
            &format!("/v1/webhooks/workspaces/{}/subscriptions", fixture.workspace_id),
            &fixture.admin_token,
            json!({
                "libraryId": other_library.id,
                "displayName": "Cross-scope subscription",
                "targetUrl": "https://example.com/webhook",
            "secret": "x".repeat(48),
                "eventTypes": ["revision.ready"]
            }),
        )
        .await?;

        assert_eq!(status, StatusCode::BAD_REQUEST, "body={response}");
        let created: i64 = sqlx::query_scalar(
            "select count(*)::bigint
             from webhook_subscription
             where workspace_id = $1",
        )
        .bind(fixture.workspace_id)
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(created, 0, "invalid tenant scope must not create a subscription");

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn active_webhook_subscription_quota_is_atomic_for_create_and_reactivation() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        sqlx::query(
            "insert into webhook_subscription (
                id, workspace_id, library_id, display_name, target_url, secret,
                event_types, custom_headers_json, active
             )
             select
                uuidv7(), $1, $2, 'Quota fixture ' || ordinal::text,
                'https://example.com/webhook', 'legacy-synthetic-signing-secret',
                array['revision.ready']::text[], '{}'::jsonb, true
             from generate_series(1, $3::integer) as series(ordinal)",
        )
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(i32::try_from(webhook_repository::MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE)?)
        .execute(fixture.pool())
        .await?;

        let candidate_id = Uuid::now_v7();
        let encrypted_secret = fixture.state.credential_cipher.encrypt(
            SecretPurpose::WebhookSigningSecret,
            candidate_id,
            "quota-candidate-signing-secret",
        )?;
        let serialized_headers = custom_headers::validate_and_serialize(&json!({}))?;
        let encrypted_headers = fixture.state.credential_cipher.encrypt(
            SecretPurpose::WebhookCustomHeaders,
            candidate_id,
            serialized_headers.as_str(),
        )?;
        let create_error = webhook_repository::create_webhook_subscription(
            fixture.pool(),
            &NewWebhookSubscription {
                id: candidate_id,
                workspace_id: fixture.workspace_id,
                library_id: Some(fixture.library_id),
                display_name: "Quota candidate".to_string(),
                target_url: "https://example.com/webhook".to_string(),
                secret: encrypted_secret,
                event_types: vec!["revision.ready".to_string()],
                custom_headers_json: encrypted_headers,
                created_by_principal_id: None,
            },
        )
        .await
        .expect_err("the 101st active subscription must fail");
        assert!(webhook_repository::is_active_webhook_subscription_quota_error(&create_error));

        let raw_bypass_error = sqlx::query(
            "insert into webhook_subscription (
                id, workspace_id, library_id, display_name, target_url, secret,
                event_types, custom_headers_json, active
             ) values (
                $1, $2, $3, 'Old pod quota bypass', 'https://example.com/webhook',
                'legacy-synthetic-signing-secret', array['revision.ready']::text[], '{}'::jsonb,
                true
             )",
        )
        .bind(Uuid::now_v7())
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .execute(fixture.pool())
        .await
        .expect_err("database trigger must reject an old-pod quota bypass");
        assert!(webhook_repository::is_active_webhook_subscription_quota_error(&raw_bypass_error));

        let inactive_id = Uuid::now_v7();
        let concurrent_inactive_id = Uuid::now_v7();
        sqlx::query(
            "insert into webhook_subscription (
                id, workspace_id, library_id, display_name, target_url, secret,
                event_types, custom_headers_json, active
             ) values (
                $1, $2, $3, 'Inactive quota fixture', 'https://example.com/webhook',
                'legacy-synthetic-signing-secret', array['revision.ready']::text[], '{}'::jsonb,
                false
             ), (
                $4, $2, $3, 'Concurrent inactive quota fixture', 'https://example.com/webhook',
                'legacy-synthetic-signing-secret', array['revision.ready']::text[], '{}'::jsonb,
                false
             )",
        )
        .bind(inactive_id)
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(concurrent_inactive_id)
        .execute(fixture.pool())
        .await?;
        let activation_error = webhook_repository::update_webhook_subscription(
            fixture.pool(),
            inactive_id,
            &webhook_repository::UpdateWebhookSubscription {
                display_name: None,
                target_url: None,
                secret: None,
                event_types: None,
                custom_headers_json: None,
                active: Some(true),
            },
        )
        .await
        .expect_err("reactivation above quota must fail");
        assert!(webhook_repository::is_active_webhook_subscription_quota_error(&activation_error));

        sqlx::query(
            "update webhook_subscription
             set active = false
             where id = (
                 select id from webhook_subscription
                 where workspace_id = $1 and active = true
                 order by id limit 1
             )",
        )
        .bind(fixture.workspace_id)
        .execute(fixture.pool())
        .await?;
        let activate = webhook_repository::UpdateWebhookSubscription {
            display_name: None,
            target_url: None,
            secret: None,
            event_types: None,
            custom_headers_json: None,
            active: Some(true),
        };
        let concurrent_activate = webhook_repository::UpdateWebhookSubscription {
            display_name: None,
            target_url: None,
            secret: None,
            event_types: None,
            custom_headers_json: None,
            active: Some(true),
        };
        let (first_activation, second_activation) =
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                tokio::join!(
                    webhook_repository::update_webhook_subscription(
                        fixture.pool(),
                        inactive_id,
                        &activate,
                    ),
                    webhook_repository::update_webhook_subscription(
                        fixture.pool(),
                        concurrent_inactive_id,
                        &concurrent_activate,
                    ),
                )
            })
            .await
            .context("concurrent quota reactivation deadlocked")?;
        let outcomes = [first_activation, second_activation];
        let mut activated = 0;
        let mut rejected = 0;
        for outcome in outcomes {
            match outcome {
                Ok(Some(row)) if row.active => activated += 1,
                Err(error)
                    if webhook_repository::is_active_webhook_subscription_quota_error(&error) =>
                {
                    rejected += 1;
                }
                other => anyhow::bail!("unexpected quota race outcome: {other:?}"),
            }
        }
        assert_eq!(activated, 1);
        assert_eq!(rejected, 1);
        let active_count: i64 = sqlx::query_scalar(
            "select count(*)::bigint
             from webhook_subscription
             where workspace_id = $1 and active",
        )
        .bind(fixture.workspace_id)
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(
            active_count,
            webhook_repository::MAX_ACTIVE_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE
        );

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn total_subscription_quota_bounds_inactive_history_without_age_purging() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        // Seed the boundary without executing the row-level counting trigger
        // 1,000 times. This test database is isolated and owned by the test
        // role; the trigger is re-enabled before the behavior under test.
        sqlx::query(
            "alter table webhook_subscription
             disable trigger trg_webhook_subscription_workspace_quota",
        )
        .execute(fixture.pool())
        .await?;
        sqlx::query(
            "insert into webhook_subscription (
                id, workspace_id, library_id, display_name, target_url, secret,
                event_types, custom_headers_json, active
             )
             select
                uuidv7(), $1, $2, 'Inactive retained fixture ' || ordinal::text,
                'https://example.com/webhook', 'legacy-synthetic-signing-secret',
                array['revision.ready']::text[], '{}'::jsonb, false
             from generate_series(1, $3::integer) as series(ordinal)",
        )
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(i32::try_from(webhook_repository::MAX_TOTAL_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE)?)
        .execute(fixture.pool())
        .await?;
        sqlx::query(
            "alter table webhook_subscription
             enable trigger trg_webhook_subscription_workspace_quota",
        )
        .execute(fixture.pool())
        .await?;

        let candidate_id = Uuid::now_v7();
        let encrypted_secret = fixture.state.credential_cipher.encrypt(
            SecretPurpose::WebhookSigningSecret,
            candidate_id,
            "total-quota-candidate-signing-secret-at-least-32-bytes",
        )?;
        let serialized_headers = custom_headers::validate_and_serialize(&json!({}))?;
        let encrypted_headers = fixture.state.credential_cipher.encrypt(
            SecretPurpose::WebhookCustomHeaders,
            candidate_id,
            serialized_headers.as_str(),
        )?;
        let error = webhook_repository::create_webhook_subscription(
            fixture.pool(),
            &NewWebhookSubscription {
                id: candidate_id,
                workspace_id: fixture.workspace_id,
                library_id: Some(fixture.library_id),
                display_name: "Total quota candidate".to_string(),
                target_url: "https://example.com/webhook".to_string(),
                secret: encrypted_secret,
                event_types: vec!["revision.ready".to_string()],
                custom_headers_json: encrypted_headers,
                created_by_principal_id: None,
            },
        )
        .await
        .expect_err("the 1001st subscription must fail even when retained rows are inactive");
        assert!(webhook_repository::is_total_webhook_subscription_quota_error(&error));

        let retained_count: i64 = sqlx::query_scalar(
            "select count(*)::bigint
             from webhook_subscription
             where workspace_id = $1",
        )
        .bind(fixture.workspace_id)
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(
            retained_count,
            webhook_repository::MAX_TOTAL_WEBHOOK_SUBSCRIPTIONS_PER_WORKSPACE,
            "quota rejection must not silently purge inactive audit history",
        );

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_outbound_publishes_revision_ready_after_publish_webhook_event() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        // Create a subscription filtered to revision.ready
        let sub = fixture
            .create_subscription_direct(
                Some(fixture.library_id),
                &["revision.ready"],
                true,
                "https://example.com/hook-ready",
            )
            .await?;

        let event_id = Uuid::now_v7().to_string();
        let event = WebhookEvent {
            event_type: "revision.ready".to_string(),
            event_id: event_id.clone(),
            occurred_at: Utc::now(),
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            payload_json: json!({ "revision_id": Uuid::now_v7() }),
        };

        let errors = publish_webhook_event(fixture.pool(), &event).await;
        assert!(errors.is_empty(), "publish should not produce errors; errors={errors:?}");

        // Assert: delivery_attempt row created with delivery_state=delivering (set when job linked)
        let attempt = sqlx::query(
            "select delivery_state::text as delivery_state, job_id \
             from webhook_delivery_attempt \
             where subscription_id = $1 and event_id = $2",
        )
        .bind(sub.id)
        .bind(&event_id)
        .fetch_optional(fixture.pool())
        .await
        .context("failed to query delivery attempt")?
        .context("no delivery_attempt row found after publish")?;

        // State should be 'delivering' (job linked) or 'pending' (pre-link race)
        let state = attempt.try_get::<Option<String>, _>("delivery_state")?.unwrap_or_default();
        assert!(
            state == "delivering" || state == "pending",
            "expected delivering or pending delivery_state, got {state}"
        );

        // Assert: ingest_job admitted with job_kind=webhook_delivery
        let job_id = attempt
            .try_get::<Option<Uuid>, _>("job_id")?
            .context("job_id should be linked to attempt")?;
        let job_kind: Option<String> =
            sqlx::query_scalar("select job_kind::text from ingest_job where id = $1")
                .bind(job_id)
                .fetch_optional(fixture.pool())
                .await
                .context("failed to query ingest_job")?
                .context("ingest_job not found after publish")?;

        assert_eq!(job_kind.unwrap_or_default(), "webhook_delivery");

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_outbound_duplicate_event_is_atomic_and_idempotent() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        let subscription = fixture
            .create_subscription_direct(
                Some(fixture.library_id),
                &["revision.ready"],
                true,
                "https://example.com/idempotent",
            )
            .await?;
        let event = WebhookEvent {
            event_type: "revision.ready".to_string(),
            event_id: Uuid::now_v7().to_string(),
            occurred_at: Utc::now(),
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            payload_json: json!({ "revision_id": Uuid::now_v7() }),
        };

        assert!(publish_webhook_event(fixture.pool(), &event).await.is_empty());
        assert!(publish_webhook_event(fixture.pool(), &event).await.is_empty());

        let attempt_count: i64 = sqlx::query_scalar(
            "select count(*) from webhook_delivery_attempt
             where subscription_id = $1 and event_id = $2",
        )
        .bind(subscription.id)
        .bind(&event.event_id)
        .fetch_one(fixture.pool())
        .await?;
        let job_count: i64 = sqlx::query_scalar(
            "select count(*) from ingest_job
             where library_id = $1 and dedupe_key = $2",
        )
        .bind(fixture.library_id)
        .bind(format!("wh-delivery-{}-{}", subscription.id, event.event_id))
        .fetch_one(fixture.pool())
        .await?;

        assert_eq!(attempt_count, 1, "duplicate publication must roll back its attempt row");
        assert_eq!(job_count, 1, "duplicate publication must reuse the existing queue job");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_lifecycle_outbox_drains_once_and_preserves_event_identity() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        let subscription = fixture
            .create_subscription_direct(
                Some(fixture.library_id),
                &["revision.ready"],
                true,
                "https://example.com/outbox",
            )
            .await?;
        let deactivated_after_event = fixture
            .create_subscription_direct(
                Some(fixture.library_id),
                &["revision.ready"],
                true,
                "https://example.com/outbox-deactivated",
            )
            .await?;
        let revision_id = Uuid::now_v7();
        let event = WebhookEvent {
            event_type: "revision.ready".to_string(),
            event_id: revision_ready_event_id(revision_id),
            occurred_at: Utc::now(),
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            payload_json: json!({
                "document_id": Uuid::now_v7(),
                "revision_id": revision_id,
                "library_id": fixture.library_id,
            }),
        };

        let first =
            webhook_outbox_repository::enqueue_webhook_lifecycle_event(fixture.pool(), &event)
                .await
                .context("enqueue first lifecycle event")?;

        webhook_repository::update_webhook_subscription(
            fixture.pool(),
            deactivated_after_event.id,
            &webhook_repository::UpdateWebhookSubscription {
                display_name: None,
                target_url: None,
                secret: None,
                event_types: None,
                custom_headers_json: None,
                active: Some(false),
            },
        )
        .await
        .context("deactivate snapshotted lifecycle recipient")?;
        let late_subscription = fixture
            .create_subscription_direct(
                Some(fixture.library_id),
                &["revision.ready"],
                true,
                "https://example.com/outbox-late",
            )
            .await?;
        let duplicate =
            webhook_outbox_repository::enqueue_webhook_lifecycle_event(fixture.pool(), &event)
                .await
                .context("enqueue duplicate lifecycle event")?;
        assert_eq!(first.id, duplicate.id, "same event identity must reuse one outbox row");
        let snapshotted_recipient_count: i64 = sqlx::query_scalar(
            "select count(*)
             from webhook_lifecycle_outbox_recipient
             where outbox_id = $1",
        )
        .bind(first.id)
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(
            snapshotted_recipient_count, 2,
            "duplicate enqueue must preserve the original event-time recipient set",
        );
        let active_snapshot_targets =
            webhook_outbox_repository::list_active_webhook_lifecycle_recipient_targets(
                fixture.pool(),
                first.id,
            )
            .await?;
        assert_eq!(active_snapshot_targets.len(), 1);
        assert_eq!(
            active_snapshot_targets[0].id, subscription.id,
            "recipient lookup must use the event-time snapshot and terminally skip inactive rows",
        );
        let mut conflicting_event = event.clone();
        conflicting_event.payload_json = json!({ "revision_id": Uuid::now_v7() });
        assert!(
            webhook_outbox_repository::enqueue_webhook_lifecycle_event(
                fixture.pool(),
                &conflicting_event,
            )
            .await
            .is_err(),
            "one deterministic identity must never overwrite different immutable event data",
        );

        let first_drain = drain_webhook_lifecycle_outbox_once(
            &fixture.state,
            "webhook-outbox-integration-test",
            8,
        )
        .await
        .context("drain lifecycle outbox")?;
        assert_eq!(first_drain.leased, 1);
        assert_eq!(first_drain.dispatched, 1);
        assert_eq!(first_drain.retried, 0);

        let second_drain = drain_webhook_lifecycle_outbox_once(
            &fixture.state,
            "webhook-outbox-integration-test",
            8,
        )
        .await
        .context("re-drain lifecycle outbox")?;
        assert_eq!(second_drain.leased, 0, "dispatched rows must never be leased again");

        let attempt_count: i64 = sqlx::query_scalar(
            "select count(*) from webhook_delivery_attempt
             where subscription_id = $1 and event_id = $2",
        )
        .bind(subscription.id)
        .bind(&event.event_id)
        .fetch_one(fixture.pool())
        .await?;
        let deactivated_attempt_count: i64 = sqlx::query_scalar(
            "select count(*) from webhook_delivery_attempt
             where subscription_id = $1 and event_id = $2",
        )
        .bind(deactivated_after_event.id)
        .bind(&event.event_id)
        .fetch_one(fixture.pool())
        .await?;
        let late_attempt_count: i64 = sqlx::query_scalar(
            "select count(*) from webhook_delivery_attempt
             where subscription_id = $1 and event_id = $2",
        )
        .bind(late_subscription.id)
        .bind(&event.event_id)
        .fetch_one(fixture.pool())
        .await?;
        let outbox_state: String = sqlx::query_scalar(
            "select dispatch_state from webhook_lifecycle_outbox where event_id = $1",
        )
        .bind(&event.event_id)
        .fetch_one(fixture.pool())
        .await?;

        assert_eq!(attempt_count, 1, "outbox replay must not duplicate delivery attempts");
        assert_eq!(
            deactivated_attempt_count, 0,
            "a recipient deactivated after the event must be terminally skipped",
        );
        assert_eq!(
            late_attempt_count, 0,
            "a subscription created after the event must not receive old events",
        );
        assert_eq!(outbox_state, "dispatched");
        sqlx::query(
            "update webhook_lifecycle_outbox
             set dispatched_at = now() - interval '31 days'
             where id = $1",
        )
        .bind(first.id)
        .execute(fixture.pool())
        .await?;
        let pruned = webhook_outbox_repository::prune_dispatched_webhook_lifecycle_outbox(
            fixture.pool(),
            Utc::now() - chrono::Duration::days(30),
            100,
        )
        .await?;
        assert_eq!(pruned, 1, "old dispatched outbox rows should be pruned in bounded batches");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_lifecycle_outbox_fences_stale_lease_tokens() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        let revision_id = Uuid::now_v7();
        let event = WebhookEvent {
            event_type: "revision.ready".to_string(),
            event_id: revision_ready_event_id(revision_id),
            occurred_at: Utc::now(),
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            payload_json: json!({ "revision_id": revision_id }),
        };
        webhook_outbox_repository::enqueue_webhook_lifecycle_event(fixture.pool(), &event).await?;
        let lease = webhook_outbox_repository::lease_webhook_lifecycle_outbox_batch(
            fixture.pool(),
            "webhook-outbox-fencing-test",
            chrono::Duration::minutes(5),
            1,
        )
        .await?;
        let leased = lease.events.first().context("expected one leased outbox event")?;

        assert!(
            !webhook_outbox_repository::mark_webhook_lifecycle_outbox_dispatched(
                fixture.pool(),
                leased.id,
                Uuid::now_v7(),
            )
            .await?,
            "a stale token must not complete another relay's lease",
        );
        let released = webhook_outbox_repository::fail_webhook_lifecycle_outbox_dispatch(
            fixture.pool(),
            leased.id,
            lease.lease_token,
            Utc::now(),
            "fanout_failed",
            "redacted synthetic failure",
            12,
            true,
        )
        .await?;
        assert_eq!(released.as_deref(), Some("pending"));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_outbound_skips_inactive_subscriptions() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        let active_sub = fixture
            .create_subscription_direct(
                None,
                &["document.deleted"],
                true,
                "https://example.com/active",
            )
            .await?;
        let _inactive_sub = fixture
            .create_subscription_direct(
                None,
                &["document.deleted"],
                false,
                "https://example.com/inactive",
            )
            .await?;

        let event_id = Uuid::now_v7().to_string();
        let event = WebhookEvent {
            event_type: "document.deleted".to_string(),
            event_id: event_id.clone(),
            occurred_at: Utc::now(),
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            payload_json: json!({ "document_id": Uuid::now_v7() }),
        };

        let errors = publish_webhook_event(fixture.pool(), &event).await;
        assert!(errors.is_empty(), "publish should not error; errors={errors:?}");

        let attempt_count: i64 =
            sqlx::query_scalar("select count(*) from webhook_delivery_attempt where event_id = $1")
                .bind(&event_id)
                .fetch_one(fixture.pool())
                .await
                .context("failed to count delivery attempts")?;

        assert_eq!(attempt_count, 1, "only the active subscription should get a delivery attempt");

        let active_count: i64 = sqlx::query_scalar(
            "select count(*) from webhook_delivery_attempt \
             where event_id = $1 and subscription_id = $2",
        )
        .bind(&event_id)
        .bind(active_sub.id)
        .fetch_one(fixture.pool())
        .await
        .context("failed to count active subscription attempts")?;
        assert_eq!(active_count, 1, "active subscription should have exactly one delivery attempt");

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_outbound_skips_event_type_mismatch() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        // Subscription listens only to revision.ready
        fixture
            .create_subscription_direct(
                None,
                &["revision.ready"],
                true,
                "https://example.com/mismatch",
            )
            .await?;

        let event_id = Uuid::now_v7().to_string();
        let event = WebhookEvent {
            event_type: "document.deleted".to_string(), // different event type
            event_id: event_id.clone(),
            occurred_at: Utc::now(),
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            payload_json: json!({}),
        };

        let errors = publish_webhook_event(fixture.pool(), &event).await;
        assert!(errors.is_empty(), "publish should not error on no-match; errors={errors:?}");

        let attempt_count: i64 =
            sqlx::query_scalar("select count(*) from webhook_delivery_attempt where event_id = $1")
                .bind(&event_id)
                .fetch_one(fixture.pool())
                .await
                .context("failed to count delivery attempts")?;

        assert_eq!(attempt_count, 0, "event type mismatch should produce zero delivery attempts");

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_outbound_library_scoping() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        // Scoped subscription: library_id = L1
        let scoped_sub = fixture
            .create_subscription_direct(
                Some(fixture.library_id),
                &["revision.ready"],
                true,
                "https://example.com/scoped",
            )
            .await?;

        // Workspace-wide subscription: library_id = null
        let wide_sub = fixture
            .create_subscription_direct(None, &["revision.ready"], true, "https://example.com/wide")
            .await?;

        // Create a second library (different from fixture.library_id)
        let other_library = fixture
            .state
            .canonical_services
            .catalog
            .create_library(
                &fixture.state,
                CreateLibraryCommand {
                    workspace_id: fixture.workspace_id,
                    slug: Some(format!("wh-other-lib-{}", Uuid::now_v7().simple())),
                    display_name: "Other Library".to_string(),
                    description: None,
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create other library")?;

        // Publish for L2 (other_library) — scoped sub should NOT fire; wide sub SHOULD fire
        let event_id_l2 = Uuid::now_v7().to_string();
        let event_l2 = WebhookEvent {
            event_type: "revision.ready".to_string(),
            event_id: event_id_l2.clone(),
            occurred_at: Utc::now(),
            workspace_id: fixture.workspace_id,
            library_id: other_library.id,
            payload_json: json!({}),
        };
        let errors = publish_webhook_event(fixture.pool(), &event_l2).await;
        assert!(errors.is_empty(), "publish for L2 should not error; errors={errors:?}");

        let scoped_l2_count: i64 = sqlx::query_scalar(
            "select count(*) from webhook_delivery_attempt \
             where event_id = $1 and subscription_id = $2",
        )
        .bind(&event_id_l2)
        .bind(scoped_sub.id)
        .fetch_one(fixture.pool())
        .await
        .context("count scoped for L2")?;
        assert_eq!(scoped_l2_count, 0, "L1-scoped sub should NOT fire for L2 event");

        let wide_l2_count: i64 = sqlx::query_scalar(
            "select count(*) from webhook_delivery_attempt \
             where event_id = $1 and subscription_id = $2",
        )
        .bind(&event_id_l2)
        .bind(wide_sub.id)
        .fetch_one(fixture.pool())
        .await
        .context("count wide for L2")?;
        assert_eq!(wide_l2_count, 1, "workspace-wide sub should fire for L2 event");

        // Publish for L1 (fixture.library_id) — both scoped and wide should fire
        let event_id_l1 = Uuid::now_v7().to_string();
        let event_l1 = WebhookEvent {
            event_type: "revision.ready".to_string(),
            event_id: event_id_l1.clone(),
            occurred_at: Utc::now(),
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            payload_json: json!({}),
        };
        let errors = publish_webhook_event(fixture.pool(), &event_l1).await;
        assert!(errors.is_empty(), "publish for L1 should not error; errors={errors:?}");

        let scoped_l1_count: i64 = sqlx::query_scalar(
            "select count(*) from webhook_delivery_attempt \
             where event_id = $1 and subscription_id = $2",
        )
        .bind(&event_id_l1)
        .bind(scoped_sub.id)
        .fetch_one(fixture.pool())
        .await
        .context("count scoped for L1")?;
        assert_eq!(scoped_l1_count, 1, "L1-scoped sub should fire for L1 event");

        let wide_l1_count: i64 = sqlx::query_scalar(
            "select count(*) from webhook_delivery_attempt \
             where event_id = $1 and subscription_id = $2",
        )
        .bind(&event_id_l1)
        .bind(wide_sub.id)
        .fetch_one(fixture.pool())
        .await
        .context("count wide for L1")?;
        assert_eq!(wide_l1_count, 1, "workspace-wide sub should fire for L1 event");

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_subscription_validates_event_types_nonempty() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        // The DB CHECK constraint enforces non-empty event_types[].
        // The HTTP handler passes validation straight through to the DB, so the
        // constraint violation surfaces as a 500 (internal error) rather than 400.
        // App-level validation before the DB call is a follow-up improvement.
        let body = json!({
            "displayName": "Bad Subscription",
            "targetUrl": "https://example.com/bad",
            "secret": "x".repeat(48),
            "eventTypes": [],  // violates CHECK cardinality(event_types) > 0
            "customHeaders": {}
        });
        let (status, _) = http_post_json(
            fixture.app(),
            &format!("/v1/webhooks/workspaces/{}/subscriptions", fixture.workspace_id),
            &fixture.admin_token,
            body,
        )
        .await?;
        // App-level validation returns 400 before reaching the DB.
        assert_eq!(status, StatusCode::BAD_REQUEST, "empty event_types must return 400");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_subscription_validates_target_url_is_http_or_https() -> Result<()> {
    let fixture = WebhookOutboundFixture::create().await?;
    let result = async {
        // The DB CHECK constraint enforces http:// or https:// prefix on target_url.
        // The HTTP handler passes straight through to the DB, so the constraint
        // violation surfaces as a 500 (internal error) rather than 400.
        // App-level validation before the DB call is a follow-up improvement.
        let body = json!({
            "displayName": "Ftp Subscription",
            "targetUrl": "ftp://example.com/hook",   // violates CHECK
            "secret": "x".repeat(48),
            "eventTypes": ["revision.ready"],
            "customHeaders": {}
        });
        let (status, _) = http_post_json(
            fixture.app(),
            &format!("/v1/webhooks/workspaces/{}/subscriptions", fixture.workspace_id),
            &fixture.admin_token,
            body,
        )
        .await?;
        // App-level validation returns 400 before reaching the DB.
        assert_eq!(status, StatusCode::BAD_REQUEST, "ftp:// target_url must return 400");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
