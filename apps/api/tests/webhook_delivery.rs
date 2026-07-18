//! Integration tests for the outbound webhook delivery worker stage.
//!
//! Uses an in-process fake HTTP server (tokio) to capture requests and simulate
//! various HTTP response codes.  All tests require real Postgres and are gated
//! by `#[ignore]`.
//!
//! Run with:
//!   cargo test -p ironrag-backend --features test-support --test webhook_delivery -- --ignored

#![cfg(feature = "test-support")]

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::Request,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use serde_json::json;
use sqlx::{Row, postgres::PgPoolOptions};
use tokio::{net::TcpListener, sync::Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        persistence::Persistence,
        repositories::{
            ingest_repository::{self, NewIngestAttempt, NewIngestJob},
            webhook_repository::{
                self, DeleteWebhookSubscriptionOutcome, NewWebhookDeliveryAttempt,
                NewWebhookSubscription, WebhookDeliveryClaimOutcome, WebhookDeliveryCompletion,
            },
        },
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        webhook::{
            custom_headers,
            delivery::{
                WebhookDeliveryJobOutcome, run_webhook_delivery_job_with_loopback_test_transport,
                run_webhook_delivery_job_with_loopback_test_transport_and_ingest_lease,
            },
            error::WebhookServiceError,
            signature,
        },
    },
    shared::secret_encryption::SecretPurpose,
};

fn test_credential_master_key() -> String {
    STANDARD.encode([43_u8; 32])
}

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
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{name}\"")))
            .execute(&admin)
            .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
            .execute(&admin)
            .await?;
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
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin)
            .await?;
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
    pub library_id: Uuid,
}

impl WebhookDeliveryFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for webhook_delivery test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;
        settings.credential_master_key = Some(test_credential_master_key());
        settings.credential_encryption_write_enabled = true;

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("connect webhook_delivery postgres")?;

        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("apply migrations for webhook_delivery test")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("create redis client for webhook_delivery test")?;
        let persistence = Persistence::for_tests(postgres, redis);
        let state = Arc::new(AppState::from_dependencies(settings, persistence)?);

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

        let library = state
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

        Ok(Self { state, temp_database, workspace_id: workspace.id, library_id: library.id })
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
        let subscription_id = Uuid::now_v7();
        let encrypted_secret = self.state.credential_cipher.encrypt(
            SecretPurpose::WebhookSigningSecret,
            subscription_id,
            "delivery-test-secret",
        )?;
        let serialized_custom_headers = custom_headers::validate_and_serialize(&custom_headers)?;
        let encrypted_custom_headers = self.state.credential_cipher.encrypt(
            SecretPurpose::WebhookCustomHeaders,
            subscription_id,
            serialized_custom_headers.as_str(),
        )?;
        let sub = webhook_repository::create_webhook_subscription(
            self.pool(),
            &NewWebhookSubscription {
                id: subscription_id,
                workspace_id: self.workspace_id,
                library_id: Some(self.library_id),
                display_name: "Delivery Test Sub".to_string(),
                target_url: target_url.to_string(),
                secret: encrypted_secret,
                event_types: vec!["revision.ready".to_string()],
                custom_headers_json: encrypted_custom_headers,
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
                library_id: self.library_id,
                event_type: "revision.ready".to_string(),
                event_id: event_id.clone(),
                occurred_at: Utc::now(),
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

        let linked_attempt =
            webhook_repository::get_webhook_delivery_attempt_by_id(self.pool(), attempt.id)
                .await?
                .context("reload delivery attempt after commit-time queue repair")?;
        let job_id =
            linked_attempt.job_id.context("commit-time webhook queue repair did not link a job")?;
        let job = ingest_repository::get_ingest_job_by_id(self.pool(), job_id)
            .await?
            .context("commit-time webhook queue job missing")?;

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
#[allow(
    clippy::expect_used,
    reason = "a fake server task must fail immediately if its listener stops unexpectedly"
)]
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

#[allow(
    clippy::expect_used,
    reason = "a fake server task must fail immediately if its listener stops unexpectedly"
)]
async fn spawn_blocking_delivery_server()
-> Result<(String, Arc<AtomicUsize>, Arc<Semaphore>, Arc<Semaphore>)> {
    let hits = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let app = Router::new().route(
        "/hook",
        post({
            let hits = Arc::clone(&hits);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            move |_req: Request<Body>| {
                let hits = Arc::clone(&hits);
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    started.add_permits(1);
                    if let Ok(permit) = release.acquire().await {
                        permit.forget();
                    }
                    StatusCode::OK
                }
            }
        }),
    );
    let listener =
        TcpListener::bind("127.0.0.1:0").await.context("failed to bind blocking webhook server")?;
    let address = listener.local_addr().context("get blocking webhook server address")?;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("blocking webhook server failed");
    });
    Ok((format!("http://127.0.0.1:{}/hook", address.port()), hits, started, release))
}

