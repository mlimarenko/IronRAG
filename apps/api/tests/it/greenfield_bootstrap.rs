//! Greenfield bootstrap integration tests. Ignored by default: they need a
//! live PostgreSQL with the pgvector extension (same image as
//! docker-compose.yml, `pgvector/pgvector:pg18`) reachable via
//! `IRONRAG_DATABASE_URL`; each test creates and drops its own temporary
//! database on that server. Run with:
//!
//! ```sh
//! docker run -d --name ironrag-test-pg -e POSTGRES_PASSWORD=postgres \
//!     -p 127.0.0.1:55433:5432 pgvector/pgvector:pg18
//! IRONRAG_DATABASE_URL='postgres://postgres:postgres@127.0.0.1:55433/ironrag' cargo test -p ironrag-backend --test it -- --ignored greenfield_bootstrap:: # pragma: allowlist secret
//! ```

use crate::{database_url, http_response_support};
use ironrag_backend::infra::persistence;

use database_url::database_url_with_name;
use http_response_support::response_json;

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha384};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::io::AsyncReadExt;
use tower::ServiceExt;
use uuid::Uuid;

use ironrag_backend::{
    app::{
        config::{
            Settings, UiBootstrapAiBindingDefault, UiBootstrapAiProviderSecret, UiBootstrapAiSetup,
        },
        seed_clean_v1_ai_catalog,
        state::AppState,
    },
    domains::ai::AiBindingPurpose,
    infra::{
        persistence::Persistence,
        repositories::{self, catalog_repository},
    },
    integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
    },
    interfaces::http::router,
    services::content::service::snapshot::{
        IncludeKind, OverwriteMode, export_library_archive, restore_library_archive,
    },
    shared::secret_encryption::SecretPurpose,
};

fn test_credential_master_key() -> String {
    STANDARD.encode([41_u8; 32])
}

// The env path seeds the six required purposes. The provider-bundle path also
// installs the optional `query_rerank` and `graph_entity_type` bindings its
// presets carry, so it seeds eight while still requiring only six.
const ENV_BOOTSTRAP_BINDING_COUNT: i64 = 6;
const BUNDLE_BOOTSTRAP_BINDING_COUNT: i64 = 8;

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = database_url_with_name(base_database_url, "postgres")?;
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
    async fn generate(&self, mut request: ChatRequest) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
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
        settings.credential_master_key = Some(test_credential_master_key());

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("failed to connect greenfield bootstrap test postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply greenfield bootstrap migrations")?;
        seed_clean_v1_ai_catalog(&postgres)
            .await
            .context("failed to seed authoritative clean-v1 AI catalog")?;
        disable_provider_credential_validation(&postgres).await?;

        let state = build_test_state(settings, postgres, ui_bootstrap_ai_setup)?;
        Ok(Self { state, temp_database })
    }

    fn app(&self) -> Router {
        Router::new().nest("/v1", router()).with_state(self.state.clone())
    }

    async fn get(&self, path: &str) -> Result<axum::response::Response> {
        let request = Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .with_context(|| format!("failed to build GET {path} request"))?;
        self.app().oneshot(request).await.with_context(|| format!("GET {path} failed"))
    }

    async fn post_json(&self, path: &str, payload: &Value) -> Result<axum::response::Response> {
        let request = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string()))
            .with_context(|| format!("failed to build POST {path} request"))?;
        self.app().oneshot(request).await.with_context(|| format!("POST {path} failed"))
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

async fn disable_provider_credential_validation(postgres: &PgPool) -> Result<()> {
    sqlx::query(
        "update ai_provider_catalog
         set capability_flags_json = jsonb_set(
             capability_flags_json,
             '{credentials,validationMode}',
             '\"none\"'::jsonb,
             false
         )",
    )
    .execute(postgres)
    .await
    .context("failed to disable outbound provider validation in test fixture")?;
    Ok(())
}

fn authoritative_bootstrap_binding_defaults() -> Vec<UiBootstrapAiBindingDefault> {
    [
        (AiBindingPurpose::ExtractText, "gpt-6-luna"),
        (AiBindingPurpose::ExtractGraph, "gpt-6-luna"),
        (AiBindingPurpose::EmbedChunk, "text-embedding-3-large"),
        (AiBindingPurpose::QueryCompile, "gpt-6-luna"),
        (AiBindingPurpose::QueryAnswer, "gpt-6-luna"),
        (AiBindingPurpose::Agent, "gpt-6-luna"),
    ]
    .into_iter()
    .map(|(binding_purpose, model_name)| UiBootstrapAiBindingDefault {
        binding_purpose,
        provider_kind: Some("openai".to_string()),
        model_name: Some(model_name.to_string()),
    })
    .collect()
}

fn compose_like_bootstrap_ai_setup() -> UiBootstrapAiSetup {
    UiBootstrapAiSetup {
        provider_secrets: vec![UiBootstrapAiProviderSecret {
            provider_kind: "openai".to_string(),
            api_key: "test-openai-bootstrap-token".to_string(),
        }],
        binding_defaults: authoritative_bootstrap_binding_defaults(),
    }
}

fn bootstrap_setup_payload(ai_setup: Option<Value>) -> Value {
    let mut payload = json!({
        "login": "admin",
        "email": "admin@example.test",
        "displayName": "Admin",
        "password": "super-secret-password",
    });
    if let Some(ai_setup) = ai_setup {
        payload["aiSetup"] = ai_setup;
    }
    payload
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
        .application_services
        .ai_catalog
        .apply_configured_bootstrap_ai_setup(&fixture.state, workspace.id, library.id, None)
        .await
        .context("failed to seed orphaned bootstrap AI runtime")?;

    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn fresh_baseline_adds_structured_block_indexes() -> Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for structured block index baseline test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect structured block index baseline test postgres")?;

    let result = async {
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply fresh baseline")?;

        let index_definition =
            sqlx::query_scalar::<_, String>("select indexdef from pg_indexes where indexname = $1")
                .bind("idx_knowledge_structured_block_setup_order")
                .fetch_one(&pool)
                .await
                .context("failed to inspect structured block setup index")?
                .to_lowercase();

        assert!(index_definition.contains("revision_id, ordinal, block_id"));
        for block_kind in ["table", "table_row", "code_block", "source_unit"] {
            assert!(index_definition.contains(block_kind));
        }

        for (index_name, table_name, column_name) in [
            ("idx_knowledge_chunk_primary_block_fk", "knowledge_chunk", "primary_block_id"),
            ("idx_knowledge_evidence_block_fk", "knowledge_evidence", "block_id"),
            (
                "idx_knowledge_structured_block_parent_block_fk",
                "knowledge_structured_block",
                "parent_block_id",
            ),
        ] {
            let index_definition = sqlx::query_scalar::<_, String>(
                "select indexdef from pg_indexes where schemaname = 'public' and indexname = $1",
            )
            .bind(index_name)
            .fetch_one(&pool)
            .await
            .with_context(|| format!("failed to inspect {index_name}"))?
            .to_lowercase();

            assert!(
                index_definition
                    .contains(&format!("on public.{table_name} using btree ({column_name})")),
                "unexpected {index_name} definition: {index_definition}"
            );
            assert!(
                index_definition.contains(&format!("where ({column_name} is not null)")),
                "{index_name} must exclude null foreign keys: {index_definition}"
            );
        }
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

#[test]
fn baseline_and_live_recovery_cover_structured_block_fk_children() {
    let baseline = include_str!("../../migrations/0001_init.sql");
    let live_recovery = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/ops/apply-knowledge-structured-block-fk-indexes.sql"
    ));

    for (table_name, column_name, index_name, constraint_name) in [
        (
            "knowledge_chunk",
            "primary_block_id",
            "idx_knowledge_chunk_primary_block_fk",
            "knowledge_chunk_primary_block_id_fkey",
        ),
        (
            "knowledge_evidence",
            "block_id",
            "idx_knowledge_evidence_block_fk",
            "knowledge_evidence_block_id_fkey",
        ),
        (
            "knowledge_structured_block",
            "parent_block_id",
            "idx_knowledge_structured_block_parent_block_fk",
            "knowledge_structured_block_parent_block_id_fkey",
        ),
    ] {
        let baseline_definition = format!(
            "CREATE INDEX {index_name} ON public.{table_name} USING btree ({column_name}) WHERE ({column_name} IS NOT NULL);"
        );
        let recovery_definition = format!(
            "CREATE INDEX CONCURRENTLY {index_name} ON public.{table_name} USING btree ({column_name}) WHERE ({column_name} IS NOT NULL)"
        );
        let constraint_definition = format!(
            "ADD CONSTRAINT {constraint_name} FOREIGN KEY ({column_name}) REFERENCES public.knowledge_structured_block(block_id) ON DELETE SET NULL;"
        );

        assert!(baseline.contains(&baseline_definition), "missing {baseline_definition}");
        assert!(baseline.contains(&constraint_definition), "missing {constraint_definition}");
        assert!(live_recovery.contains(&recovery_definition), "missing {recovery_definition}");
    }
}

