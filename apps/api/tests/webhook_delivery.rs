//! Integration tests for the outbound webhook delivery worker stage.
//!
//! Uses an in-process fake HTTP server (tokio) to capture requests and simulate
//! various HTTP response codes.  All tests require real Postgres and are gated
//! by `#[ignore]`.
//!
//! Run with:
//!   cargo test -p ironrag-backend --test webhook_delivery -- --include-ignored

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result};
use axum::{Router, body::Body, extract::Request, http::StatusCode, routing::post};
use serde_json::json;
use sqlx::{Row, postgres::PgPoolOptions};
use tokio::net::TcpListener;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        persistence::Persistence,
        repositories::{
            ingest_repository::{self, NewIngestJob},
            webhook_repository::{self, NewWebhookDeliveryAttempt, NewWebhookSubscription},
        },
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        webhook::{delivery::run_webhook_delivery_job, signature},
    },
};

// ============================================================================
// Temp database
// ============================================================================

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base: &str) -> Result<Self> {
        let admin_url = replace_db_name(base, "postgres")?;
        let name = format!("webhook_delivery_{}", Uuid::now_v7().simple());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("admin connect for webhook_delivery")?;
        terminate_connections(&admin, &name).await?;
        sqlx::query(&format!("drop database if exists \"{name}\"")).execute(&admin).await?;
        sqlx::query(&format!("create database \"{name}\"")).execute(&admin).await?;
        admin.close().await;
        Ok(Self { name: name.clone(), admin_url, database_url: replace_db_name(base, &name)? })
    }

    async fn drop(self) -> Result<()> {
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("admin reconnect for webhook_delivery cleanup")?;
        terminate_connections(&admin, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name)).execute(&admin).await?;
        admin.close().await;
        Ok(())
    }
}

// ============================================================================
// Fixture
// ============================================================================

struct WebhookDeliveryFixture {
    pub state: Arc<AppState>,
    temp_database: TempDatabase,
    pub workspace_id: Uuid,
}