#[derive(Clone)]
struct CapturedWebhookRequest {
    headers: HeaderMap,
    body: Bytes,
}

type CapturedWebhookRequestSlot = Arc<tokio::sync::Mutex<Option<CapturedWebhookRequest>>>;

/// Spawns a fake server that captures the exact first request before replying.
#[allow(
    clippy::expect_used,
    reason = "a fake server task must fail immediately if its listener stops unexpectedly"
)]
async fn spawn_request_capture_server(
    response_status: u16,
) -> Result<(String, CapturedWebhookRequestSlot)> {
    let captured: CapturedWebhookRequestSlot = Arc::new(tokio::sync::Mutex::new(None));
    let cap_clone = captured.clone();

    let app = Router::new().route(
        "/hook",
        post(move |headers: HeaderMap, body: Bytes| {
            let cap = cap_clone.clone();
            async move {
                let mut guard = cap.lock().await;
                if guard.is_none() {
                    *guard = Some(CapturedWebhookRequest { headers, body });
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

#[allow(
    clippy::expect_used,
    reason = "a fake server task must fail immediately if its listener stops unexpectedly"
)]
async fn spawn_validating_delivery_server(
    expected_secret: &'static str,
    expected_header_value: &'static str,
) -> Result<(String, Arc<AtomicUsize>)> {
    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_for_handler = Arc::clone(&accepted);
    let app = Router::new().route(
        "/hook",
        post(move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
            let accepted = Arc::clone(&accepted_for_handler);
            async move {
                let signature_valid = headers
                    .get(signature::header_name())
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| {
                        signature::verify(expected_secret.as_bytes(), value, &body, 300).is_ok()
                    });
                let custom_header_valid =
                    headers.get("x-config-version").and_then(|value| value.to_str().ok())
                        == Some(expected_header_value);
                if signature_valid && custom_header_valid {
                    accepted.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                } else {
                    StatusCode::BAD_REQUEST
                }
            }
        }),
    );
    let listener =
        TcpListener::bind("127.0.0.1:0").await.context("bind validating delivery server")?;
    let addr = listener.local_addr().context("validating delivery server address")?;
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("validating delivery server failed");
    });
    Ok((format!("http://127.0.0.1:{}/hook", addr.port()), accepted))
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

        run_webhook_delivery_job_with_loopback_test_transport(&fixture.state, &job)
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
        assert!(attempt.delivery_lease_token.is_none());
        assert!(attempt.error_code.is_none());
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

        run_webhook_delivery_job_with_loopback_test_transport(&fixture.state, &job)
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
        assert_eq!(attempt.error_code.as_deref(), Some("remote_http_status"));
        assert_eq!(
            attempt.error_message.as_deref(),
            Some("Remote endpoint returned an unsuccessful HTTP status")
        );
        assert!(attempt.response_body_excerpt.is_none());
        assert!(attempt.delivery_lease_token.is_none());
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

        run_webhook_delivery_job_with_loopback_test_transport(&fixture.state, &job)
            .await
            .context("run_webhook_delivery_job should not hard-fail on 422")?;

        let attempt =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await
                .context("query attempt after 422")?
                .context("attempt not found after 422")?;

        assert_eq!(attempt.delivery_state, "failed", "delivery_state should be 'failed' after 422");
        assert_eq!(attempt.response_status, Some(422));
        assert_eq!(attempt.error_code.as_deref(), Some("remote_http_status"));
        assert!(attempt.delivery_lease_token.is_none());
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

        run_webhook_delivery_job_with_loopback_test_transport(&fixture.state, &job)
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
        let (url, captured_request) = spawn_request_capture_server(200).await?;
        let (job, attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;

        run_webhook_delivery_job_with_loopback_test_transport(&fixture.state, &job)
            .await
            .context("run_webhook_delivery_job for signature test")?;

        // Give the server a moment to process the request
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let request = captured_request
            .lock()
            .await
            .as_ref()
            .context("no request captured — server may not have been reached")?
            .clone();
        let headers = &request.headers;

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

        signature::verify(b"delivery-test-secret", sig_header, &request.body, 300).map_err(
            |message| anyhow::anyhow!("signature must cover the exact transmitted body: {message}"),
        )?;
        let attempt =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await?
                .context("delivery attempt missing after request capture")?;
        assert_eq!(
            headers.get("x-ironrag-event-type").and_then(|value| value.to_str().ok()),
            Some(attempt.event_type.as_str())
        );
        assert_eq!(
            headers.get("x-ironrag-event-id").and_then(|value| value.to_str().ok()),
            Some(attempt.event_id.as_str())
        );
        let body: serde_json::Value =
            serde_json::from_slice(&request.body).context("transmitted body must be JSON")?;
        assert_eq!(body["event_type"], attempt.event_type);
        assert_eq!(body["event_id"], attempt.event_id);
        assert_eq!(body["occurred_at"], attempt.occurred_at.to_rfc3339());
        assert_eq!(body["workspace_id"], fixture.workspace_id.to_string());
        assert_eq!(body["library_id"], fixture.library_id.to_string());
        assert!(body.get("revision_id").is_some());

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
        let (url, captured_request) = spawn_request_capture_server(200).await?;
        let custom_headers = json!({ "X-Foo": "bar", "X-Provider-Beta": "alpha-suite" });
        let (job, _) = fixture.setup_attempt(&url, 0, custom_headers).await?;

        run_webhook_delivery_job_with_loopback_test_transport(&fixture.state, &job)
            .await
            .context("run_webhook_delivery_job for custom headers test")?;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let request = captured_request
            .lock()
            .await
            .as_ref()
            .context("no headers captured — server may not have been reached")?
            .clone();
        let headers = &request.headers;

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

#[tokio::test]
#[ignore = "requires local postgres"]
async fn webhook_delivery_uses_one_coherent_current_subscription_configuration() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (old_url, old_hits) = spawn_fake_server(200).await?;
        let (new_url, accepted) =
            spawn_validating_delivery_server("rotated-signing-secret", "v2").await?;
        let (job, attempt_id) =
            fixture.setup_attempt(&old_url, 0, json!({"X-Config-Version": "v1"})).await?;
        let attempt =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await?
                .context("delivery attempt missing before config rotation")?;
        let rotated_secret = fixture.state.credential_cipher.encrypt(
            SecretPurpose::WebhookSigningSecret,
            attempt.subscription_id,
            "rotated-signing-secret",
        )?;
        let serialized_rotated_headers =
            custom_headers::validate_and_serialize(&json!({"X-Config-Version": "v2"}))?;
        let rotated_headers = fixture.state.credential_cipher.encrypt(
            SecretPurpose::WebhookCustomHeaders,
            attempt.subscription_id,
            serialized_rotated_headers.as_str(),
        )?;
        drop(serialized_rotated_headers);
        webhook_repository::update_webhook_subscription(
            fixture.pool(),
            attempt.subscription_id,
            &webhook_repository::UpdateWebhookSubscription {
                display_name: None,
                target_url: Some(new_url.clone()),
                secret: Some(rotated_secret),
                event_types: None,
                custom_headers_json: Some(rotated_headers),
                active: None,
            },
        )
        .await?
        .context("subscription missing during config rotation")?;

        run_webhook_delivery_job_with_loopback_test_transport(&fixture.state, &job).await?;

        assert_eq!(old_hits.load(Ordering::SeqCst), 0);
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        let delivered =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await?
                .context("delivery attempt missing after config rotation")?;
        assert_eq!(delivered.delivery_state, "delivered");
        assert_eq!(delivered.target_url, new_url);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn inactive_subscription_is_terminal_and_performs_no_http_request() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, hits) = spawn_fake_server(200).await?;
        let (job, attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;
        let attempt =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await?
                .context("delivery attempt missing before deactivation")?;
        webhook_repository::update_webhook_subscription(
            fixture.pool(),
            attempt.subscription_id,
            &webhook_repository::UpdateWebhookSubscription {
                display_name: None,
                target_url: None,
                secret: None,
                event_types: None,
                custom_headers_json: None,
                active: Some(false),
            },
        )
        .await?
        .context("subscription missing during deactivation")?;

        run_webhook_delivery_job_with_loopback_test_transport(&fixture.state, &job).await?;

        assert_eq!(hits.load(Ordering::SeqCst), 0);
        let failed =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await?
                .context("delivery attempt missing after deactivation")?;
        assert_eq!(failed.delivery_state, "failed");
        assert!(failed.next_attempt_at.is_none());
        assert_eq!(failed.error_code.as_deref(), Some("subscription_inactive"));
        assert!(failed.delivery_lease_token.is_none());
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn concurrent_duplicate_workers_send_at_most_one_http_request() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = Box::pin(async {
        let (url, hits) = spawn_fake_server(200).await?;
        let (job, attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;
        let duplicate_job = job.clone();

        let (first, duplicate) = tokio::join!(
            run_webhook_delivery_job_with_loopback_test_transport(&fixture.state, &job),
            run_webhook_delivery_job_with_loopback_test_transport(&fixture.state, &duplicate_job,),
        );
        for outcome in [first, duplicate] {
            if let Err(error) = outcome
                && !matches!(&error, WebhookServiceError::DeliveryLeaseInFlight { .. })
            {
                return Err(error.into());
            }
        }

        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let delivered =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await?
                .context("delivery attempt missing after duplicate workers")?;
        assert_eq!(delivered.delivery_state, "delivered");
        assert_eq!(delivered.attempt_number, 1);
        assert!(delivered.delivery_lease_token.is_none());
        let retry_count: i64 = sqlx::query_scalar(
            "select count(*)
             from ingest_job
             where library_id = $1
               and dedupe_key like 'wh-retry-%'",
        )
        .bind(fixture.library_id)
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(retry_count, 0);
        Ok(())
    })
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn recent_delivery_owner_defers_duplicate_without_http_or_false_terminal_state() -> Result<()>
{
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, hits) = spawn_fake_server(200).await?;
        let (job, attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;
        let first_claim =
            webhook_repository::claim_attempt_for_delivery(fixture.pool(), attempt_id, job.id)
                .await?;
        let WebhookDeliveryClaimOutcome::Claimed { attempt, .. } = first_claim else {
            anyhow::bail!("first worker should own the delivery lease");
        };
        let lease_token = attempt.delivery_lease_token.context("claim token missing")?;

        let duplicate = run_webhook_delivery_job_with_loopback_test_transport(&fixture.state, &job)
            .await
            .expect_err("recent delivery ownership must defer duplicate queue work");
        let WebhookServiceError::DeliveryLeaseInFlight { retry_at, .. } = duplicate else {
            anyhow::bail!("duplicate returned the wrong typed outcome");
        };
        assert!(retry_at > Utc::now());
        assert_eq!(hits.load(Ordering::SeqCst), 0, "duplicate claim must not send HTTP");

        let persisted =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await?
                .context("claimed delivery disappeared")?;
        assert_eq!(persisted.delivery_state, "delivering");
        assert_eq!(persisted.delivery_lease_token, Some(lease_token));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn in_flight_delivery_deferral_requeues_exactly_at_lease_expiry_without_budget_failure()
-> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, _) = spawn_fake_server(200).await?;
        let (job, delivery_attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;
        let first_queue_token = format!("defer-first-{}", Uuid::now_v7());
        let leased_job = ingest_repository::claim_next_queued_ingest_job(
            fixture.pool(),
            &first_queue_token,
            "defer-worker-one",
            10,
            10,
            10,
        )
        .await?
        .context("first webhook queue lease missing")?;
        assert_eq!(leased_job.id, job.id);
        let first_ingest_attempt = ingest_repository::create_ingest_attempt_for_queue_lease(
            fixture.pool(),
            &NewIngestAttempt {
                job_id: job.id,
                attempt_number: 0,
                worker_principal_id: None,
                lease_token: Some("defer-ingest-one".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("webhook_delivery".to_string()),
                started_at: None,
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 0,
                retryable: false,
            },
            &first_queue_token,
        )
        .await?
        .context("first webhook ingest attempt missing")?;
        assert_eq!(first_ingest_attempt.attempt_number, 1);

        let first_delivery_claim = webhook_repository::claim_attempt_for_delivery(
            fixture.pool(),
            delivery_attempt_id,
            job.id,
        )
        .await?;
        let WebhookDeliveryClaimOutcome::Claimed { subscription, .. } = first_delivery_claim else {
            anyhow::bail!("first delivery claim missing");
        };
        let subscription_id = subscription.id;
        let duplicate_claim = webhook_repository::claim_attempt_for_delivery(
            fixture.pool(),
            delivery_attempt_id,
            job.id,
        )
        .await?;
        let WebhookDeliveryClaimOutcome::InFlight { retry_at, .. } = duplicate_claim else {
            anyhow::bail!("duplicate claim should expose the current lease expiry");
        };
        let database_now: chrono::DateTime<Utc> =
            sqlx::query_scalar("select now()").fetch_one(fixture.pool()).await?;
        let retry_delay = retry_at.signed_duration_since(database_now).num_seconds();
        assert!(
            (298..=300).contains(&retry_delay),
            "delivery retry eligibility must be derived from the database clock"
        );

        let (deferred, delete_outcome) =
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                tokio::join!(
                    ingest_repository::defer_webhook_delivery_in_flight(
                        fixture.pool(),
                        first_ingest_attempt.id,
                        job.id,
                        &first_queue_token,
                        retry_at,
                    ),
                    webhook_repository::delete_webhook_subscription(
                        fixture.pool(),
                        subscription_id,
                    ),
                )
            })
            .await
            .context("delete/defer lock-order race timed out")?;
        let deferred = deferred?;
        assert_eq!(
            delete_outcome?,
            DeleteWebhookSubscriptionOutcome::Draining { in_flight_deliveries: 1 },
        );
        assert!(deferred);
        let deferred_attempt =
            ingest_repository::get_ingest_attempt_by_id(fixture.pool(), first_ingest_attempt.id)
                .await?
                .context("deferred ingest attempt missing")?;
        assert_eq!(deferred_attempt.attempt_state, "failed");
        assert!(deferred_attempt.retryable);
        assert_eq!(deferred_attempt.failure_code.as_deref(), Some("delivery_lease_in_flight"));
        let deferred_job = ingest_repository::get_ingest_job_by_id(fixture.pool(), job.id)
            .await?
            .context("deferred webhook job missing")?;
        assert_eq!(deferred_job.queue_state, "queued");
        assert_eq!(deferred_job.available_at, retry_at);
        assert!(deferred_job.completed_at.is_none());
        assert!(deferred_job.queue_lease_token.is_none());

        // Advance only the test fixture clock gate, then prove the same job
        // can take its next normal queue lease/attempt instead of being
        // terminalized by the generic five-attempt budget path.
        sqlx::query("update ingest_job set available_at = now() where id = $1")
            .bind(job.id)
            .execute(fixture.pool())
            .await?;
        let second_queue_token = format!("defer-second-{}", Uuid::now_v7());
        let second_lease = ingest_repository::claim_next_queued_ingest_job(
            fixture.pool(),
            &second_queue_token,
            "defer-worker-two",
            10,
            10,
            10,
        )
        .await?
        .context("deferred webhook job was not leaseable again")?;
        assert_eq!(second_lease.id, job.id);
        let second_ingest_attempt = ingest_repository::create_ingest_attempt_for_queue_lease(
            fixture.pool(),
            &NewIngestAttempt {
                job_id: job.id,
                attempt_number: 0,
                worker_principal_id: None,
                lease_token: Some("defer-ingest-two".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("webhook_delivery".to_string()),
                started_at: None,
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 0,
                retryable: false,
            },
            &second_queue_token,
        )
        .await?
        .context("second webhook ingest attempt missing")?;
        assert_eq!(second_ingest_attempt.attempt_number, 2);
        assert_eq!(second_ingest_attempt.attempt_state, "leased");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn retry_handoff_atomically_retires_current_job_before_delivery_relink_is_visible()
-> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, _) = spawn_fake_server(503).await?;
        let (job, delivery_attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;
        let queue_token = format!("retry-handoff-{}", Uuid::now_v7());
        let leased_job = ingest_repository::claim_next_queued_ingest_job(
            fixture.pool(),
            &queue_token,
            "retry-handoff-worker",
            10,
            10,
            10,
        )
        .await?
        .context("webhook job was not queue-leased")?;
        assert_eq!(leased_job.id, job.id);
        let ingest_attempt = ingest_repository::create_ingest_attempt_for_queue_lease(
            fixture.pool(),
            &NewIngestAttempt {
                job_id: job.id,
                attempt_number: 0,
                worker_principal_id: None,
                lease_token: Some("retry-handoff-ingest".to_string()),
                knowledge_generation_id: None,
                attempt_state: "leased".to_string(),
                current_stage: Some("webhook_delivery".to_string()),
                started_at: None,
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                failure_message: None,
                progress_percent: 0,
                retryable: false,
            },
            &queue_token,
        )
        .await?
        .context("webhook ingest attempt was not created")?;
        let cancellation_token = CancellationToken::new();
        let outcome = run_webhook_delivery_job_with_loopback_test_transport_and_ingest_lease(
            &fixture.state,
            &leased_job,
            ingest_attempt.id,
            &queue_token,
            &cancellation_token,
        )
        .await?;
        assert_eq!(outcome, WebhookDeliveryJobOutcome::IngestAlreadyFinalized);
        let delivery = webhook_repository::get_webhook_delivery_attempt_by_id(
            fixture.pool(),
            delivery_attempt_id,
        )
        .await?
        .context("delivery attempt missing after retry handoff")?;
        let retry_job_id = delivery.job_id.context("delivery was not linked to retry job")?;
        assert_ne!(retry_job_id, job.id);

        let old_job = ingest_repository::get_ingest_job_by_id(fixture.pool(), job.id)
            .await?
            .context("old webhook job missing")?;
        assert_eq!(old_job.queue_state, "completed");
        assert!(old_job.queue_lease_token.is_none());
        let old_attempt =
            ingest_repository::get_ingest_attempt_by_id(fixture.pool(), ingest_attempt.id)
                .await?
                .context("old webhook ingest attempt missing")?;
        assert_eq!(old_attempt.attempt_state, "succeeded");
        let retry_job = ingest_repository::get_ingest_job_by_id(fixture.pool(), retry_job_id)
            .await?
            .context("retry webhook job missing")?;
        assert_eq!(retry_job.queue_state, "queued");
        assert!(retry_job.dedupe_key.as_deref().is_some_and(|key| key.starts_with("wh-retry-")));

        // Simulate a post-commit process crash followed by the ordinary stale
        // recovery sweep. The old job is already terminal and cannot be
        // requeued after the delivery link moved.
        let recovered = ingest_repository::recover_stale_canonical_leases(
            fixture.pool(),
            chrono::Duration::zero(),
        )
        .await?;
        assert_eq!(recovered, 0);
        let old_job_after_recovery =
            ingest_repository::get_ingest_job_by_id(fixture.pool(), job.id)
                .await?
                .context("old webhook job missing after recovery")?;
        assert_eq!(old_job_after_recovery.queue_state, "completed");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn newer_success_fences_stale_failure_and_retry_enqueue() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, _) = spawn_fake_server(200).await?;
        let (job, attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;

        let stale_claim =
            webhook_repository::claim_attempt_for_delivery(fixture.pool(), attempt_id, job.id)
                .await?;
        let WebhookDeliveryClaimOutcome::Claimed { attempt: stale_claim, .. } = stale_claim else {
            anyhow::bail!("stale worker should acquire the initial delivery lease");
        };
        let stale_token =
            stale_claim.delivery_lease_token.context("initial delivery lease token missing")?;
        sqlx::query(
            "update webhook_delivery_attempt
             set updated_at = now() - interval '6 minutes'
             where id = $1",
        )
        .bind(attempt_id)
        .execute(fixture.pool())
        .await?;

        let current_claim =
            webhook_repository::claim_attempt_for_delivery(fixture.pool(), attempt_id, job.id)
                .await?;
        let WebhookDeliveryClaimOutcome::Claimed { attempt: current_claim, .. } = current_claim
        else {
            anyhow::bail!("new worker should reclaim the stale delivery lease");
        };
        let current_token =
            current_claim.delivery_lease_token.context("reclaimed delivery lease token missing")?;
        assert_ne!(stale_token, current_token);

        let delivered = webhook_repository::record_webhook_delivery_result(
            fixture.pool(),
            &WebhookDeliveryCompletion {
                attempt_id,
                job_id: job.id,
                lease_token: current_token,
                delivery_state: "delivered",
                attempt_number: 1,
                response_status: Some(200),
                error_code: None,
                error_summary: None,
                next_attempt_at: None,
            },
        )
        .await?;
        assert!(delivered.is_some(), "current worker must commit its 2xx result");

        let retry_at = chrono::Utc::now() + chrono::Duration::minutes(2);
        let stale_retry = NewIngestJob {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            mutation_id: None,
            mutation_item_id: None,
            connector_id: None,
            async_operation_id: None,
            knowledge_document_id: None,
            knowledge_revision_id: None,
            job_kind: "webhook_delivery".to_string(),
            queue_state: "queued".to_string(),
            priority: 5,
            dedupe_key: Some(format!("wh-retry-stale-{attempt_id}")),
            queued_at: None,
            available_at: Some(retry_at),
            completed_at: None,
        };
        let stale_result =
            webhook_repository::record_webhook_delivery_failure_and_enqueue_retry_detached(
                fixture.pool(),
                &WebhookDeliveryCompletion {
                    attempt_id,
                    job_id: job.id,
                    lease_token: stale_token,
                    delivery_state: "failed",
                    attempt_number: 1,
                    response_status: Some(503),
                    error_code: Some("remote_http_status"),
                    error_summary: Some("Remote endpoint returned an unsuccessful HTTP status"),
                    next_attempt_at: Some(retry_at),
                },
                &stale_retry,
            )
            .await?;
        assert!(
            stale_result.is_none(),
            "stale worker must not overwrite the new result or enqueue retry work"
        );

        let final_attempt =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await?
                .context("delivery attempt missing after fencing race")?;
        assert_eq!(final_attempt.delivery_state, "delivered");
        assert_eq!(final_attempt.response_status, Some(200));
        let stale_retry_count: i64 =
            sqlx::query_scalar("select count(*) from ingest_job where dedupe_key = $1")
                .bind(stale_retry.dedupe_key.as_deref())
                .fetch_one(fixture.pool())
                .await?;
        assert_eq!(stale_retry_count, 0);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn deleting_subscription_cancels_pending_and_leased_delivery_jobs_atomically() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, _) = spawn_fake_server(200).await?;
        let (leased_job, first_attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;
        let first_attempt = webhook_repository::get_webhook_delivery_attempt_by_id(
            fixture.pool(),
            first_attempt_id,
        )
        .await?
        .context("first delivery attempt missing")?;

        let second_attempt = webhook_repository::create_webhook_delivery_attempt(
            fixture.pool(),
            &NewWebhookDeliveryAttempt {
                subscription_id: first_attempt.subscription_id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                event_type: "revision.ready".to_string(),
                event_id: Uuid::now_v7().to_string(),
                occurred_at: Utc::now(),
                payload_json: json!({ "revision_id": Uuid::now_v7() }),
                target_url: url,
            },
        )
        .await?;
        let second_attempt = webhook_repository::get_webhook_delivery_attempt_by_id(
            fixture.pool(),
            second_attempt.id,
        )
        .await?
        .context("reload commit-repaired pending delete-race attempt")?;
        let pending_job = ingest_repository::get_ingest_job_by_id(
            fixture.pool(),
            second_attempt.job_id.context("pending delete-race job was not linked")?,
        )
        .await?
        .context("pending delete-race job missing")?;
        let cross_link_error = sqlx::query(
            "update webhook_delivery_attempt
             set job_id = $2
             where id = $1",
        )
        .bind(second_attempt.id)
        .bind(leased_job.id)
        .execute(fixture.pool())
        .await
        .expect_err("one webhook queue job must never link to two delivery attempts");
        assert_eq!(
            cross_link_error.as_database_error().and_then(|error| error.code()).as_deref(),
            Some("23503")
        );
        sqlx::query(
            "update ingest_job
             set queue_state = 'failed', completed_at = now()
             where id = $1",
        )
        .bind(pending_job.id)
        .execute(fixture.pool())
        .await?;
        let leased_claim = webhook_repository::claim_attempt_for_delivery(
            fixture.pool(),
            first_attempt_id,
            leased_job.id,
        )
        .await?;
        let WebhookDeliveryClaimOutcome::Claimed { attempt: leased_claim, .. } = leased_claim
        else {
            anyhow::bail!("claim leased delete-race delivery");
        };
        let deleted_lease_token =
            leased_claim.delivery_lease_token.context("delete-race lease token missing")?;
        sqlx::query(
            "update ingest_job
             set queue_state = 'leased',
                 queue_leased_at = now(),
                 queue_lease_token = 'delete-race-lease',
                 queue_lease_owner = 'delete-race-worker'
             where id = $1",
        )
        .bind(leased_job.id)
        .execute(fixture.pool())
        .await?;

        let first_delete = webhook_repository::delete_webhook_subscription(
            fixture.pool(),
            first_attempt.subscription_id,
        )
        .await?;
        assert_eq!(
            first_delete,
            DeleteWebhookSubscriptionOutcome::Draining { in_flight_deliveries: 1 },
        );
        let active: bool =
            sqlx::query_scalar("select active from webhook_subscription where id = $1")
                .bind(first_attempt.subscription_id)
                .fetch_one(fixture.pool())
                .await?;
        assert!(!active, "a draining delete must fence every later HTTP claim");
        let reactivation = webhook_repository::update_webhook_subscription(
            fixture.pool(),
            first_attempt.subscription_id,
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
        .expect_err("a draining tombstone must never be reactivated");
        assert!(webhook_repository::is_draining_webhook_subscription_reactivation_error(
            &reactivation
        ));

        let attempt_count: i64 = sqlx::query_scalar(
            "select count(*)
             from webhook_delivery_attempt
             where subscription_id = $1",
        )
        .bind(first_attempt.subscription_id)
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(attempt_count, 2, "draining delete must preserve claimed attempt state");

        let recorded_success = webhook_repository::record_webhook_delivery_result(
            fixture.pool(),
            &WebhookDeliveryCompletion {
                attempt_id: first_attempt_id,
                job_id: leased_job.id,
                lease_token: deleted_lease_token,
                delivery_state: "delivered",
                attempt_number: 1,
                response_status: Some(200),
                error_code: None,
                error_summary: None,
                next_attempt_at: None,
            },
        )
        .await?;
        assert!(
            recorded_success.is_some(),
            "already-started HTTP must be allowed to persist its fenced result"
        );
        let second_delete = webhook_repository::delete_webhook_subscription(
            fixture.pool(),
            first_attempt.subscription_id,
        )
        .await?;
        assert_eq!(second_delete, DeleteWebhookSubscriptionOutcome::Deleted);

        let attempt_count_after_delete: i64 = sqlx::query_scalar(
            "select count(*)
             from webhook_delivery_attempt
             where subscription_id = $1",
        )
        .bind(first_attempt.subscription_id)
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(attempt_count_after_delete, 0, "completed drain may now cascade safely");

        let orphan_retry_at = chrono::Utc::now() + chrono::Duration::minutes(2);
        let orphan_retry = NewIngestJob {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            mutation_id: None,
            mutation_item_id: None,
            connector_id: None,
            async_operation_id: None,
            knowledge_document_id: None,
            knowledge_revision_id: None,
            job_kind: "webhook_delivery".to_string(),
            queue_state: "queued".to_string(),
            priority: 5,
            dedupe_key: Some(format!("wh-retry-after-delete-{first_attempt_id}")),
            queued_at: None,
            available_at: Some(orphan_retry_at),
            completed_at: None,
        };
        let stale_completion = WebhookDeliveryCompletion {
            attempt_id: first_attempt_id,
            job_id: leased_job.id,
            lease_token: deleted_lease_token,
            delivery_state: "failed",
            attempt_number: 1,
            response_status: Some(503),
            error_code: Some("remote_http_status"),
            error_summary: Some("Remote endpoint returned an unsuccessful HTTP status"),
            next_attempt_at: Some(orphan_retry_at),
        };
        assert!(
            webhook_repository::record_webhook_delivery_failure_and_enqueue_retry_detached(
                fixture.pool(),
                &stale_completion,
                &orphan_retry,
            )
            .await?
            .is_none(),
            "a deleted subscription must fence an in-flight worker's retry"
        );
        let orphan_retry_count: i64 =
            sqlx::query_scalar("select count(*) from ingest_job where dedupe_key = $1")
                .bind(orphan_retry.dedupe_key.as_deref())
                .fetch_one(fixture.pool())
                .await?;
        assert_eq!(orphan_retry_count, 0);

        let job_states: Vec<String> = sqlx::query_scalar(
            "select queue_state::text
             from ingest_job
             where id = any($1)
             order by id",
        )
        .bind(vec![leased_job.id, pending_job.id])
        .fetch_all(fixture.pool())
        .await?;
        assert_eq!(job_states, vec!["canceled".to_string(), "canceled".to_string()]);
        let active_or_leased: i64 = sqlx::query_scalar(
            "select count(*)
             from ingest_job
             where id = any($1)
               and (
                    queue_state in ('queued', 'leased', 'paused')
                    or queue_lease_token is not null
                    or queue_lease_owner is not null
               )",
        )
        .bind(vec![leased_job.id, pending_job.id])
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(active_or_leased, 0, "deleted subscriptions must leave no retryable orphan");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn delete_remains_draining_while_delayed_owner_can_still_finish_http() -> Result<()> {
    let fixture = WebhookDeliveryFixture::create().await?;
    let result = async {
        let (url, hits, started, release) = spawn_blocking_delivery_server().await?;
        let (job, attempt_id) = fixture.setup_attempt(&url, 0, json!({})).await?;
        let attempt =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await?
                .context("delayed-owner attempt missing")?;
        let subscription_id = attempt.subscription_id;
        let state = Arc::clone(&fixture.state);
        let worker_job = job.clone();
        let delivery = tokio::spawn(async move {
            run_webhook_delivery_job_with_loopback_test_transport(&state, &worker_job).await
        });

        let started_permit =
            tokio::time::timeout(std::time::Duration::from_secs(5), started.acquire())
                .await
                .context("delivery HTTP did not start")??;
        started_permit.forget();
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        let first_delete =
            webhook_repository::delete_webhook_subscription(fixture.pool(), subscription_id)
                .await?;
        assert_eq!(
            first_delete,
            DeleteWebhookSubscriptionOutcome::Draining { in_flight_deliveries: 1 },
        );

        // Lease age is not cancellation acknowledgement. Even an artificially
        // old claim remains a hard-delete blocker while its owner can resume.
        sqlx::query(
            "update webhook_delivery_attempt
             set updated_at = now() - interval '30 minutes'
             where id = $1",
        )
        .bind(attempt_id)
        .execute(fixture.pool())
        .await?;
        let aged_delete =
            webhook_repository::delete_webhook_subscription(fixture.pool(), subscription_id)
                .await?;
        assert_eq!(
            aged_delete,
            DeleteWebhookSubscriptionOutcome::Draining { in_flight_deliveries: 1 },
            "lease age alone must never authorize hard delete"
        );

        release.add_permits(1);
        delivery.await.context("join delayed delivery worker")??;
        let delivered =
            webhook_repository::get_webhook_delivery_attempt_by_id(fixture.pool(), attempt_id)
                .await?
                .context("delayed delivery disappeared before delete acknowledgement")?;
        assert_eq!(delivered.delivery_state, "delivered");

        let final_delete =
            webhook_repository::delete_webhook_subscription(fixture.pool(), subscription_id)
                .await?;
        assert_eq!(final_delete, DeleteWebhookSubscriptionOutcome::Deleted);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
