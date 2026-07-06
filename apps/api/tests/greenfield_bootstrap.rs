//! Greenfield bootstrap integration tests. Ignored by default: they need a
//! live PostgreSQL with the pgvector extension (same image as
//! docker-compose.yml, `pgvector/pgvector:pg18`) reachable via
//! `IRONRAG_DATABASE_URL`; each test creates and drops its own temporary
//! database on that server. Run with:
//!
//! ```sh
//! docker run -d --name ironrag-test-pg -e POSTGRES_PASSWORD=postgres \
//!     -p 127.0.0.1:55433:5432 pgvector/pgvector:pg18
//! IRONRAG_DATABASE_URL='postgres://postgres:postgres@127.0.0.1:55433/ironrag' cargo test -p ironrag-backend --test greenfield_bootstrap -- --ignored # pragma: allowlist secret
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{borrow::Cow, path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{PgPool, migrate::Migrator, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{
        config::{
            Settings, UiBootstrapAiBindingDefault, UiBootstrapAiProviderSecret, UiBootstrapAiSetup,
        },
        state::AppState,
    },
    infra::{
        persistence::{Persistence, canonical_ai_catalog_seeded, canonical_baseline_present},
        repositories::{self, catalog_repository},
    },
    integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
    },
    interfaces::http::router,
};

const SEEDED_PROVIDER_COUNT: i64 = 8;
const SEEDED_MODEL_COUNT: i64 = 1143;
const SEEDED_PRICE_COUNT: i64 = 2147;

// Env-backed bootstrap seeds the six canonical required purposes plus vision;
// the interactive provider-bundle path additionally covers extract_text.
const ENV_BOOTSTRAP_BINDING_COUNT: i64 = 7;
const BUNDLE_BOOTSTRAP_BINDING_COUNT: i64 = 8;

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
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
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

#[derive(Clone, Default)]
struct FakeBootstrapGateway;

#[async_trait]
impl LlmGateway for FakeBootstrapGateway {
    async fn generate(&self, request: ChatRequest) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            provider_kind: request.provider_kind,
            model_name: request.model_name,
            output_text: "OK".to_string(),
            usage_json: json!({}),
        })
    }

    async fn embed(&self, request: EmbeddingRequest) -> anyhow::Result<EmbeddingResponse> {
        Err(anyhow!("embed not used in bootstrap test: {}", request.provider_kind))
    }

    async fn embed_many(
        &self,
        request: EmbeddingBatchRequest,
    ) -> anyhow::Result<EmbeddingBatchResponse> {
        Err(anyhow!("embed_many not used in bootstrap test: {}", request.provider_kind))
    }

    async fn vision_extract(&self, request: VisionRequest) -> anyhow::Result<VisionResponse> {
        Err(anyhow!("vision_extract not used in bootstrap test: {}", request.provider_kind))
    }
}

impl GreenfieldBootstrapFixture {
    async fn create() -> Result<Self> {
        Self::create_with_ui_bootstrap_ai_setup(None).await
    }

    async fn create_with_ui_bootstrap_ai_setup(
        ui_bootstrap_ai_setup: Option<UiBootstrapAiSetup>,
    ) -> Result<Self> {
        let mut settings = Settings::from_env()
            .context("failed to load settings for greenfield bootstrap test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = temp_database.database_url.clone();
        settings.destructive_fresh_bootstrap_required = true;

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect greenfield bootstrap test postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply greenfield bootstrap migrations")?;

        let state = build_test_state(settings, postgres, ui_bootstrap_ai_setup)?;
        Ok(Self { state, temp_database })
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
}

fn build_test_state(
    settings: Settings,
    postgres: PgPool,
    ui_bootstrap_ai_setup: Option<UiBootstrapAiSetup>,
) -> Result<AppState> {
    let bootstrap_settings = settings.bootstrap_settings();
    let redis = redis::Client::open(settings.redis_url.clone())
        .context("failed to create redis client for bootstrap test state")?;
    let persistence = Persistence::for_tests(postgres, redis);
    let mut state = AppState::from_dependencies(
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
    )?;
    state.llm_gateway = Arc::new(FakeBootstrapGateway);
    state.ui_bootstrap_ai_setup = ui_bootstrap_ai_setup;
    Ok(state)
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
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("select count(*) from {table_name}")))
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

fn migrator_with_versions(source: &Migrator, min_version: i64, max_version: i64) -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            source
                .iter()
                .filter(|migration| {
                    migration.version >= min_version && migration.version <= max_version
                })
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
        table_name: Cow::Borrowed("_sqlx_migrations"),
        create_schemas: Cow::Borrowed(&[]),
    }
}

async fn response_json(response: axum::response::Response) -> Result<Value> {
    let bytes =
        response.into_body().collect().await.context("failed to collect response body")?.to_bytes();
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).context("failed to decode response json")
}