/// A baseline edit must ship its live-recovery statement in a canonical,
/// idempotent operator file, or an operator cannot reach a live database from
/// an edited baseline. The operator commands are documented in docs/en/CLI.md.
#[test]
fn baseline_and_live_recovery_cover_ops_failure_disposition() {
    let baseline = include_str!("../../migrations/0001_init.sql");
    let live_recovery = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/api/ops/apply-ops-failure-disposition-schema.sql"
    ));

    assert!(
        baseline.contains("failure_disposition text,"),
        "the baseline must declare the nullable failure_disposition column"
    );
    assert!(
        live_recovery.contains(
            "ALTER TABLE public.ops_async_operation\n    ADD COLUMN IF NOT EXISTS failure_disposition text;"
        ),
        "the recovery file must add the column idempotently"
    );
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn fresh_baseline_graph_index_accepts_long_entity_labels() -> Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for graph index baseline test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect graph index baseline test postgres")?;

    let result = async {
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply fresh baseline")?;

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &pool,
            &format!("graph-index-{suffix}"),
            "Graph Index Migration",
            None,
        )
        .await
        .context("failed to create graph index baseline workspace")?;
        let library = catalog_repository::create_library(
            &pool,
            workspace.id,
            &format!("graph-index-library-{suffix}"),
            "Graph Index Migration Library",
            None,
            None,
        )
        .await
        .context("failed to create graph index baseline library")?;

        // Long enough to overflow the pre-0.5.0 double-term btree index
        // (lower(label) + raw label > 2704 bytes), short enough to stay under
        // the 2000-byte write clamp so the exact-match lookup sees the
        // verbatim label.
        let long_label = format!("{}{}", "Alpha ".repeat(320), suffix);
        let node = repositories::upsert_runtime_graph_node(
            &pool,
            repositories::RuntimeGraphNodeInput {
                library_id: library.id,
                identity_key: &format!("entity:{suffix}"),
                label: &long_label,
                node_type: "entity",
                document_id: None,
                aliases_json: json!([]),
                summary: Some("Fresh baseline graph index long label fixture"),
                metadata_json: json!({}),
                support_count: 3,
                projection_version: 1,
            },
        )
        .await
        .context("failed to insert long-label runtime graph node")?;

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
async fn fresh_bootstrap_starts_without_default_catalog_side_effect_rows() -> Result<()> {
    let fixture = GreenfieldBootstrapFixture::create().await?;

    let result = async {
        let response = fixture
            .get("/v1/openapi/ironrag.openapi.yaml")
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
async fn setup_required_stays_true_until_an_admin_exists() -> Result<()> {
    let fixture = GreenfieldBootstrapFixture::create().await?;

    let result = async {
        let status_response =
            fixture.get("/v1/iam/bootstrap").await.context("bootstrap status request failed")?;
        let status_body = response_json(status_response).await?;
        assert_eq!(status_body["setupRequired"], true);

        // A non-admin JIT-provisioned user (an OIDC login that is not the
        // configured bootstrap-admin subject, for example) must not close
        // bootstrap: a deployment with users but no administrator still
        // needs the interactive setup path available.
        repositories::iam_repository::create_user(
            fixture.pool(),
            "viewer-one",
            "viewer-one@example.invalid",
            "Viewer One",
            "synthetic-password-hash",
            repositories::iam_repository::SystemRole::Viewer,
        )
        .await
        .context("failed to seed the non-admin user")?;

        let status_response =
            fixture.get("/v1/iam/bootstrap").await.context("bootstrap status request failed")?;
        let status_body = response_json(status_response).await?;
        assert_eq!(
            status_body["setupRequired"], true,
            "an active user without the admin role must not close interactive bootstrap"
        );

        repositories::iam_repository::create_user(
            fixture.pool(),
            "admin-one",
            "admin-one@example.invalid",
            "Admin One",
            "synthetic-password-hash",
            repositories::iam_repository::SystemRole::Admin,
        )
        .await
        .context("failed to seed the admin user")?;

        let status_response =
            fixture.get("/v1/iam/bootstrap").await.context("bootstrap status request failed")?;
        let status_body = response_json(status_response).await?;
        assert_eq!(status_body["setupRequired"], false);

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
        let payload = bootstrap_setup_payload(None);

        let response = fixture
            .post_json("/v1/iam/bootstrap", &payload)
            .await
            .context("bootstrap setup request failed")?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_json(response).await?;
        assert_eq!(body["code"], "bad_request");

        let status_response =
            fixture.get("/v1/iam/bootstrap").await.context("bootstrap status request failed")?;
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
        let payload = bootstrap_setup_payload(None);

        let response = fixture
            .post_json("/v1/iam/bootstrap", &payload)
            .await
            .context("env-backed bootstrap setup request failed")?;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(response.headers().contains_key(header::SET_COOKIE));

        let status_response =
            fixture.get("/v1/iam/bootstrap").await.context("bootstrap status request failed")?;
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
async fn env_account_sync_rotates_authoritative_accounts_in_every_scope() -> Result<()> {
    let configured_key = "test-current-bootstrap-token"; // pragma: allowlist secret
    let fixture =
        GreenfieldBootstrapFixture::create_with_ui_bootstrap_ai_setup(Some(UiBootstrapAiSetup {
            provider_secrets: vec![UiBootstrapAiProviderSecret {
                provider_kind: "openai".to_string(),
                api_key: configured_key.to_string(),
            }],
            binding_defaults: vec![],
        }))
        .await?;

    let result = async {
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            fixture.pool(),
            &format!("credential-sync-workspace-{suffix}"),
            "Credential Sync Workspace",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            fixture.pool(),
            workspace.id,
            &format!("credential-sync-library-{suffix}"),
            "Credential Sync Library",
            None,
            None,
        )
        .await?;
        let provider_id = sqlx::query_scalar::<_, Uuid>(
            "select id from ai_provider_catalog where provider_kind = 'openai'",
        )
        .fetch_one(fixture.pool())
        .await?;
        let account_id = Uuid::now_v7();
        let stale_encrypted_key = fixture.state.credential_cipher.encrypt(
            SecretPurpose::AiAccountApiKey,
            account_id,
            "test-stale-bootstrap-token", // pragma: allowlist secret
        )?;
        let library_account = repositories::ai_repository::create_account(
            fixture.pool(),
            repositories::ai_repository::NewAiAccount {
                account_id,
                scope_kind: "library",
                workspace_id: Some(workspace.id),
                library_id: Some(library.id),
                provider_catalog_id: provider_id,
                label: "Bootstrap OpenAI",
                base_url: None,
                created_by_principal_id: None,
            },
            Some(&stale_encrypted_key),
        )
        .await?;
        sqlx::query("update ai_account set credential_state = 'revoked' where id = $1")
            .bind(library_account.id)
            .execute(fixture.pool())
            .await?;

        assert_eq!(
            fixture
                .state
                .application_services
                .ai_catalog
                .ensure_env_ai_accounts(&fixture.state)
                .await?,
            2,
        );

        let accounts = sqlx::query_as::<_, (Uuid, String, String, Option<String>)>(
            "select id, scope_kind::text, credential_state::text, api_key
             from ai_account
             where provider_catalog_id = $1 and label = 'Bootstrap OpenAI'
             order by scope_kind",
        )
        .bind(provider_id)
        .fetch_all(fixture.pool())
        .await?;
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].1, "instance");
        assert_eq!(accounts[0].2, "active");
        assert_eq!(accounts[1].1, "library");
        assert_eq!(accounts[1].2, "revoked");
        for (account_id, _, _, stored_key) in &accounts {
            let stored_key = stored_key.as_deref().context("expected encrypted API key")?;
            assert!(stored_key.starts_with("ironrag:enc:v3:"));
            assert_eq!(
                fixture
                    .state
                    .credential_cipher
                    .decrypt(SecretPurpose::AiAccountApiKey, *account_id, stored_key)?
                    .expose_secret(),
                configured_key,
            );
        }
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
        let status_response =
            fixture.get("/v1/iam/bootstrap").await.context("bootstrap status request failed")?;
        assert_eq!(status_response.status(), StatusCode::OK);
        let status_body = response_json(status_response).await?;
        assert_eq!(status_body["setupRequired"], true);
        let binding_bundles = status_body["aiSetup"]["bindingBundles"]
            .as_array()
            .context("bootstrap status is missing the binding bundles array")?;
        assert!(binding_bundles.iter().any(|bundle| {
            bundle["providerKind"] == "openai"
                && bundle["apiKeyRequired"] == true
                && bundle["baseUrlRequired"] == false
                && bundle["bindings"].as_array().is_some_and(|bindings| {
                    bindings.iter().any(|binding| {
                        binding["bindingPurpose"] == "extract_graph"
                            && binding["modelName"] == "gpt-6-luna"
                    })
                })
        }));
        for bundle in binding_bundles {
            let bindings = bundle["bindings"]
                .as_array()
                .context("bootstrap bundle is missing its bindings array")?;
            for purpose in [
                AiBindingPurpose::ExtractText,
                AiBindingPurpose::ExtractGraph,
                AiBindingPurpose::EmbedChunk,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::Agent,
            ] {
                assert!(
                    bindings.iter().any(|binding| {
                        binding["bindingPurpose"].as_str() == Some(purpose.as_str())
                    }),
                    "published bootstrap bundle is missing required purpose {}",
                    purpose.as_str()
                );
            }
        }

        let payload = bootstrap_setup_payload(Some(json!({
            "providerKind": "openai",
            "apiKey": "test-openai-bootstrap-token"
        })));

        let response = fixture
            .post_json("/v1/iam/bootstrap", &payload)
            .await
            .context("provider bundle bootstrap setup request failed")?;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(response.headers().contains_key(header::SET_COOKIE));

        let status_response =
            fixture.get("/v1/iam/bootstrap").await.context("bootstrap status request failed")?;
        let status_body = response_json(status_response).await?;
        assert_eq!(status_body["setupRequired"], false);

        assert_eq!(scalar_count(fixture.pool(), "iam_user").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "ai_account").await?, 1);
        assert_eq!(
            scalar_count(fixture.pool(), "ai_binding").await?,
            BUNDLE_BOOTSTRAP_BINDING_COUNT
        );

        let (binding_model, binding_parameters) = sqlx::query_as::<_, (String, serde_json::Value)>(
            "select amc.model_name, ab.extra_parameters_json
             from ai_binding ab
             join ai_model_catalog amc on amc.id = ab.model_catalog_id
             where ab.binding_purpose = 'extract_graph'",
        )
        .fetch_one(fixture.pool())
        .await
        .context("failed to load extract_graph bootstrap model")?;
        assert_eq!(binding_model, "gpt-6-luna");
        // The preset's request parameters reach the created binding: graph
        // extraction runs without reasoning on the half-price Flex tier.
        assert_eq!(binding_parameters, json!({"reasoning_effort": "none", "service_tier": "flex"}));

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
    // (Anthropic has no embedding model). Even with an env-backed OpenAI
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
        let status_response =
            fixture.get("/v1/iam/bootstrap").await.context("bootstrap status request failed")?;
        let status_body = response_json(status_response).await?;
        let binding_bundles = status_body["aiSetup"]["bindingBundles"]
            .as_array()
            .context("bootstrap status is missing the binding bundles array")?;
        assert!(!binding_bundles.iter().any(|bundle| bundle["providerKind"] == "anthropic"));

        let payload = bootstrap_setup_payload(Some(json!({
            "providerKind": "anthropic",
            "apiKey": "test-anthropic-bootstrap-token"
        })));

        let response = fixture
            .post_json("/v1/iam/bootstrap", &payload)
            .await
            .context("Anthropic provider bundle bootstrap setup request failed")?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_json(response).await?;
        assert_eq!(body["code"], "bad_request");

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
        assert_eq!(scalar_count(fixture.pool(), "ai_account").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "ai_binding").await?, ENV_BOOTSTRAP_BINDING_COUNT);

        let payload = bootstrap_setup_payload(None);

        let response = fixture
            .post_json("/v1/iam/bootstrap", &payload)
            .await
            .context("orphaned bootstrap recovery request failed")?;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(response.headers().contains_key(header::SET_COOKIE));

        let status_response =
            fixture.get("/v1/iam/bootstrap").await.context("bootstrap status request failed")?;
        let status_body = response_json(status_response).await?;
        assert_eq!(status_body["setupRequired"], false);

        assert_eq!(scalar_count(fixture.pool(), "iam_principal").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "iam_user").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "catalog_workspace").await?, 1);
        assert_eq!(scalar_count(fixture.pool(), "catalog_library").await?, 1);
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
async fn binding_schema_enforces_authoritative_model_purpose_and_provider_ownership() -> Result<()>
{
    let fixture = GreenfieldBootstrapFixture::create().await?;

    let result = async {
        let scalar_columns: Vec<String> = sqlx::query_scalar(
            "select column_name
             from information_schema.columns
             where table_schema = 'public'
               and table_name = 'ai_model_catalog'
               and column_name = any($1::text[])",
        )
        .bind(vec!["capability_kind", "modality_kind"])
        .fetch_all(fixture.pool())
        .await
        .context("failed to inspect model catalog scalar eligibility columns")?;
        assert!(scalar_columns.is_empty());

        let provider_a = Uuid::now_v7();
        let provider_b = Uuid::now_v7();
        for (provider_id, provider_kind) in
            [(provider_a, "synthetic_a"), (provider_b, "synthetic_b")]
        {
            sqlx::query(
                "insert into ai_provider_catalog (
                    id, provider_kind, display_name, api_style, lifecycle_state,
                    capability_flags_json
                 ) values ($1, $2, $2, 'openai_compatible', 'active', '{}')",
            )
            .bind(provider_id)
            .bind(provider_kind)
            .execute(fixture.pool())
            .await
            .context("failed to insert synthetic provider")?;
        }

        let account_a = Uuid::now_v7();
        let account_b = Uuid::now_v7();
        for (account_id, provider_id, label) in [
            (account_a, provider_a, "synthetic account a"),
            (account_b, provider_b, "synthetic account b"),
        ] {
            sqlx::query(
                "insert into ai_account (
                    id, provider_catalog_id, label, credential_state, scope_kind
                 ) values ($1, $2, $3, 'active', 'instance')",
            )
            .bind(account_id)
            .bind(provider_id)
            .bind(label)
            .execute(fixture.pool())
            .await
            .context("failed to insert synthetic account")?;
        }

        let model_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_model_catalog (
                id, provider_catalog_id, model_name, lifecycle_state, metadata_json
             ) values ($1, $2, 'synthetic-model', 'active', '{}')",
        )
        .bind(model_id)
        .bind(provider_b)
        .execute(fixture.pool())
        .await
        .context("failed to insert synthetic model")?;
        let duplicate = sqlx::query(
            "insert into ai_model_catalog (id, provider_catalog_id, model_name, lifecycle_state, metadata_json)
             values ($1, $2, 'synthetic-model', 'active', '{}')",
        ).bind(Uuid::now_v7()).bind(provider_b).execute(fixture.pool()).await
            .expect_err("model names must be unique within a provider");
        assert_eq!(duplicate.as_database_error().and_then(sqlx::error::DatabaseError::code).as_deref(), Some("23505"));
        sqlx::query(
            "insert into ai_model_catalog (id, provider_catalog_id, model_name, lifecycle_state, metadata_json)
             values ($1, $2, 'synthetic-model', 'active', '{}')",
        ).bind(Uuid::now_v7()).bind(provider_a).execute(fixture.pool()).await
            .context("another provider may own the same model name")?;
        sqlx::query(
            "insert into ai_model_binding_purpose (model_catalog_id, binding_purpose)
             values ($1, 'query_answer')",
        )
        .bind(model_id)
        .execute(fixture.pool())
        .await
        .context("failed to insert synthetic model purpose")?;

        const INSERT_BINDING: &str = "insert into ai_binding (
                id, binding_purpose, account_id, provider_catalog_id, binding_state,
                scope_kind, model_catalog_id
             ) values ($1, $2::ai_binding_purpose, $3, $4, 'active', 'instance', $5)";

        let mismatched_model_provider = sqlx::query(INSERT_BINDING)
            .bind(Uuid::now_v7())
            .bind("query_answer")
            .bind(account_a)
            .bind(provider_a)
            .bind(model_id)
            .execute(fixture.pool())
            .await;
        assert_eq!(
            mismatched_model_provider
                .expect_err("binding provider must own the model")
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("23503")
        );

        let mismatched_account_provider = sqlx::query(INSERT_BINDING)
            .bind(Uuid::now_v7())
            .bind("query_answer")
            .bind(account_a)
            .bind(provider_b)
            .bind(model_id)
            .execute(fixture.pool())
            .await;
        assert_eq!(
            mismatched_account_provider
                .expect_err("binding provider must own the account")
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("23503")
        );

        let unsupported_purpose = sqlx::query(INSERT_BINDING)
            .bind(Uuid::now_v7())
            .bind("extract_graph")
            .bind(account_b)
            .bind(provider_b)
            .bind(model_id)
            .execute(fixture.pool())
            .await;
        assert_eq!(
            unsupported_purpose
                .expect_err("model purpose relation must remain authoritative")
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("23503")
        );

        sqlx::query(INSERT_BINDING)
            .bind(Uuid::now_v7())
            .bind("query_answer")
            .bind(account_b)
            .bind(provider_b)
            .bind(model_id)
            .execute(fixture.pool())
            .await
            .context("valid same-provider binding was rejected")?;

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// A checksum comparison cannot see a missing column or constraint, so
/// readiness has to look for each baseline object itself. The reachable failure
/// is a hand-stamped checksum on a database that never took all additive DDL.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn migration_state_rejects_a_current_checksum_without_its_baseline_object() -> Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for baseline object presence test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect baseline object presence test postgres")?;

    let result = async {
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply fresh baseline")?;

        let current_checksum: Vec<u8> =
            sqlx::query_scalar("select checksum from _sqlx_migrations where version = 1")
                .fetch_one(&pool)
                .await?;
        sqlx::query("update _sqlx_migrations set checksum = $1 where version = 1")
            .bind(vec![0_u8; 48])
            .execute(&pool)
            .await?;
        let checksum_error = ironrag_backend::services::maintenance::service_identity_schema::migrate_integration_provisioning_schema(
            &pool, true,
        )
        .await
        .expect_err("an unrelated historical checksum must not be stamped as current");
        assert!(
            format!("{checksum_error:#}").contains("exact integration-provisioning predecessor")
        );
        sqlx::query("update _sqlx_migrations set checksum = $1 where version = 1")
            .bind(current_checksum)
            .execute(&pool)
            .await?;

        ironrag_backend::infra::persistence::validate_postgres_migration_state(&pool)
            .await
            .context("a fresh baseline must pass migration state validation")?;

        for (ddl, expected_object) in [
            (
                "alter table public.ops_async_operation drop column result_json",
                "ops_async_operation.result_json",
            ),
            (
                "alter table public.ops_async_operation drop column failure_disposition",
                "ops_async_operation.failure_disposition",
            ),
            (
                "alter table public.catalog_workspace drop column provisioned_by_principal_id cascade",
                "catalog_workspace.provisioned_by_principal_id",
            ),
            (
                "alter table public.catalog_library drop constraint catalog_library_provisioned_by_principal_id_fkey",
                "catalog_library provisioning-owner foreign key",
            ),
            (
                "alter table public.iam_service_identity drop constraint iam_service_identity_provisioning_authority_check",
                "iam_service_identity provisioning-authority constraint",
            ),
            (
                "drop index public.idx_catalog_library_provisioning_owner",
                "catalog_library provisioning-owner index",
            ),
            (
                "drop function public.prevent_catalog_provisioning_owner_change() cascade",
                "prevent_catalog_provisioning_owner_change function",
            ),
            (
                "drop trigger trg_catalog_workspace_provisioning_owner_immutable on public.catalog_workspace",
                "catalog_workspace provisioning-owner immutability trigger",
            ),
            (
                "alter type public.ai_binding_purpose rename value 'query_rerank' to 'query_rerank_absent'",
                "ai_binding_purpose query_rerank",
            ),
        ] {
            let mut transaction = pool.begin().await?;
            sqlx::query(sqlx::AssertSqlSafe(ddl))
                .execute(&mut *transaction)
                .await
                .with_context(|| format!("failed to remove baseline object {expected_object}"))?;
            let error = ironrag_backend::infra::persistence::validate_baseline_objects_present_on(
                transaction.as_mut(),
            )
            .await
            .expect_err("readiness must refuse an absent baseline object");
            transaction.rollback().await?;
            let message = format!("{error:#}");
            assert!(
                message.contains(expected_object),
                "the failure must name {expected_object}, got: {message}"
            );
        }
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// The upgrade runs against a live database, so every lock it waits for also
/// queues the statements that arrive behind it: one `ALTER TABLE` parked in
/// front of `iam_service_identity` stalls authentication for the whole
/// instance. Waiting without a bound turns a maintenance command into an
/// outage, which is why it has to give up and let the operator pick a quieter
/// moment. Measured in piping-eu on 2026-09-12 (BOT-24): the run queued behind
/// a long delete, and its backend kept the advisory lock for ~25 minutes after
/// its pod was gone.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn integration_provisioning_upgrade_gives_up_rather_than_queue_behind_a_held_lock()
-> Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for lock-wait bound test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect lock-wait bound test postgres")?;

    let result = async {
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("failed to apply fresh baseline")?;

        // An independent session holding the command's own advisory lock is the
        // observed shape: a second operator run, or an orphaned backend left by
        // a killed pod.
        let blocker = PgPoolOptions::new()
            .max_connections(1)
            .connect(&temp_database.database_url)
            .await
            .context("failed to connect the blocking session")?;
        sqlx::query("select pg_advisory_lock(902093)").execute(&blocker).await?;

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            ironrag_backend::services::maintenance::service_identity_schema::migrate_integration_provisioning_schema(
                &pool, false,
            ),
        )
        .await
        .context("the upgrade kept waiting on a held lock instead of giving up")?;

        let error = outcome.expect_err("a lock held by another session must surface as an error");
        let message = format!("{error:#}");
        assert!(
            message.contains("lock timeout"),
            "the failure must name the lock timeout so the operator knows to retry later, got: {message}"
        );

        sqlx::query("select pg_advisory_unlock(902093)").execute(&blocker).await?;

        // The ownership assignment takes `ACCESS EXCLUSIVE` on both catalog
        // tables, so it carries the same outage risk and needs the same bound.
        sqlx::query("select pg_advisory_lock(902094)").execute(&blocker).await?;
        let ownership_outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            ironrag_backend::services::maintenance::service_identity_schema::assign_integration_provisioning_ownership(
                &pool,
                false,
                Uuid::now_v7(),
                Uuid::now_v7(),
                vec![Uuid::now_v7()],
            ),
        )
        .await
        .context("the ownership assignment kept waiting on a held lock instead of giving up")?;
        let ownership_message =
            format!("{:#}", ownership_outcome.expect_err("a held lock must surface as an error"));
        assert!(
            ownership_message.contains("lock timeout"),
            "the failure must name the lock timeout, got: {ownership_message}"
        );

        sqlx::query("select pg_advisory_unlock(902094)").execute(&blocker).await?;
        blocker.close().await;
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// The shipped baseline carries exactly one more `ai_binding_purpose` value
/// than the revision a live database records before this upgrade: `query_rerank`.
/// This test reproduces that predecessor state byte for byte (it is the file
/// digest `PREDECESSOR_BASELINE_SHA384` certifies), runs the operator command in
/// dry-run and apply mode, and proves the recorded checksum moves with the
/// schema. A mock would prove neither the real DDL nor the real compare-and-set.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn query_rerank_binding_purpose_upgrade_adds_enum_value() -> Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for query-rerank binding purpose upgrade test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect query-rerank binding purpose upgrade test postgres")?;

    async fn enum_has_query_rerank(pool: &PgPool) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, bool>(
            "select exists (
                 select 1 from pg_enum
                 where enumtypid = 'public.ai_binding_purpose'::regtype
                   and enumlabel = 'query_rerank'
             )",
        )
        .fetch_one(pool)
        .await?)
    }

    /// The live `ai_binding_purpose` labels in the order the runtime consumes
    /// them: `order by binding_purpose` follows `pg_enum.enumsortorder`.
    async fn binding_purpose_enum_order(pool: &PgPool) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "select enumlabel::text from pg_enum
             where enumtypid = 'public.ai_binding_purpose'::regtype
             order by enumsortorder",
        )
        .fetch_all(pool)
        .await?)
    }

    let result = async {
        // This delta moves the IRG-173 revision to the IRG-163 revision; the
        // later IRG-182 decision-catalog values belong to the next delta, so
        // the target is the shipped baseline without them.
        let baseline =
            without_decision_catalog_additions();
        let predecessor = irg173_revision_baseline();
        let predecessor_checksum = Sha384::digest(predecessor.as_bytes());
        assert_eq!(
            hex::encode(predecessor_checksum),
            ironrag_backend::services::maintenance::ai_binding_purpose_schema::PREDECESSOR_BASELINE_SHA384,
        );
        let binding_purpose_enum = predecessor
            .split_once("CREATE TYPE public.ai_binding_purpose AS ENUM (")
            .and_then(|(_, suffix)| suffix.split_once(");"))
            .map(|(body, _)| body)
            .expect("the predecessor must declare the ai_binding_purpose enum");
        assert!(
            !binding_purpose_enum.contains("'query_rerank'"),
            "the predecessor binding-purpose enum must not carry query_rerank"
        );

        // The predecessor is built from the compiled-in baseline, never user
        // input; `AssertSqlSafe` is how sqlx 0.9 accepts a non-`'static` string.
        sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
            .execute(&pool)
            .await
            .context("failed to apply the query-rerank predecessor baseline")?;

        sqlx::query(
            "create table _sqlx_migrations (
                 version bigint primary key,
                 description text not null,
                 installed_on timestamptz not null default now(),
                 success boolean not null,
                 checksum bytea not null,
                 execution_time bigint not null
             )",
        )
        .execute(&pool)
        .await
        .context("failed to create the sqlx migration bookkeeping table")?;
        sqlx::query(
            "insert into _sqlx_migrations (version, description, success, checksum, execution_time)
             values (1, 'init', true, decode($1, 'hex'), 0)",
        )
        .bind(hex::encode(predecessor_checksum))
        .execute(&pool)
        .await
        .context("failed to record the predecessor baseline checksum")?;

        // The runtime orders rows by the database enum, so the live upgrade must
        // leave the labels in the same order a fresh install declares. Pin both
        // paths to the consumer's canonical order, not to each other.
        let consumer_order: Vec<String> = AiBindingPurpose::ALL
            .iter()
            .map(|purpose| purpose.as_str().to_string())
            .filter(|label| label != "graph_entity_type")
            .collect();
        let mut predecessor_order = consumer_order.clone();
        predecessor_order.retain(|label| label != "query_rerank");
        assert!(
            !enum_has_query_rerank(&pool).await?,
            "the predecessor baseline must not carry the query_rerank enum value"
        );
        assert_eq!(
            binding_purpose_enum_order(&pool).await?,
            predecessor_order,
            "the predecessor enum order must be the consumer order without query_rerank",
        );

        let dry_run = ironrag_backend::services::maintenance::ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(
            &pool, false,
        )
        .await
        .context("dry-run of the query-rerank binding purpose upgrade failed")?;
        assert!(!dry_run.already_current);
        assert!(!dry_run.schema_applied);
        assert!(!enum_has_query_rerank(&pool).await?, "a dry run must not change the schema");

        let applied = ironrag_backend::services::maintenance::ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(
            &pool, true,
        )
        .await
        .context("failed to apply the query-rerank binding purpose upgrade")?;
        assert!(applied.schema_applied);
        assert!(!applied.already_current);
        assert!(
            enum_has_query_rerank(&pool).await?,
            "the applied upgrade must add the query_rerank enum value"
        );
        assert_eq!(
            binding_purpose_enum_order(&pool).await?,
            consumer_order,
            "the live upgrade must keep the enum in the consumer order, not append",
        );
        let recorded_checksum: Vec<u8> =
            sqlx::query_scalar("select checksum from _sqlx_migrations where version = 1")
                .fetch_one(&pool)
                .await?;
        assert_eq!(recorded_checksum, Sha384::digest(baseline.as_bytes()).to_vec());

        let idempotent = ironrag_backend::services::maintenance::ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(
            &pool, true,
        )
        .await
        .context("idempotent re-run of the query-rerank binding purpose upgrade failed")?;
        assert!(idempotent.already_current);
        assert!(!idempotent.schema_applied);
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// The shipped baseline carries exactly two more enum values than the revision
/// a live database records before the decision-catalog upgrade:
/// `graph_entity_type` on `ai_binding_purpose` and `typesafe_system_one` on
/// `ai_provider_api_style`. This test reproduces that predecessor state byte for
/// byte (it is the file digest `PREDECESSOR_BASELINE_SHA384` certifies), runs
/// the operator command in dry-run and apply mode, and proves both enum orders
/// and the recorded checksum move with the schema. A mock would prove neither
/// the real DDL nor the real compare-and-set.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn decision_catalog_schema_upgrade_adds_both_enum_values_in_consumer_order() -> Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for decision-catalog schema upgrade test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect decision-catalog schema upgrade test postgres")?;

    let result = async {
        let predecessor = baseline_at_query_rerank_revision();
        sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
            .execute(&pool)
            .await
            .context("failed to apply the decision-catalog predecessor baseline")?;
        record_baseline_checksum(
            &pool,
            ironrag_backend::services::maintenance::decision_catalog_schema::PREDECESSOR_BASELINE_SHA384,
        )
        .await?;

        // The runtime orders rows by the database enum, so the live upgrade must
        // leave the labels in the same order a fresh install declares. Pin both
        // paths to the consumer's canonical order, not to each other.
        let consumer_binding_order: Vec<String> = AiBindingPurpose::ALL
            .iter()
            .map(|purpose| purpose.as_str().to_string())
            .collect();
        let mut predecessor_binding_order = consumer_binding_order.clone();
        predecessor_binding_order.retain(|label| label != "graph_entity_type");
        // `ai_provider_api_style` has no exported contract enum; its consumer is
        // the provider runtime-kind resolver, whose four values are a frozen
        // contract. The order below is the baseline's declaration order.
        let consumer_api_style_order: Vec<String> = [
            "openai_compatible",
            "anthropic_messages",
            "gemini_generate_content",
            "typesafe_system_one",
        ]
        .iter()
        .map(|style| (*style).to_string())
        .collect();
        let mut predecessor_api_style_order = consumer_api_style_order.clone();
        predecessor_api_style_order.retain(|label| label != "typesafe_system_one");

        assert!(
            !binding_purpose_has_graph_entity_type(&pool).await?,
            "the predecessor baseline must not carry the graph_entity_type enum value"
        );
        assert!(
            !provider_api_style_has_typesafe_system_one(&pool).await?,
            "the predecessor baseline must not carry the typesafe_system_one enum value"
        );
        assert_eq!(
            binding_purpose_enum_order(&pool).await?,
            predecessor_binding_order,
            "the predecessor enum order must be the consumer order without graph_entity_type",
        );
        assert_eq!(
            provider_api_style_enum_order(&pool).await?,
            predecessor_api_style_order,
            "the predecessor enum order must be the consumer order without typesafe_system_one",
        );

        let dry_run = ironrag_backend::services::maintenance::decision_catalog_schema::migrate_decision_catalog_schema(
            &pool, false,
        )
        .await
        .context("dry-run of the decision-catalog schema upgrade failed")?;
        assert!(!dry_run.already_current);
        assert!(!dry_run.schema_applied);
        assert!(
            !binding_purpose_has_graph_entity_type(&pool).await?,
            "a dry run must not change the schema"
        );
        assert!(
            !provider_api_style_has_typesafe_system_one(&pool).await?,
            "a dry run must not change the schema"
        );

        let applied = ironrag_backend::services::maintenance::decision_catalog_schema::migrate_decision_catalog_schema(
            &pool, true,
        )
        .await
        .context("failed to apply the decision-catalog schema upgrade")?;
        assert!(applied.schema_applied);
        assert!(!applied.already_current);
        assert!(
            binding_purpose_has_graph_entity_type(&pool).await?,
            "the applied upgrade must add the graph_entity_type enum value"
        );
        assert!(
            provider_api_style_has_typesafe_system_one(&pool).await?,
            "the applied upgrade must add the typesafe_system_one enum value"
        );
        assert_eq!(
            binding_purpose_enum_order(&pool).await?,
            consumer_binding_order,
            "the live upgrade must keep ai_binding_purpose in the consumer order, not append",
        );
        assert_eq!(
            provider_api_style_enum_order(&pool).await?,
            consumer_api_style_order,
            "the live upgrade must keep ai_provider_api_style in the consumer order, not append",
        );
        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(
                ironrag_backend::services::maintenance::baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384
            )?,
            "the applied upgrade must stamp its own frozen target revision",
        );

        let idempotent = ironrag_backend::services::maintenance::decision_catalog_schema::migrate_decision_catalog_schema(
            &pool, true,
        )
        .await
        .context("idempotent re-run of the decision-catalog schema upgrade failed")?;
        assert!(idempotent.already_current);
        assert!(!idempotent.schema_applied);
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// A pre-applied decision-catalog value under a stale checksum is refused by
/// dry-run exactly as by apply: the `delta_present` predicate is satisfied,
/// the checksum is stale, and the delta refuses rather than stamping. It never
/// touches the pre-applied DDL or the stale checksum.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn pre_applied_decision_catalog_value_with_a_stale_checksum_is_refused_in_both_modes()
-> Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for pre-applied decision-catalog test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect pre-applied decision-catalog test postgres")?;

    let result = async {
        let predecessor = baseline_at_query_rerank_revision();
        sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
            .execute(&pool)
            .await
            .context("failed to apply the decision-catalog predecessor baseline")?;
        // Outside an explicit transaction, so both values are committed and the
        // delta's own transaction sees them as pre-applied DDL.
        sqlx::raw_sql(include_str!("../../ops/apply-decision-catalog-schema.sql"))
            .execute(&pool)
            .await
            .context("failed to pre-apply the decision-catalog enum values")?;
        assert!(binding_purpose_has_graph_entity_type(&pool).await?);
        assert!(provider_api_style_has_typesafe_system_one(&pool).await?);
        record_baseline_checksum(
            &pool,
            ironrag_backend::services::maintenance::decision_catalog_schema::PREDECESSOR_BASELINE_SHA384,
        )
        .await?;

        for apply in [false, true] {
            let error = ironrag_backend::services::maintenance::decision_catalog_schema::migrate_decision_catalog_schema(
                &pool, apply,
            )
            .await
            .expect_err("pre-applied DDL under a stale checksum must be refused");
            let message = format!("{error:#}");
            assert!(
                message.contains("already present with a stale baseline checksum"),
                "the refusal must say the DDL is pre-applied (apply={apply}), got: {message}"
            );
        }
        assert!(
            binding_purpose_has_graph_entity_type(&pool).await?,
            "a refusal must leave the pre-applied DDL in place"
        );
        assert!(
            provider_api_style_has_typesafe_system_one(&pool).await?,
            "a refusal must leave the pre-applied DDL in place"
        );
        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(
                ironrag_backend::services::maintenance::decision_catalog_schema::PREDECESSOR_BASELINE_SHA384
            )?,
            "a refusal must leave the stale checksum untouched",
        );
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// SHA-384 of the baseline revision at IRG-75 (`b092f2f4`): the predecessor
/// the integration-provisioning delta accepts, and the revision the live
/// database started from.
const IRG75_BASELINE_SHA384_HEX: &str = "8396274aa8c5d8af9f6d87d5622660fb69755dceb86476fc0fffab4475ae10898dc7be7df5f80a8f80739e7fb3614abe";
/// SHA-384 of the baseline revision after the integration-provisioning delta
/// (`c3df7ba7`/`f7202d8b`); its target.
const PROVISIONING_BASELINE_SHA384_HEX: &str = "076b343d29f7938cefe595ddba393065024270f81b3908d552c2021def71e38d52ac4e95dcb53756fa9201a961767264";
/// SHA-384 of the baseline revision after the IRG-112 FK indexes
/// (`efdc413a`); the predecessor the ops-failure-disposition delta accepts.
const IRG112_BASELINE_SHA384_HEX: &str =
    ironrag_backend::services::maintenance::baseline_delta::BASELINE_REVISION_IRG112_SHA384;
