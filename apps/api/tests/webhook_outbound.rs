//! Integration tests for the outbound webhook subsystem.
//!
//! All tests require real Postgres — they are gated by `#[ignore]`.
//! Run with:
//!   cargo test -p ironrag-backend --test webhook_outbound -- --include-ignored

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
use sqlx::{Row, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    domains::webhook::WebhookEvent,
    infra::{
        persistence::Persistence,
        repositories::{
            iam_repository,
            webhook_repository::{self, NewWebhookSubscription},
        },
    },
    interfaces::http::{auth::hash_token, router},
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        webhook::outbound::publish_webhook_event,
    },
};

// ============================================================================
// Temp database (Postgres only — outbound tests don't need ArangoDB)
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
        sqlx::query(&format!("drop database if exists \"{database_name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale db {database_name}"))?;
        sqlx::query(&format!("create database \"{database_name}\""))
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
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
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

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect webhook_outbound postgres")?;

        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply migrations for webhook_outbound test")?;

        // Outbound tests don't need ArangoDB — use a stub arango client
        let arango_client = Arc::new(
            ironrag_backend::infra::arangodb::client::ArangoClient::from_settings(&settings)
                .context("failed to build arango client stub for webhook_outbound")?,
        );

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for webhook_outbound test")?;
        let persistence = Persistence::for_tests(postgres, redis);
        let state = AppState::from_dependencies(settings, persistence, arango_client)?;

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
    ) -> Result<webhook_repository::WebhookSubscriptionRow> {
        let row = webhook_repository::create_webhook_subscription(
            self.pool(),
            &NewWebhookSubscription {
                workspace_id: self.workspace_id,
                library_id,
                display_name: "Test Subscription".to_string(),
                target_url: target_url.to_string(),
                secret: format!("sub-secret-{}", Uuid::now_v7().simple()),
                event_types: event_types.iter().map(|s| s.to_string()).collect(),
                custom_headers_json: json!({}),
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

async fn http_get_json(app: Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .expect("build GET request");
    let resp = app.oneshot(req).await.expect("GET request failed");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("collect GET body").to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

async fn http_post_json(app: Router, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let body_bytes = serde_json::to_vec(&body).expect("serialize POST body");
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body_bytes))
        .expect("build POST request");
    let resp = app.oneshot(req).await.expect("POST request failed");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("collect POST body").to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

async fn http_patch_json(app: Router, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let body_bytes = serde_json::to_vec(&body).expect("serialize PATCH body");
    let req = Request::builder()
        .method("PATCH")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body_bytes))
        .expect("build PATCH request");
    let resp = app.oneshot(req).await.expect("PATCH request failed");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("collect PATCH body").to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

async fn http_delete(app: Router, uri: &str, token: &str) -> StatusCode {
    let req = Request::builder()
        .method("DELETE")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .expect("build DELETE request");
    let resp = app.oneshot(req).await.expect("DELETE request failed");
    resp.status()
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
            "workspaceId": workspace_id,
            "displayName": "Alpha Suite Subscription",
            "targetUrl": "https://example.com/webhook",
            "secret": "my-secret-abc",
            "eventTypes": ["revision.ready", "document.deleted"],
            "customHeaders": {}
        });
        let (create_status, create_resp) =
            http_post_json(fixture.app(), "/v1/webhooks/subscriptions", token, create_body).await;
        assert_eq!(
            create_status,
            StatusCode::CREATED,
            "create should return 201; body={create_resp}"
        );
        let sub_id = create_resp["id"].as_str().context("id missing in create response")?;
        assert_eq!(create_resp["displayName"], "Alpha Suite Subscription");
        assert_eq!(create_resp["active"], true);
        assert_eq!(create_resp["eventTypes"].as_array().map(|a| a.len()), Some(2));

        // GET by id
        let (get_status, get_resp) =
            http_get_json(fixture.app(), &format!("/v1/webhooks/subscriptions/{sub_id}"), token)
                .await;
        assert_eq!(get_status, StatusCode::OK, "GET should return 200; body={get_resp}");
        assert_eq!(get_resp["id"], sub_id);
        assert_eq!(get_resp["targetUrl"], "https://example.com/webhook");

        // LIST
        let (list_status, list_resp) = http_get_json(
            fixture.app(),
            &format!("/v1/webhooks/subscriptions?workspaceId={workspace_id}"),
            token,
        )
        .await;
        assert_eq!(list_status, StatusCode::OK, "list should return 200; body={list_resp}");
        let items = list_resp.as_array().context("list response should be array")?;
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
        .await;
        assert_eq!(patch_status, StatusCode::OK, "PATCH should return 200; body={patch_resp}");
        assert_eq!(patch_resp["displayName"], "Alpha Suite Subscription Updated");
        assert_eq!(patch_resp["active"], false);

        // DELETE
        let delete_status =
            http_delete(fixture.app(), &format!("/v1/webhooks/subscriptions/{sub_id}"), token)
                .await;
        assert_eq!(delete_status, StatusCode::NO_CONTENT, "DELETE should return 204");

        // GET after delete — should 404
        let (get_after_status, _) =
            http_get_json(fixture.app(), &format!("/v1/webhooks/subscriptions/{sub_id}"), token)
                .await;
        assert_eq!(get_after_status, StatusCode::NOT_FOUND, "GET after delete should return 404");

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
            workspace_id: fixture.workspace_id,
            library_id: Some(fixture.library_id),
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
            workspace_id: fixture.workspace_id,
            library_id: Some(fixture.library_id),
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
            workspace_id: fixture.workspace_id,
            library_id: Some(fixture.library_id),
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
            workspace_id: fixture.workspace_id,
            library_id: Some(other_library.id),
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
            workspace_id: fixture.workspace_id,
            library_id: Some(fixture.library_id),
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
            "workspaceId": fixture.workspace_id,
            "displayName": "Bad Subscription",
            "targetUrl": "https://example.com/bad",
            "secret": "sec",
            "eventTypes": [],  // violates CHECK cardinality(event_types) > 0
            "customHeaders": {}
        });
        let (status, _) =
            http_post_json(fixture.app(), "/v1/webhooks/subscriptions", &fixture.admin_token, body)
                .await;
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
            "workspaceId": fixture.workspace_id,
            "displayName": "Ftp Subscription",
            "targetUrl": "ftp://example.com/hook",   // violates CHECK
            "secret": "sec",
            "eventTypes": ["revision.ready"],
            "customHeaders": {}
        });
        let (status, _) =
            http_post_json(fixture.app(), "/v1/webhooks/subscriptions", &fixture.admin_token, body)
                .await;
        // App-level validation returns 400 before reaching the DB.
        assert_eq!(status, StatusCode::BAD_REQUEST, "ftp:// target_url must return 400");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