impl WebhookDeliveryFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for webhook_delivery test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("connect webhook_delivery postgres")?;

        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("apply migrations for webhook_delivery test")?;

        let arango_client = Arc::new(
            ironrag_backend::infra::arangodb::client::ArangoClient::from_settings(&settings)
                .context("build arango client stub for webhook_delivery")?,
        );
        let redis = redis::Client::open(settings.redis_url.clone())
            .context("create redis client for webhook_delivery test")?;
        let persistence = Persistence::for_tests(postgres, redis);
        let state = Arc::new(AppState::from_dependencies(settings, persistence, arango_client)?);

        let workspace = state
            .canonical_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("wh-delivery-ws-{}", Uuid::now_v7().simple())),
                    display_name: "Webhook Delivery Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("create workspace for webhook_delivery")?;

        state
            .canonical_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("wh-delivery-lib-{}", Uuid::now_v7().simple())),
                    display_name: "Webhook Delivery Library".to_string(),
                    description: None,
                    created_by_principal_id: None,
                },
            )
            .await
            .context("create library for webhook_delivery")?;

        Ok(Self { state, temp_database, workspace_id: workspace.id })
    }

    fn pool(&self) -> &sqlx::PgPool {
        &self.state.persistence.postgres
    }

    /// Insert a subscription + delivery_attempt + ingest_job and link them.
    /// Returns `(job_row, attempt_id)`.
    async fn setup_attempt(
        &self,
        target_url: &str,
        attempt_number: i32,
        custom_headers: serde_json::Value,
    ) -> Result<(ingest_repository::IngestJobRow, Uuid)> {
        let sub = webhook_repository::create_webhook_subscription(
            self.pool(),
            &NewWebhookSubscription {
                workspace_id: self.workspace_id,
                library_id: None,
                display_name: "Delivery Test Sub".to_string(),
                target_url: target_url.to_string(),
                secret: "delivery-test-secret".to_string(),
                event_types: vec!["revision.ready".to_string()],
                custom_headers_json: custom_headers,
                created_by_principal_id: None,
            },
        )
        .await
        .context("create subscription for delivery test")?;

        let event_id = Uuid::now_v7().to_string();
        let attempt = webhook_repository::create_webhook_delivery_attempt(
            self.pool(),
            &NewWebhookDeliveryAttempt {
                subscription_id: sub.id,
                workspace_id: self.workspace_id,
                library_id: None,
                event_type: "revision.ready".to_string(),
                event_id: event_id.clone(),
                payload_json: json!({ "revision_id": Uuid::now_v7() }),
                target_url: target_url.to_string(),
            },
        )
        .await
        .context("create delivery attempt for delivery test")?;

        // Override attempt_number if needed
        if attempt_number > 0 {
            sqlx::query("update webhook_delivery_attempt set attempt_number = $2 where id = $1")
                .bind(attempt.id)
                .bind(attempt_number)
                .execute(self.pool())
                .await
                .context("set attempt_number")?;
        }

        let job = ingest_repository::create_ingest_job(
            self.pool(),
            &NewIngestJob {
                workspace_id: self.workspace_id,
                library_id: Uuid::nil(),
                mutation_id: None,
                connector_id: None,
                async_operation_id: None,
                knowledge_document_id: None,
                knowledge_revision_id: None,
                job_kind: "webhook_delivery".to_string(),
                queue_state: "queued".to_string(),
                priority: 5,
                dedupe_key: Some(format!("wh-delivery-test-{}", event_id)),
                queued_at: None,
                available_at: None,
                completed_at: None,
            },
        )
        .await
        .context("create ingest_job for delivery test")?;

        webhook_repository::link_attempt_to_job(self.pool(), attempt.id, job.id)
            .await
            .context("link job to attempt")?;

        Ok((job, attempt.id))
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

// ============================================================================
// Fake HTTP server helpers
// ============================================================================

/// Spawns a fake HTTP server that responds with `response_status` on every POST.
/// Returns the server's URL and the request count (for assertion).
async fn spawn_fake_server(response_status: u16) -> Result<(String, Arc<AtomicUsize>)> {
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = count.clone();

    let app = Router::new().route(
        "/hook",
        post({
            let cnt = count_clone;
            move |_req: Request<Body>| {
                let cnt = cnt.clone();
                async move {
                    cnt.fetch_add(1, Ordering::SeqCst);
                    StatusCode::from_u16(response_status)
                        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
                }
            }
        }),
    );

    let listener =
        TcpListener::bind("127.0.0.1:0").await.context("failed to bind fake webhook server")?;
    let addr: SocketAddr = listener.local_addr().context("failed to get local addr")?;

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("fake webhook server failed");
    });

    Ok((format!("http://127.0.0.1:{}/hook", addr.port()), count))
}

/// Spawns a fake server that captures all request headers from the first request.
async fn spawn_header_capture_server(
    response_status: u16,
) -> Result<(String, Arc<tokio::sync::Mutex<Option<axum::http::HeaderMap>>>)> {
    let captured: Arc<tokio::sync::Mutex<Option<axum::http::HeaderMap>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let cap_clone = captured.clone();

    let app = Router::new().route(
        "/hook",
        post(move |headers: axum::http::HeaderMap, _body: Body| {
            let cap = cap_clone.clone();
            async move {
                let mut guard = cap.lock().await;
                if guard.is_none() {
                    *guard = Some(headers);
                }
                StatusCode::from_u16(response_status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }),
    );

    let listener =
        TcpListener::bind("127.0.0.1:0").await.context("failed to bind header capture server")?;
    let addr = listener.local_addr().context("get header capture server addr")?;

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("header capture server failed");
    });

    Ok((format!("http://127.0.0.1:{}/hook", addr.port()), captured))
}

// ============================================================================
// Helpers
// ============================================================================