/// SHA-384 of the baseline revision the IRG-173 `failure_disposition` delta
/// produces (`1308ab74`); the predecessor the query-rerank delta accepts. It is
/// the same digest the public `PREDECESSOR_BASELINE_SHA384` certifies.
const IRG173_BASELINE_SHA384_HEX: &str =
    ironrag_backend::services::maintenance::ai_binding_purpose_schema::PREDECESSOR_BASELINE_SHA384;

/// The three IRG-112 `ON DELETE SET NULL` indexes, exactly as the baseline
/// declares them (the non-concurrent form the in-process test can run).
const IRG112_INDEX_DDL: [&str; 3] = [
    "CREATE INDEX idx_knowledge_chunk_primary_block_fk ON public.knowledge_chunk USING btree (primary_block_id) WHERE (primary_block_id IS NOT NULL)",
    "CREATE INDEX idx_knowledge_evidence_block_fk ON public.knowledge_evidence USING btree (block_id) WHERE (block_id IS NOT NULL)",
    "CREATE INDEX idx_knowledge_structured_block_parent_block_fk ON public.knowledge_structured_block USING btree (parent_block_id) WHERE (parent_block_id IS NOT NULL)",
];

/// Frozen predecessor bytes certify the immutable upgrade contract independently
/// of the current fresh baseline's dump formatting. This fixture is not a runtime
/// migration or a second fresh-install authority.
fn visibility_policy_predecessor() -> &'static str {
    let predecessor = include_str!("../fixtures/schema/admission_visibility_policy.sql");
    assert_eq!(hex::encode(Sha384::digest(predecessor.as_bytes())),
        ironrag_backend::services::maintenance::baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384);
    predecessor
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn visibility_contract_fresh_baseline_and_current_objects() -> Result<()> {
    use ironrag_backend::services::maintenance::content_visibility_intent_schema::migrate_content_visibility_intent_schema;
    let settings = Settings::from_env()?;
    let (_storage_root, storage) = visibility_migration_storage(&settings)?;
    let database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    let result = async {
        sqlx::migrate!("./migrations").run(&pool).await?;
        persistence::validate_baseline_objects_present(&pool).await?;
        assert_retired_visibility_policy_absent(&pool).await?;
        for apply in [false, true] {
            let report = migrate_content_visibility_intent_schema(&pool, apply, &storage, None).await?;
            assert!(report.already_current && !report.schema_applied);
        }
        // Historical CLI reruns validate the recorded final certificate, not
        // the retired policy objects, and cannot stamp or recreate that policy.
        let final_checksum = baseline_checksum(&pool).await?;
        for apply in [false, true] {
            let report = ironrag_backend::services::maintenance::admission_visibility_policy_schema::migrate_admission_visibility_policy_schema(&pool, apply).await?;
            assert!(report.already_current && !report.schema_applied);
            assert_eq!(baseline_checksum(&pool).await?, final_checksum);
            assert_retired_visibility_policy_absent(&pool).await?;
        }
        sqlx::query("alter table content_mutation disable trigger content_mutation_request_identity_immutable").execute(&pool).await?;
        let rejected = migrate_content_visibility_intent_schema(&pool, true, &storage, None).await.expect_err("disabled identity boundary cannot be stamped");
        assert!(rejected.to_string().contains("immutable semantic identity trigger"));
        for apply in [false, true] {
            let rejected = ironrag_backend::services::maintenance::admission_visibility_policy_schema::migrate_admission_visibility_policy_schema(&pool, apply).await.expect_err("historical CLI cannot accept an invalid recorded current certificate");
            assert!(rejected.to_string().contains("immutable semantic identity trigger"));
            assert_eq!(baseline_checksum(&pool).await?, final_checksum);
            assert_retired_visibility_policy_absent(&pool).await?;
        }
        sqlx::query("alter table content_mutation enable trigger content_mutation_request_identity_immutable").execute(&pool).await?;
        persistence::validate_baseline_objects_present(&pool).await?;
        let view: String = sqlx::query_scalar("select pg_get_viewdef('public.content_current_revision'::regclass)").fetch_one(&pool).await?;
        let view_body = view.trim().trim_end_matches(';');
        let function: String = sqlx::query_scalar("select pg_get_functiondef('public.preserve_content_request_identity()'::regprocedure)").fetch_one(&pool).await?;
        for (drift, restore) in [
            (format!("create or replace view public.content_current_revision as select * from ({view_body}) canonical where false"), format!("create or replace view public.content_current_revision as {view_body}")),
            ("create or replace function public.preserve_content_request_identity() returns trigger language plpgsql as $$ begin return NEW; end; $$".to_owned(), function),
        ] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(drift)).execute(&pool).await?;
            for apply in [false, true] {
                let refused = migrate_content_visibility_intent_schema(&pool, apply, &storage, None).await.expect_err("same-shape view/function drift cannot pass a name-only certificate");
                assert!(refused.to_string().contains("current-view certificate"));
                assert_eq!(baseline_checksum(&pool).await?, final_checksum);
            }
            sqlx::raw_sql(sqlx::AssertSqlSafe(restore)).execute(&pool).await?;
            persistence::validate_baseline_objects_present(&pool).await?;
        }
        Ok(())
    }.await;
    pool.close().await;
    database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn visibility_contract_exact_predecessor_upgrade_and_refusals() -> Result<()> {
    use ironrag_backend::services::maintenance::{
        baseline_delta, content_visibility_intent_schema::migrate_content_visibility_intent_schema,
    };
    let settings = Settings::from_env()?;
    let (_storage_root, storage) = visibility_migration_storage(&settings)?;
    let database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    let result = async {
        sqlx::raw_sql(visibility_policy_predecessor()).execute(&pool).await?;
        // Record the exact frozen fixture checksum, not the current source checksum.
        sqlx::query("create table _sqlx_migrations (version bigint primary key, description text not null, installed_on timestamptz not null default now(), success boolean not null, checksum bytea not null, execution_time bigint not null)").execute(&pool).await?;
        sqlx::query("insert into _sqlx_migrations(version,description,success,checksum,execution_time) values (1,'baseline v1',true,$1,0)")
            .bind(hex::decode(baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384)?).execute(&pool).await?;
        let pending = migrate_content_visibility_intent_schema(&pool, false, &storage, None).await?;
        assert!(!pending.already_current && !pending.schema_applied);
        let (document_id, item_id, attempt_id, admission) = seed_visibility_predecessor_publication(&pool, &storage).await?;
        let original_readable: Option<Uuid> = sqlx::query_scalar("select readable_revision_id from content_document_head where document_id=$1")
            .bind(document_id).fetch_one(&pool).await?;
        for apply in [false, true] {
            migrate_content_visibility_intent_schema(&pool, apply, &storage, None).await
                .expect_err("readable predecessor requires an explicit certificate even in dry-run");
        }
        sqlx::query("update content_document_head set document_summary='certified historical source summary' where document_id=$1")
            .bind(document_id).execute(&pool).await?;
        for (break_sql, repair_sql) in [
            ("update ingest_job set queue_state='queued'", "update ingest_job set queue_state='completed'"),
            ("update ingest_attempt set attempt_state='leased'", "update ingest_attempt set attempt_state='succeeded'"),
            ("update content_mutation set mutation_state='accepted'", "update content_mutation set mutation_state='applied'"),
            ("update content_mutation set idempotency_key='unreservable'", "update content_mutation set idempotency_key=null"),
            ("update content_mutation_item set item_state='pending'", "update content_mutation_item set item_state='applied'"),
            ("update ingest_job set mutation_item_id=null", "update ingest_job set mutation_item_id=(select id from content_mutation_item limit 1)"),
            ("update content_mutation set visibility_policy='suppressed_until_ready'; update content_document_head set readable_revision_id=null", "update content_mutation set visibility_policy='readable'; update content_document_head set readable_revision_id=(select id from content_revision limit 1)"),
        ] {
            sqlx::raw_sql(break_sql).execute(&pool).await?;
            for apply in [false,true] {
                let rejected = migrate_content_visibility_intent_schema(&pool,apply, &storage, Some(&admission)).await.expect_err(&format!("incoherent upgrade must reject: {break_sql}"));
                assert!(rejected.to_string().contains("preflight refuses"), "{rejected:#}");
                let checksum: Vec<u8> = sqlx::query_scalar("select checksum from _sqlx_migrations where version=1").fetch_one(&pool).await?;
                assert_eq!(hex::encode(checksum),baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384);
                let intent: Option<String> = sqlx::query_scalar("select to_regclass('public.content_visibility_intent')::text").fetch_one(&pool).await?;
                assert!(intent.is_none(), "reject must not apply partial DDL");
            }
            sqlx::raw_sql(repair_sql).execute(&pool).await?;
        }
        // Operational owner history is not certified publication authority.
        // Missing attempts and duplicate applied history cannot invalidate an
        // exact revision/blob certificate or mutate the predecessor in dry-run.
        for (change_sql, restore_sql) in [
            ("update content_document_head set latest_successful_attempt_id=null", "update content_document_head set latest_successful_attempt_id=(select id from ingest_attempt limit 1)"),
            ("insert into content_mutation_item(mutation_id,document_id,result_revision_id,item_state,message) select mutation_id,document_id,result_revision_id,item_state,'ambiguous' from content_mutation_item limit 1", "delete from content_mutation_item where message='ambiguous'"),
        ] {
            sqlx::raw_sql(change_sql).execute(&pool).await?;
            let pending = migrate_content_visibility_intent_schema(&pool, false, &storage, Some(&admission)).await?;
            assert!(!pending.already_current && !pending.schema_applied);
            assert_eq!(hex::encode(baseline_checksum(&pool).await?), baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384);
            sqlx::raw_sql(restore_sql).execute(&pool).await?;
        }
        let unknown_checksum = "00".repeat(48);
        for wrong_checksum in [baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384, unknown_checksum.as_str()] {
            sqlx::query("update _sqlx_migrations set checksum=$1 where version=1")
                .bind(hex::decode(wrong_checksum)?).execute(&pool).await?;
            for apply in [false, true] {
                let wrong = migrate_content_visibility_intent_schema(&pool, apply, &storage, Some(&admission)).await.expect_err("earlier or unknown predecessor");
                assert!(wrong.to_string().contains("exact content-visibility-intent predecessor"));
                assert_eq!(hex::encode(baseline_checksum(&pool).await?), wrong_checksum);
                let intent: Option<String> = sqlx::query_scalar("select to_regclass('public.content_visibility_intent')::text").fetch_one(&pool).await?;
                assert!(intent.is_none(), "wrong checksum refusal cannot apply DDL");
            }
        }
        sqlx::query("update _sqlx_migrations set checksum=$1 where version=1").bind(hex::decode(baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384)?).execute(&pool).await?;
        // A missing predecessor object must fail even in dry-run mode.
        sqlx::query("alter table content_mutation drop column visibility_policy").execute(&pool).await?;
        for apply in [false,true] {
            let error = migrate_content_visibility_intent_schema(&pool, apply, &storage, Some(&admission)).await.expect_err("missing prerequisite");
            assert!(error.to_string().contains("visibility_policy"));
            let historical_error = ironrag_backend::services::maintenance::admission_visibility_policy_schema::migrate_admission_visibility_policy_schema(&pool, apply).await.expect_err("historical target checksum still requires its own policy certificate");
            assert!(historical_error.to_string().contains("visibility_policy"));
            assert_eq!(hex::encode(baseline_checksum(&pool).await?), baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384);
        }
        sqlx::query("alter table content_mutation add column visibility_policy admission_visibility_policy not null default 'readable'").execute(&pool).await?;
        let applied = migrate_content_visibility_intent_schema(&pool, true, &storage, Some(&admission)).await?;
        assert!(applied.schema_applied && !applied.already_current);
        persistence::validate_baseline_objects_present(&pool).await?;
        assert_retired_visibility_policy_absent(&pool).await?;
        let summary: Option<String> = sqlx::query_scalar("select document_summary from content_document_head where document_id=$1")
            .bind(document_id).fetch_one(&pool).await?;
        assert_eq!(summary.as_deref(), Some("certified historical source summary"), "certification must preserve existing head metadata");
        let (published_revision, floor): (Option<Uuid>, i32) = sqlx::query_as("select h.readable_revision_id,d.revision_sequence from content_document_head h join content_document d on d.id=h.document_id where d.id=$1")
            .bind(document_id).fetch_one(&pool).await?;
        assert_eq!(published_revision, original_readable, "certification must preserve the exact readable revision");
        assert!(published_revision.is_some());
        assert_eq!(floor,7);
        let retained_attempt: Option<Uuid> = sqlx::query_scalar("select latest_successful_attempt_id from content_document_head where document_id=$1").bind(document_id).fetch_one(&pool).await?;
        assert_eq!(retained_attempt, Some(attempt_id), "import must retain genuine historical attempt pointers");
        let retained_history: bool = sqlx::query_scalar("select exists(select 1 from content_mutation_item i join content_mutation_predecessor_audit a on a.id=i.mutation_id join content_mutation_reference r on r.id=a.id join ingest_job j on j.mutation_id=r.id and j.mutation_item_id=i.id join ingest_attempt attempt on attempt.job_id=j.id where i.id=$1 and attempt.id=$2 and a.request_fingerprint is null and a.predecessor_baseline_sha384=decode($3,'hex'))")
            .bind(item_id).bind(attempt_id).bind(baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384).fetch_one(&pool).await?;
        assert!(retained_history, "historical audit, common item and job/attempt must be retained exactly");
        let fresh = TempDatabase::create(&settings.database_url).await?;
        let fresh_pool = PgPoolOptions::new().max_connections(2).connect(&fresh.database_url).await?;
        let comparison = async {
            sqlx::migrate!("./migrations").run(&fresh_pool).await?;
            assert_retired_visibility_policy_absent(&fresh_pool).await?;
            assert_eq!(visibility_schema_catalog(&pool).await?,visibility_schema_catalog(&fresh_pool).await?,
                "registered hop and fresh authority must create identical database objects");
            // Run the same filename/READY boundary matrix against both catalogs.
            for catalog in [&pool, &fresh_pool] {
                sqlx::raw_sql(include_str!("../fixtures/schema/content_visibility_filename.sql"))
                    .execute(catalog).await?;
            }
            Ok::<(),anyhow::Error>(())
        }.await;
        fresh_pool.close().await;
        fresh.drop().await?;
        comparison?;
        let missing_identity = sqlx::query("insert into content_mutation(workspace_id,library_id,operation_kind,mutation_state) select workspace_id,library_id,'replace','applied' from content_document where id=$1")
            .bind(document_id).execute(&pool).await.expect_err("new writers cannot omit semantic identity");
        assert_eq!(missing_identity.as_database_error().and_then(|error|error.code()).as_deref(),Some("23514"));
        let immutable = sqlx::query("update content_mutation_predecessor_audit set request_fingerprint='invented'").execute(&pool).await.expect_err("historic audit cannot be rewritten as SAFE");
        assert_eq!(immutable.as_database_error().and_then(|error|error.code()).as_deref(),Some("23514"));
        let current = migrate_content_visibility_intent_schema(&pool, true, &storage, None).await?;
        assert!(current.already_current && !current.schema_applied);
        sqlx::query("update _sqlx_migrations set checksum=$1 where version=1")
            .bind(hex::decode(baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384)?).execute(&pool).await?;
        let duplicate = migrate_content_visibility_intent_schema(&pool,true, &storage, Some(&admission)).await.expect_err("duplicate objects under old checksum");
        assert!(duplicate.to_string().contains("visibility_policy"),
            "retired target objects under a stale predecessor checksum lack the certified policy prerequisite: {duplicate:#}");
        let recorded: Vec<u8> = sqlx::query_scalar("select checksum from _sqlx_migrations where version=1").fetch_one(&pool).await?;
        assert_eq!(hex::encode(recorded), baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384);
        Ok(())
    }.await;
    pool.close().await;
    database.drop().await?;
    result
}