fn compose_like_bootstrap_ai_setup() -> UiBootstrapAiSetup {
    UiBootstrapAiSetup {
        provider_secrets: vec![
            UiBootstrapAiProviderSecret {
                provider_kind: "deepseek".to_string(),
                api_key: "test-deepseek-bootstrap-token".to_string(),
            },
            UiBootstrapAiProviderSecret {
                provider_kind: "openai".to_string(),
                api_key: "test-openai-bootstrap-token".to_string(),
            },
        ],
        binding_defaults: vec![
            UiBootstrapAiBindingDefault {
                binding_purpose: "extract_graph".to_string(),
                provider_kind: Some("deepseek".to_string()),
                model_name: Some("deepseek-chat".to_string()),
            },
            UiBootstrapAiBindingDefault {
                binding_purpose: "embed_chunk".to_string(),
                provider_kind: Some("openai".to_string()),
                model_name: Some("text-embedding-3-large".to_string()),
            },
            UiBootstrapAiBindingDefault {
                binding_purpose: "query_answer".to_string(),
                provider_kind: Some("openai".to_string()),
                model_name: Some("gpt-5.4".to_string()),
            },
            UiBootstrapAiBindingDefault {
                binding_purpose: "vision".to_string(),
                provider_kind: Some("openai".to_string()),
                model_name: Some("gpt-5.4-mini".to_string()),
            },
        ],
    }
}

