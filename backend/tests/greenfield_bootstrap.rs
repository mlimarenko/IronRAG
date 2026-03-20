use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
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
        graph_store::{
            GraphProjectionData, GraphProjectionEdgeWrite, GraphProjectionNodeWrite,
            GraphProjectionWriteError, GraphStore,
        },
        persistence::{Persistence, canonical_ai_catalog_seeded, canonical_baseline_present},
    },
    interfaces::http::router,
};

const SEEDED_PROVIDER_COUNT: i64 = 3;
const SEEDED_MODEL_COUNT: i64 = 7;
const SEEDED_PRICE_COUNT: i64 = 12;
const TEST_BOOTSTRAP_SECRET: &str = "greenfield-bootstrap-secret";

struct NoopGraphStore;

#[async_trait]
impl GraphStore for NoopGraphStore {
    fn backend_name(&self) -> &'static str {
        "noop"
    }

    async fn ping(&self) -> Result<()> {
        Ok(())
    }

    async fn replace_library_projection(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _nodes: &[GraphProjectionNodeWrite],
        _edges: &[GraphProjectionEdgeWrite],
    ) -> Result<(), GraphProjectionWriteError> {
        Ok(())
    }

    async fn refresh_library_projection_targets(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _remove_node_ids: &[Uuid],
        _remove_edge_ids: &[Uuid],
        _nodes: &[GraphProjectionNodeWrite],
        _edges: &[GraphProjectionEdgeWrite],
    ) -> Result<(), GraphProjectionWriteError> {
        Ok(())
    }

    async fn load_library_projection(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
    ) -> Result<GraphProjectionData> {
        Ok(GraphProjectionData::default())
    }
}

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("greenfield_bootstrap_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect bootstrap test admin postgres")?;

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
            .context("failed to reconnect bootstrap test admin postgres for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct GreenfieldBootstrapFixture {
    state: AppState,
    temp_database: TempDatabase,
}

impl GreenfieldBootstrapFixture {
    async fn create() -> Result<Self> {
        let mut settings = Settings::from_env()
            .context("failed to load settings for greenfield bootstrap test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.bootstrap_token = Some(TEST_BOOTSTRAP_SECRET.to_string());
        settings.bootstrap_claim_enabled = true;
        settings.legacy_ui_bootstrap_enabled = false;
        settings.legacy_bootstrap_token_endpoint_enabled = false;
        settings.destructive_fresh_bootstrap_required = true;
        settings.destructive_allow_legacy_startup_side_effects = false;

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect greenfield bootstrap test postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply greenfield bootstrap migrations")?;

        let state = build_test_state(settings, postgres)?;
        Ok(Self { state, temp_database })
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    fn pool(&self) -> &PgPool {
        &self.state.persistence.postgres
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let bootstrap_settings = settings.bootstrap_settings();
    let persistence = Persistence {
        postgres,
        redis: redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for bootstrap test state")?,
    };

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
        Arc::new(NoopGraphStore),
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

async fn scalar_count(postgres: &PgPool, table_name: &str) -> Result<i64> {
    sqlx::query_scalar::<_, i64>(&format!("select count(*) from {table_name}"))
        .fetch_one(postgres)
        .await
        .with_context(|| format!("failed to count rows in {table_name}"))
}

async fn table_exists(postgres: &PgPool, table_name: &str) -> Result<bool> {
    sqlx::query_scalar::<_, bool>("select to_regclass($1) is not null")
        .bind(format!("public.{table_name}"))
        .fetch_one(postgres)
        .await
        .with_context(|| format!("failed to inspect table {table_name}"))
}

async fn response_json(response: axum::response::Response) -> Result<Value> {
    let bytes =
        response.into_body().collect().await.context("failed to collect response body")?.to_bytes();
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).context("failed to decode response json")
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn fresh_bootstrap_migration_creates_canonical_schema_and_seeded_catalog() -> Result<()> {
    let fixture = GreenfieldBootstrapFixture::create().await?;

    let result = async {
        assert!(canonical_baseline_present(fixture.pool()).await?);
        assert!(canonical_ai_catalog_seeded(fixture.pool()).await?);
        assert_eq!(
            scalar_count(fixture.pool(), "ai_provider_catalog").await?,
            SEEDED_PROVIDER_COUNT
        );
        assert_eq!(scalar_count(fixture.pool(), "ai_model_catalog").await?, SEEDED_MODEL_COUNT);
        assert_eq!(scalar_count(fixture.pool(), "ai_price_catalog").await?, SEEDED_PRICE_COUNT);
        assert!(!table_exists(fixture.pool(), "workspace").await?);
        assert!(!table_exists(fixture.pool(), "project").await?);
        assert!(!table_exists(fixture.pool(), "runtime_ingestion_run").await?);
        assert!(!table_exists(fixture.pool(), "mcp_audit_event").await?);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn bootstrap_claim_route_succeeds_once_and_records_audit_event() -> Result<()> {
    let fixture = GreenfieldBootstrapFixture::create().await?;

    let result = async {
        let payload = json!({
            "bootstrapSecret": TEST_BOOTSTRAP_SECRET,
            "email": "founder@example.local",
            "displayName": "Founder",
            "password": "super-secret-password",
        });

        let first_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/iam/bootstrap/claim")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("build first bootstrap claim request"),
            )
            .await
            .context("first bootstrap claim route failed")?;
        assert_eq!(first_response.status(), StatusCode::OK);
        let first_body = response_json(first_response).await?;
        assert_eq!(first_body["email"], "founder@example.local");
        assert_eq!(first_body["displayName"], "Founder");

        let second_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/iam/bootstrap/claim")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("build second bootstrap claim request"),
            )
            .await
            .context("second bootstrap claim route failed")?;
        assert_eq!(second_response.status(), StatusCode::CONFLICT);
        let second_body = response_json(second_response).await?;
        assert_eq!(second_body["errorKind"], "bootstrap_already_claimed");

        assert_eq!(scalar_count(fixture.pool(), "iam_principal").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "iam_user").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "audit_event").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "audit_event_subject").await?, 1);

        let action_kind =
            sqlx::query_scalar::<_, String>("select action_kind from audit_event limit 1")
                .fetch_one(fixture.pool())
                .await
                .context("failed to read bootstrap audit action")?;
        assert_eq!(action_kind, "iam.bootstrap.claim");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn fresh_bootstrap_starts_without_default_catalog_side_effect_rows() -> Result<()> {
    let fixture = GreenfieldBootstrapFixture::create().await?;

    let result = async {
        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/openapi/rustrag.openapi.yaml")
                    .body(Body::empty())
                    .expect("build openapi discovery request"),
            )
            .await
            .context("openapi discovery request failed")?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(scalar_count(fixture.pool(), "catalog_workspace").await?, 0);
        assert_eq!(scalar_count(fixture.pool(), "catalog_library").await?, 0);
        assert_eq!(scalar_count(fixture.pool(), "catalog_library_connector").await?, 0);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
