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

#[path = "support/http_response_support.rs"]
mod http_response_support;
use http_response_support::response_json;

use std::{borrow::Cow, path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
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
    domains::ai::AiBindingPurpose,
    infra::{
        persistence::{Persistence, canonical_ai_catalog_seeded, canonical_baseline_present},
        repositories::{self, catalog_repository},
    },
    integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
    },
    interfaces::http::router,
    shared::secret_encryption::SecretPurpose,
};

fn test_credential_master_key() -> String {
    STANDARD.encode([41_u8; 32])
}

const SEEDED_PROVIDER_COUNT: i64 = 8;
const SEEDED_MODEL_COUNT: i64 = 1161;
const SEEDED_PRICE_COUNT: i64 = 2183;

// Both bootstrap paths seed the five required profiles plus optional document
// understanding when a multimodal model is available.
const ENV_BOOTSTRAP_BINDING_COUNT: i64 = 6;
const BUNDLE_BOOTSTRAP_BINDING_COUNT: i64 = 6;

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
        settings.credential_encryption_write_enabled = true;

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

async fn inject_historical_library_creator_orphan(pool: &PgPool, library_id: Uuid) -> Result<()> {
    let historical_orphan_principal_id = Uuid::now_v7();
    let mut fixture = pool.begin().await?;
    sqlx::query("set local session_replication_role = 'replica'").execute(&mut *fixture).await?;
    sqlx::query(
        "update catalog_library
         set created_by_principal_id = $1
         where id = $2",
    )
    .bind(historical_orphan_principal_id)
    .bind(library_id)
    .execute(&mut *fixture)
    .await?;
    fixture.commit().await?;

    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from catalog_library library
             left join iam_principal principal
               on principal.id = library.created_by_principal_id
             where library.id = $1
               and library.created_by_principal_id is not null
               and principal.id is null",
        )
        .bind(library_id)
        .fetch_one(pool)
        .await?,
        1,
        "the fixture must contain one historical orphan creator reference"
    );
    Ok(())
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
                binding_purpose: AiBindingPurpose::ExtractGraph,
                provider_kind: Some("deepseek".to_string()),
                model_name: Some("deepseek-chat".to_string()),
            },
            UiBootstrapAiBindingDefault {
                binding_purpose: AiBindingPurpose::EmbedChunk,
                provider_kind: Some("openai".to_string()),
                model_name: Some("text-embedding-3-large".to_string()),
            },
            UiBootstrapAiBindingDefault {
                binding_purpose: AiBindingPurpose::QueryAnswer,
                provider_kind: Some("openai".to_string()),
                model_name: Some("gpt-5.4".to_string()),
            },
            UiBootstrapAiBindingDefault {
                binding_purpose: AiBindingPurpose::ExtractText,
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
async fn setup_structured_block_migration_adds_bounded_order_index() -> Result<()> {
    let settings = Settings::from_env()
        .context("failed to load settings for structured block index migration test")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect structured block index migration test postgres")?;

    let result = async {
        let migrations = Migrator::new(Path::new("./migrations"))
            .await
            .context("failed to load migration files")?;
        migrator_with_versions(&migrations, 1, 6)
            .run(&pool)
            .await
            .context("failed to apply public migration baseline")?;

        let migration = migrations
            .iter()
            .find(|migration| migration.version == 7)
            .cloned()
            .context("release-pending migration 7 is missing")?;
        Migrator {
            migrations: Cow::Owned(vec![migration]),
            ignore_missing: true,
            locking: true,
            no_tx: false,
            table_name: Cow::Borrowed("_sqlx_migrations"),
            create_schemas: Cow::Borrowed(&[]),
        }
        .run(&pool)
        .await
        .context("failed to apply release-pending migration 7")?;

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
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
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
        migrator_with_versions(&migrations, 1, 6)
            .run(&pool)
            .await
            .context("failed to apply public schema before graph index migration")?;

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
        assert_eq!(
            sqlx::query_scalar::<_, Vec<String>>(
                "select array_agg(enum_value.enumlabel::text order by enum_value.enumsortorder)
                 from pg_enum enum_value
                 join pg_type enum_type on enum_type.oid = enum_value.enumtypid
                 where enum_type.typnamespace = 'public'::regnamespace
                   and enum_type.typname = 'ai_binding_purpose'",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to inspect canonical AI binding purposes")?,
            vec![
                "extract_text".to_string(),
                "extract_graph".to_string(),
                "embed_chunk".to_string(),
                "query_compile".to_string(),
                "query_answer".to_string(),
                "agent".to_string(),
            ]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from ai_model_catalog model
                 cross join lateral jsonb_array_elements_text(
                     model.metadata_json -> 'defaultRoles'
                 ) role
                 where role in ('query_retrieve', 'rerank', 'vision', 'utility')",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to inspect canonical model roles")?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from ai_provider_catalog provider
                 cross join lateral jsonb_array_elements(
                     provider.capability_flags_json -> 'bootstrapPresets'
                 ) preset
                 where preset ->> 'purpose' in ('query_retrieve', 'rerank', 'vision', 'utility')",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to inspect canonical provider presets")?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from ai_model_catalog model
                 join ai_provider_catalog provider on provider.id = model.provider_catalog_id
                 where model.metadata_json ->> 'seedSource' = 'provider_catalog'
                   and (model.metadata_json -> 'defaultRoles') @> '[\"agent\"]'::jsonb
                   and not exists (
                       select 1
                       from jsonb_array_elements(
                           case
                               when jsonb_typeof(
                                   provider.capability_flags_json -> 'bootstrapPresets'
                               ) = 'array'
                                   then provider.capability_flags_json -> 'bootstrapPresets'
                               else '[]'::jsonb
                           end
                       ) preset
                       where preset ->> 'purpose' = 'agent'
                         and preset ->> 'modelName' = model.model_name
                   )",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to inspect seeded Agent role provenance")?,
            0,
            "a catalog-seeded Agent role must be backed by its provider's explicit Agent preset"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from ai_provider_catalog provider
                 cross join lateral jsonb_array_elements(
                     provider.capability_flags_json -> 'bootstrapPresets'
                 ) preset
                 where preset ->> 'purpose' = 'agent'
                   and not exists (
                       select 1
                       from ai_model_catalog model
                       where model.provider_catalog_id = provider.id
                         and model.model_name = preset ->> 'modelName'
                         and (model.metadata_json -> 'defaultRoles') @> '[\"agent\"]'::jsonb
                   )",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to inspect explicit Agent model eligibility")?,
            0,
            "every built-in Agent preset must target an independently Agent-eligible model"
        );
        assert!(
            sqlx::query_scalar::<_, bool>(
                "select exists (
                    select 1
                    from pg_type type
                    join pg_enum enum_value on enum_value.enumtypid = type.oid
                    where type.typnamespace = 'public'::regnamespace
                      and type.typname = 'billing_unit'
                      and enum_value.enumlabel = 'per_1m_cache_write_input_tokens'
                )",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to inspect cache-write billing unit")?
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from ai_price_catalog
                 where billing_unit = 'per_1m_cache_write_input_tokens'::billing_unit",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to inspect cache-write catalog prices")?,
            0,
            "the schema must not invent a cache-write price"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from pg_constraint
                 where convalidated
                   and (
                       (conrelid = 'public.ai_price_catalog'::regclass
                        and conname = 'ai_price_catalog_unit_price_nonnegative')
                       or
                       (conrelid = 'public.billing_usage'::regclass
                        and conname = 'billing_usage_quantity_nonnegative')
                   )",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to inspect nonnegative billing constraints")?,
            2
        );
        // The migration carries 47 compatibility keys. Four historical
        // deep-research rows are intentionally filtered from a fresh catalog;
        // the replay case below verifies that an upgraded installation which
        // still has such a row receives the policy by natural key.
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from ai_model_catalog
                 where metadata_json ? 'requestPolicy'",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to count catalog-backed provider request policies")?,
            43
        );

        assert_eq!(
            sqlx::query_as::<_, (String, String, String, String, Option<i32>)>(
                "select
                     provider.provider_kind,
                     model.model_name,
                     model.metadata_json -> 'requestPolicy' ->> 'sampling',
                     model.metadata_json -> 'requestPolicy' ->> 'toolChoice',
                     (model.metadata_json -> 'requestPolicy'
                         ->> 'defaultToolMaxOutputTokens')::integer
                 from ai_model_catalog model
                 join ai_provider_catalog provider on provider.id = model.provider_catalog_id
                 where (provider.provider_kind, model.model_name) in (
                     ('deepseek', 'deepseek-reasoner'),
                     ('gptunnel', 'o3'),
                     ('openai', 'gpt-5.5'),
                     ('openai', 'gpt-5.6-luna'),
                     ('openai', 'o1-mini')
                 )
                 order by provider.provider_kind, model.model_name",
            )
            .fetch_all(fixture.pool())
            .await
            .context("failed to inspect catalog-backed provider request policies")?,
            vec![
                (
                    "deepseek".to_string(),
                    "deepseek-reasoner".to_string(),
                    "forward".to_string(),
                    "auto_only".to_string(),
                    None,
                ),
                (
                    "gptunnel".to_string(),
                    "o3".to_string(),
                    "forward".to_string(),
                    "auto_only".to_string(),
                    None,
                ),
                (
                    "openai".to_string(),
                    "gpt-5.5".to_string(),
                    "omit".to_string(),
                    "auto_only".to_string(),
                    None,
                ),
                (
                    "openai".to_string(),
                    "gpt-5.6-luna".to_string(),
                    "omit".to_string(),
                    "required_capable".to_string(),
                    Some(65_536),
                ),
                (
                    "openai".to_string(),
                    "o1-mini".to_string(),
                    "forward".to_string(),
                    "auto_only".to_string(),
                    None,
                ),
            ]
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<bool>>(
                "select (preset -> 'extraParametersJson' ->> 'enable_thinking')::boolean
                 from ai_provider_catalog provider
                 cross join lateral jsonb_array_elements(
                     provider.capability_flags_json -> 'bootstrapPresets'
                 ) preset
                 where provider.provider_kind = 'qwen'
                   and preset ->> 'purpose' = 'query_answer'",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to inspect Qwen persisted tool-use defaults")?,
            Some(false)
        );

        let openai_provider_id = sqlx::query_scalar::<_, Uuid>(
            "select id from ai_provider_catalog where provider_kind = 'openai'",
        )
        .fetch_one(fixture.pool())
        .await?;
        let historical_model_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_model_catalog (
                id, provider_catalog_id, model_name, capability_kind,
                modality_kind, lifecycle_state, metadata_json
             ) values (
                $1, $2, 'o4-mini-deep-research-2025-06-26', 'chat',
                'text', 'active', '{\"defaultRoles\":[\"query_answer\"]}'::jsonb
             )",
        )
        .bind(historical_model_id)
        .bind(openai_provider_id)
        .execute(fixture.pool())
        .await?;

        let qwen_provider_id = sqlx::query_scalar::<_, Uuid>(
            "select id from ai_provider_catalog where provider_kind = 'qwen'",
        )
        .fetch_one(fixture.pool())
        .await?;
        let qwen_model_id = sqlx::query_scalar::<_, Uuid>(
            "select id
             from ai_model_catalog
             where provider_catalog_id = $1 and capability_kind = 'chat'
             order by model_name
             limit 1",
        )
        .bind(qwen_provider_id)
        .fetch_one(fixture.pool())
        .await?;
        let qwen_account_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_account (
                id, provider_catalog_id, label, scope_kind, credential_state
             ) values ($1, $2, 'Migration Qwen', 'instance', 'active')",
        )
        .bind(qwen_account_id)
        .bind(qwen_provider_id)
        .execute(fixture.pool())
        .await?;
        let qwen_binding_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_binding (
                id, binding_purpose, account_id, model_catalog_id,
                binding_state, scope_kind, extra_parameters_json
             ) values (
                $1, 'agent', $2, $3, 'active', 'instance',
                '{\"sentinel\":\"kept\"}'::jsonb
             )",
        )
        .bind(qwen_binding_id)
        .bind(qwen_account_id)
        .bind(qwen_model_id)
        .execute(fixture.pool())
        .await?;

        sqlx::raw_sql(include_str!("../migrations/0007_safe_catalog_defaults.sql"))
            .execute(fixture.pool())
            .await
            .context("failed to replay provider request-policy migration")?;
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select metadata_json -> 'requestPolicy'
                 from ai_model_catalog
                 where id = $1",
            )
            .bind(historical_model_id)
            .fetch_one(fixture.pool())
            .await?,
            json!({"sampling": "forward", "toolChoice": "auto_only"})
        );
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select extra_parameters_json from ai_binding where id = $1",
            )
            .bind(qwen_binding_id)
            .fetch_one(fixture.pool())
            .await?,
            json!({"enable_thinking": false, "sentinel": "kept"})
        );

        let gpt_56_models = sqlx::query_as::<_, (String, String)>(
            "select provider.provider_kind, model.model_name
             from ai_model_catalog model
             join ai_provider_catalog provider on provider.id = model.provider_catalog_id
             where model.model_name like '%gpt-5.6%'
               and model.capability_kind = 'chat'
               and model.modality_kind = 'multimodal'
               and model.lifecycle_state = 'active'
               and model.context_window = 1050000
               and model.max_output_tokens = 128000
               and model.metadata_json -> 'defaultRoles'
                   @> '[\"extract_text\",\"extract_graph\",\"query_compile\",\"query_answer\"]'::jsonb
             order by provider.provider_kind, model.model_name",
        )
        .fetch_all(fixture.pool())
        .await
        .context("failed to inspect seeded GPT-5.6 provider models")?;
        assert_eq!(
            gpt_56_models,
            vec![
                ("gptunnel".to_string(), "gpt-5.6-luna".to_string()),
                ("gptunnel".to_string(), "gpt-5.6-sol".to_string()),
                ("gptunnel".to_string(), "gpt-5.6-terra".to_string()),
                ("openai".to_string(), "gpt-5.6-luna".to_string()),
                ("openai".to_string(), "gpt-5.6-sol".to_string()),
                ("openai".to_string(), "gpt-5.6-terra".to_string()),
                ("openrouter".to_string(), "openai/gpt-5.6-luna".to_string()),
                ("openrouter".to_string(), "openai/gpt-5.6-luna-pro".to_string()),
                ("openrouter".to_string(), "openai/gpt-5.6-sol".to_string()),
                ("openrouter".to_string(), "openai/gpt-5.6-sol-pro".to_string()),
                ("openrouter".to_string(), "openai/gpt-5.6-terra".to_string()),
                ("openrouter".to_string(), "openai/gpt-5.6-terra-pro".to_string()),
                ("routerai".to_string(), "openai/gpt-5.6-luna".to_string()),
                ("routerai".to_string(), "openai/gpt-5.6-luna-pro".to_string()),
                ("routerai".to_string(), "openai/gpt-5.6-sol".to_string()),
                ("routerai".to_string(), "openai/gpt-5.6-sol-pro".to_string()),
                ("routerai".to_string(), "openai/gpt-5.6-terra".to_string()),
                ("routerai".to_string(), "openai/gpt-5.6-terra-pro".to_string()),
            ]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)
                 from ai_price_catalog price
                 join ai_model_catalog model on model.id = price.model_catalog_id
                 join ai_provider_catalog provider on provider.id = model.provider_catalog_id
                 where model.model_name like '%gpt-5.6%'
                   and provider.provider_kind in ('openai', 'openrouter', 'gptunnel', 'routerai')
                   and price.billing_unit in ('per_1m_input_tokens', 'per_1m_output_tokens')
                   and price.currency_code = 'USD'",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to inspect seeded GPT-5.6 prices")?,
            36
        );
        assert_eq!(
            sqlx::query_as::<_, (String, String, String, Option<String>)>(
                "select
                     provider.provider_kind,
                     preset ->> 'purpose',
                     preset ->> 'modelName',
                     preset -> 'extraParametersJson' ->> 'reasoning_effort'
                 from ai_provider_catalog provider
                 cross join lateral jsonb_array_elements(
                     provider.capability_flags_json -> 'bootstrapPresets'
                 ) preset
                 where provider.provider_kind in ('openai', 'openrouter', 'gptunnel', 'routerai')
                 order by provider.provider_kind, preset ->> 'purpose'",
            )
            .fetch_all(fixture.pool())
            .await
            .context("failed to inspect GPT-5.6 provider defaults")?,
            vec![
                ("gptunnel".to_string(), "agent".to_string(), "gpt-5.6-sol".to_string(), Some("none".to_string())),
                ("gptunnel".to_string(), "embed_chunk".to_string(), "text-embedding-3-large".to_string(), None),
                ("gptunnel".to_string(), "extract_graph".to_string(), "gpt-5.4-nano".to_string(), None),
                ("gptunnel".to_string(), "extract_text".to_string(), "gpt-5.6-luna".to_string(), None),
                ("gptunnel".to_string(), "query_answer".to_string(), "gpt-5.6-luna".to_string(), None),
                ("gptunnel".to_string(), "query_compile".to_string(), "gpt-5.6-luna".to_string(), None),
                ("openai".to_string(), "agent".to_string(), "gpt-5.6-sol".to_string(), Some("none".to_string())),
                ("openai".to_string(), "embed_chunk".to_string(), "text-embedding-3-large".to_string(), None),
                ("openai".to_string(), "extract_graph".to_string(), "gpt-5.4-nano".to_string(), None),
                ("openai".to_string(), "extract_text".to_string(), "gpt-5.6-luna".to_string(), None),
                ("openai".to_string(), "query_answer".to_string(), "gpt-5.6-luna".to_string(), None),
                ("openai".to_string(), "query_compile".to_string(), "gpt-5.6-luna".to_string(), None),
                ("openrouter".to_string(), "agent".to_string(), "openai/gpt-5.6-sol".to_string(), Some("none".to_string())),
                ("openrouter".to_string(), "embed_chunk".to_string(), "openai/text-embedding-3-large".to_string(), None),
                ("openrouter".to_string(), "extract_graph".to_string(), "openai/gpt-5.4-nano".to_string(), None),
                ("openrouter".to_string(), "extract_text".to_string(), "openai/gpt-5.6-luna".to_string(), None),
                ("openrouter".to_string(), "query_answer".to_string(), "openai/gpt-5.6-luna".to_string(), None),
                ("openrouter".to_string(), "query_compile".to_string(), "openai/gpt-5.6-luna".to_string(), None),
                ("routerai".to_string(), "agent".to_string(), "openai/gpt-5.6-sol".to_string(), Some("none".to_string())),
                ("routerai".to_string(), "embed_chunk".to_string(), "openai/text-embedding-3-large".to_string(), None),
                ("routerai".to_string(), "extract_graph".to_string(), "openai/gpt-5.4-nano".to_string(), None),
                ("routerai".to_string(), "extract_text".to_string(), "openai/gpt-5.6-luna".to_string(), None),
                ("routerai".to_string(), "query_answer".to_string(), "openai/gpt-5.6-luna".to_string(), None),
                ("routerai".to_string(), "query_compile".to_string(), "openai/gpt-5.6-luna".to_string(), None),
            ]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)
                 from ai_provider_catalog provider
                 cross join lateral jsonb_array_elements(
                     provider.capability_flags_json -> 'bootstrapPresets'
                 ) preset
                 where provider.provider_kind in ('openai', 'openrouter', 'gptunnel', 'routerai')
                   and preset ->> 'purpose' = 'agent'
                   and (preset ->> 'maxOutputTokensOverride')::integer = 65536",
            )
            .fetch_one(fixture.pool())
            .await
            .context("failed to inspect GPT-5.6 agent output limits")?,
            4
        );
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
            .post_json("/v1/iam/bootstrap/setup", &payload)
            .await
            .context("bootstrap setup request failed")?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_json(response).await?;
        assert_eq!(body["code"], "bad_request");

        let status_response = fixture
            .get("/v1/iam/bootstrap/status")
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
            .post_json("/v1/iam/bootstrap/setup", &payload)
            .await
            .context("env-backed bootstrap setup request failed")?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(header::SET_COOKIE));

        let status_response = fixture
            .get("/v1/iam/bootstrap/status")
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
async fn env_account_sync_is_deferred_during_dual_reader_rollout() -> Result<()> {
    let fixture =
        GreenfieldBootstrapFixture::create_with_ui_bootstrap_ai_setup(Some(UiBootstrapAiSetup {
            provider_secrets: vec![UiBootstrapAiProviderSecret {
                provider_kind: "openai".to_string(),
                api_key: "test-bootstrap-token".to_string(), // pragma: allowlist secret
            }],
            binding_defaults: vec![],
        }))
        .await?;

    let result = async {
        assert_eq!(
            fixture
                .state
                .canonical_services
                .ai_catalog
                .ensure_env_ai_accounts(&fixture.state)
                .await?,
            1,
        );
        let before = sqlx::query_as::<_, (Option<String>, String, Option<String>)>(
            "select api_key, credential_state::text, base_url
             from ai_account
             where label = 'Bootstrap OpenAI'",
        )
        .fetch_one(fixture.pool())
        .await?;

        let mut rollout_state = fixture.state.clone();
        rollout_state.settings.credential_encryption_write_enabled = false;
        rollout_state.ui_bootstrap_ai_setup = Some(UiBootstrapAiSetup {
            provider_secrets: vec![UiBootstrapAiProviderSecret {
                provider_kind: "openai".to_string(),
                api_key: "test-rotated-bootstrap-token".to_string(), // pragma: allowlist secret
            }],
            binding_defaults: vec![],
        });

        assert_eq!(
            rollout_state
                .canonical_services
                .ai_catalog
                .ensure_env_ai_accounts(&rollout_state)
                .await?,
            0,
        );
        assert_eq!(scalar_count(fixture.pool(), "ai_account").await?, 1);
        let after = sqlx::query_as::<_, (Option<String>, String, Option<String>)>(
            "select api_key, credential_state::text, base_url
             from ai_account
             where label = 'Bootstrap OpenAI'",
        )
        .fetch_one(fixture.pool())
        .await?;
        assert_eq!(after, before, "deferred sync must not mutate credential state");
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn env_account_sync_rotates_canonical_accounts_in_every_scope() -> Result<()> {
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
            account_id,
            "library",
            Some(workspace.id),
            Some(library.id),
            provider_id,
            "Bootstrap OpenAI",
            Some(&stale_encrypted_key),
            None,
            None,
        )
        .await?;
        sqlx::query("update ai_account set credential_state = 'revoked' where id = $1")
            .bind(library_account.id)
            .execute(fixture.pool())
            .await?;

        assert_eq!(
            fixture
                .state
                .canonical_services
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
        let status_response = fixture
            .get("/v1/iam/bootstrap/status")
            .await
            .context("bootstrap status request failed")?;
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
                            && binding["modelName"] == "gpt-5.4-nano"
                    })
                })
        }));
        for bundle in binding_bundles {
            let bindings = bundle["bindings"]
                .as_array()
                .context("bootstrap bundle is missing its bindings array")?;
            for purpose in [
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
            .post_json("/v1/iam/bootstrap/setup", &payload)
            .await
            .context("provider bundle bootstrap setup request failed")?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(header::SET_COOKIE));

        let status_response = fixture
            .get("/v1/iam/bootstrap/status")
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
            .get("/v1/iam/bootstrap/status")
            .await
            .context("bootstrap status request failed")?;
        let status_body = response_json(status_response).await?;
        let binding_bundles = status_body["aiSetup"]["bindingBundles"]
            .as_array()
            .context("bootstrap status is missing the binding bundles array")?;
        assert!(!binding_bundles.iter().any(|bundle| bundle["providerKind"] == "deepseek"));

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
            .post_json("/v1/iam/bootstrap/setup", &payload)
            .await
            .context("deepseek provider bundle bootstrap setup request failed")?;
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
        assert_eq!(scalar_count(fixture.pool(), "ai_account").await?, 2);
        assert_eq!(scalar_count(fixture.pool(), "ai_binding").await?, ENV_BOOTSTRAP_BINDING_COUNT);

        let payload = json!({
            "login": "admin",
            "displayName": "Admin",
            "password": "super-secret-password",
        });

        let response = fixture
            .post_json("/v1/iam/bootstrap/setup", &payload)
            .await
            .context("orphaned bootstrap recovery request failed")?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(header::SET_COOKIE));

        let status_response = fixture
            .get("/v1/iam/bootstrap/status")
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

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn consolidated_migration_0007_repairs_orphan_creator_before_generation_updates() -> Result<()>
{
    let settings = Settings::from_env().context("failed to load settings")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect provider request policy migration test postgres")?;

    let result = async {
        let migrations = Migrator::new(Path::new("./migrations"))
            .await
            .context("failed to load migration files")?;
        migrator_with_versions(&migrations, 1, 6)
            .run(&pool)
            .await
            .context("failed to apply schema before provider request policy migration")?;

        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        sqlx::query(
            "insert into catalog_workspace (id, slug, display_name)
             values ($1, 'request-policy-migration', 'Request policy migration')",
        )
        .bind(workspace_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into catalog_library (id, workspace_id, slug, display_name)
             values ($1, $2, 'request-policy-migration', 'Request policy migration')",
        )
        .bind(library_id)
        .bind(workspace_id)
        .execute(&pool)
        .await?;
        let (provider_id, model_id) = sqlx::query_as::<_, (Uuid, Uuid)>(
            "select provider.id, model.id
             from ai_provider_catalog provider
             join ai_model_catalog model on model.provider_catalog_id = provider.id
             where provider.provider_kind = 'openai'
               and model.model_name = 'gpt-5.6-sol'
               and model.capability_kind = 'chat'::ai_model_capability_kind",
        )
        .fetch_one(&pool)
        .await?;
        let account_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_account (
                id, provider_catalog_id, label, credential_state, scope_kind
             ) values ($1, $2, 'Request policy migration', 'active', 'instance')",
        )
        .bind(account_id)
        .bind(provider_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into ai_binding (
                id, workspace_id, library_id, binding_purpose, account_id,
                model_catalog_id, binding_state, scope_kind, extra_parameters_json
             ) values (
                $1, $2, $3, 'query_answer', $4, $5, 'active', 'library', '{}'
             )",
        )
        .bind(Uuid::now_v7())
        .bind(workspace_id)
        .bind(library_id)
        .bind(account_id)
        .bind(model_id)
        .execute(&pool)
        .await?;
        inject_historical_library_creator_orphan(&pool, library_id).await?;

        migrator_with_versions(&migrations, 1, 7)
            .run(&pool)
            .await
            .context("failed to apply provider request policy migration")?;

        assert_eq!(
            sqlx::query_scalar::<_, Option<Uuid>>(
                "select created_by_principal_id
                 from catalog_library
                 where id = $1",
            )
            .bind(library_id)
            .fetch_one(&pool)
            .await?,
            None,
            "migration must repair stale library creator references before generation updates"
        );
        assert!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from ai_model_catalog
                 where metadata_json ? 'requestPolicy'",
            )
            .fetch_one(&pool)
            .await?
                > 0,
            "the migration fixture must exercise at least one request-policy model update"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn consolidated_migration_0007_is_idempotent_and_does_not_seed_cache_write_prices()