/// Current visibility depends on immutable replacement snapshots, not labels.
async fn assert_retired_visibility_policy_absent(pool: &PgPool) -> Result<()> {
    let absent: bool = sqlx::query_scalar("select to_regtype('public.admission_visibility_policy') is null and not exists (select 1 from information_schema.columns where table_schema='public' and table_name='content_mutation' and column_name='visibility_policy')")
        .fetch_one(pool).await?;
    assert!(absent, "current schema cannot retain the retired policy enum or column");
    Ok(())
}

/// Current visibility depends on immutable replacement snapshots, not labels.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn visibility_contract_immutable_filename_survives_label_rename() -> Result<()> {
    let settings = Settings::from_env()?;
    let database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(2).connect(&database.database_url).await?;
    let result = async {
        sqlx::migrate!("./migrations").run(&pool).await?;
        sqlx::raw_sql(include_str!("../fixtures/schema/content_visibility_filename.sql"))
            .execute(&pool)
            .await?;
        Ok(())
    }
    .await;
    pool.close().await;
    database.drop().await?;
    result
}

/// Seeds exact old-schema lineage without inventing a new request identity.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn visibility_contract_mutation_reference_is_exclusive_and_reserves_historical_keys()
-> Result<()> {
    use ironrag_backend::infra::repositories::{
        admission_repository as admission, content_mutation_repository as mutations,
    };
    let settings = Settings::from_env()?;
    let database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    let result = async {
        sqlx::migrate!("./migrations").run(&pool).await?;
        sqlx::raw_sql(include_str!("../fixtures/schema/content_mutation_reference.sql")).execute(&pool).await?;
        let (historical, workspace, library): (Uuid,Uuid,Uuid) = sqlx::query_as("select id,workspace_id,library_id from content_mutation_predecessor_audit").fetch_one(&pool).await?;
        assert!(mutations::get_mutation_by_id(&pool,historical).await?.is_none(), "historical receipt is not a modern semantic replay");
        assert!(admission::has_bound_content_key(&pool,workspace,None,"rest",Some("retained-real-key")).await?);
        let request = admission::ContentAdmissionRequest {
            workspace_id:workspace,library_id:library,operation_kind:"upload".to_owned(),
            requested_by_principal_id:None,request_surface:"rest".to_owned(),idempotency_key:Some("retained-real-key".to_owned()),
            source_identity:None,target:admission::ContentAdmissionTarget::New {
                external_key:Some("must-not-create".to_owned()),file_name:None,parent_external_key:None,
            },revision:None,parent_async_operation_id:None,priority:0,
        };
        let denied = admission::admit_content_with_failpoint(&pool,&request,None).await.expect_err("historical namespace must reject before creating rows");
        assert!(matches!(denied,admission::AdmissionError::IdempotencyConflict{mutation_id} if mutation_id==historical));
        let identity=serde_json::json!({"operationKind":"delete","workspaceId":workspace,"libraryId":library});
        let input=mutations::NewContentMutation {
            request_identity:&identity,workspace_id:workspace,library_id:library,operation_kind:"delete",
            requested_by_principal_id:None,request_surface:"rest",idempotency_key:Some("concurrent-key"),
            source_identity:None,mutation_state:"applied",failure_code:None,conflict_code:None,
        };
        let (left,right)=tokio::join!(mutations::create_mutation(&pool,&input),mutations::create_mutation(&pool,&input));
        assert_ne!(left.is_ok(),right.is_ok(),"one atomic reservation must win");
        let winner=left.as_ref().ok().or_else(||right.as_ref().ok()).expect("one reservation wins").id;
        let error=left.err().or_else(||right.err()).expect("one duplicate reservation fails");
        assert_eq!(error.as_database_error().and_then(|error|error.code()).as_deref(),Some("23505"));
        let counts:(i64,i64)=sqlx::query_as("select count(*),count(m.id) from content_mutation_reference r left join content_mutation m on m.id=r.id where r.idempotency_key='concurrent-key'").fetch_one(&pool).await?;
        assert_eq!(counts,(1,1),"loser cannot leave an orphan reservation");
        let duplicate="insert into content_mutation_predecessor_audit(id,workspace_id,library_id,operation_kind,mutation_state,requested_at,predecessor_baseline_sha384,origin_workspace_id,origin_library_id) values($1,$2,$3,'replace','applied',now(),decode('bc9ee0ed6d3b14c32ced3759377f9323d7944e2e18cebb09f8e841dfaa9a20b123bd3532654b8d4edba8efaf2e4768bb','hex'),$2,$3)";
        let first=sqlx::query(duplicate).bind(winner).bind(workspace).bind(library).execute(&pool);
        let second=sqlx::query(duplicate).bind(winner).bind(workspace).bind(library).execute(&pool);
        let (first,second)=tokio::join!(first,second);
        for attempted in [first,second] {
            let rejected=attempted.expect_err("parent serialization must reject a concurrent second representation");
            assert_eq!(rejected.as_database_error().and_then(|error|error.code()).as_deref(),Some("23514"));
        }
        Ok(())
    }.await;
    pool.close().await;
    database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn visibility_contract_snapshot_text_and_missing_projection_preserve_heads() -> Result<()> {
    use ironrag_backend::services::maintenance::{
        baseline_delta,
        content_visibility_intent_schema::{
            CertifiedImportAdmission, migrate_content_visibility_intent_schema,
        },
    };
    let settings = Settings::from_env()?;
    let (_storage_root, storage) = visibility_migration_storage(&settings)?;
    for retained_knowledge in [true, false] {
        let database = TempDatabase::create(&settings.database_url).await?;
        let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
        let result: Result<()> = async {
            sqlx::raw_sql(visibility_policy_predecessor()).execute(&pool).await?;
            sqlx::query("create table _sqlx_migrations (version bigint primary key, description text not null, installed_on timestamptz not null default now(), success boolean not null, checksum bytea not null, execution_time bigint not null)").execute(&pool).await?;
            sqlx::query("insert into _sqlx_migrations(version,description,success,checksum,execution_time) values (1,'baseline v1',true,$1,0)")
                .bind(hex::decode(baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384)?).execute(&pool).await?;
            let (document, _, _, _) = seed_visibility_predecessor_publication(&pool, &storage).await?;
            let (workspace, library, original_key): (Uuid, Uuid, String) = sqlx::query_as("select workspace_id,library_id,storage_key from content_revision where document_id=$1")
                .bind(document).fetch_one(&pool).await?;
            assert!(storage.delete_revision_source(workspace, library, &original_key).await?);
            sqlx::query("update content_revision set storage_key=null where document_id=$1").bind(document).execute(&pool).await?;
            let text = "retained normalized text, not original file bytes";
            let digest = hex::encode(sha2::Sha256::digest(text.as_bytes()));
            if retained_knowledge {
                sqlx::query("update knowledge_revision set normalized_text=$2,text_checksum=$3 where document_id=$1")
                    .bind(document).bind(text).bind(&digest).execute(&pool).await?;
            } else {
                sqlx::query("delete from knowledge_revision where document_id=$1").bind(document).execute(&pool).await?;
            }
            let head_before: serde_json::Value = sqlx::query_scalar("select to_jsonb(h) from content_document_head h where document_id=$1").bind(document).fetch_one(&pool).await?;
            let revision_before: serde_json::Value = sqlx::query_scalar("select to_jsonb(r) from content_revision r where document_id=$1").bind(document).fetch_one(&pool).await?;
            let evidence: serde_json::Value = sqlx::query_scalar("select jsonb_build_object('kind','snapshot_revision','knowledgeRevisionPresent',k.revision_id is not null,'normalizedTextSha256',$2::text,'textChecksum',k.text_checksum,'textState',k.text_state,'textReadableAt',k.text_readable_at) from content_revision r left join knowledge_revision k on k.revision_id=r.id where r.document_id=$1")
                .bind(document).bind(retained_knowledge.then_some(digest)).fetch_one(&pool).await?;
            let row: serde_json::Value = sqlx::query_scalar("select jsonb_build_object('revisionId',r.id,'documentId',r.document_id,'workspaceId',r.workspace_id,'libraryId',r.library_id,'storageKey',r.storage_key,'sourceEvidence',$2::jsonb,'replacement',jsonb_build_object('checksum',r.checksum,'byteSize',r.byte_size,'mimeType',r.mime_type,'fileName','published.txt','title',r.title,'languageCode',r.language_code,'sourceUri',r.source_uri,'documentHint',r.document_hint)) from content_revision r where r.document_id=$1")
                .bind(document).bind(evidence).fetch_one(&pool).await?;
            let snapshot = "a".repeat(64);
            let baseline = baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384;
            let operator = "snapshot-evidence-integration-fixture";
            let bytes = serde_json::to_vec(&serde_json::json!({"recoverySnapshotId":snapshot,"sourceBaselineSha384":baseline,"admittedBy":operator,"revisions":[row]}))?;
            let root = tempfile::tempdir()?;
            let path = root.path().join("admission.json");
            tokio::fs::write(&path, &bytes).await?;
            let admission = CertifiedImportAdmission::from_file(&path, &hex::encode(sha2::Sha256::digest(&bytes)), &snapshot, baseline, operator).await?;
            for apply in [false, true] {
                let report = migrate_content_visibility_intent_schema(&pool, apply, &storage, Some(&admission)).await?;
                assert_eq!(report.schema_applied, apply);
                let head_preserved: bool = sqlx::query_scalar("select $2::jsonb <@ to_jsonb(h) from content_document_head h where document_id=$1").bind(document).bind(&head_before).fetch_one(&pool).await?;
                let revision_preserved: bool = sqlx::query_scalar("select $2::jsonb <@ to_jsonb(r) from content_revision r where document_id=$1").bind(document).bind(&revision_before).fetch_one(&pool).await?;
                assert!(head_preserved && revision_preserved);
                let knowledge_count: i64 = sqlx::query_scalar("select count(*) from knowledge_revision where document_id=$1").bind(document).fetch_one(&pool).await?;
                assert_eq!(knowledge_count, i64::from(retained_knowledge));
            }
            let public_count: i64 = sqlx::query_scalar("select count(*) from content_current_revision where document_id=$1").bind(document).fetch_one(&pool).await?;
            assert_eq!(public_count, i64::from(retained_knowledge));
            Ok(())
        }.await;
        pool.close().await;
        database.drop().await?;
        result?;
    }
    Ok(())
}

fn visibility_migration_storage(
    settings: &Settings,
) -> Result<(tempfile::TempDir, ironrag_backend::services::content::storage::ContentStorageService)>
{
    let root = tempfile::Builder::new().prefix("visibility-migration-").tempdir()?;
    let mut settings = settings.clone();
    settings.content_storage_provider = "filesystem".to_owned();
    settings.content_storage_topology = "single_node".to_owned();
    settings.content_storage_key_prefix = String::new();
    settings.content_storage_root = root.path().to_string_lossy().into_owned();
    let storage =
        ironrag_backend::services::content::storage::ContentStorageService::from_settings(
            &settings,
        )?;
    Ok((root, storage))
}

/// Seeds exact old-schema lineage and a certificate over real isolated fixture bytes.
async fn seed_visibility_predecessor_publication(
    pool: &sqlx::PgPool,
    storage: &ironrag_backend::services::content::storage::ContentStorageService,
) -> Result<(Uuid, Uuid, Uuid, ironrag_backend::services::maintenance::content_visibility_intent_schema::CertifiedImportAdmission)>{
    let workspace = catalog_repository::create_workspace(
        pool,
        "visibility-upgrade",
        "Visibility upgrade",
        None,
    )
    .await?;
    let library = catalog_repository::create_library(
        pool,
        workspace.id,
        "visibility-upgrade",
        "Visibility upgrade",
        None,
        None,
    )
    .await?;
    let document = Uuid::now_v7();
    let revision = Uuid::now_v7();
    let mutation = Uuid::now_v7();
    let item = Uuid::now_v7();
    let job = Uuid::now_v7();
    let attempt = Uuid::now_v7();
    let bytes = b"historical fixture publication";
    let checksum = format!("sha256:{}", hex::encode(sha2::Sha256::digest(bytes)));
    let storage_key = storage
        .persist_revision_source(workspace.id, library.id, "published.txt", &checksum, bytes)
        .await?;
    sqlx::query("insert into content_document(id,workspace_id,library_id,external_key,source_file_name) values($1,$2,$3,'published','published.txt')").bind(document).bind(workspace.id).bind(library.id).execute(pool).await?;
    sqlx::query("insert into content_revision(id,document_id,workspace_id,library_id,revision_number,content_source_kind,checksum,mime_type,byte_size,storage_key) values($1,$2,$3,$4,7,'upload',$5,'text/plain',$6,$7)")
        .bind(revision).bind(document).bind(workspace.id).bind(library.id).bind(&checksum).bind(i64::try_from(bytes.len())?).bind(&storage_key).execute(pool).await?;
    sqlx::query("insert into content_mutation(id,workspace_id,library_id,operation_kind,request_surface,mutation_state) values($1,$2,$3,'replace','rest','applied')").bind(mutation).bind(workspace.id).bind(library.id).execute(pool).await?;
    sqlx::query("insert into content_mutation_item(id,mutation_id,document_id,result_revision_id,item_state) values($1,$2,$3,$4,'applied')").bind(item).bind(mutation).bind(document).bind(revision).execute(pool).await?;
    sqlx::query("insert into ingest_job(id,workspace_id,library_id,mutation_id,mutation_item_id,knowledge_document_id,knowledge_revision_id,job_kind,queue_state) values($1,$2,$3,$4,$5,$6,$7,'content_mutation','completed')").bind(job).bind(workspace.id).bind(library.id).bind(mutation).bind(item).bind(document).bind(revision).execute(pool).await?;
    sqlx::query("insert into ingest_attempt(id,job_id,attempt_number,attempt_state) values($1,$2,1,'succeeded')").bind(attempt).bind(job).execute(pool).await?;
    sqlx::query("insert into content_document_head(document_id,active_revision_id,readable_revision_id,latest_mutation_id,latest_successful_attempt_id) values($1,$2,$2,$3,$4)").bind(document).bind(revision).bind(mutation).bind(attempt).execute(pool).await?;
    sqlx::query("insert into knowledge_document(document_id,workspace_id,library_id,external_key,file_name,document_state,active_revision_id,readable_revision_id,latest_revision_no,document_role,created_at,updated_at)
        values($1,$2,$3,'published','published.txt','active',$4,$4,7,'primary',now(),now())")
        .bind(document).bind(workspace.id).bind(library.id).bind(revision).execute(pool).await?;
    sqlx::query("insert into knowledge_revision(revision_id,document_id,workspace_id,library_id,revision_number,revision_state,revision_kind,checksum,mime_type,byte_size,text_state,text_readable_at,vector_state,graph_state,created_at)
        values($1,$2,$3,$4,7,'active','upload',$5,'text/plain',$6,'text_readable',now(),'accepted','accepted',now())")
        .bind(revision).bind(document).bind(workspace.id).bind(library.id).bind(&checksum).bind(i64::try_from(bytes.len())?).execute(pool).await?;
    let snapshot_id = "a".repeat(64);
    let baseline = ironrag_backend::services::maintenance::baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384;
    let operator = "synthetic-integration-test-fixture";
    let certificate = serde_json::to_vec(&serde_json::json!({
        "recoverySnapshotId": snapshot_id, "sourceBaselineSha384": baseline, "admittedBy": operator,
        "revisions": [{
            "revisionId": revision, "documentId": document, "workspaceId": workspace.id, "libraryId": library.id,
            "storageKey": storage_key,
            "sourceEvidence": {"kind": "source_blob"},
            "replacement": ironrag_contracts::documents::ReplacementIdentity {
                checksum, byte_size: i64::try_from(bytes.len())?, mime_type: "text/plain".to_owned(), file_name: "published.txt".to_owned(),
                title: None, language_code: None, source_uri: None, document_hint: None,
            },
        }],
    }))?;
    let certificate_root = tempfile::Builder::new().prefix("visibility-certificate-").tempdir()?;
    let certificate_path = certificate_root.path().join("admission.json");
    tokio::fs::write(&certificate_path, &certificate).await?;
    let admission = ironrag_backend::services::maintenance::content_visibility_intent_schema::CertifiedImportAdmission::from_file(
        &certificate_path, &hex::encode(sha2::Sha256::digest(&certificate)), &snapshot_id, baseline, operator,
    ).await?;
    Ok((document, item, attempt, admission))
}

/// Database-owned definitions, not source-string assertions about wire behavior.
async fn visibility_schema_catalog(pool: &sqlx::PgPool) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as("with owned as (
        select oid,relname from pg_class where relnamespace='public'::regnamespace
        and relname in ('content_document','content_revision','content_document_head','content_mutation','content_mutation_reference','content_mutation_predecessor_audit','content_mutation_item','content_visibility_intent','content_current_revision')
    )
    select 'column:'||o.relname||':'||a.attname as key,
        format_type(a.atttypid,a.atttypmod)||':'||a.attnotnull::text||':'||coalesce(pg_get_expr(d.adbin,d.adrelid),'') as definition
    from owned o join pg_attribute a on a.attrelid=o.oid and a.attnum>0 and not a.attisdropped
    left join pg_attrdef d on d.adrelid=o.oid and d.adnum=a.attnum
    union all select 'constraint:'||o.relname||':'||c.conname,pg_get_constraintdef(c.oid)
        from owned o join pg_constraint c on c.conrelid=o.oid
    union all select 'index:'||o.relname||':'||i.indexrelid::regclass::text,pg_get_indexdef(i.indexrelid)
        from owned o join pg_index i on i.indrelid=o.oid
    union all select 'trigger:'||o.relname||':'||t.tgname,pg_get_triggerdef(t.oid)
        from owned o join pg_trigger t on t.tgrelid=o.oid where not t.tgisinternal
    union all select 'view:'||o.relname,pg_get_viewdef(o.oid) from owned o where o.relname='content_current_revision'
    union all select 'function:'||p.proname,pg_get_functiondef(p.oid) from pg_proc p
        where p.pronamespace='public'::regnamespace and p.proname in ('preserve_content_request_identity','preserve_content_mutation_reference','verify_content_mutation_representation','preserve_content_lineage_owner','verify_content_mutation_lineage')
    union all select 'enum:content_mutation_operation_kind',string_agg(e.enumlabel,',' order by e.enumsortorder)
        from pg_enum e where e.enumtypid='public.content_mutation_operation_kind'::regtype
    order by key").fetch_all(pool).await?)
}

fn without_admission_visibility_policy_additions() -> String {
    let baseline = visibility_policy_predecessor();
    let without_enum = baseline.replacen(
        "--\n-- Name: admission_visibility_policy; Type: TYPE; Schema: public; Owner: -\n--\n\nCREATE TYPE public.admission_visibility_policy AS ENUM (\n    'readable',\n    'suppressed_until_ready'\n);\n\n\n",
        "",
        1,
    );
    assert_ne!(
        without_enum, baseline,
        "the revision must carry the admission_visibility_policy enum"
    );
    let without_column = without_enum.replacen(
        ",\n    visibility_policy public.admission_visibility_policy DEFAULT 'readable'::public.admission_visibility_policy NOT NULL",
        "",
        1,
    );
    assert_ne!(
        without_column, without_enum,
        "the revision must carry the visibility_policy column"
    );
    let predecessor = without_column.replacen(
        "--\n-- Name: COLUMN content_mutation.visibility_policy; Type: COMMENT; Schema: public; Owner: -\n--\n\nCOMMENT ON COLUMN public.content_mutation.visibility_policy IS 'Durable, typed admission visibility decision. `readable` is the single legacy default: the accepted replacement is published without suppressing the current readable revision. `suppressed_until_ready` means this REQUIRED replacement cleared both head projections in its admission transaction and only a later accepted mutation that publishes READY restores readability. Every null-head repair predicate reads this column.';\n\n\n",
        "",
        1,
    );
    assert_ne!(
        predecessor, without_column,
        "the revision must carry the visibility_policy column comment"
    );
    predecessor
}

/// Removes the two IRG-182 decision-catalog additions from a baseline revision
/// that carries them: the `graph_entity_type` binding purpose and the
/// `typesafe_system_one` provider API style (the removal consumes the comma that
/// precedes the last enum value). The IRG-92 admission-visibility-policy
/// additions are removed first. Later reconstructions start from this text, so
/// the lines are reversed in one place.
fn without_decision_catalog_additions() -> String {
    let baseline = without_admission_visibility_policy_additions();
    let without_entity_type = baseline.replacen("    'graph_entity_type',\n", "", 1);
    assert_ne!(without_entity_type, baseline, "the revision must carry graph_entity_type");
    let predecessor = without_entity_type.replacen(",\n    'typesafe_system_one'", "", 1);
    assert_ne!(predecessor, without_entity_type, "the revision must carry typesafe_system_one");
    predecessor
}

/// The IRG-112 revision baseline: the shipped baseline minus all additive edits
/// the later revisions introduced — the `query_rerank`, `graph_entity_type` and
/// `typesafe_system_one` enum values and the `ops_async_operation.failure_disposition`
/// column. Its digest is the predecessor the ops-failure-disposition delta
/// certifies, so the helper asserts the text edits really land on it.
fn irg112_revision_baseline() -> String {
    let without_decision_catalog = without_decision_catalog_additions();
    let without_rerank = without_decision_catalog.replacen("    'query_rerank',\n", "", 1);
    assert_ne!(
        without_rerank, without_decision_catalog,
        "the predecessor must drop one enum value"
    );
    let predecessor = without_rerank.replacen("    failure_disposition text,\n", "", 1);
    assert_ne!(
        predecessor, without_rerank,
        "the predecessor must drop the failure_disposition column"
    );
    assert_eq!(
        hex::encode(Sha384::digest(predecessor.as_bytes())),
        IRG112_BASELINE_SHA384_HEX,
        "removing every additive edit must yield the IRG-112 revision",
    );
    predecessor
}

/// The IRG-173 revision baseline: the shipped baseline minus all three enum
/// additions (`query_rerank`, `graph_entity_type`, `typesafe_system_one`),
/// keeping the `failure_disposition` column. Its digest is the predecessor the
/// query-rerank delta certifies, so the helper asserts the text edits really
/// land on it.
fn irg173_revision_baseline() -> String {
    let without_decision_catalog = without_decision_catalog_additions();
    let predecessor = without_decision_catalog.replacen("    'query_rerank',\n", "", 1);
    assert_ne!(predecessor, without_decision_catalog, "the predecessor must drop one enum value");
    assert_eq!(
        hex::encode(Sha384::digest(predecessor.as_bytes())),
        IRG173_BASELINE_SHA384_HEX,
        "removing the three enum values must yield the IRG-173 revision",
    );
    predecessor
}

/// The IRG-163 revision baseline: the shipped baseline minus only the two
/// IRG-182 decision-catalog additions, keeping `query_rerank` and
/// `failure_disposition`. Its digest is the predecessor the decision-catalog
/// delta certifies.
fn baseline_at_query_rerank_revision() -> String {
    let predecessor = without_decision_catalog_additions();
    assert_eq!(
        hex::encode(Sha384::digest(predecessor.as_bytes())),
        ironrag_backend::services::maintenance::baseline_delta::BASELINE_REVISION_QUERY_RERANK_SHA384,
        "removing only the IRG-182 additions must yield the query_rerank revision",
    );
    predecessor
}

/// Creates the `_sqlx_migrations` bookkeeping table and records one baseline
/// checksum, exactly the state `sqlx` leaves after applying that revision.
async fn record_baseline_checksum(pool: &PgPool, sha384_hex: &str) -> Result<()> {
    sqlx::query(
        "create table _sqlx_migrations (
             version bigint primary key,
             description text not null,
             installed_on timestamptz not null default now(),
             success boolean not null,
             checksum bytea not null,
             execution_time bigint not null
         )",
    )
    .execute(pool)
    .await
    .context("failed to create the sqlx migration bookkeeping table")?;
    sqlx::query(
        "insert into _sqlx_migrations (version, description, success, checksum, execution_time)
         values (1, 'init', true, decode($1, 'hex'), 0)",
    )
    .bind(sha384_hex)
    .execute(pool)
    .await
    .context("failed to record the baseline checksum")?;
    Ok(())
}

/// Builds the IRG-75 (`b092f2f4`) predecessor object state: the IRG-112
/// revision minus the provisioning DDL and the IRG-112 indexes, keeping
/// `ops_async_operation.result_json` (it exists in IRG-75). Records the IRG-75
/// checksum, so the provisioning delta must accept it.
async fn seed_irg75_revision(pool: &PgPool) -> Result<()> {
    let predecessor = irg112_revision_baseline();
    // The predecessor is built from the compiled-in baseline, never user
    // input; `AssertSqlSafe` is how sqlx 0.9 accepts a non-`'static` string.
    sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
        .execute(pool)
        .await
        .context("failed to apply the IRG-112 revision baseline")?;
    for ddl in [
        "alter table public.catalog_workspace drop column provisioned_by_principal_id cascade",
        "alter table public.catalog_library drop column provisioned_by_principal_id cascade",
        "alter table public.iam_service_identity drop column provisioning_authority cascade",
        "drop function public.prevent_catalog_provisioning_owner_change() cascade",
        "drop index public.idx_knowledge_chunk_primary_block_fk",
        "drop index public.idx_knowledge_evidence_block_fk",
        "drop index public.idx_knowledge_structured_block_parent_block_fk",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(ddl))
            .execute(pool)
            .await
            .with_context(|| format!("failed to reverse-additive object with: {ddl}"))?;
    }
    record_baseline_checksum(pool, IRG75_BASELINE_SHA384_HEX).await
}