fn replace_db_name(url: &str, new_db: &str) -> Result<String> {
    let (without_query, query_suffix) =
        url.split_once('?').map_or((url, None), |(p, s)| (p, Some(s)));
    let slash =
        without_query.rfind('/').with_context(|| format!("db url missing db name: {url}"))?;
    let mut rebuilt = format!("{}{new_db}", &without_query[..=slash]);
    if let Some(q) = query_suffix {
        rebuilt.push('?');
        rebuilt.push_str(q);
    }
    Ok(rebuilt)
}

async fn terminate_connections(pool: &sqlx::PgPool, db: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid) from pg_stat_activity \
         where datname = $1 and pid <> pg_backend_pid()",
    )
    .bind(db)
    .execute(pool)
    .await
    .with_context(|| format!("failed to terminate connections for {db}"))?;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_delivery_2xx_marks_delivered() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, hit_count) = spawn_fake_server(200).await?;
        let (job, attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;

        run_webhook_delivery_job(&fixture.state, &job)
            .await
            .context("run_webhook_delivery_job should succeed")?;

        assert_eq!(hit_count.load(Ordering::SeqCst), 1, "fake server should have been hit once");

        let attempt =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await
                .context("query attempt after delivery")?
                .context("attempt not found after delivery")?;

        assert_eq!(attempt.delivery_state, "delivered", "delivery_state should be 'delivered'");
        assert!(attempt.delivered_at.is_some(), "delivered_at should be set");
        assert_eq!(attempt.response_status, Some(200));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_delivery_5xx_schedules_retry() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, hit_count) = spawn_fake_server(503).await?;
        let (job, attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;

        run_webhook_delivery_job(&fixture.state, &job)
            .await
            .context("run_webhook_delivery_job should not hard-fail on 503")?;

        assert_eq!(hit_count.load(Ordering::SeqCst), 1, "fake server should have been hit once");

        let attempt =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await
                .context("query attempt after 503")?
                .context("attempt not found after 503")?;

        // After attempt_number 0+1=1 with 503: state=failed, retry scheduled
        // delivery.rs sets new_state="failed" for non-success, and schedules retry
        // when attempt_number < MAX_ATTEMPTS (8).
        assert_eq!(attempt.delivery_state, "failed", "delivery_state should be 'failed' after 503");
        assert_eq!(attempt.attempt_number, 1, "attempt_number should have been incremented to 1");
        assert!(
            attempt.next_attempt_at.is_some(),
            "next_attempt_at should be set when retry is scheduled (attempt < 8)"
        );

        // Verify retry job was created
        let retry_job_count: i64 = sqlx::query(
            "select count(*) from ingest_job \
             where workspace_id = $1 and job_kind::text = 'webhook_delivery'",
        )
        .bind(fixture.workspace_id)
        .fetch_one(fixture.pool())
        .await
        .context("count retry jobs")?
        .try_get(0)
        .unwrap_or(0i64);
        assert!(retry_job_count >= 2, "a retry ingest_job should have been created");

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_delivery_4xx_marks_failed_no_retry() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, _) = spawn_fake_server(422).await?;
        let (job, attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;

        run_webhook_delivery_job(&fixture.state, &job)
            .await
            .context("run_webhook_delivery_job should not hard-fail on 422")?;

        let attempt =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await
                .context("query attempt after 422")?
                .context("attempt not found after 422")?;

        assert_eq!(attempt.delivery_state, "failed", "delivery_state should be 'failed' after 422");
        assert_eq!(attempt.response_status, Some(422));
        // 4xx (other than 408/429) is terminal — no retry should be scheduled.
        assert!(
            attempt.next_attempt_at.is_none(),
            "next_attempt_at must NOT be set for terminal 4xx (422)"
        );

        // Verify no retry job was created — only the original job should exist.
        let retry_job_count: i64 = sqlx::query(
            "select count(*) from ingest_job \
             where workspace_id = $1 and job_kind::text = 'webhook_delivery'",
        )
        .bind(fixture.workspace_id)
        .fetch_one(fixture.pool())
        .await
        .context("count retry jobs after 422")?
        .try_get(0)
        .unwrap_or(0i64);
        assert_eq!(retry_job_count, 1, "no retry ingest_job should be created for terminal 4xx");

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_delivery_max_attempts_marks_abandoned() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, _) = spawn_fake_server(503).await?;
        // Pre-set attempt_number=8 — delivery.rs: attempt_number+1=9 >= MAX_ATTEMPTS(8) → abandoned
        let (job, attempt_id) = fixture.setup_attempt(&url, 8, json!({})).await?;

        run_webhook_delivery_job(&fixture.state, &job)
            .await
            .context("run_webhook_delivery_job should not hard-fail at max attempts")?;

        let attempt =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await
                .context("query attempt after max attempts")?
                .context("attempt not found after max attempts")?;

        assert_eq!(
            attempt.delivery_state, "abandoned",
            "delivery_state should be 'abandoned' when attempt_number >= MAX_ATTEMPTS"
        );
        assert!(
            attempt.next_attempt_at.is_none(),
            "next_attempt_at should NOT be set for abandoned delivery"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_delivery_signs_outgoing_request() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, captured_headers) = spawn_header_capture_server(200).await?;
        let (job, _) = fixture.setup_attempt(&url, 0, json!({})).await?;

        run_webhook_delivery_job(&fixture.state, &job)
            .await
            .context("run_webhook_delivery_job for signature test")?;

        // Give the server a moment to process the request
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let headers_guard = captured_headers.lock().await;
        let headers = headers_guard
            .as_ref()
            .context("no headers captured — server may not have been reached")?;

        let sig_header = headers
            .get(signature::header_name())
            .context("X-Ironrag-Signature header missing from outgoing request")?
            .to_str()
            .context("X-Ironrag-Signature header is not valid UTF-8")?;

        // Parse and validate the header format: t=<ts>,v1=<hex>
        assert!(sig_header.contains("t="), "signature header should contain t= field");
        assert!(sig_header.contains(",v1="), "signature header should contain v1= field");

        // Extract the ts and hex parts for structural validation
        let ts_part = sig_header
            .split(',')
            .find(|p| p.starts_with("t="))
            .context("t= part not found")?
            .strip_prefix("t=")
            .context("strip t= prefix")?;
        let _ts: u64 = ts_part.parse().context("t= value should be a valid u64 unix timestamp")?;

        let v1_part = sig_header
            .split(',')
            .find(|p| p.starts_with("v1="))
            .context("v1= part not found")?
            .strip_prefix("v1=")
            .context("strip v1= prefix")?;

        assert_eq!(v1_part.len(), 64, "v1 hex HMAC-SHA256 should be 64 hex chars (32 bytes)");
        assert!(
            v1_part.chars().all(|c| c.is_ascii_hexdigit()),
            "v1 should contain only hex digits"
        );

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_delivery_includes_custom_headers() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, captured_headers) = spawn_header_capture_server(200).await?;
        let custom_headers = json!({ "X-Foo": "bar", "X-Provider-Beta": "alpha-suite" });
        let (job, _) = fixture.setup_attempt(&url, 0, custom_headers).await?;

        run_webhook_delivery_job(&fixture.state, &job)
            .await
            .context("run_webhook_delivery_job for custom headers test")?;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let headers_guard = captured_headers.lock().await;
        let headers = headers_guard
            .as_ref()
            .context("no headers captured — server may not have been reached")?;

        let foo = headers
            .get("x-foo")
            .context("X-Foo header missing from outgoing request")?
            .to_str()
            .context("X-Foo is not valid UTF-8")?;
        assert_eq!(foo, "bar", "X-Foo should have value 'bar'");

        let provider = headers
            .get("x-provider-beta")
            .context("X-Provider-Beta header missing from outgoing request")?
            .to_str()
            .context("X-Provider-Beta is not valid UTF-8")?;
        assert_eq!(provider, "alpha-suite", "X-Provider-Beta should have value 'alpha-suite'");

        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