async fn seed_orphaned_default_catalog_ai_runtime(
    fixture: &GreenfieldBootstrapFixture,
) -> Result<()> {
    let workspace =
        catalog_repository::create_workspace(fixture.pool(), "default", "Default workspace", None)
            .await
            .context("failed to create orphaned default workspace")?;
    let library = catalog_repository::create_library(
        fixture.pool(),
        workspace.id,
        "default-library",
        "Default library",
        Some("Backstage default library for the primary documents and ask flow"),
        None,
    )
    .await
    .context("failed to create orphaned default library")?;

    fixture
        .state
        .canonical_services
        .ai_catalog
        .apply_configured_bootstrap_ai_setup(&fixture.state, workspace.id, library.id, None)
        .await
        .context("failed to seed orphaned bootstrap AI runtime")?;

    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn graph_index_migration_accepts_long_entity_labels() -> Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for graph index migration test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect graph index migration test postgres")?;

    let result = async {
        let migrations = Migrator::new(Path::new("./migrations"))
            .await
            .context("failed to load migration files")?;
        migrator_with_versions(&migrations, 1, 13)
            .run(&pool)
            .await
            .context("failed to apply migrations before graph index migration")?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("graph-index-{suffix}"),
            "Graph Index Migration",
            None,
        )
        .await
        .context("failed to create graph index migration workspace")?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("graph-index-library-{suffix}"),
            "Graph Index Migration Library",
            None,
            None,
        )
        .await
        .context("failed to create graph index migration library")?;

        // Long enough to overflow the pre-0.5.0 double-term btree index
        // (lower(label) + raw label > 2704 bytes), short enough to stay under
        // the 2000-byte write clamp so the exact-match lookup sees the
        // verbatim label.
        let long_label = format!("{}{}", "Alpha ".repeat(320), suffix);
        let node = repositories::upsert_runtime_graph_node(
            &pool,
            library.id,
            &format!("entity:{suffix}"),
            &long_label,
            "entity",
            None,
            json!([]),
            Some("Graph index migration long label fixture"),
            json!({}),
            3,
            1,
        )
        .await
        .context("failed to insert long-label runtime graph node")?;

        migrations
            .run(&pool)
            .await
            .context("failed to apply graph index migration with long labels")?;

        let exact_index_definition =
            sqlx::query_scalar::<_, String>("select indexdef from pg_indexes where indexname = $1")
                .bind("idx_runtime_graph_node_entity_label_exact")
                .fetch_one(&pool)
                .await
                .context("failed to inspect exact graph label index")?;
        let exact_index_definition = exact_index_definition.to_lowercase();
        assert!(exact_index_definition.contains("lower(label)"));
        // The pre-0.5.0 bug indexed the raw label alongside lower(label)
        // ("support_count desc, label, created_at"), overflowing the btree
        // tuple limit; the baseline must keep a single lower(label) term.
        assert!(!exact_index_definition.contains("desc, label,"));

        let edge_index_definition =
            sqlx::query_scalar::<_, String>("select indexdef from pg_indexes where indexname = $1")
                .bind("idx_runtime_graph_edge_projection_support_admitted")
                .fetch_one(&pool)
                .await
                .context("failed to inspect graph edge support index")?
                .to_lowercase();
        assert!(!edge_index_definition.contains("relation_type asc"));

        let rows = repositories::search_admitted_runtime_graph_entities_by_query_text(
            &pool,
            library.id,
            1,
            &long_label,
            5,
        )
        .await
        .context("failed to search exact long-label runtime graph node")?;
        assert_eq!(rows.first().map(|row| row.id), Some(node.id));

        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
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
        assert!(!table_exists(fixture.pool(), "mcp_audit_event").await?);
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
                    .uri("/v1/openapi/ironrag.openapi.yaml")
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

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn bootstrap_setup_route_rejects_missing_ai_payload_without_leaving_first_user_behind()
-> Result<()> {
    let fixture = GreenfieldBootstrapFixture::create().await?;

    let result = async {
        let payload = json!({
            "login": "admin",
            "displayName": "Admin",
            "password": "super-secret-password",
        });

        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/iam/bootstrap/setup")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("build bootstrap setup request"),
            )
            .await
            .context("bootstrap setup request failed")?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_json(response).await?;
        assert_eq!(body["errorKind"], "bad_request");

        let status_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/iam/bootstrap/status")
                    .body(Body::empty())
                    .expect("build bootstrap status request"),
            )
            .await
            .context("bootstrap status request failed")?;
        assert_eq!(status_response.status(), StatusCode::OK);
        let status_body = response_json(status_response).await?;
        assert_eq!(status_body["setupRequired"], true);
        assert_eq!(scalar_count(fixture.pool(), "iam_principal").await?, 0);
        assert_eq!(scalar_count(fixture.pool(), "iam_user").await?, 0);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn bootstrap_setup_route_uses_env_backed_openai_defaults() -> Result<()> {
    let fixture =
        GreenfieldBootstrapFixture::create_with_ui_bootstrap_ai_setup(Some(UiBootstrapAiSetup {
            provider_secrets: vec![UiBootstrapAiProviderSecret {
                provider_kind: "openai".to_string(),
                api_key: "test-openai-bootstrap-token".to_string(),
            }],
            binding_defaults: vec![],
        }))
        .await?;

    let result = async {
        let payload = json!({
            "login": "admin",
            "displayName": "Admin",
            "password": "super-secret-password",
        });

        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/iam/bootstrap/setup")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("build env-backed bootstrap setup request"),
            )
            .await
            .context("env-backed bootstrap setup request failed")?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(header::SET_COOKIE));

        let status_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/iam/bootstrap/status")
                    .body(Body::empty())
                    .expect("build bootstrap status request"),
            )
            .await
            .context("bootstrap status request failed")?;
        let status_body = response_json(status_response).await?;
        assert_eq!(status_body["setupRequired"], false);

        assert_eq!(scalar_count(fixture.pool(), "iam_user").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "ai_account").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "ai_binding").await?, ENV_BOOTSTRAP_BINDING_COUNT);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn bootstrap_setup_route_accepts_provider_bundle_payload() -> Result<()> {
    let fixture = GreenfieldBootstrapFixture::create().await?;

    let result = async {
        let status_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/iam/bootstrap/status")
                    .body(Body::empty())
                    .expect("build bootstrap status request"),
            )
            .await
            .context("bootstrap status request failed")?;
        assert_eq!(status_response.status(), StatusCode::OK);
        let status_body = response_json(status_response).await?;
        assert_eq!(status_body["setupRequired"], true);
        assert!(status_body["aiSetup"]["bindingBundles"].is_array());
        assert!(
            status_body["aiSetup"]["bindingBundles"]
                .as_array()
                .expect("binding bundles array")
                .iter()
                .any(|bundle| {
                    bundle["providerKind"] == "openai"
                        && bundle["apiKeyRequired"] == true
                        && bundle["baseUrlRequired"] == false
                        && bundle["bindings"]
                            .as_array()
                            .expect("provider bindings array")
                            .iter()
                            .any(|binding| {
                                binding["bindingPurpose"] == "extract_graph"
                                    && binding["modelName"] == "gpt-5.4-nano"
                            })
                })
        );
        assert!(
            status_body["aiSetup"]["bindingBundles"]
                .as_array()
                .expect("binding bundles array")
                .iter()
                .any(|bundle| {
                    bundle["providerKind"] == "ollama"
                        && bundle["apiKeyRequired"] == false
                        && bundle["baseUrlRequired"] == true
                        && bundle["defaultBaseUrl"] == "http://localhost:11434/v1"
                })
        );

        let payload = json!({
            "login": "admin",
            "displayName": "Admin",
            "password": "super-secret-password",
            "aiSetup": {
                "providerKind": "openai",
                "apiKey": "test-openai-bootstrap-token"
            }
        });

        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/iam/bootstrap/setup")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("build interactive env-backed bootstrap setup request"),
            )
            .await
            .context("provider bundle bootstrap setup request failed")?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(header::SET_COOKIE));

        let status_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/iam/bootstrap/status")
                    .body(Body::empty())
                    .expect("build bootstrap status request"),
            )
            .await
            .context("bootstrap status request failed")?;
        let status_body = response_json(status_response).await?;
        assert_eq!(status_body["setupRequired"], false);

        assert_eq!(scalar_count(fixture.pool(), "iam_user").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "ai_account").await?, 1);
        assert_eq!(
            scalar_count(fixture.pool(), "ai_binding").await?,
            BUNDLE_BOOTSTRAP_BINDING_COUNT
        );

        let binding_models = sqlx::query_scalar::<_, String>(
            "select amc.model_name
             from ai_binding ab
             join ai_model_catalog amc on amc.id = ab.model_catalog_id
             where ab.binding_purpose = 'extract_graph'",
        )
        .fetch_one(fixture.pool())
        .await
        .context("failed to load extract_graph bootstrap model")?;
        assert_eq!(binding_models, "gpt-5.4-nano");

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn bootstrap_setup_route_rejects_provider_without_self_contained_bundle() -> Result<()> {
    // A provider bundle must cover every required purpose with its own models
    // (deepseek ships no embedding models). Even with an env-backed openai
    // secret available, the bundle must not borrow models from another
    // provider — the request is rejected without leaving partial state.
    let fixture =
        GreenfieldBootstrapFixture::create_with_ui_bootstrap_ai_setup(Some(UiBootstrapAiSetup {
            provider_secrets: vec![UiBootstrapAiProviderSecret {
                provider_kind: "openai".to_string(),
                api_key: "test-openai-bootstrap-token".to_string(),
            }],
            binding_defaults: vec![],
        }))
        .await?;

    let result = async {
        let status_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/iam/bootstrap/status")
                    .body(Body::empty())
                    .expect("build bootstrap status request"),
            )
            .await
            .context("bootstrap status request failed")?;
        let status_body = response_json(status_response).await?;
        assert!(
            !status_body["aiSetup"]["bindingBundles"]
                .as_array()
                .expect("binding bundles array")
                .iter()
                .any(|bundle| bundle["providerKind"] == "deepseek")
        );

        let payload = json!({
            "login": "admin",
            "displayName": "Admin",
            "password": "super-secret-password",
            "aiSetup": {
                "providerKind": "deepseek",
                "apiKey": "test-deepseek-bootstrap-token"
            }
        });

        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/iam/bootstrap/setup")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("build deepseek provider bundle bootstrap setup request"),
            )
            .await
            .context("deepseek provider bundle bootstrap setup request failed")?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_json(response).await?;
        assert_eq!(body["errorKind"], "bad_request");

        assert_eq!(scalar_count(fixture.pool(), "iam_principal").await?, 0);
        assert_eq!(scalar_count(fixture.pool(), "iam_user").await?, 0);
        assert_eq!(scalar_count(fixture.pool(), "ai_account").await?, 0);
        assert_eq!(scalar_count(fixture.pool(), "ai_binding").await?, 0);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn bootstrap_setup_route_recovers_from_orphaned_env_backed_ai_state() -> Result<()> {
    let fixture = GreenfieldBootstrapFixture::create_with_ui_bootstrap_ai_setup(Some(
        compose_like_bootstrap_ai_setup(),
    ))
    .await?;

    let result = async {
        seed_orphaned_default_catalog_ai_runtime(&fixture).await?;
        assert_eq!(scalar_count(fixture.pool(), "iam_principal").await?, 0);
        assert_eq!(scalar_count(fixture.pool(), "iam_user").await?, 0);
        assert_eq!(scalar_count(fixture.pool(), "catalog_workspace").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "catalog_library").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "ai_account").await?, 2);
        assert_eq!(scalar_count(fixture.pool(), "ai_binding").await?, ENV_BOOTSTRAP_BINDING_COUNT);

        let payload = json!({
            "login": "admin",
            "displayName": "Admin",
            "password": "super-secret-password",
        });

        let response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/iam/bootstrap/setup")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("build orphaned bootstrap recovery request"),
            )
            .await
            .context("orphaned bootstrap recovery request failed")?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(header::SET_COOKIE));

        let status_response = fixture
            .app()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/iam/bootstrap/status")
                    .body(Body::empty())
                    .expect("build bootstrap status request"),
            )
            .await
            .context("bootstrap status request failed")?;
        let status_body = response_json(status_response).await?;
        assert_eq!(status_body["setupRequired"], false);

        assert_eq!(scalar_count(fixture.pool(), "iam_principal").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "iam_user").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "catalog_workspace").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "catalog_library").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "ai_account").await?, 2);
        assert_eq!(scalar_count(fixture.pool(), "ai_binding").await?, ENV_BOOTSTRAP_BINDING_COUNT);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