async fn baseline_checksum(pool: &PgPool) -> Result<Vec<u8>> {
    Ok(sqlx::query_scalar("select checksum from _sqlx_migrations where version = 1")
        .fetch_one(pool)
        .await?)
}

async fn binding_purpose_has_query_rerank(pool: &PgPool) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "select exists (
             select 1 from pg_enum
             where enumtypid = 'public.ai_binding_purpose'::regtype
               and enumlabel = 'query_rerank'
         )",
    )
    .fetch_one(pool)
    .await?)
}

/// Whether the IRG-182 `graph_entity_type` binding purpose is present.
async fn binding_purpose_has_graph_entity_type(pool: &PgPool) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "select exists (
             select 1 from pg_enum
             where enumtypid = 'public.ai_binding_purpose'::regtype
               and enumlabel = 'graph_entity_type'
         )",
    )
    .fetch_one(pool)
    .await?)
}

/// Whether the IRG-182 `typesafe_system_one` provider API style is present.
async fn provider_api_style_has_typesafe_system_one(pool: &PgPool) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "select exists (
             select 1 from pg_enum
             where enumtypid = 'public.ai_provider_api_style'::regtype
               and enumlabel = 'typesafe_system_one'
         )",
    )
    .fetch_one(pool)
    .await?)
}

