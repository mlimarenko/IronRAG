//! Integration smoke test for the inbound webhook receiver stub.
//!
//! Verifies that `POST /v1/webhooks/inbound/{connector_kind}` returns:
//!   - 401 when called without credentials
//!   - 400 when `connector_kind` is not a recognised value
//!   - 501 when `connector_kind` is valid, with `error = "inbound_webhook_not_implemented"`
//!
//! Run with:
//!   cargo test -p ironrag-backend --test webhook_inbound_stub -- --include-ignored

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{persistence::Persistence, repositories::iam_repository},
    interfaces::http::{auth::hash_token, router},
    services::catalog_service::CreateWorkspaceCommand,
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
    async fn create(base_url: &str) -> Result<Self> {
        let admin_url = replace_db(base_url, "postgres")?;
        let name = format!("wh_inbound_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("connect admin postgres")?;
        terminate_connections(&admin_pool, &name).await?;
        sqlx::query(&format!("drop database if exists \"{name}\"")).execute(&admin_pool).await?;
        sqlx::query(&format!("create database \"{name}\"")).execute(&admin_pool).await?;
        admin_pool.close().await;
        Ok(Self { name: name.clone(), admin_url, database_url: replace_db(base_url, &name)? })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new().max_connections(1).connect(&self.admin_url).await?;
        terminate_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await?;
        admin_pool.close().await;
        Ok(())
    }
}

fn replace_db(url: &str, new_db: &str) -> Result<String> {
    let (base, query) = url.split_once('?').map_or((url, None), |(a, b)| (a, Some(b)));
    let slash = base.rfind('/').context("no slash in db url")?;
    let mut out = format!("{}{new_db}", &base[..=slash]);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Ok(out)
}

async fn terminate_connections(pool: &sqlx::PgPool, db: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid) from pg_stat_activity \
         where datname = $1 and pid <> pg_backend_pid()",
    )
    .bind(db)
    .execute(pool)
    .await?;
    Ok(())
}

// ============================================================================
// Fixture
// ============================================================================

struct InboundFixture {
    state: AppState,
    temp_db: TempDatabase,
    pub token: String,
}

impl InboundFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("Settings::from_env for inbound stub test")?;
        let temp_db = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_db.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;

        let postgres =
            PgPoolOptions::new().max_connections(4).connect(&settings.database_url).await?;
        sqlx::migrate!("./migrations").run(&postgres).await?;

        let arango_client = Arc::new(
            ironrag_backend::infra::arangodb::client::ArangoClient::from_settings(&settings)
                .context("arango client for inbound stub test")?,
        );
        let redis = redis::Client::open(settings.redis_url.clone()).context("redis client")?;
        let persistence = Persistence::for_tests(postgres, redis);
        let state = AppState::from_dependencies(settings, persistence, arango_client)?;

        let ws = state
            .canonical_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("wh-inbound-ws-{}", Uuid::now_v7().simple())),
                    display_name: "Inbound Stub Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await?;

        let plaintext = format!("wh-inbound-tok-{}", Uuid::now_v7().simple());
        let tok_row = iam_repository::create_api_token(
            &state.persistence.postgres,
            Some(ws.id),
            "inbound-stub-test",
            "rest",
            None,
            None,
        )
        .await?;
        iam_repository::create_api_token_secret(
            &state.persistence.postgres,
            tok_row.principal_id,
            &hash_token(&plaintext),
        )
        .await?;
        // Grant workspace_admin so AuthContext resolves
        iam_repository::create_grant(
            &state.persistence.postgres,
            tok_row.principal_id,
            "workspace",
            ws.id,
            "workspace_admin",
            None,
            None,
        )
        .await?;

        Ok(Self { state, temp_db, token: plaintext })
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_db.drop().await
    }
}

// ============================================================================
// Helpers
// ============================================================================

async fn post(app: Router, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("POST").uri(uri);
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = builder.body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
#[ignore = "requires local postgres"]
async fn inbound_stub_returns_401_without_credentials() -> Result<()> {
    let f = InboundFixture::create().await?;
    let result = async {
        let (status, _) = post(f.app(), "/v1/webhooks/inbound/web", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "missing auth must yield 401");
        Ok(())
    }
    .await;
    f.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres"]
async fn inbound_stub_returns_400_for_unknown_connector_kind() -> Result<()> {
    let f = InboundFixture::create().await?;
    let result = async {
        let (status, body) =
            post(f.app(), "/v1/webhooks/inbound/totally_unknown_xyz", Some(&f.token)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "unknown kind must yield 400; body={body}");
        Ok(())
    }
    .await;
    f.cleanup().await?;
    result
}

/// Core assertion: known connector_kind + valid auth → 501 with documented error code.
#[tokio::test]
#[ignore = "requires local postgres"]
async fn inbound_stub_returns_501_with_error_code_for_known_connector_kind() -> Result<()> {
    let f = InboundFixture::create().await?;
    let result = async {
        for kind in &["generic", "filesystem", "github", "s3", "web"] {
            let uri = format!("/v1/webhooks/inbound/{kind}");
            let (status, body) = post(f.app(), &uri, Some(&f.token)).await;
            assert_eq!(
                status,
                StatusCode::NOT_IMPLEMENTED,
                "kind={kind} must yield 501; body={body}"
            );
            assert_eq!(
                body["error"], "inbound_webhook_not_implemented",
                "error code must be 'inbound_webhook_not_implemented' for kind={kind}"
            );
            assert_eq!(
                body["connectorKind"], *kind,
                "connectorKind echo must match for kind={kind}"
            );
        }
        Ok(())
    }
    .await;
    f.cleanup().await?;
    result
}