-> Result<()> {
    let settings = Settings::from_env().context("failed to load settings")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect cache-write billing migration test postgres")?;

    let result = async {
        let migrations = Migrator::new(Path::new("./migrations"))
            .await
            .context("failed to load migration files")?;
        migrator_with_versions(&migrations, 1, 6)
            .run(&pool)
            .await
            .context("failed to apply public migration baseline")?;
        let migration = include_str!("../migrations/0007_safe_catalog_defaults.sql");
        sqlx::raw_sql(migration).execute(&pool).await?;
        sqlx::raw_sql(migration).execute(&pool).await?;

        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from pg_type type
                 join pg_enum enum_value on enum_value.enumtypid = type.oid
                 where type.typnamespace = 'public'::regnamespace
                   and type.typname = 'billing_unit'
                   and enum_value.enumlabel = 'per_1m_cache_write_input_tokens'",
            )
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from ai_price_catalog
                 where billing_unit = 'per_1m_cache_write_input_tokens'::billing_unit",
            )
            .fetch_one(&pool)
            .await?,
            0
        );
        let negative_price = sqlx::query(
            "update ai_price_catalog
             set unit_price = -1
             where id = (select id from ai_price_catalog order by id limit 1)",
        )
        .execute(&pool)
        .await;
        assert!(negative_price.is_err(), "explicit catalog prices must be nonnegative");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from pg_constraint
                 where convalidated
                   and (
                       (conrelid = 'public.ai_price_catalog'::regclass
                        and conname = 'ai_price_catalog_unit_price_nonnegative')
                       or
                       (conrelid = 'public.billing_usage'::regclass
                        and conname = 'billing_usage_quantity_nonnegative')
                   )",
            )
            .fetch_one(&pool)
            .await?,
            2
        );
        Ok(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn consolidated_migration_0007_collapses_obsolete_bindings_and_preserves_references()
-> Result<()> {
    let settings = Settings::from_env().context("failed to load settings")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect canonical binding migration test postgres")?;

    let result = async {
        let migrations = Migrator::new(Path::new("./migrations"))
            .await
            .context("failed to load migration files")?;
        migrator_with_versions(&migrations, 1, 6)
            .run(&pool)
            .await
            .context("failed to apply public schema before canonical binding migration")?;

        let provider_id = Uuid::now_v7();
        let dedicated_agent_provider_id = Uuid::now_v7();
        let malformed_provider_id = Uuid::now_v7();
        let missing_presets_provider_id = Uuid::now_v7();
        let model_id = Uuid::now_v7();
        let answer_model_id = Uuid::now_v7();
        let dedicated_answer_model_id = Uuid::now_v7();
        let dedicated_agent_model_id = Uuid::now_v7();
        let legacy_seeded_extra_model_id = Uuid::now_v7();
        let legacy_unknown_seeded_model_id = Uuid::now_v7();
        let obsolete_only_model_id = Uuid::now_v7();
        let rerank_only_model_id = Uuid::now_v7();
        let non_string_only_model_id = Uuid::now_v7();
        let account_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_provider_catalog (
                id, provider_kind, display_name, api_style, lifecycle_state,
                capability_flags_json
             ) values (
                $1, 'migration-provider', 'Migration provider',
                'openai_compatible', 'active', $2
             )",
        )
        .bind(provider_id)
        .bind(json!({
            "sentinel": {"kept": true},
            "capabilities": {"chat": "supported", "tools": "supported"},
            "bootstrapPresets": [
                {"purpose": "embed_chunk", "modelName": "canonical-embedding", "marker": "canonical"},
                {"purpose": "query_retrieve", "modelName": "obsolete-embedding", "marker": "obsolete"},
                {"purpose": "query_compile", "modelName": "invalid-compile-model", "maxOutputTokensOverride": 1.5, "marker": "invalid-canonical"},
                {"purpose": "rerank", "modelName": "compile-model"},
                {"purpose": "vision", "modelName": "document-model"},
                {"purpose": "extract_graph", "modelName": "\t\n"},
                {"purpose": "extract_graph", "modelName": "graph-model", "extraParametersJson": []},
                {"purpose": "agent", "modelName": "invalid-agent-model", "systemPrompt": 42},
                {"purpose": "query_answer", "modelName": "answer-model"},
                {"purpose": "query_answer", "modelName": "invalid-answer-model", "topP": "high"},
                {"purpose": "utility", "modelName": "obsolete-utility-model"},
                {"purpose": "custom_profile", "modelName": "unsupported-profile-model"},
                {"purpose": 42, "modelName": "non-string-purpose-model"},
                {"extension": "first-untyped-preset"},
                "opaque-extension-preset",
                {"extension": "second-untyped-preset"}
            ]
        }))
        .execute(&pool)
        .await?;
        let dedicated_agent_provider_flags = json!({
            "sentinel": {"dedicated": true},
            "capabilities": {"chat": "supported", "tools": "unknown"},
            "bootstrapPresets": [
                {
                    "purpose": "query_answer",
                    "modelName": "dedicated-answer-model",
                    "marker": "answer"
                },
                {
                    "purpose": "agent",
                    "modelName": "dedicated-agent-model",
                    "systemPrompt": "Use available tools when required.",
                    "temperature": 0.1,
                    "topP": 0.8,
                    "maxOutputTokensOverride": 2048,
                    "extraParametersJson": {"tool_choice": "auto"},
                    "marker": "dedicated"
                }
            ]
        });
        sqlx::query(
            "insert into ai_provider_catalog (
                id, provider_kind, display_name, api_style, lifecycle_state,
                capability_flags_json
             ) values (
                $1, 'migration-dedicated-agent-provider', 'Dedicated agent migration provider',
                'openai_compatible', 'active', $2
             )",
        )
        .bind(dedicated_agent_provider_id)
        .bind(&dedicated_agent_provider_flags)
        .execute(&pool)
        .await?;
        let malformed_provider_flags =
            json!({"bootstrapPresets": {"purpose": "vision"}, "sentinel": "malformed"});
        let expected_malformed_provider_flags =
            json!({"bootstrapPresets": [], "sentinel": "malformed"});
        let missing_presets_provider_flags = json!({"sentinel": "missing"});
        sqlx::query(
            "insert into ai_provider_catalog (
                id, provider_kind, display_name, api_style, lifecycle_state,
                capability_flags_json
             ) values
                ($1, 'migration-malformed-provider', 'Malformed migration provider',
                 'openai_compatible', 'active', $3),
                ($2, 'migration-missing-presets-provider', 'Missing presets migration provider',
                 'openai_compatible', 'active', $4)",
        )
        .bind(malformed_provider_id)
        .bind(missing_presets_provider_id)
        .bind(&malformed_provider_flags)
        .bind(&missing_presets_provider_flags)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into ai_model_catalog (
                id, provider_catalog_id, model_name, capability_kind,
                modality_kind, lifecycle_state, metadata_json
             ) values
                ($1, $2, 'migration-model', 'chat', 'multimodal', 'active', $3),
                ($4, $2, 'obsolete-only-model', 'chat', 'text', 'active', $5),
                ($6, $2, 'rerank-only-model', 'chat', 'text', 'active', $7),
                ($8, $2, 'non-string-only-model', 'chat', 'text', 'active', $9),
                ($10, $2, 'answer-model', 'chat', 'text', 'active', $11)",
        )
        .bind(model_id)
        .bind(provider_id)
        .bind(json!({
            "defaultRoles": [
                "embed_chunk",
                "query_retrieve",
                "rerank",
                "query_compile",
                "vision",
                "extract_text",
                "utility",
                "custom_profile",
                42
            ],
            "sentinel": {"kept": true}
        }))
        .bind(obsolete_only_model_id)
        .bind(json!({
            "defaultRoles": ["utility", "custom_profile"],
            "sentinel": {"kept": true}
        }))
        .bind(rerank_only_model_id)
        .bind(json!({
            "defaultRoles": ["rerank"],
            "sentinel": {"kept": true}
        }))
        .bind(non_string_only_model_id)
        .bind(json!({
            "defaultRoles": [42],
            "sentinel": {"kept": true}
        }))
        .bind(answer_model_id)
        .bind(json!({
            "defaultRoles": ["query_answer"],
            "sentinel": {"materializeAgent": true}
        }))
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into ai_model_catalog (
                id, provider_catalog_id, model_name, capability_kind,
                modality_kind, lifecycle_state, metadata_json
             ) values
                ($1, $2, 'dedicated-answer-model', 'chat', 'text', 'active', $3),
                ($4, $2, 'dedicated-agent-model', 'chat', 'text', 'active', $5)",
        )
        .bind(dedicated_answer_model_id)
        .bind(dedicated_agent_provider_id)
        .bind(json!({"defaultRoles": ["query_answer"]}))
        .bind(dedicated_agent_model_id)
        .bind(json!({"defaultRoles": ["agent"]}))
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into ai_model_catalog (
                id, provider_catalog_id, model_name, capability_kind,
                modality_kind, lifecycle_state, metadata_json
             ) values
                ($1, $2, 'legacy-seeded-extra-model', 'chat', 'text', 'active', $3),
                ($4, $5, 'legacy-unknown-seeded-model', 'chat', 'text', 'active', $6)",
        )
        .bind(legacy_seeded_extra_model_id)
        .bind(provider_id)
        .bind(json!({
            "defaultRoles": ["query_answer", "agent"],
            "seedSource": "provider_catalog"
        }))
        .bind(legacy_unknown_seeded_model_id)
        .bind(dedicated_agent_provider_id)
        .bind(json!({
            "defaultRoles": ["query_answer", "agent"],
            "seedSource": "provider_catalog"
        }))
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into ai_account (
                id, provider_catalog_id, label, credential_state, scope_kind
             ) values ($1, $2, 'Migration account', 'active', 'instance')",
        )
        .bind(account_id)
        .bind(provider_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into catalog_workspace (id, slug, display_name)
             values ($1, 'binding-migration', 'Binding migration')",
        )
        .bind(workspace_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into catalog_library (id, workspace_id, slug, display_name)
             values ($1, $2, 'binding-migration', 'Binding migration')",
        )
        .bind(library_id)
        .bind(workspace_id)
        .execute(&pool)
        .await?;

        let canonical_embedding_id = Uuid::now_v7();
        let duplicate_query_retrieve_id = Uuid::now_v7();
        let standalone_query_retrieve_id = Uuid::now_v7();
        let standalone_rerank_id = Uuid::now_v7();
        let standalone_vision_id = Uuid::now_v7();
        let utility_id = Uuid::now_v7();
        for (id, purpose, scope, workspace, library) in [
            (
                canonical_embedding_id,
                "embed_chunk",
                "instance",
                None,
                None,
            ),
            (
                duplicate_query_retrieve_id,
                "query_retrieve",
                "instance",
                None,
                None,
            ),
            (
                standalone_query_retrieve_id,
                "query_retrieve",
                "workspace",
                Some(workspace_id),
                None,
            ),
            (
                standalone_rerank_id,
                "rerank",
                "workspace",
                Some(workspace_id),
                None,
            ),
            (
                standalone_vision_id,
                "vision",
                "workspace",
                Some(workspace_id),
                None,
            ),
            (
                utility_id,
                "utility",
                "library",
                Some(workspace_id),
                Some(library_id),
            ),
        ] {
            sqlx::query(
                "insert into ai_binding (
                    id, workspace_id, library_id, binding_purpose, account_id,
                    model_catalog_id, binding_state, scope_kind, extra_parameters_json
                 ) values (
                    $1, $2, $3, $4::ai_binding_purpose, $5,
                    $6, 'active', $7::ai_scope_kind, '{}'
                 )",
            )
            .bind(id)
            .bind(workspace)
            .bind(library)
            .bind(purpose)
            .bind(account_id)
            .bind(model_id)
            .bind(scope)
            .execute(&pool)
            .await?;
        }

        let validation_id = Uuid::now_v7();
        let billing_call_id = Uuid::now_v7();
        let conversation_id = Uuid::now_v7();
        let query_execution_id = Uuid::now_v7();
        let runtime_execution_id = Uuid::now_v7();
        let stage_record_id = Uuid::now_v7();
        let action_record_id = Uuid::now_v7();
        sqlx::query(
            "insert into ai_binding_validation (id, binding_id, validation_state)
             values ($1, $2, 'succeeded')",
        )
        .bind(validation_id)
        .bind(duplicate_query_retrieve_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into billing_provider_call (
                id, workspace_id, library_id, binding_id,
                owning_execution_kind, owning_execution_id,
                provider_catalog_id, model_catalog_id, call_kind
             ) values (
                $1, $2, $3, $4, 'binding_validation', $5, $6, $7, 'query_retrieve'
             )",
        )
        .bind(billing_call_id)
        .bind(workspace_id)
        .bind(library_id)
        .bind(duplicate_query_retrieve_id)
        .bind(validation_id)
        .bind(provider_id)
        .bind(model_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into query_conversation (id, workspace_id, library_id)
             values ($1, $2, $3)",
        )
        .bind(conversation_id)
        .bind(workspace_id)
        .bind(library_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into query_execution (
                id, workspace_id, library_id, conversation_id,
                context_bundle_id, binding_id, query_text
             ) values ($1, $2, $3, $4, $5, $6, 'Synthetic migration query')",
        )
        .bind(query_execution_id)
        .bind(workspace_id)
        .bind(library_id)
        .bind(conversation_id)
        .bind(Uuid::now_v7())
        .bind(duplicate_query_retrieve_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into runtime_execution (
                id, owner_kind, owner_id, task_kind, surface_kind,
                contract_name, contract_version, lifecycle_state,
                turn_budget, parallel_action_limit
             ) values (
                $1, 'query_execution', $2, 'query_plan', 'internal',
                'migration-test', '1', 'completed', 1, 1
             )",
        )
        .bind(runtime_execution_id)
        .bind(query_execution_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into runtime_stage_record (
                id, runtime_execution_id, stage_kind, stage_ordinal,
                attempt_no, stage_state
             ) values ($1, $2, 'plan', 0, 1, 'completed')",
        )
        .bind(stage_record_id)
        .bind(runtime_execution_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into runtime_action_record (
                id, runtime_execution_id, stage_record_id, action_kind,
                action_ordinal, action_state, provider_binding_id
             ) values ($1, $2, $3, 'model_request', 0, 'completed', $4)",
        )
        .bind(action_record_id)
        .bind(runtime_execution_id)
        .bind(stage_record_id)
        .bind(duplicate_query_retrieve_id)
        .execute(&pool)
        .await?;

        // Some pre-release databases were restored with constraint triggers
        // disabled and retained a creator id whose principal no longer exists.
        // Reproduce that historical state immediately before migration 0012
        // while keeping the FK itself present and validated. Its AI binding
        // changes advance this library generation and recheck the stale FK.
        inject_historical_library_creator_orphan(&pool, library_id).await?;

        migrations
            .run(&pool)
            .await
            .context("failed to apply canonical binding migration")?;

        assert_eq!(
            sqlx::query_scalar::<_, Option<Uuid>>(
                "select created_by_principal_id
                 from catalog_library
                 where id = $1",
            )
            .bind(library_id)
            .fetch_one(&pool)
            .await?,
            None,
            "migration must restore the FK's ON DELETE SET NULL semantics for historical orphans"
        );

        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint from ai_binding where id = $1",
            )
            .bind(duplicate_query_retrieve_id)
            .fetch_one(&pool)
            .await?,
            0,
            "canonical row must win a same-scope collision"
        );
        assert_eq!(
            sqlx::query_as::<_, (Uuid, String)>(
                "select id, binding_purpose::text
                 from ai_binding
                 where id in ($1, $2, $3)
                 order by binding_purpose::text",
            )
            .bind(standalone_query_retrieve_id)
            .bind(standalone_rerank_id)
            .bind(standalone_vision_id)
            .fetch_all(&pool)
            .await?,
            vec![
                (standalone_query_retrieve_id, "embed_chunk".to_string()),
                (standalone_vision_id, "extract_text".to_string()),
                (standalone_rerank_id, "query_compile".to_string()),
            ]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint from ai_binding where id = $1",
            )
            .bind(utility_id)
            .fetch_one(&pool)
            .await?,
            0,
            "non-executable utility bindings must be removed"
        );
        assert_eq!(
            sqlx::query_scalar::<_, Uuid>(
                "select binding_id from ai_binding_validation where id = $1",
            )
            .bind(validation_id)
            .fetch_one(&pool)
            .await?,
            canonical_embedding_id
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<Uuid>>(
                "select binding_id from billing_provider_call where id = $1",
            )
            .bind(billing_call_id)
            .fetch_one(&pool)
            .await?,
            Some(canonical_embedding_id)
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "select call_kind from billing_provider_call where id = $1",
            )
            .bind(billing_call_id)
            .fetch_one(&pool)
            .await?,
            "query_embedding"
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<Uuid>>(
                "select binding_id from query_execution where id = $1",
            )
            .bind(query_execution_id)
            .fetch_one(&pool)
            .await?,
            Some(canonical_embedding_id)
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<Uuid>>(
                "select provider_binding_id from runtime_action_record where id = $1",
            )
            .bind(action_record_id)
            .fetch_one(&pool)
            .await?,
            Some(canonical_embedding_id)
        );

        let expected_model_metadata = json!({
            "defaultRoles": ["embed_chunk", "query_compile", "extract_text"],
            "sentinel": {"kept": true}
        });
        let expected_provider_flags = json!({
            "sentinel": {"kept": true},
            "capabilities": {"chat": "supported", "tools": "supported"},
            "bootstrapPresets": [
                {"purpose": "embed_chunk", "modelName": "canonical-embedding", "marker": "canonical"},
                {"purpose": "query_compile", "modelName": "compile-model"},
                {"purpose": "extract_text", "modelName": "document-model"},
                {"purpose": "query_answer", "modelName": "answer-model"},
                {"purpose": "agent", "modelName": "answer-model"}
            ]
        });
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select metadata_json from ai_model_catalog where id = $1",
            )
            .bind(model_id)
            .fetch_one(&pool)
            .await?,
            expected_model_metadata
        );
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select metadata_json from ai_model_catalog where id = $1",
            )
            .bind(answer_model_id)
            .fetch_one(&pool)
            .await?,
            json!({
                "defaultRoles": ["query_answer", "query_compile", "agent"],
                "sentinel": {"materializeAgent": true}
            }),
            "the one-time migration must persist independent Agent eligibility"
        );
        assert_eq!(
            sqlx::query_as::<_, (String, Value)>(
                "select lifecycle_state::text, metadata_json
                 from ai_model_catalog
                 where id = $1",
            )
            .bind(obsolete_only_model_id)
            .fetch_one(&pool)
            .await?,
            (
                "active".to_string(),
                json!({
                    "defaultRoles": ["query_compile"],
                    "sentinel": {"kept": true}
                }),
            )
        );
        let rediscovered_obsolete_model =
            repositories::ai_repository::upsert_model_catalog(
                &pool,
                provider_id,
                "obsolete-only-model",
                "chat",
                "text",
                json!({"defaultRoles": ["query_answer"]}),
            )
            .await?;
        assert_eq!(rediscovered_obsolete_model.lifecycle_state, "active");
        assert_eq!(
            rediscovered_obsolete_model.metadata_json,
            json!({
                "defaultRoles": ["query_compile"],
                "sentinel": {"kept": true}
            })
        );
        assert_eq!(
            sqlx::query_as::<_, (String, Value)>(
                "select lifecycle_state::text, metadata_json
                 from ai_model_catalog
                 where id = $1",
            )
            .bind(rerank_only_model_id)
            .fetch_one(&pool)
            .await?,
            (
                "active".to_string(),
                json!({
                    "defaultRoles": ["query_compile"],
                    "sentinel": {"kept": true}
                }),
            )
        );
        assert_eq!(
            sqlx::query_as::<_, (String, Value)>(
                "select lifecycle_state::text, metadata_json
                 from ai_model_catalog
                 where id = $1",
            )
            .bind(non_string_only_model_id)
            .fetch_one(&pool)
            .await?,
            (
                "disabled".to_string(),
                json!({
                    "defaultRoles": [],
                    "sentinel": {"kept": true}
                }),
            )
        );
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select capability_flags_json from ai_provider_catalog where id = $1",
            )
            .bind(provider_id)
            .fetch_one(&pool)
            .await?,
            expected_provider_flags
        );
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select capability_flags_json from ai_provider_catalog where id = $1",
            )
            .bind(dedicated_agent_provider_id)
            .fetch_one(&pool)
            .await?,
            dedicated_agent_provider_flags,
            "an explicit Agent preset must win byte-for-byte over materialization"
        );
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select metadata_json from ai_model_catalog where id = $1",
            )
            .bind(dedicated_answer_model_id)
            .fetch_one(&pool)
            .await?,
            json!({"defaultRoles": ["query_answer", "query_compile"]}),
            "provider request-policy enrichment must preserve the absence of Agent eligibility"
        );
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select metadata_json from ai_model_catalog where id = $1",
            )
            .bind(dedicated_agent_model_id)
            .fetch_one(&pool)
            .await?,
            json!({"defaultRoles": ["agent", "query_compile"]}),
            "the consolidated migration must preserve explicit Agent eligibility and add rerank capability"
        );
        for model_id in [legacy_seeded_extra_model_id, legacy_unknown_seeded_model_id] {
            assert_eq!(
                sqlx::query_scalar::<_, Value>(
                    "select metadata_json from ai_model_catalog where id = $1",
                )
                .bind(model_id)
                .fetch_one(&pool)
                .await?,
                json!({
                    "defaultRoles": ["query_answer", "query_compile"],
                    "seedSource": "provider_catalog"
                }),
                "legacy catalog-wide Agent inference must be removed unless a dedicated \
                 Agent preset or binding backs that model"
            );
        }

        let migrated_query_answer_binding_id = Uuid::now_v7();
        let migrated_agent_binding_id = Uuid::now_v7();
        for (binding_id, purpose) in [
            (migrated_query_answer_binding_id, "query_answer"),
            (migrated_agent_binding_id, "agent"),
        ] {
            sqlx::query(
                "insert into ai_binding (
                    id, workspace_id, library_id, binding_purpose, account_id,
                    model_catalog_id, binding_state, scope_kind, extra_parameters_json
                 ) values ($1, $2, $3, $4::ai_binding_purpose, $5, $6,
                           'active', 'library', '{}')",
            )
            .bind(binding_id)
            .bind(workspace_id)
            .bind(library_id)
            .bind(purpose)
            .bind(account_id)
            .bind(answer_model_id)
            .execute(&pool)
            .await?;
        }
        assert_eq!(
            sqlx::query_scalar::<_, Vec<String>>(
                "select array_agg(binding_purpose::text order by binding_purpose::text)
                 from ai_binding
                 where id in ($1, $2)",
            )
            .bind(migrated_query_answer_binding_id)
            .bind(migrated_agent_binding_id)
            .fetch_one(&pool)
            .await?,
            vec!["agent".to_string(), "query_answer".to_string()],
            "runtime bindings must persist Agent and QueryAnswer as separate purposes"
        );
        for (provider_id, expected_flags) in [
            (malformed_provider_id, &expected_malformed_provider_flags),
            (missing_presets_provider_id, &missing_presets_provider_flags),
        ] {
            assert_eq!(
                &sqlx::query_scalar::<_, Value>(
                    "select capability_flags_json from ai_provider_catalog where id = $1",
                )
                .bind(provider_id)
                .fetch_one(&pool)
                .await?,
                expected_flags,
                "migration must remove malformed preset shapes and preserve an absent field"
            );
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from ai_provider_catalog provider
                 where (
                         provider.capability_flags_json ? 'bootstrapPresets'
                         and jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets')
                             is distinct from 'array'
                     )
                    or exists (
                        select 1
                        from jsonb_array_elements(
                            case
                                when jsonb_typeof(
                                    provider.capability_flags_json -> 'bootstrapPresets'
                                ) = 'array'
                                    then provider.capability_flags_json -> 'bootstrapPresets'
                                else '[]'::jsonb
                            end
                        ) preset
                        where jsonb_typeof(preset) is distinct from 'object'
                           or jsonb_typeof(preset -> 'purpose') is distinct from 'string'
                           or preset ->> 'purpose' not in (
                               'extract_text', 'extract_graph', 'embed_chunk',
                               'query_compile', 'query_answer', 'agent'
                           )
                           or jsonb_typeof(preset -> 'modelName') is distinct from 'string'
                           or preset ->> 'modelName' !~ '[^[:space:]]'
                           or case
                               when not (preset ? 'temperature') then false
                               when jsonb_typeof(preset -> 'temperature') = 'null' then false
                               when jsonb_typeof(preset -> 'temperature') = 'number' then
                                   abs((preset ->> 'temperature')::numeric) >
                                       1.7976931348623157e308::numeric
                               else true
                           end
                           or case
                               when not (preset ? 'topP') then false
                               when jsonb_typeof(preset -> 'topP') = 'null' then false
                               when jsonb_typeof(preset -> 'topP') = 'number' then
                                   abs((preset ->> 'topP')::numeric) >
                                       1.7976931348623157e308::numeric
                               else true
                           end
                           or case
                               when not (preset ? 'maxOutputTokensOverride') then false
                               when jsonb_typeof(preset -> 'maxOutputTokensOverride') = 'null'
                                   then false
                               when jsonb_typeof(preset -> 'maxOutputTokensOverride') = 'number'
                                   then preset ->> 'maxOutputTokensOverride' !~ '^-?[0-9]+$'
                                       or (preset ->> 'maxOutputTokensOverride')::numeric
                                           not between -2147483648 and 2147483647
                               else true
                           end
                           or case
                               when not (preset ? 'systemPrompt') then false
                               else jsonb_typeof(preset -> 'systemPrompt')
                                   not in ('string', 'null')
                           end
                           or case
                               when not (preset ? 'extraParametersJson') then false
                               else jsonb_typeof(preset -> 'extraParametersJson')
                                   is distinct from 'object'
                           end
                    )
                    or exists (
                        select 1
                        from jsonb_array_elements(
                            case
                                when jsonb_typeof(
                                    provider.capability_flags_json -> 'bootstrapPresets'
                                ) = 'array'
                                    then provider.capability_flags_json -> 'bootstrapPresets'
                                else '[]'::jsonb
                            end
                        ) preset
                        group by preset ->> 'purpose'
                        having count(*) > 1
                    )",
            )
            .fetch_one(&pool)
            .await?,
            0,
            "every remaining provider preset must use the canonical six-purpose contract"
        );
        assert_eq!(
            sqlx::query_scalar::<_, Vec<String>>(
                "select array_agg(enum_value.enumlabel::text order by enum_value.enumsortorder)
                 from pg_enum enum_value
                 join pg_type enum_type on enum_type.oid = enum_value.enumtypid
                 where enum_type.typnamespace = 'public'::regnamespace
                   and enum_type.typname = 'ai_binding_purpose'",
            )
            .fetch_one(&pool)
            .await?,
            vec![
                "extract_text".to_string(),
                "extract_graph".to_string(),
                "embed_chunk".to_string(),
                "query_compile".to_string(),
                "query_answer".to_string(),
                "agent".to_string(),
            ]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from pg_constraint
                 where convalidated
                   and conname in (
                       'ai_binding_validation_binding_id_fkey',
                       'billing_provider_call_binding_id_fkey',
                       'query_execution_binding_id_fkey',
                       'runtime_action_record_provider_binding_id_fkey'
                   )",
            )
            .fetch_one(&pool)
            .await?,
            4
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from pg_index index_state
                 join pg_class index_relation on index_relation.oid = index_state.indexrelid
                 where index_state.indisvalid
                   and index_state.indisunique
                   and index_relation.relname in (
                       'ai_binding_instance_purpose_key',
                       'ai_binding_workspace_purpose_key',
                       'ai_binding_library_purpose_key'
                   )",
            )
            .fetch_one(&pool)
            .await?,
            3
        );

        let conflicting_binding = sqlx::query(
            "insert into ai_binding (
                id, workspace_id, binding_purpose, account_id, model_catalog_id,
                binding_state, scope_kind, extra_parameters_json
             ) values ($1, $2, 'embed_chunk', $3, $4, 'active', 'workspace', '{}')
             on conflict do nothing",
        )
        .bind(Uuid::now_v7())
        .bind(workspace_id)
        .bind(account_id)
        .bind(model_id)
        .execute(&pool)
        .await?;
        assert_eq!(conflicting_binding.rows_affected(), 0);

        sqlx::raw_sql(include_str!(
            "../migrations/0007_safe_catalog_defaults.sql"
        ))
        .execute(&pool)
        .await
        .context("failed to replay canonical binding migration")?;
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select metadata_json from ai_model_catalog where id = $1",
            )
            .bind(model_id)
            .fetch_one(&pool)
            .await?,
            expected_model_metadata
        );
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select capability_flags_json from ai_provider_catalog where id = $1",
            )
            .bind(provider_id)
            .fetch_one(&pool)
            .await?,
            expected_provider_flags
        );
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select metadata_json from ai_model_catalog where id = $1",
            )
            .bind(answer_model_id)
            .fetch_one(&pool)
            .await?,
            json!({
                "defaultRoles": ["query_answer", "query_compile", "agent"],
                "sentinel": {"materializeAgent": true}
            }),
            "replay must not duplicate Agent eligibility"
        );
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "select capability_flags_json from ai_provider_catalog where id = $1",
            )
            .bind(dedicated_agent_provider_id)
            .fetch_one(&pool)
            .await?,
            dedicated_agent_provider_flags,
            "replay must preserve the dedicated Agent preset"
        );
        assert_eq!(
            sqlx::query_scalar::<_, Vec<String>>(
                "select array_agg(binding_purpose::text order by binding_purpose::text)
                 from ai_binding
                 where id in ($1, $2)",
            )
            .bind(migrated_query_answer_binding_id)
            .bind(migrated_agent_binding_id)
            .fetch_one(&pool)
            .await?,
            vec!["agent".to_string(), "query_answer".to_string()],
            "replay must keep the two physical runtime purposes distinct"
        );
        for (provider_id, expected_flags) in [
            (malformed_provider_id, &expected_malformed_provider_flags),
            (missing_presets_provider_id, &missing_presets_provider_flags),
        ] {
            assert_eq!(
                &sqlx::query_scalar::<_, Value>(
                    "select capability_flags_json from ai_provider_catalog where id = $1",
                )
                .bind(provider_id)
                .fetch_one(&pool)
                .await?,
                expected_flags
            );
        }

        Ok::<(), anyhow::Error>(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn migration_0007_is_idempotent_and_preserves_operator_catalog_metadata() -> Result<()> {
    let settings = Settings::from_env().context("failed to load settings")?;
    let temp_database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&temp_database.database_url)
        .await
        .context("failed to connect safe catalog defaults migration test postgres")?;

    let result = async {
        let migrations = Migrator::new(Path::new("./migrations"))
            .await
            .context("failed to load migration files")?;
        migrator_with_versions(&migrations, 1, 6)
            .run(&pool)
            .await
            .context("failed to apply schema before safe catalog defaults migration")?;
        let provider_id = sqlx::query_scalar::<_, Uuid>(
            "select id from ai_provider_catalog order by provider_kind limit 1",
        )
        .fetch_one(&pool)
        .await?;
        let generative_id = Uuid::now_v7();
        let document_understanding_id = Uuid::now_v7();
        let embedding_id = Uuid::now_v7();
        let already_query_compile_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let legacy_default_library_id = Uuid::now_v7();
        let custom_policy_library_id = Uuid::now_v7();
        let custom_shape_library_id = Uuid::now_v7();
        let post_migration_library_id = Uuid::now_v7();
        let custom_policy = json!({
            "crawlFilter": {
                "allowPatterns": [{"kind": "path_prefix", "value": "/operator-owned"}],
                "blockPatterns": [{"kind": "glob", "value": "*draft=*"}]
            },
            "materializationFilter": {"allowPatterns": [], "blockPatterns": []}
        });
        let custom_shape_policy = json!({
            "operatorOwned": true,
            "crawlFilter": {"blockPatterns": {"kind": "custom"}}
        });

        sqlx::query(
            "insert into catalog_workspace (
                id, slug, display_name, lifecycle_state, created_at, updated_at
             ) values ($1, 'migration-0007', 'Migration 0007', 'active', now(), now())",
        )
        .bind(workspace_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into catalog_library (
                id, workspace_id, slug, display_name, lifecycle_state,
                web_ingest_policy, created_at, updated_at
             ) values (
                $1, $2, 'operator-shape', 'Operator shape', 'active', $3, now(), now()
             )",
        )
        .bind(custom_shape_library_id)
        .bind(workspace_id)
        .bind(&custom_shape_policy)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into catalog_library (
                id, workspace_id, slug, display_name, lifecycle_state, created_at, updated_at
             ) values ($1, $2, 'legacy-default', 'Legacy default', 'active', now(), now())",
        )
        .bind(legacy_default_library_id)
        .bind(workspace_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "insert into catalog_library (
                id, workspace_id, slug, display_name, lifecycle_state,
                web_ingest_policy, created_at, updated_at
             ) values (
                $1, $2, 'operator-policy', 'Operator policy', 'active', $3, now(), now()
             )",
        )
        .bind(custom_policy_library_id)
        .bind(workspace_id)
        .bind(&custom_policy)
        .execute(&pool)
        .await?;

        for (id, model_name, capability, modality, metadata) in [
            (
                generative_id,
                "migration-generative",
                "chat",
                "text",
                json!({"defaultRoles": ["query_answer"], "marker": {"kept": true}}),
            ),
            (
                document_understanding_id,
                "migration-document-understanding",
                "chat",
                "multimodal",
                json!({"defaultRoles": ["extract_text"], "marker": "document_understanding"}),
            ),
            (
                embedding_id,
                "migration-embedding",
                "embedding",
                "text",
                json!({"defaultRoles": ["embed_chunk"], "marker": "embedding"}),
            ),
            (
                already_query_compile_id,
                "migration-already-query-compile",
                "chat",
                "text",
                json!({"defaultRoles": ["query_answer", "query_compile"], "marker": "existing"}),
            ),
        ] {
            sqlx::query(
                "insert into ai_model_catalog (
                    id, provider_catalog_id, model_name, capability_kind,
                    modality_kind, lifecycle_state, metadata_json
                 ) values (
                    $1, $2, $3, $4::ai_model_capability_kind,
                    $5::ai_model_modality_kind, 'active', $6
                 )",
            )
            .bind(id)
            .bind(provider_id)
            .bind(model_name)
            .bind(capability)
            .bind(modality)
            .bind(metadata)
            .execute(&pool)
            .await?;
        }

        let migration = include_str!("../migrations/0007_safe_catalog_defaults.sql");
        sqlx::raw_sql(migration).execute(&pool).await?;
        let resolution_columns = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from information_schema.columns
             where table_schema = 'public'
               and table_name = 'webhook_lifecycle_outbox'
               and column_name in ('resolution_reason_code', 'resolved_at')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(resolution_columns, 2);
        let dispatch_state_constraint = sqlx::query_scalar::<_, String>(
            "select pg_get_constraintdef(oid)
             from pg_constraint
             where conrelid = 'webhook_lifecycle_outbox'::regclass
               and conname = 'webhook_lifecycle_outbox_dispatch_state'",
        )
        .fetch_one(&pool)
        .await?
        .to_lowercase();
        assert!(dispatch_state_constraint.contains("'resolved'"));
        let migration_outbox_id = Uuid::now_v7();
        sqlx::query(
            "insert into webhook_lifecycle_outbox (
                id, event_id, event_type, occurred_at, workspace_id, library_id,
                payload_json, dispatch_state, dispatch_attempts,
                last_error_code, last_error
             ) values (
                $1, $2, 'revision.ready', now(), $3, $4, '{}',
                'dead_letter', 12, 'fanout_failed', 'redacted failure'
             )",
        )
        .bind(migration_outbox_id)
        .bind(format!("revision.ready:migration-resolved:{migration_outbox_id}"))
        .bind(workspace_id)
        .bind(custom_policy_library_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "update webhook_lifecycle_outbox
             set dispatch_state = 'resolved',
                 resolution_reason_code = 'receiver_retired',
                 resolved_at = now(),
                 updated_at = now()
             where id = $1",
        )
        .bind(migration_outbox_id)
        .execute(&pool)
        .await?;
        let first_pass = sqlx::query_scalar::<_, Value>(
            "select metadata_json from ai_model_catalog where id = $1",
        )
        .bind(generative_id)
        .fetch_one(&pool)
        .await?;

        let neutral_policy = json!({
            "crawlFilter": {"allowPatterns": [], "blockPatterns": []},
            "materializationFilter": {"allowPatterns": [], "blockPatterns": []}
        });
        let migrated_legacy_policy = sqlx::query_scalar::<_, Value>(
            "select web_ingest_policy from catalog_library where id = $1",
        )
        .bind(legacy_default_library_id)
        .fetch_one(&pool)
        .await?;
        let preserved_custom_policy = sqlx::query_scalar::<_, Value>(
            "select web_ingest_policy from catalog_library where id = $1",
        )
        .bind(custom_policy_library_id)
        .fetch_one(&pool)
        .await?;
        let preserved_custom_shape = sqlx::query_scalar::<_, Value>(
            "select web_ingest_policy from catalog_library where id = $1",
        )
        .bind(custom_shape_library_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(migrated_legacy_policy, neutral_policy);
        assert_eq!(preserved_custom_policy, custom_policy);
        assert_eq!(preserved_custom_shape, custom_shape_policy);

        sqlx::query(
            "insert into catalog_library (
                id, workspace_id, slug, display_name, lifecycle_state, created_at, updated_at
             ) values ($1, $2, 'new-default', 'New default', 'active', now(), now())",
        )
        .bind(post_migration_library_id)
        .bind(workspace_id)
        .execute(&pool)
        .await?;
        let post_migration_default = sqlx::query_scalar::<_, Value>(
            "select web_ingest_policy from catalog_library where id = $1",
        )
        .bind(post_migration_library_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(post_migration_default, neutral_policy);

        sqlx::raw_sql(migration).execute(&pool).await?;
        let second_pass = sqlx::query_scalar::<_, Value>(
            "select metadata_json from ai_model_catalog where id = $1",
        )
        .bind(generative_id)
        .fetch_one(&pool)
        .await?;

        assert_eq!(first_pass, second_pass, "migration replay must be a no-op");
        for (library_id, expected_policy) in [
            (legacy_default_library_id, &neutral_policy),
            (custom_policy_library_id, &custom_policy),
            (custom_shape_library_id, &custom_shape_policy),
            (post_migration_library_id, &neutral_policy),
        ] {
            let replayed_policy = sqlx::query_scalar::<_, Value>(
                "select web_ingest_policy from catalog_library where id = $1",
            )
            .bind(library_id)
            .fetch_one(&pool)
            .await?;
            assert_eq!(
                &replayed_policy, expected_policy,
                "migration replay changed library policy"
            );
        }
        assert_eq!(first_pass["marker"]["kept"], true);
        assert_eq!(first_pass["defaultRoles"], json!(["query_answer", "query_compile"]));

        for (id, expected_roles, expected_marker) in [
            (
                document_understanding_id,
                json!(["extract_text", "query_compile"]),
                json!("document_understanding"),
            ),
            (embedding_id, json!(["embed_chunk"]), json!("embedding")),
            (already_query_compile_id, json!(["query_answer", "query_compile"]), json!("existing")),
        ] {
            let metadata = sqlx::query_scalar::<_, Value>(
                "select metadata_json from ai_model_catalog where id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await?;
            assert_eq!(metadata["defaultRoles"], expected_roles);
            assert_eq!(metadata["marker"], expected_marker);
        }

        Ok::<(), anyhow::Error>(())
    }
    .await;

    pool.close().await;
    temp_database.drop().await?;
    result
}