/// The `ai_binding_purpose` labels in the order the runtime consumes them:
/// `order by binding_purpose` follows `pg_enum.enumsortorder`.
async fn binding_purpose_enum_order(pool: &PgPool) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "select enumlabel::text from pg_enum
         where enumtypid = 'public.ai_binding_purpose'::regtype
         order by enumsortorder",
    )
    .fetch_all(pool)
    .await?)
}

/// The `ai_provider_api_style` labels in `pg_enum.enumsortorder`.
async fn provider_api_style_enum_order(pool: &PgPool) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "select enumlabel::text from pg_enum
         where enumtypid = 'public.ai_provider_api_style'::regtype
         order by enumsortorder",
    )
    .fetch_all(pool)
    .await?)
}

/// Whether the IRG-173 nullable `failure_disposition` column is present.
async fn failure_disposition_present(pool: &PgPool) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "select exists (
             select 1 from information_schema.columns
             where table_schema = 'public'
               and table_name = 'ops_async_operation'
               and column_name = 'failure_disposition'
               and data_type = 'text'
               and is_nullable = 'YES'
         )",
    )
    .fetch_one(pool)
    .await?)
}

/// Advances a database that already recorded the provisioning revision to the
/// IRG-112 revision: applies the three FK indexes, then compare-and-sets the
/// recorded checksum from the provisioning target to the IRG-112 revision.
async fn advance_to_irg112_revision(pool: &PgPool) -> Result<()> {
    for ddl in IRG112_INDEX_DDL {
        sqlx::query(sqlx::AssertSqlSafe(ddl))
            .execute(pool)
            .await
            .with_context(|| format!("failed to apply IRG-112 index: {ddl}"))?;
    }
    let updated = sqlx::query(
        "update _sqlx_migrations
         set checksum = decode($1, 'hex')
         where version = 1 and success
           and checksum = decode($2, 'hex')",
    )
    .bind(IRG112_BASELINE_SHA384_HEX)
    .bind(PROVISIONING_BASELINE_SHA384_HEX)
    .execute(pool)
    .await?;
    assert_eq!(updated.rows_affected(), 1, "the IRG-112 checksum compare-and-set must match");
    assert_eq!(baseline_checksum(pool).await?, hex::decode(IRG112_BASELINE_SHA384_HEX)?);
    Ok(())
}

/// The additive live path is one ordered chain: IRG-75 → integration
/// provisioning → IRG-112 indexes → IRG-173 `failure_disposition` → the
/// `query_rerank` purpose → the decision catalog (`graph_entity_type` and
/// `typesafe_system_one`), the shipped baseline. Each delta accepts only its own
/// predecessor revision and validates only the object groups that revision
/// declares, so the documented order is executable without a delta demanding a
/// future object.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn additive_baseline_chain_advances_irg75_to_shipped_revision() -> Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for additive baseline chain test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect additive baseline chain test postgres")?;

    let result = async {
        let baseline = include_str!("../../migrations/0001_init.sql");
        let shipped_checksum = Sha384::digest(baseline.as_bytes()).to_vec();

        seed_irg75_revision(&pool).await?;
        assert_eq!(baseline_checksum(&pool).await?, hex::decode(IRG75_BASELINE_SHA384_HEX)?);

        // T1: the provisioning delta accepts the IRG-75 revision and validates
        // only the provisioning objects, never the future enum value.
        let provisioning = ironrag_backend::services::maintenance::service_identity_schema::migrate_integration_provisioning_schema(
            &pool, true,
        )
        .await
        .context("provisioning delta must accept the IRG-75 predecessor")?;
        assert!(provisioning.schema_applied, "the provisioning delta must apply its DDL");
        assert!(!provisioning.already_current);
        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(PROVISIONING_BASELINE_SHA384_HEX)?,
            "the provisioning delta must stamp its own target revision",
        );
        let provisioning_authority_present: bool = sqlx::query_scalar(
            "select exists (
                 select 1 from information_schema.columns
                 where table_schema = 'public'
                   and table_name = 'iam_service_identity'
                   and column_name = 'provisioning_authority'
             )",
        )
        .fetch_one(&pool)
        .await?;
        assert!(provisioning_authority_present, "the provisioning delta must create its column");

        // T2: the IRG-112 indexes bring the database to the
        // ops-failure-disposition predecessor; the checksum is compare-and-set
        // to the IRG-112 revision.
        advance_to_irg112_revision(&pool).await?;
        assert!(
            !failure_disposition_present(&pool).await?,
            "the IRG-112 predecessor must not carry failure_disposition"
        );

        // T3: the ops-failure-disposition delta accepts the IRG-112 revision,
        // adds the nullable column, and stamps the IRG-173 revision.
        let disposition = ironrag_backend::services::maintenance::ops_failure_disposition_schema::migrate_ops_failure_disposition_schema(
            &pool, true,
        )
        .await
        .context("ops-failure-disposition delta must accept the IRG-112 predecessor")?;
        assert!(
            disposition.schema_applied,
            "the ops-failure-disposition delta must add the column"
        );
        assert!(!disposition.already_current);
        assert!(failure_disposition_present(&pool).await?);
        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(IRG173_BASELINE_SHA384_HEX)?,
            "the ops-failure-disposition delta must stamp the IRG-173 revision",
        );
        assert!(
            !binding_purpose_has_query_rerank(&pool).await?,
            "the IRG-173 predecessor must not carry query_rerank"
        );

        // T4: the query-rerank delta accepts the IRG-173 revision, adds the
        // enum value, and stamps the IRG-163 revision; a re-run is a no-op.
        let rerank = ironrag_backend::services::maintenance::ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(
            &pool, true,
        )
        .await
        .context("query-rerank delta must accept the IRG-173 predecessor")?;
        assert!(rerank.schema_applied, "the query-rerank delta must add the enum value");
        assert!(!rerank.already_current);
        assert!(binding_purpose_has_query_rerank(&pool).await?);
        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(ironrag_backend::services::maintenance::baseline_delta::BASELINE_REVISION_QUERY_RERANK_SHA384)?,
            "the query-rerank delta must stamp the IRG-163 revision",
        );
        let idempotent = ironrag_backend::services::maintenance::ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(
            &pool, true,
        )
        .await
        .context("idempotent re-run of the query-rerank delta failed")?;
        assert!(idempotent.already_current);
        assert!(!idempotent.schema_applied);

        // T5: the decision-catalog delta accepts the IRG-163 revision, adds both
        // enum values, and stamps the shipped baseline; a re-run is a no-op.
        assert!(
            !binding_purpose_has_graph_entity_type(&pool).await?,
            "the IRG-163 predecessor must not carry graph_entity_type"
        );
        assert!(
            !provider_api_style_has_typesafe_system_one(&pool).await?,
            "the IRG-163 predecessor must not carry typesafe_system_one"
        );
        let decision = ironrag_backend::services::maintenance::decision_catalog_schema::migrate_decision_catalog_schema(
            &pool, true,
        )
        .await
        .context("decision-catalog delta must accept the IRG-163 predecessor")?;
        assert!(decision.schema_applied, "the decision-catalog delta must add both enum values");
        assert!(!decision.already_current);
        assert!(binding_purpose_has_graph_entity_type(&pool).await?);
        assert!(provider_api_style_has_typesafe_system_one(&pool).await?);
        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(ironrag_backend::services::maintenance::baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384)?,
            "the decision-catalog delta must stamp the decision-catalog revision, not a later one",
        );
        let idempotent = ironrag_backend::services::maintenance::decision_catalog_schema::migrate_decision_catalog_schema(
            &pool, true,
        )
        .await
        .context("idempotent re-run of the decision-catalog delta failed")?;
        assert!(idempotent.already_current);
        assert!(!idempotent.schema_applied);

        // T6: the admission-visibility-policy delta accepts the decision-catalog
        // revision, adds the enum and the typed policy column, and stamps the
        // shipped baseline; a re-run is a no-op.
        let policy_present_before: bool = sqlx::query_scalar(
            "select exists (
                 select 1 from information_schema.columns
                 where table_schema = 'public'
                   and table_name = 'content_mutation'
                   and column_name = 'visibility_policy'
             )",
        )
        .fetch_one(&pool)
        .await?;
        assert!(!policy_present_before, "the predecessor must not carry visibility_policy");
        let policy = ironrag_backend::services::maintenance::admission_visibility_policy_schema::migrate_admission_visibility_policy_schema(
            &pool, true,
        )
        .await
        .context("admission-visibility-policy delta must accept the decision-catalog predecessor")?;
        assert!(policy.schema_applied, "the delta must add the policy column");
        assert!(!policy.already_current);
        let policy_present_after: bool = sqlx::query_scalar(
            "select exists (
                 select 1 from information_schema.columns
                 where table_schema = 'public'
                   and table_name = 'content_mutation'
                   and column_name = 'visibility_policy'
                   and data_type = 'USER-DEFINED'
                   and udt_name = 'admission_visibility_policy'
                   and is_nullable = 'NO'
                   and column_default = '''readable''::admission_visibility_policy'
             )",
        )
        .fetch_one(&pool)
        .await?;
        assert!(policy_present_after, "the delta must create the typed policy column");
        assert_eq!(baseline_checksum(&pool).await?, hex::decode(ironrag_backend::services::maintenance::baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384)?);
        let idempotent = ironrag_backend::services::maintenance::admission_visibility_policy_schema::migrate_admission_visibility_policy_schema(
            &pool, true,
        )
        .await
        .context("idempotent re-run of the admission-visibility-policy delta failed")?;
        assert!(idempotent.already_current);
        assert!(!idempotent.schema_applied);
        let (_storage_root, storage) = visibility_migration_storage(&settings)?;
        let intent = ironrag_backend::services::maintenance::content_visibility_intent_schema::migrate_content_visibility_intent_schema(&pool,true, &storage, None).await?;
        assert!(intent.schema_applied && !intent.already_current);
        assert_retired_visibility_policy_absent(&pool).await?;
        assert_eq!(baseline_checksum(&pool).await?, shipped_checksum);
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// The IRG-173 transition's success path: a database at the IRG-112 predecessor
/// with `ops_async_operation.failure_disposition` absent must come out of
/// `migrate --apply` carrying the nullable column and the IRG-173 checksum, and
/// a re-run must be a no-op. Removing the delta would make the stamp refuse on
/// the absent column, so this proves the real DDL and compare-and-set.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn ops_failure_disposition_upgrade_adds_column_and_stamps_checksum() -> Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for ops-failure-disposition upgrade test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect ops-failure-disposition upgrade test postgres")?;

    let result = async {
        let predecessor = irg112_revision_baseline();
        sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
            .execute(&pool)
            .await
            .context("failed to apply the IRG-112 revision baseline")?;
        record_baseline_checksum(&pool, IRG112_BASELINE_SHA384_HEX).await?;
        assert!(
            !failure_disposition_present(&pool).await?,
            "the IRG-112 predecessor must not carry failure_disposition"
        );

        let applied = ironrag_backend::services::maintenance::ops_failure_disposition_schema::migrate_ops_failure_disposition_schema(
            &pool, true,
        )
        .await
        .context("the ops-failure-disposition delta must accept the IRG-112 predecessor")?;
        assert!(applied.schema_applied, "the delta must add the column");
        assert!(!applied.already_current);
        assert!(failure_disposition_present(&pool).await?);
        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(IRG173_BASELINE_SHA384_HEX)?,
            "the delta must stamp the IRG-173 revision",
        );

        let idempotent = ironrag_backend::services::maintenance::ops_failure_disposition_schema::migrate_ops_failure_disposition_schema(
            &pool, true,
        )
        .await
        .context("idempotent re-run of the ops-failure-disposition delta failed")?;
        assert!(idempotent.already_current);
        assert!(!idempotent.schema_applied);
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// The IRG-173 target checksum on a database that never took the column is
/// refused in both modes: the target revision's own object group is validated
/// before the delta may report `already_current`, so a hand-stamped checksum
/// cannot stand for the absent column.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn ops_failure_disposition_delta_rejects_a_target_checksum_without_the_column() -> Result<()>
{
    let settings = Settings::from_env()
        .context("failed to load settings for ops-failure-disposition target test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect ops-failure-disposition target test postgres")?;

    let result = async {
        let predecessor = irg112_revision_baseline();
        sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
            .execute(&pool)
            .await
            .context("failed to apply the IRG-112 revision baseline")?;
        record_baseline_checksum(&pool, IRG173_BASELINE_SHA384_HEX).await?;
        assert!(!failure_disposition_present(&pool).await?);

        for apply in [false, true] {
            let error = ironrag_backend::services::maintenance::ops_failure_disposition_schema::migrate_ops_failure_disposition_schema(
                &pool, apply,
            )
            .await
            .expect_err("a target checksum without the column must be refused");
            let message = format!("{error:#}");
            assert!(
                message.contains("ops_async_operation.failure_disposition"),
                "the refusal must name the absent object (apply={apply}), got: {message}"
            );
        }
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// A pre-applied `failure_disposition` column under the IRG-112 checksum is
/// refused by dry-run exactly as by apply: the column is present, the checksum
/// is stale, and the delta refuses rather than stamping.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn pre_applied_ops_failure_disposition_with_a_stale_checksum_is_refused_in_both_modes()
-> Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for pre-applied ops-failure-disposition test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect pre-applied ops-failure-disposition test postgres")?;

    let result = async {
        let predecessor = irg112_revision_baseline();
        sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
            .execute(&pool)
            .await
            .context("failed to apply the IRG-112 revision baseline")?;
        // Outside an explicit transaction, so the column is committed and the
        // delta's own transaction sees it as pre-applied DDL.
        sqlx::query(
            "alter table public.ops_async_operation add column if not exists failure_disposition text",
        )
        .execute(&pool)
        .await
        .context("failed to pre-apply the failure_disposition column")?;
        assert!(failure_disposition_present(&pool).await?);
        record_baseline_checksum(&pool, IRG112_BASELINE_SHA384_HEX).await?;

        for apply in [false, true] {
            let error = ironrag_backend::services::maintenance::ops_failure_disposition_schema::migrate_ops_failure_disposition_schema(
                &pool, apply,
            )
            .await
            .expect_err("pre-applied DDL under a stale checksum must be refused");
            let message = format!("{error:#}");
            assert!(
                message.contains("already present with a stale baseline checksum"),
                "the refusal must say the DDL is pre-applied (apply={apply}), got: {message}"
            );
        }
        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(IRG112_BASELINE_SHA384_HEX)?,
            "a refusal must leave the stale checksum untouched",
        );
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// A database already at a later chain revision than the provisioning delta's
/// target — the IRG-112 revision — must be reported `already_current`, not
/// refused and not re-stamped: the delta validates its own target's object
/// groups and leaves the later checksum untouched. This covers the
/// `current_index > target_index` branch.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn integration_provisioning_is_already_current_at_a_later_chain_revision() -> Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for later-revision provisioning test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect later-revision provisioning test postgres")?;

    let result = async {
        seed_irg75_revision(&pool).await?;
        let provisioning =
            ironrag_backend::services::maintenance::service_identity_schema::migrate_integration_provisioning_schema(
                &pool, true,
            )
            .await
            .context("provisioning delta must accept the IRG-75 predecessor")?;
        assert!(provisioning.schema_applied);
        assert_eq!(baseline_checksum(&pool).await?, hex::decode(PROVISIONING_BASELINE_SHA384_HEX)?);

        // Advance to the IRG-112 revision, one hop past the provisioning
        // delta's target, exactly as the chain test does.
        advance_to_irg112_revision(&pool).await?;

        // Re-running the provisioning delta from a later revision is a no-op:
        // `already_current` without a stamp, and the later checksum stays.
        let already =
            ironrag_backend::services::maintenance::service_identity_schema::migrate_integration_provisioning_schema(
                &pool, true,
            )
            .await
            .context("re-running the provisioning delta from a later revision failed")?;
        assert!(already.already_current, "a later chain revision must be reported already_current");
        assert!(!already.schema_applied, "a later chain revision must not re-apply the delta");
        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(IRG112_BASELINE_SHA384_HEX)?,
            "the later revision's checksum must not be stamped over",
        );
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// A hand-stamped shipped checksum on a database that never took the enum
/// value must fail readiness with the object named — the reason the per-object
/// check exists at all.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn migration_state_rejects_query_rerank_checksum_when_enum_value_absent() -> Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for absent query_rerank object test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect absent query_rerank object test postgres")?;

    let result = async {
        let predecessor = irg173_revision_baseline();
        sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
            .execute(&pool)
            .await
            .context("failed to apply the IRG-173 revision baseline")?;
        let shipped_hex =
            hex::encode(Sha384::digest(include_str!("../../migrations/0001_init.sql").as_bytes()));
        record_baseline_checksum(&pool, &shipped_hex).await?;
        assert!(
            !binding_purpose_has_query_rerank(&pool).await?,
            "the IRG-173 predecessor must not carry query_rerank"
        );

        let error = ironrag_backend::infra::persistence::validate_postgres_migration_state(&pool)
            .await
            .expect_err("a shipped checksum without the enum value must be rejected");
        let message = format!("{error:#}");
        assert!(
            message.contains("ai_binding_purpose query_rerank"),
            "the failure must name the absent object, got: {message}"
        );
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// A checksum in no chain revision is refused identically by dry-run and apply:
/// the delta can neither start from it nor count it as already passed, so both
/// modes refuse and neither stamps.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn additive_baseline_delta_refuses_an_unknown_checksum_in_both_modes() -> Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for unknown-checksum test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect unknown-checksum test postgres")?;

    let result = async {
        let unknown_checksum = hex::encode([0_u8; 48]);
        record_baseline_checksum(&pool, &unknown_checksum).await?;

        for apply in [false, true] {
            let error = ironrag_backend::services::maintenance::ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(
                &pool, apply,
            )
            .await
            .expect_err("a checksum in no chain revision must be refused");
            let message = format!("{error:#}");
            assert!(
                message.contains("refusing to apply or stamp"),
                "the refusal must say it neither applies nor stamps (apply={apply}), got: {message}"
            );
        }
        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(&unknown_checksum)?,
            "a refusal must leave the recorded checksum untouched",
        );
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// The query-rerank delta's predecessor is the IRG-173 revision, so a database
/// at an earlier chain revision — the IRG-75 revision — is refused in both
/// modes rather than skipped over to a stamp.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn query_rerank_delta_refuses_a_revision_before_its_predecessor_in_both_modes() -> Result<()>
{
    let settings = Settings::from_env()
        .context("failed to load settings for earlier-revision query-rerank test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect earlier-revision query-rerank test postgres")?;

    let result = async {
        record_baseline_checksum(&pool, IRG75_BASELINE_SHA384_HEX).await?;

        for apply in [false, true] {
            let error = ironrag_backend::services::maintenance::ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(
                &pool, apply,
            )
            .await
            .expect_err("a revision before the query-rerank predecessor must be refused");
            let message = format!("{error:#}");
            assert!(
                message.contains("refusing to apply or stamp"),
                "the refusal must say it neither applies nor stamps (apply={apply}), got: {message}"
            );
        }
        assert_eq!(baseline_checksum(&pool).await?, hex::decode(IRG75_BASELINE_SHA384_HEX)?);
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// The provisioning delta's precondition — `iam_service_identity` — is
/// evaluated in dry-run too, so a database without it is refused identically by
/// both modes instead of a dry-run promising an apply that then refuses.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn integration_provisioning_refuses_a_missing_precondition_object_in_both_modes() -> Result<()>
{
    let settings =
        Settings::from_env().context("failed to load settings for missing-precondition test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect missing-precondition test postgres")?;

    let result = async {
        record_baseline_checksum(&pool, IRG75_BASELINE_SHA384_HEX).await?;

        for apply in [false, true] {
            let error = ironrag_backend::services::maintenance::service_identity_schema::migrate_integration_provisioning_schema(
                &pool, apply,
            )
            .await
            .expect_err("a missing precondition object must be refused");
            let message = format!("{error:#}");
            assert!(
                message.contains("iam_service_identity"),
                "the refusal must name the absent precondition (apply={apply}), got: {message}"
            );
        }
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// The query-rerank target checksum on a database that never took the enum value
/// is refused in both modes: the target revision's own object group is validated
/// before the delta may report `already_current`, so a hand-stamped checksum
/// cannot stand for an object the revision declares.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn query_rerank_delta_rejects_a_target_checksum_without_the_enum_value() -> Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for target-checksum test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect target-checksum test postgres")?;

    let result = async {
        let predecessor = irg173_revision_baseline();
        sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
            .execute(&pool)
            .await
            .context("failed to apply the IRG-173 revision baseline")?;
        record_baseline_checksum(
            &pool,
            ironrag_backend::services::maintenance::baseline_delta::BASELINE_REVISION_QUERY_RERANK_SHA384,
        )
        .await?;
        assert!(
            !binding_purpose_has_query_rerank(&pool).await?,
            "the IRG-173 predecessor must not carry query_rerank"
        );

        for apply in [false, true] {
            let error = ironrag_backend::services::maintenance::ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(
                &pool, apply,
            )
            .await
            .expect_err("a target checksum without the enum value must be refused");
            let message = format!("{error:#}");
            assert!(
                message.contains("ai_binding_purpose query_rerank"),
                "the refusal must name the absent object (apply={apply}), got: {message}"
            );
        }
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// A pre-applied DDL under a stale checksum is refused by dry-run exactly as by
/// apply. Before the shared-delta refactor the dry-run reported `pending`
/// (`already_current: false`) for this state while apply refused it, so this
/// test is the dry-run/apply agreement guard for the `delta_present` branch.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn pre_applied_ddl_with_a_stale_checksum_is_refused_in_both_modes() -> Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for pre-applied-DDL test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect pre-applied-DDL test postgres")?;

    let result = async {
        let predecessor = irg173_revision_baseline();
        sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
            .execute(&pool)
            .await
            .context("failed to apply the IRG-173 revision baseline")?;
        // Outside an explicit transaction, so the value is committed and the
        // delta's own transaction sees it as pre-applied DDL.
        sqlx::query(
            "alter type public.ai_binding_purpose add value if not exists 'query_rerank' before 'query_answer'",
        )
        .execute(&pool)
        .await
        .context("failed to pre-apply the query_rerank enum value")?;
        assert!(binding_purpose_has_query_rerank(&pool).await?);
        record_baseline_checksum(&pool, IRG173_BASELINE_SHA384_HEX).await?;

        for apply in [false, true] {
            let error = ironrag_backend::services::maintenance::ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(
                &pool, apply,
            )
            .await
            .expect_err("pre-applied DDL under a stale checksum must be refused");
            let message = format!("{error:#}");
            assert!(
                message.contains("already present with a stale baseline checksum"),
                "the refusal must say the DDL is pre-applied (apply={apply}), got: {message}"
            );
        }
        assert!(
            binding_purpose_has_query_rerank(&pool).await?,
            "a refusal must leave the pre-applied DDL in place"
        );
        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(IRG173_BASELINE_SHA384_HEX)?,
            "a refusal must leave the stale checksum untouched",
        );
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// At the query-rerank predecessor the earlier target's objects are the
/// prerequisite of the hop: dropping one must refuse in both modes, before a
/// dry-run may report a pending state or the DDL may run, and the exact
/// predecessor checksum must not be stamped over.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn query_rerank_delta_refuses_a_missing_earlier_prerequisite_in_both_modes() -> Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for missing-prerequisite test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect missing-prerequisite test postgres")?;

    let result = async {
        let predecessor = irg173_revision_baseline();
        sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
            .execute(&pool)
            .await
            .context("failed to apply the IRG-173 revision baseline")?;
        // The IRG-173 checksum is the exact predecessor, but an object the
        // earlier integration-provisioning revision declares is absent.
        sqlx::query(
            "alter table public.iam_service_identity drop column provisioning_authority cascade",
        )
        .execute(&pool)
        .await
        .context("failed to drop the earlier prerequisite column")?;
        record_baseline_checksum(&pool, IRG173_BASELINE_SHA384_HEX).await?;
        assert!(
            !binding_purpose_has_query_rerank(&pool).await?,
            "the query_rerank enum value must not exist before the delta runs"
        );

        for apply in [false, true] {
            let error = ironrag_backend::services::maintenance::ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(
                &pool, apply,
            )
            .await
            .expect_err("a missing earlier prerequisite object must be refused");
            let message = format!("{error:#}");
            assert!(
                message.contains("iam_service_identity.provisioning_authority"),
                "the refusal must name the absent prerequisite (apply={apply}), got: {message}"
            );
        }

        assert_eq!(
            baseline_checksum(&pool).await?,
            hex::decode(IRG173_BASELINE_SHA384_HEX)?,
            "a prerequisite refusal must not stamp the target checksum",
        );
        assert!(
            !binding_purpose_has_query_rerank(&pool).await?,
            "a prerequisite refusal must not apply the delta's DDL",
        );
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

/// The manual IRG-112 hop is part of the immutable predecessor state too:
/// a checksum stamped without one of its indexes must not authorize a later
/// transition, in dry-run or apply mode.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn additive_deltas_refuse_a_missing_irg112_index_in_both_modes() -> Result<()> {
    let settings =
        Settings::from_env().context("failed to load settings for IRG-112 index test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&temp_database.database_url).await?;

    let result = async {
        let predecessor = irg112_revision_baseline();
        sqlx::raw_sql(sqlx::AssertSqlSafe(predecessor.as_str()))
            .execute(&pool)
            .await?;
        sqlx::query("drop index public.idx_knowledge_evidence_block_fk")
            .execute(&pool)
            .await?;
        record_baseline_checksum(&pool, IRG112_BASELINE_SHA384_HEX).await?;

        for apply in [false, true] {
            let error = ironrag_backend::services::maintenance::ops_failure_disposition_schema::migrate_ops_failure_disposition_schema(&pool, apply)
                .await
                .expect_err("a missing IRG-112 index must refuse the IRG-173 delta");
            assert!(
                format!("{error:#}").contains("idx_knowledge_evidence_block_fk"),
                "the IRG-173 refusal must name the missing index (apply={apply}): {error:#}"
            );
        }
        assert_eq!(baseline_checksum(&pool).await?, hex::decode(IRG112_BASELINE_SHA384_HEX)?);
        assert!(!failure_disposition_present(&pool).await?);

        // Reproduce a manually mis-stamped IRG-173 database to exercise the
        // *next* hop against the same absent prerequisite.
        sqlx::raw_sql(include_str!("../../ops/apply-ops-failure-disposition-schema.sql"))
            .execute(&pool)
            .await?;
        let updated = sqlx::query(
            "update _sqlx_migrations set checksum = decode($1, 'hex')
             where version = 1 and checksum = decode($2, 'hex')",
        )
        .bind(IRG173_BASELINE_SHA384_HEX)
        .bind(IRG112_BASELINE_SHA384_HEX)
        .execute(&pool)
        .await?;
        assert_eq!(updated.rows_affected(), 1);

        for apply in [false, true] {
            let error = ironrag_backend::services::maintenance::ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(&pool, apply)
                .await
                .expect_err("a missing manual IRG-112 prerequisite must refuse the delta");
            assert!(
                format!("{error:#}").contains("idx_knowledge_evidence_block_fk"),
                "the refusal must name the missing IRG-112 index (apply={apply}): {error:#}"
            );
        }
        assert_eq!(baseline_checksum(&pool).await?, hex::decode(IRG173_BASELINE_SHA384_HEX)?);
        assert!(!binding_purpose_has_query_rerank(&pool).await?);
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

#[test]
fn baseline_declares_full_ai_binding_fk_indexes_and_deferrable_scope_guards() {
    let baseline = include_str!("../../migrations/0001_init.sql");

    for definition in [
        "CREATE INDEX ai_binding_account_provider_idx ON public.ai_binding USING btree (account_id, provider_catalog_id);",
        "CREATE INDEX ai_binding_model_provider_idx ON public.ai_binding USING btree (model_catalog_id, provider_catalog_id);",
        "CREATE INDEX ai_binding_model_purpose_idx ON public.ai_binding USING btree (model_catalog_id, binding_purpose);",
    ] {
        assert!(baseline.contains(definition), "missing full child index: {definition}");
    }
    assert!(baseline.contains(
        "CREATE CONSTRAINT TRIGGER trg_ai_binding_account_scope AFTER INSERT OR UPDATE OF scope_kind, workspace_id, library_id, account_id ON public.ai_binding DEFERRABLE INITIALLY IMMEDIATE"
    ));
    assert!(baseline.contains(
        "CREATE TRIGGER trg_ai_account_scope_immutable BEFORE UPDATE OF scope_kind, workspace_id, library_id ON public.ai_account"
    ));
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn binding_account_scope_constraint_blocks_cross_tenant_credentials() -> Result<()> {
    let fixture = GreenfieldBootstrapFixture::create().await?;

    let result = async {
        for index_name in [
            "ai_binding_account_provider_idx",
            "ai_binding_model_provider_idx",
            "ai_binding_model_purpose_idx",
        ] {
            let predicate: Option<String> = sqlx::query_scalar(
                "select pg_get_expr(index.indpred, index.indrelid)
                 from pg_index index
                 join pg_class relation on relation.oid = index.indexrelid
                 where relation.relname = $1",
            )
            .bind(index_name)
            .fetch_one(fixture.pool())
            .await
            .with_context(|| format!("failed to inspect {index_name}"))?;
            assert!(predicate.is_none(), "{index_name} must cover inactive bindings");
        }

        let workspace_a =
            catalog_repository::create_workspace(fixture.pool(), "scope-a", "Scope A", None)
                .await?;
        let workspace_b =
            catalog_repository::create_workspace(fixture.pool(), "scope-b", "Scope B", None)
                .await?;
        let library_a = catalog_repository::create_library(
            fixture.pool(),
            workspace_a.id,
            "library-a",
            "Library A",
            None,
            None,
        )
        .await?;
        let library_b = catalog_repository::create_library(
            fixture.pool(),
            workspace_b.id,
            "library-b",
            "Library B",
            None,
            None,
        )
        .await?;
        let library_a_sibling = catalog_repository::create_library(
            fixture.pool(),
            workspace_a.id,
            "library-a-sibling",
            "Library A sibling",
            None,
            None,
        )
        .await?;

        let provider_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_provider_catalog (
                id, provider_kind, display_name, api_style, lifecycle_state,
                capability_flags_json
             ) values ($1, 'scope-test', 'Scope test', 'openai_compatible', 'active', '{}')",
        )
        .bind(provider_id)
        .execute(fixture.pool())
        .await?;
        let model_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_model_catalog (
                id, provider_catalog_id, model_name, lifecycle_state, metadata_json
             ) values ($1, $2, 'scope-model', 'active', '{}')",
        )
        .bind(model_id)
        .bind(provider_id)
        .execute(fixture.pool())
        .await?;
        sqlx::query(
            "insert into ai_model_binding_purpose (model_catalog_id, binding_purpose)
             values ($1, 'query_answer'), ($1, 'extract_graph')",
        )
        .bind(model_id)
        .execute(fixture.pool())
        .await?;
        let account_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_account (
                id, workspace_id, provider_catalog_id, label, credential_state, scope_kind
             ) values ($1, $2, $3, 'workspace-a', 'active', 'workspace')",
        )
        .bind(account_id)
        .bind(workspace_a.id)
        .bind(provider_id)
        .execute(fixture.pool())
        .await?;
        let library_account_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_account (
                id, workspace_id, library_id, provider_catalog_id, label,
                credential_state, scope_kind
             ) values ($1, $2, $3, $4, 'library-a', 'active', 'library')",
        )
        .bind(library_account_id)
        .bind(workspace_a.id)
        .bind(library_a.id)
        .bind(provider_id)
        .execute(fixture.pool())
        .await?;

        const INSERT_LIBRARY_BINDING: &str = "insert into ai_binding (
                id, workspace_id, library_id, binding_purpose, account_id,
                provider_catalog_id, binding_state, scope_kind, model_catalog_id
             ) values ($1, $2, $3, 'query_answer', $4, $5, 'active', 'library', $6)";
        let cross_workspace = sqlx::query(INSERT_LIBRARY_BINDING)
            .bind(Uuid::now_v7())
            .bind(workspace_b.id)
            .bind(library_b.id)
            .bind(account_id)
            .bind(provider_id)
            .bind(model_id)
            .execute(fixture.pool())
            .await;
        assert_eq!(
            cross_workspace
                .expect_err("cross-workspace account must be rejected")
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("23514")
        );

        let binding_id = Uuid::now_v7();
        sqlx::query(INSERT_LIBRARY_BINDING)
            .bind(binding_id)
            .bind(workspace_a.id)
            .bind(library_a.id)
            .bind(account_id)
            .bind(provider_id)
            .bind(model_id)
            .execute(fixture.pool())
            .await
            .context("compatible workspace account was rejected")?;

        let cross_library = sqlx::query(INSERT_LIBRARY_BINDING)
            .bind(Uuid::now_v7())
            .bind(workspace_a.id)
            .bind(library_a_sibling.id)
            .bind(library_account_id)
            .bind(provider_id)
            .bind(model_id)
            .execute(fixture.pool())
            .await;
        assert_eq!(
            cross_library
                .expect_err("cross-library account must be rejected")
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("23514")
        );

        let incompatible_account_update = sqlx::query(
            "update ai_account
             set scope_kind = 'library', workspace_id = $2, library_id = $3
             where id = $1",
        )
        .bind(account_id)
        .bind(workspace_b.id)
        .bind(library_b.id)
        .execute(fixture.pool())
        .await;
        assert_eq!(
            incompatible_account_update
                .expect_err("account scope must be immutable after creation")
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("23514")
        );

        let mut tx = fixture.pool().begin().await?;
        sqlx::query("set constraints trg_ai_binding_account_scope deferred")
            .execute(&mut *tx)
            .await?;
        let deferred_binding_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_binding (
                id, workspace_id, library_id, binding_purpose, account_id,
                provider_catalog_id, binding_state, scope_kind, model_catalog_id
             ) values ($1, $2, $3, 'extract_graph', $4, $5, 'active', 'library', $6)",
        )
        .bind(deferred_binding_id)
        .bind(workspace_a.id)
        .bind(library_a_sibling.id)
        .bind(library_account_id)
        .bind(provider_id)
        .bind(model_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update ai_binding
             set library_id = $2
             where id = $1",
        )
        .bind(deferred_binding_id)
        .bind(library_a.id)
        .execute(&mut *tx)
        .await?;
        tx.commit()
            .await
            .context("deferred scope constraint rejected a compatible final binding scope")?;

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn ai_config_snapshot_round_trips_model_purpose_before_binding() -> Result<()> {
    let fixture = GreenfieldBootstrapFixture::create().await?;

    let result = Box::pin(async {
        let workspace = catalog_repository::create_workspace(
            fixture.pool(),
            "snapshot-scope",
            "Snapshot scope",
            None,
        )
        .await?;
        let library = catalog_repository::create_library(
            fixture.pool(),
            workspace.id,
            "snapshot-library",
            "Snapshot library",
            None,
            None,
        )
        .await?;
        let provider_id = Uuid::now_v7();
        let model_id = Uuid::now_v7();
        let account_id = Uuid::now_v7();
        let binding_id = Uuid::now_v7();

        sqlx::query(
            "insert into ai_provider_catalog (
                id, provider_kind, display_name, api_style, lifecycle_state,
                capability_flags_json
             ) values ($1, 'snapshot-test', 'Snapshot test', 'openai_compatible', 'active', '{}')",
        )
        .bind(provider_id)
        .execute(fixture.pool())
        .await?;
        sqlx::query(
            "insert into ai_model_catalog (
                id, provider_catalog_id, model_name, lifecycle_state, metadata_json
             ) values ($1, $2, 'snapshot-model', 'active', '{}')",
        )
        .bind(model_id)
        .bind(provider_id)
        .execute(fixture.pool())
        .await?;
        sqlx::query(
            "insert into ai_model_binding_purpose (model_catalog_id, binding_purpose)
             values ($1, 'query_answer')",
        )
        .bind(model_id)
        .execute(fixture.pool())
        .await?;
        sqlx::query(
            "insert into ai_account (
                id, workspace_id, library_id, provider_catalog_id, label,
                credential_state, scope_kind
             ) values ($1, $2, $3, $4, 'snapshot-account', 'active', 'library')",
        )
        .bind(account_id)
        .bind(workspace.id)
        .bind(library.id)
        .bind(provider_id)
        .execute(fixture.pool())
        .await?;
        sqlx::query(
            "insert into ai_binding (
                id, workspace_id, library_id, binding_purpose, account_id,
                provider_catalog_id, binding_state, scope_kind, model_catalog_id
             ) values ($1, $2, $3, 'query_answer', $4, $5, 'disabled', 'library', $6)",
        )
        .bind(binding_id)
        .bind(workspace.id)
        .bind(library.id)
        .bind(account_id)
        .bind(provider_id)
        .bind(model_id)
        .execute(fixture.pool())
        .await?;

        let (writer, mut reader) = tokio::io::duplex(64 * 1024);
        let export = tokio::spawn(export_library_archive(
            fixture.state.clone(),
            library.id,
            vec![IncludeKind::AiConfig],
            writer,
        ));
        let mut archive = Vec::new();
        reader.read_to_end(&mut archive).await?;
        export.await.context("snapshot export task failed")??;

        sqlx::query("delete from ai_binding where id = $1")
            .bind(binding_id)
            .execute(fixture.pool())
            .await?;
        sqlx::query(
            "delete from ai_model_binding_purpose
             where model_catalog_id = $1 and binding_purpose = 'query_answer'",
        )
        .bind(model_id)
        .execute(fixture.pool())
        .await?;
        sqlx::query("delete from ai_account where id = $1")
            .bind(account_id)
            .execute(fixture.pool())
            .await?;
        sqlx::query("delete from ai_model_catalog where id = $1")
            .bind(model_id)
            .execute(fixture.pool())
            .await?;
        sqlx::query("delete from ai_provider_catalog where id = $1")
            .bind(provider_id)
            .execute(fixture.pool())
            .await?;

        restore_library_archive(
            &fixture.state,
            library.id,
            archive.as_slice(),
            OverwriteMode::Reject,
        )
        .await?;

        let restored_purpose: bool = sqlx::query_scalar(
            "select exists (
                select 1 from ai_model_binding_purpose
                where model_catalog_id = $1 and binding_purpose = 'query_answer'
             )",
        )
        .bind(model_id)
        .fetch_one(fixture.pool())
        .await?;
        let restored_binding_state: Option<String> =
            sqlx::query_scalar("select binding_state::text from ai_binding where id = $1")
                .bind(binding_id)
                .fetch_optional(fixture.pool())
                .await?;
        assert!(restored_purpose);
        assert_eq!(restored_binding_state.as_deref(), Some("disabled"));

        Ok(())
    })
    .await;

    fixture.cleanup().await?;
    result
}
