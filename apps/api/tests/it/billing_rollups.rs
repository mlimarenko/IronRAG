use anyhow::{Context, Result};
use chrono::Utc;
use rust_decimal::Decimal;
use sqlx::{AssertSqlSafe, PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use ironrag_backend::{
    app::{config::Settings, seed_clean_v1_ai_catalog, state::AppState},
    domains::{
        agent_runtime::{RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimeTaskKind},
        billing::{BillingExecutionOwnerKind, CostRollupFreshness, PricingBillingUnit},
    },
    infra::{
        persistence::Persistence,
        repositories::{
            billing_repository, content_document_list_repository::DocumentListSortColumn,
            query_repository, runtime_repository,
        },
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        content::service::ListDocumentsPageCommand,
        ingest::service::{AdmitIngestJobCommand, INGEST_STAGE_EMBED_CHUNK, LeaseAttemptCommand},
        ops::billing::{
            CaptureExecutionBillingCommand, CaptureIngestAttemptBillingCommand,
            CaptureQueryExecutionBillingCommand, ReserveExecutionProviderCallCommand,
        },
    },
};

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("billing_rollups_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect admin postgres for billing_rollups test")?;

        terminate_database_connections(&admin_pool, &database_name).await?;
        sqlx::query(AssertSqlSafe(format!("drop database if exists \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {database_name}"))?;
        sqlx::query(AssertSqlSafe(format!("create database \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {database_name}"))?;
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
            .context("failed to reconnect admin postgres for billing_rollups cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct BillingRollupsFixture {
    state: AppState,
    temp_database: TempDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
    query_execution_id: Uuid,
    query_plan_runtime_execution_id: Uuid,
    query_runtime_execution_id: Uuid,
    query_rerank_runtime_execution_id: Uuid,
    ingest_attempt_id: Uuid,
}

impl BillingRollupsFixture {
    async fn create() -> Result<Self> {
        Self::create_with_max_connections(1).await
    }

    async fn create_with_max_connections(max_connections: u32) -> Result<Self> {
        let settings =
            Settings::from_env().context("failed to load settings for billing_rollups test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        let postgres = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(&temp_database.database_url)
            .await
            .context("failed to connect billing_rollups postgres")?;

        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply authoritative baseline migrations for billing_rollups")?;
        // Migrations leave the AI catalog empty; startup seeds it separately.
        // Every priced assertion below resolves its model from that catalog.
        seed_clean_v1_ai_catalog(&postgres)
            .await
            .context("failed to seed billing_rollups AI catalog")?;

        let state = build_test_state(settings, postgres)?;
        let workspace = state
            .application_services
            .catalog
            .create_workspace(
                &state,
                CreateWorkspaceCommand {
                    slug: Some(format!("billing-workspace-{}", Uuid::now_v7().simple())),
                    display_name: "Billing Rollups Workspace".to_string(),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create billing test workspace")?;
        let library = state
            .application_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("billing-library-{}", Uuid::now_v7().simple())),
                    display_name: "Billing Rollups Library".to_string(),
                    description: Some("authoritative billing rollup test fixture".to_string()),
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create billing test library")?;

        let conversation = query_repository::create_conversation(
            &state.persistence.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: workspace.id,
                library_id: library.id,
                created_by_principal_id: None,
                title: Some("Billing Rollup Conversation"),
                conversation_state: "active",
                request_surface: "ui",
            },
            5,
        )
        .await
        .context("failed to create query conversation")?;
        let request_turn = query_repository::create_turn(
            &state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: conversation.id,
                turn_kind: "user",
                author_principal_id: None,
                content_text: "How much did this execution cost?",
                execution_id: None,
            },
        )
        .await
        .context("failed to create query request turn")?;
        let execution_id = Uuid::now_v7();
        let query_plan_runtime_execution_id = Uuid::now_v7();
        let query_runtime_execution_id = Uuid::now_v7();
        let query_rerank_runtime_execution_id = Uuid::now_v7();

        for (runtime_execution_id, task_kind, contract_name) in [
            (query_plan_runtime_execution_id, RuntimeTaskKind::QueryPlan, "query_plan"),
            (query_runtime_execution_id, RuntimeTaskKind::QueryAnswer, "query_answer"),
            (query_rerank_runtime_execution_id, RuntimeTaskKind::QueryRerank, "query_rerank"),
        ] {
            runtime_repository::create_runtime_execution(
                &state.persistence.postgres,
                &runtime_repository::NewRuntimeExecution {
                    id: runtime_execution_id,
                    owner_kind: RuntimeExecutionOwnerKind::QueryExecution.as_str(),
                    owner_id: execution_id,
                    task_kind: task_kind.as_str(),
                    surface_kind: "rest",
                    contract_name,
                    contract_version: "1",
                    lifecycle_state: RuntimeLifecycleState::Running.as_str(),
                    active_stage: None,
                    turn_budget: 4,
                    turn_count: 1,
                    parallel_action_limit: 1,
                    failure_code: None,
                    failure_summary_redacted: None,
                    parent_execution_id: None,
                },
            )
            .await
            .with_context(|| {
                format!("failed to create {contract_name} billing runtime execution")
            })?;
        }
        let query_execution = query_repository::create_execution(
            &state.persistence.postgres,
            &query_repository::NewQueryExecution {
                execution_id,
                context_bundle_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                conversation_id: conversation.id,
                request_turn_id: Some(request_turn.id),
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id: query_runtime_execution_id,
                query_text: "How much did this execution cost?",
                failure_code: None,
            },
        )
        .await
        .context("failed to create query execution")?;

        let ingest_job = state
            .application_services
            .ingest
            .admit_job(
                &state,
                AdmitIngestJobCommand {
                    workspace_id: workspace.id,
                    library_id: library.id,
                    mutation_id: None,
                    mutation_item_id: None,
                    connector_id: None,
                    async_operation_id: None,
                    knowledge_document_id: None,
                    knowledge_revision_id: None,
                    job_kind: "content_mutation".to_string(),
                    priority: 100,
                    dedupe_key: Some(format!("billing-ingest-{}", Uuid::now_v7())),
                    available_at: None,
                },
            )
            .await
            .context("failed to create ingest job")?;
        let ingest_attempt = state
            .application_services
            .ingest
            .lease_attempt(
                &state,
                LeaseAttemptCommand {
                    job_id: ingest_job.id,
                    worker_principal_id: None,
                    lease_token: Some("billing-rollup-lease".to_string()),
                    expected_queue_lease_token: None,
                    knowledge_generation_id: None,
                    current_stage: Some(INGEST_STAGE_EMBED_CHUNK.to_string()),
                },
            )
            .await
            .context("failed to create ingest attempt")?;

        Ok(Self {
            state,
            temp_database,
            workspace_id: workspace.id,
            library_id: library.id,
            query_execution_id: query_execution.id,
            query_plan_runtime_execution_id,
            query_runtime_execution_id,
            query_rerank_runtime_execution_id,
            ingest_attempt_id: ingest_attempt.id,
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

struct OpenAiCatalogModel {
    provider_id: Uuid,
    model_id: Uuid,
    model_name: String,
    /// The model's own rates, so a test can state the arithmetic it means
    /// instead of a total copied from the catalog of the day. The helper below
    /// already refuses to pin a model identifier for that reason; a pinned
    /// total is the same coupling one step later, and it broke the moment the
    /// catalog gained a model that sorts earlier.
    input_price_per_1m: Decimal,
    output_price_per_1m: Decimal,
}

impl OpenAiCatalogModel {
    fn cost_for(&self, prompt_tokens: u32, completion_tokens: u32) -> Decimal {
        let per_million = Decimal::from(1_000_000);
        (self.input_price_per_1m * Decimal::from(prompt_tokens)
            + self.output_price_per_1m * Decimal::from(completion_tokens))
            / per_million
    }
}

async fn openai_catalog_model_for_purpose(
    postgres: &PgPool,
    binding_purpose: &str,
) -> Result<OpenAiCatalogModel> {
    let (provider_id, model_id, model_name, input_price_per_1m, output_price_per_1m) =
        sqlx::query_as::<_, (Uuid, Uuid, String, Decimal, Decimal)>(
            // Resolve the seeded model by its formal capability instead of pinning
            // an identifier that changes whenever the clean catalog advances.
            "select provider.id, model.id, model.model_name, model.input_price_per_1m,
                model.output_price_per_1m
         from ai_provider_catalog provider
         join ai_model_catalog model on model.provider_catalog_id = provider.id
         where provider.provider_kind = 'openai'
           and model.input_price_per_1m > 0
           and exists (
               select 1
               from ai_model_binding_purpose purpose
               where purpose.model_catalog_id = model.id
                 and purpose.binding_purpose = $1::ai_binding_purpose
           )
         order by model.model_name
         limit 1",
        )
        .bind(binding_purpose)
        .fetch_one(postgres)
        .await
        .with_context(|| format!("failed to resolve OpenAI {binding_purpose} pricing catalog"))?;
    Ok(OpenAiCatalogModel {
        provider_id,
        model_id,
        model_name,
        input_price_per_1m,
        output_price_per_1m,
    })
}

async fn openai_chat_catalog_ids(postgres: &PgPool) -> Result<(Uuid, Uuid)> {
    let model = openai_catalog_model_for_purpose(postgres, "query_answer").await?;
    Ok((model.provider_id, model.model_id))
}

struct SyntheticDisjointPricingCatalog {
    provider_catalog_id: Uuid,
    model_catalog_id: Uuid,
    provider_kind: String,
    model_name: String,
}

async fn create_synthetic_disjoint_pricing_catalog(
    postgres: &PgPool,
) -> Result<SyntheticDisjointPricingCatalog> {
    let provider_catalog_id = Uuid::now_v7();
    let model_catalog_id = Uuid::now_v7();
    let suffix = Uuid::now_v7().simple();
    let provider_kind = format!("synthetic-provider-{suffix}");
    let model_name = format!("synthetic-model-{suffix}");
    sqlx::query(
        "insert into ai_provider_catalog (
            id,
            provider_kind,
            display_name,
            api_style,
            lifecycle_state,
            default_base_url,
            capability_flags_json
         )
         values (
            $1,
            $2,
            'Synthetic Billing Provider',
            'openai_compatible'::ai_provider_api_style,
            'active'::ai_provider_lifecycle_state,
            null,
            $3
         )",
    )
    .bind(provider_catalog_id)
    .bind(&provider_kind)
    .bind(serde_json::json!({"usagePolicy": "disjoint_cache_counters"}))
    .execute(postgres)
    .await
    .context("failed to create synthetic billing provider")?;
    sqlx::query(
        "insert into ai_model_catalog (
            id,
            provider_catalog_id,
            model_name,
            context_window,
            max_output_tokens,
            lifecycle_state,
            metadata_json,
            currency_code,
            input_price_per_1m,
            cached_input_price_per_1m,
            cache_write_input_price_per_1m,
            output_price_per_1m
         )
         values (
            $1,
            $2,
            $3,
            8000000,
            1000000,
            'active'::ai_model_lifecycle_state,
            $4,
            'USD',
            1,
            0.1,
            0,
            2
         )",
    )
    .bind(model_catalog_id)
    .bind(provider_catalog_id)
    .bind(&model_name)
    .bind(serde_json::json!({
        "seedSource": "synthetic-test"
    }))
    .execute(postgres)
    .await
    .context("failed to create synthetic billing model")?;
    sqlx::query(
        "insert into ai_model_binding_purpose (model_catalog_id, binding_purpose)
         values ($1, 'query_answer')",
    )
    .bind(model_catalog_id)
    .execute(postgres)
    .await
    .context("failed to create synthetic billing model purpose")?;

    Ok(SyntheticDisjointPricingCatalog {
        provider_catalog_id,
        model_catalog_id,
        provider_kind,
        model_name,
    })
}

fn build_test_state(mut settings: Settings, postgres: PgPool) -> Result<AppState> {
    let redis = redis::Client::open(settings.redis_url.clone())
        .context("failed to create redis client for billing_rollups test state")?;
    let persistence = Persistence::for_tests(postgres, redis);
    settings.credential_master_key =
        Some("KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=".to_string());
    AppState::from_dependencies(settings, persistence)
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

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn cache_write_usage_stays_separate_and_unpriced_without_an_explicit_catalog_entry()
-> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let catalog =
            create_synthetic_disjoint_pricing_catalog(&fixture.state.persistence.postgres).await?;
        let SyntheticDisjointPricingCatalog { provider_kind, model_name, .. } = catalog;
        let cost = fixture
            .state
            .application_services
            .billing
            .capture_execution_provider_call(
                &fixture.state,
                CaptureExecutionBillingCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    owning_execution_kind: "query_execution".to_string(),
                    owning_execution_id: fixture.query_execution_id,
                    runtime_execution_id: Some(fixture.query_runtime_execution_id),
                    runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer),
                    binding_id: None,
                    provider_kind,
                    model_name,
                    call_kind: "query_answer".to_string(),
                    usage_json: serde_json::json!({
                        "input_tokens": 1000000,
                        "cache_creation_input_tokens": 1000000,
                        "cache_read_input_tokens": 1000000,
                        "output_tokens": 1000000,
                    }),
                },
            )
            .await
            .context("failed to capture disjoint cache billing")?
            .context("disjoint cache billing should produce a rollup")?;
        assert_eq!(cost.total_cost, Decimal::new(31, 1));
        assert_eq!(cost.provider_call_count, 1);

        let provider_call = fixture
            .state
            .application_services
            .billing
            .list_execution_provider_calls_page(
                &fixture.state,
                ironrag_backend::services::ops::billing::BillingExecutionPageQuery {
                    execution_kind: BillingExecutionOwnerKind::QueryExecution,
                    execution_id: fixture.query_execution_id,
                    cursor: None,
                    limit: 100,
                    include_total: false,
                },
            )
            .await?
            .items
            .into_iter()
            .next()
            .context("captured provider call is missing")?;
        let usage_rows = billing_repository::list_usage_by_provider_call(
            &fixture.state.persistence.postgres,
            provider_call.id,
        )
        .await?;
        assert_eq!(usage_rows.len(), 4);
        let cache_write_usage = usage_rows
            .iter()
            .find(|usage| usage.usage_kind == "cache_creation_input_tokens")
            .context("cache-write usage row is missing")?;
        assert_eq!(
            cache_write_usage.billing_unit,
            PricingBillingUnit::Per1MCacheWriteInputTokens.as_str()
        );
        assert_eq!(cache_write_usage.quantity, Decimal::from(1_000_000));
        assert_eq!(
            usage_rows
                .iter()
                .find(|usage| usage.usage_kind == "prompt_tokens")
                .map(|usage| usage.quantity),
            Some(Decimal::from(1_000_000)),
            "cache writes must not inflate ordinary input usage"
        );

        let charges = fixture
            .state
            .application_services
            .billing
            .list_execution_charges_page(
                &fixture.state,
                ironrag_backend::services::ops::billing::BillingExecutionPageQuery {
                    execution_kind: BillingExecutionOwnerKind::QueryExecution,
                    execution_id: fixture.query_execution_id,
                    cursor: None,
                    limit: 100,
                    include_total: false,
                },
            )
            .await?
            .items;
        // Pricing lives on the model row, so the cache-write usage prices at
        // the model's explicit zero rate instead of being skipped as
        // uncovered: every usage row now carries a charge.
        assert_eq!(charges.len(), 4);
        let cache_write_charge = charges
            .iter()
            .find(|charge| charge.usage_id == cache_write_usage.id)
            .context("cache-write usage must be charged at the explicit zero rate")?;
        assert_eq!(cache_write_charge.total_price, Decimal::ZERO);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn long_context_pricing_boundary_is_persisted_on_provider_call_charges() -> Result<()> {
    const THRESHOLD_TOKENS: i32 = 100;

    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let catalog =
            create_synthetic_disjoint_pricing_catalog(&fixture.state.persistence.postgres).await?;
        let base_input_price = Decimal::from(1);
        let long_context_input_price = Decimal::from(10);
        let updated = sqlx::query(
            "update ai_model_catalog
             set input_price_per_1m = $2,
                 long_context_threshold_tokens = $3,
                 long_context_input_price_per_1m = $4,
                 long_context_cached_input_price_per_1m = $5,
                 long_context_output_price_per_1m = $6
             where id = $1",
        )
        .bind(catalog.model_catalog_id)
        .bind(base_input_price)
        .bind(THRESHOLD_TOKENS)
        .bind(long_context_input_price)
        .bind(Decimal::from(20))
        .bind(Decimal::from(30))
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to configure synthetic long-context pricing")?;
        assert_eq!(
            updated.rows_affected(),
            1,
            "the boundary fixture must update exactly one model",
        );

        let billing = &fixture.state.application_services.billing;
        for (request_input_tokens, expected_unit_price, expected_total_price) in [
            (
                THRESHOLD_TOKENS - 1,
                base_input_price,
                Decimal::new(99, 6),
            ),
            (
                THRESHOLD_TOKENS,
                base_input_price,
                Decimal::new(1, 4),
            ),
            (
                THRESHOLD_TOKENS + 1,
                long_context_input_price,
                Decimal::new(101, 5),
            ),
        ] {
            let provider_call_id = billing
                .reserve_execution_provider_call(
                    &fixture.state,
                    ReserveExecutionProviderCallCommand {
                        workspace_id: fixture.workspace_id,
                        library_id: fixture.library_id,
                        owning_execution_kind: "query_execution".to_string(),
                        owning_execution_id: fixture.query_execution_id,
                        runtime_execution_id: Some(fixture.query_runtime_execution_id),
                        runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer),
                        binding_id: None,
                        provider_catalog_id: catalog.provider_catalog_id,
                        model_catalog_id: catalog.model_catalog_id,
                        call_kind: "query_answer".to_string(),
                    },
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to reserve provider call at {request_input_tokens} request tokens"
                    )
                })?;
            billing
                .complete_reserved_provider_call(
                    &fixture.state,
                    provider_call_id,
                    &serde_json::json!({"input_tokens": request_input_tokens}),
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to complete provider call at {request_input_tokens} request tokens"
                    )
                })?
                .with_context(|| {
                    format!(
                        "provider call at {request_input_tokens} request tokens must produce a rollup"
                    )
                })?;

            let provider_call = billing_repository::get_provider_call_by_id(
                &fixture.state.persistence.postgres,
                provider_call_id,
            )
            .await?
            .with_context(|| format!("provider call {provider_call_id} is missing"))?;
            assert_eq!(provider_call.call_state, "completed");
            assert_eq!(provider_call.model_catalog_id, catalog.model_catalog_id);

            let usage_rows = billing_repository::list_usage_by_provider_call(
                &fixture.state.persistence.postgres,
                provider_call_id,
            )
            .await?;
            assert_eq!(
                usage_rows.len(),
                1,
                "one input-only usage row expected at {request_input_tokens} request tokens",
            );
            let input_usage = usage_rows.first().with_context(|| {
                format!("input usage is missing at {request_input_tokens} request tokens")
            })?;
            assert_eq!(
                input_usage.billing_unit,
                PricingBillingUnit::Per1MInputTokens.as_str(),
            );
            assert_eq!(
                input_usage.quantity,
                Decimal::from(request_input_tokens),
            );

            let charges = billing
                .list_execution_charges_page(
                    &fixture.state,
                    ironrag_backend::services::ops::billing::BillingExecutionPageQuery {
                        execution_kind: BillingExecutionOwnerKind::QueryExecution,
                        execution_id: fixture.query_execution_id,
                        cursor: None,
                        limit: 100,
                        include_total: false,
                    },
                )
                .await
                .with_context(|| {
                    format!("failed to list charges at {request_input_tokens} request tokens")
                })?
                .items;
            let charge = charges
                .iter()
                .find(|charge| charge.usage_id == input_usage.id)
                .with_context(|| {
                    format!(
                        "persisted input charge is missing at {request_input_tokens} request tokens"
                    )
                })?;
            assert_eq!(charge.currency_code, "USD");
            assert_eq!(
                charge.unit_price, expected_unit_price,
                "persisted unit price at {request_input_tokens} request tokens",
            );
            assert_eq!(
                charge.total_price, expected_total_price,
                "persisted total price at {request_input_tokens} request tokens",
            );
        }
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn authoritative_billing_rollups_cover_query_and_ingest_executions() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let billing = &fixture.state.application_services.billing;
        let query_model =
            openai_catalog_model_for_purpose(&fixture.state.persistence.postgres, "query_answer")
                .await?;

        let unpriced = billing
            .capture_execution_provider_call(
                &fixture.state,
                CaptureExecutionBillingCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    owning_execution_kind: "query_execution".to_string(),
                    owning_execution_id: fixture.query_execution_id,
                    runtime_execution_id: Some(fixture.query_plan_runtime_execution_id),
                    runtime_task_kind: Some(RuntimeTaskKind::QueryPlan),
                    binding_id: None,
                    provider_kind: "openai".to_string(),
                    model_name: query_model.model_name.clone(),
                    call_kind: "query_planning".to_string(),
                    usage_json: serde_json::json!({}),
                },
            )
            .await
            .context("failed to capture unpriced planning provider call")?
            .context("unpriced provider attempt should still produce a zero-cost rollup")?;
        assert_eq!(unpriced.total_cost, Decimal::ZERO);
        assert_eq!(unpriced.provider_call_count, 1);

        let query_cost = billing
            .capture_query_execution(
                &fixture.state,
                CaptureQueryExecutionBillingCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    execution_id: fixture.query_execution_id,
                    runtime_execution_id: fixture.query_runtime_execution_id,
                    binding_id: None,
                    provider_kind: "openai".to_string(),
                    model_name: query_model.model_name.clone(),
                    call_kind: "query_answer".to_string(),
                    usage_json: serde_json::json!({
                        "prompt_tokens": 4000,
                        "completion_tokens": 1000,
                        "total_tokens": 5000,
                    }),
                },
            )
            .await
            .context("failed to capture query answer billing")?
            .context("query execution cost should be priced")?;
        assert_eq!(query_cost.currency_code, "USD");
        assert_eq!(query_cost.total_cost, query_model.cost_for(4000, 1000));
        assert_eq!(query_cost.provider_call_count, 2);

        let provider_catalog_id = query_model.provider_id;
        let model_catalog_id = query_model.model_id;
        let requested_provider_call_id = Uuid::now_v7();
        let reservation_command = ReserveExecutionProviderCallCommand {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            owning_execution_kind: "query_execution".to_string(),
            owning_execution_id: fixture.query_execution_id,
            runtime_execution_id: Some(fixture.query_rerank_runtime_execution_id),
            runtime_task_kind: Some(RuntimeTaskKind::QueryRerank),
            binding_id: None,
            provider_catalog_id,
            model_catalog_id,
            call_kind: "query_rerank".to_string(),
        };
        let rerank_provider_call_id = billing
            .reserve_execution_provider_call_with_id(
                &fixture.state,
                requested_provider_call_id,
                reservation_command.clone(),
            )
            .await
            .context("failed to reserve rerank billing")?;
        assert_eq!(rerank_provider_call_id, requested_provider_call_id);
        let repeated_reservation_id = billing
            .reserve_execution_provider_call_with_id(
                &fixture.state,
                requested_provider_call_id,
                reservation_command.clone(),
            )
            .await
            .context("same stable provider-call reservation must be idempotent")?;
        assert_eq!(repeated_reservation_id, requested_provider_call_id);
        let mut mismatched_reservation = reservation_command;
        mismatched_reservation.call_kind = "query_answer".to_string();
        let mismatch = billing
            .reserve_execution_provider_call_with_id(
                &fixture.state,
                requested_provider_call_id,
                mismatched_reservation,
            )
            .await
            .expect_err("a stable event id must not alias different attribution");
        assert!(mismatch.to_string().contains("different or terminal event"));
        let reserved = billing_repository::get_provider_call_by_id(
            &fixture.state.persistence.postgres,
            rerank_provider_call_id,
        )
        .await?
        .context("reserved rerank provider call missing")?;
        assert_eq!(reserved.call_state, "started");

        let rerank_usage = serde_json::json!({
            "input_tokens": 2000,
            "total_tokens": 2000,
        });
        let (first_completion, second_completion) = tokio::join!(
            billing.complete_reserved_provider_call(
                &fixture.state,
                rerank_provider_call_id,
                &rerank_usage,
            ),
            billing.complete_reserved_provider_call(
                &fixture.state,
                rerank_provider_call_id,
                &rerank_usage,
            ),
        );
        let first_rollup = first_completion
            .context("first concurrent completion must acknowledge the stable event")?
            .context("first concurrent completion should be priced")?;
        let second_rollup = second_completion
            .context("retrying a committed completion must be idempotent")?
            .context("idempotent completion retry should return the authoritative rollup")?;
        assert_eq!(first_rollup.total_cost, second_rollup.total_cost);
        assert_eq!(first_rollup.provider_call_count, second_rollup.provider_call_count);
        let completed = billing_repository::get_provider_call_by_id(
            &fixture.state.persistence.postgres,
            rerank_provider_call_id,
        )
        .await?
        .context("completed rerank provider call missing")?;
        assert_eq!(completed.call_state, "completed");
        billing
            .finish_reserved_provider_call_without_usage(
                &fixture.state,
                rerank_provider_call_id,
                "failed",
            )
            .await
            .context("terminal reconciliation must be idempotent after concurrent completion")?;
        let still_completed = billing_repository::get_provider_call_by_id(
            &fixture.state.persistence.postgres,
            rerank_provider_call_id,
        )
        .await?
        .context("reconciled rerank provider call missing")?;
        assert_eq!(still_completed.call_state, "completed");
        let rerank_usage_rows = billing_repository::list_usage_by_provider_call(
            &fixture.state.persistence.postgres,
            rerank_provider_call_id,
        )
        .await?;
        assert_eq!(rerank_usage_rows.len(), 1, "concurrent completion must not duplicate usage");

        // Provider accounting committed before terminal query persistence.
        // A later terminal write can fail independently without rolling back
        // the already-durable event or its usage rows.
        sqlx::query(
            "update query_execution
             set response_turn_id = $2
             where id = $1",
        )
        .bind(fixture.query_execution_id)
        .bind(Uuid::now_v7())
        .execute(&fixture.state.persistence.postgres)
        .await
        .expect_err("synthetic terminal persistence must fail its response-turn foreign key");
        let durable_after_terminal_failure = billing_repository::get_provider_call_by_id(
            &fixture.state.persistence.postgres,
            rerank_provider_call_id,
        )
        .await?
        .context("provider call disappeared after terminal persistence failure")?;
        assert_eq!(durable_after_terminal_failure.call_state, "completed");
        assert_eq!(
            billing_repository::list_usage_by_provider_call(
                &fixture.state.persistence.postgres,
                rerank_provider_call_id,
            )
            .await?
            .len(),
            1,
        );
        assert_eq!(
            first_rollup.total_cost,
            query_model.cost_for(4000, 1000) + query_model.cost_for(2000, 0)
        );
        assert_eq!(first_rollup.provider_call_count, 3);

        let ingest_cost = billing
            .capture_ingest_attempt(
                &fixture.state,
                CaptureIngestAttemptBillingCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    attempt_id: fixture.ingest_attempt_id,
                    binding_id: None,
                    provider_kind: "openai".to_string(),
                    model_name: "text-embedding-3-large".to_string(),
                    call_kind: "embed_chunk".to_string(),
                    usage_json: serde_json::json!({
                        "prompt_tokens": 12000,
                        "total_tokens": 12000,
                    }),
                },
            )
            .await
            .context("failed to capture ingest attempt billing")?
            .context("ingest attempt cost should be priced")?;
        assert_eq!(ingest_cost.currency_code, "USD");
        assert_eq!(ingest_cost.total_cost, Decimal::new(156, 5));
        assert_eq!(ingest_cost.provider_call_count, 1);

        let mut provider_calls = billing
            .list_execution_provider_calls_page(
                &fixture.state,
                ironrag_backend::services::ops::billing::BillingExecutionPageQuery {
                    execution_kind: BillingExecutionOwnerKind::QueryExecution,
                    execution_id: fixture.query_execution_id,
                    cursor: None,
                    limit: 100,
                    include_total: false,
                },
            )
            .await
            .context("failed to list query execution provider calls")?
            .items;
        provider_calls.extend(
            billing
                .list_execution_provider_calls_page(
                    &fixture.state,
                    ironrag_backend::services::ops::billing::BillingExecutionPageQuery {
                        execution_kind: BillingExecutionOwnerKind::IngestAttempt,
                        execution_id: fixture.ingest_attempt_id,
                        cursor: None,
                        limit: 100,
                        include_total: false,
                    },
                )
                .await
                .context("failed to list ingest execution provider calls")?
                .items,
        );
        assert_eq!(provider_calls.len(), 4);
        assert!(provider_calls.iter().any(|row| row.call_kind == "query_planning"));
        assert!(provider_calls.iter().any(|row| row.call_kind == "query_answer"));
        assert!(provider_calls.iter().any(|row| row.call_kind == "query_rerank"));
        assert!(provider_calls.iter().any(|row| row.call_kind == "embed_chunk"));

        let mut charges = billing
            .list_execution_charges_page(
                &fixture.state,
                ironrag_backend::services::ops::billing::BillingExecutionPageQuery {
                    execution_kind: BillingExecutionOwnerKind::QueryExecution,
                    execution_id: fixture.query_execution_id,
                    cursor: None,
                    limit: 100,
                    include_total: false,
                },
            )
            .await
            .context("failed to list query execution charges")?
            .items;
        charges.extend(
            billing
                .list_execution_charges_page(
                    &fixture.state,
                    ironrag_backend::services::ops::billing::BillingExecutionPageQuery {
                        execution_kind: BillingExecutionOwnerKind::IngestAttempt,
                        execution_id: fixture.ingest_attempt_id,
                        cursor: None,
                        limit: 100,
                        include_total: false,
                    },
                )
                .await
                .context("failed to list ingest execution charges")?
                .items,
        );
        assert_eq!(charges.len(), 4);
        assert!(charges.iter().all(|row| row.currency_code == "USD"));

        let resolved_query_library = billing
            .resolve_execution_library_id(
                &fixture.state,
                "query_execution",
                fixture.query_execution_id,
            )
            .await
            .context("failed to resolve query execution library")?;
        assert_eq!(resolved_query_library, fixture.library_id);

        let resolved_ingest_library = billing
            .resolve_execution_library_id(
                &fixture.state,
                "ingest_attempt",
                fixture.ingest_attempt_id,
            )
            .await
            .context("failed to resolve ingest attempt library")?;
        assert_eq!(resolved_ingest_library, fixture.library_id);

        let stored_query_cost = billing
            .get_execution_cost(&fixture.state, "query_execution", fixture.query_execution_id)
            .await
            .context("failed to load stored query execution cost")?;
        assert_eq!(
            stored_query_cost.total_cost,
            query_model.cost_for(4000, 1000) + query_model.cost_for(2000, 0)
        );
        assert_eq!(stored_query_cost.provider_call_count, 3);

        let stored_ingest_cost = billing
            .get_execution_cost(&fixture.state, "ingest_attempt", fixture.ingest_attempt_id)
            .await
            .context("failed to load stored ingest execution cost")?;
        assert_eq!(stored_ingest_cost.total_cost, Decimal::new(156, 5));
        assert_eq!(stored_ingest_cost.provider_call_count, 1);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn provider_completion_retries_are_atomic_and_idempotent_across_commit_ambiguity()
-> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let billing = &fixture.state.application_services.billing;
        let postgres = &fixture.state.persistence.postgres;
        let (provider_catalog_id, model_catalog_id) = openai_chat_catalog_ids(postgres).await?;
        let usage_json = serde_json::json!({
            "input_tokens": 1000,
            "total_tokens": 1000,
        });
        let reserve_command = || ReserveExecutionProviderCallCommand {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            owning_execution_kind: "query_execution".to_string(),
            owning_execution_id: fixture.query_execution_id,
            runtime_execution_id: Some(fixture.query_rerank_runtime_execution_id),
            runtime_task_kind: Some(RuntimeTaskKind::QueryRerank),
            binding_id: None,
            provider_catalog_id,
            model_catalog_id,
            call_kind: "query_rerank".to_string(),
        };

        let precommit_failure_id = Uuid::now_v7();
        billing
            .reserve_execution_provider_call_with_id(
                &fixture.state,
                precommit_failure_id,
                reserve_command(),
            )
            .await
            .context("failed to reserve the pre-commit fault event")?;
        sqlx::query(AssertSqlSafe(format!(
            "create function test_fail_provider_completion() returns trigger
             language plpgsql
             as $$
             begin
                 if new.provider_call_id = '{precommit_failure_id}'::uuid then
                     raise exception 'synthetic pre-commit provider completion failure';
                 end if;
                 return new;
             end;
             $$"
        )))
        .execute(postgres)
        .await
        .context("failed to install the pre-commit completion fault function")?;
        sqlx::query(
            "create trigger test_fail_provider_completion
             before insert on billing_usage
             for each row execute function test_fail_provider_completion()",
        )
        .execute(postgres)
        .await
        .context("failed to install the pre-commit completion fault trigger")?;

        billing
            .complete_reserved_provider_call_deferred_rollup(
                &fixture.state,
                precommit_failure_id,
                &usage_json,
            )
            .await
            .expect_err("the injected usage insert failure must roll completion back");
        let after_precommit_failure =
            billing_repository::get_provider_call_by_id(postgres, precommit_failure_id)
                .await?
                .context("pre-commit fault event disappeared")?;
        assert_eq!(after_precommit_failure.call_state, "started");
        assert!(
            billing_repository::list_usage_by_provider_call(postgres, precommit_failure_id)
                .await?
                .is_empty(),
            "a pre-commit failure must leave no partial usage",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from billing_charge charge
                 join billing_usage usage on usage.id = charge.usage_id
                 where usage.provider_call_id = $1",
            )
            .bind(precommit_failure_id)
            .fetch_one(postgres)
            .await?,
            0,
            "a pre-commit failure must leave no partial charge",
        );

        sqlx::query("drop trigger test_fail_provider_completion on billing_usage")
            .execute(postgres)
            .await?;
        sqlx::query("drop function test_fail_provider_completion()").execute(postgres).await?;

        // Deterministically model a successful database COMMIT whose result is
        // lost at the caller boundary: authoritative rows commit, while the caller
        // observes an injected acknowledgement error and must retry the same
        // stable event. A socket-level race would be nondeterministic but has
        // the same observable state transition.
        let lost_acknowledgement: anyhow::Result<()> = async {
            billing
                .complete_reserved_provider_call_deferred_rollup_with_retry(
                    &fixture.state,
                    precommit_failure_id,
                    &usage_json,
                )
                .await
                .context("completion did not recover after the pre-commit fault was removed")?;
            anyhow::bail!("synthetic lost commit acknowledgement")
        }
        .await;
        assert!(
            lost_acknowledgement.is_err(),
            "the caller-boundary fault must hide a successful commit acknowledgement",
        );
        billing
            .complete_reserved_provider_call_deferred_rollup_with_retry(
                &fixture.state,
                precommit_failure_id,
                &usage_json,
            )
            .await
            .context("completion retry after a lost commit acknowledgement failed")?;

        assert_eq!(
            billing_repository::get_provider_call_by_id(postgres, precommit_failure_id)
                .await?
                .context("completed pre-commit fault event disappeared")?
                .call_state,
            "completed",
        );
        assert_eq!(
            billing_repository::list_usage_by_provider_call(postgres, precommit_failure_id)
                .await?
                .len(),
            1,
            "retry after an ambiguous commit must not duplicate usage",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from billing_charge charge
                 join billing_usage usage on usage.id = charge.usage_id
                 where usage.provider_call_id = $1",
            )
            .bind(precommit_failure_id)
            .fetch_one(postgres)
            .await?,
            1,
            "retry after an ambiguous commit must not duplicate charges",
        );

        let transient_failure_id = Uuid::now_v7();
        billing
            .reserve_execution_provider_call_with_id(
                &fixture.state,
                transient_failure_id,
                reserve_command(),
            )
            .await
            .context("failed to reserve the bounded-retry fault event")?;
        sqlx::query("create sequence test_provider_completion_fault_sequence start 1")
            .execute(postgres)
            .await?;
        sqlx::query(AssertSqlSafe(format!(
            "create function test_fail_provider_completion_once() returns trigger
             language plpgsql
             as $$
             begin
                 if new.provider_call_id = '{transient_failure_id}'::uuid
                    and nextval('test_provider_completion_fault_sequence') = 1 then
                     raise exception 'synthetic transient provider completion failure';
                 end if;
                 return new;
             end;
             $$"
        )))
        .execute(postgres)
        .await?;
        sqlx::query(
            "create trigger test_fail_provider_completion_once
             before insert on billing_usage
             for each row execute function test_fail_provider_completion_once()",
        )
        .execute(postgres)
        .await?;

        billing
            .complete_reserved_provider_call_deferred_rollup_with_retry(
                &fixture.state,
                transient_failure_id,
                &usage_json,
            )
            .await
            .context("bounded completion retry did not recover the transient pre-commit fault")?;
        assert_eq!(
            billing_repository::get_provider_call_by_id(postgres, transient_failure_id)
                .await?
                .context("completed transient fault event disappeared")?
                .call_state,
            "completed",
        );
        assert_eq!(
            billing_repository::list_usage_by_provider_call(postgres, transient_failure_id)
                .await?
                .len(),
            1,
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*)::bigint
                 from billing_charge charge
                 join billing_usage usage on usage.id = charge.usage_id
                 where usage.provider_call_id = $1",
            )
            .bind(transient_failure_id)
            .fetch_one(postgres)
            .await?,
            1,
        );

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn distinct_reserved_calls_serialize_rollup_and_terminal_failures_refresh_count() -> Result<()>
{
    let fixture = BillingRollupsFixture::create_with_max_connections(4).await?;

    let result = async {
        let billing = &fixture.state.application_services.billing;
        let (provider_catalog_id, model_catalog_id) =
            openai_chat_catalog_ids(&fixture.state.persistence.postgres).await?;
        let reserve = || ReserveExecutionProviderCallCommand {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            owning_execution_kind: "query_execution".to_string(),
            owning_execution_id: fixture.query_execution_id,
            runtime_execution_id: Some(fixture.query_rerank_runtime_execution_id),
            runtime_task_kind: Some(RuntimeTaskKind::QueryRerank),
            binding_id: None,
            provider_catalog_id,
            model_catalog_id,
            call_kind: "query_rerank".to_string(),
        };
        let first_call_id = billing
            .reserve_execution_provider_call(&fixture.state, reserve())
            .await
            .context("failed to reserve first concurrent provider call")?;
        let second_call_id = billing
            .reserve_execution_provider_call(&fixture.state, reserve())
            .await
            .context("failed to reserve second concurrent provider call")?;
        let usage = serde_json::json!({"input_tokens": 1000, "total_tokens": 1000});

        let (first, second) = tokio::join!(
            billing.complete_reserved_provider_call(&fixture.state, first_call_id, &usage),
            billing.complete_reserved_provider_call(&fixture.state, second_call_id, &usage),
        );
        first.context("first distinct provider call completion failed")?;
        second.context("second distinct provider call completion failed")?;

        let priced = billing
            .get_execution_cost(&fixture.state, "query_execution", fixture.query_execution_id)
            .await
            .context("failed to load concurrent execution cost")?;
        assert_eq!(priced.provider_call_count, 2);
        assert!(priced.total_cost > Decimal::ZERO);

        sqlx::query(
            "update billing_execution_cost
             set total_cost = 0,
                 provider_call_count = 0
             where owning_execution_kind = 'query_execution'::billing_owning_execution_kind
               and owning_execution_id = $1",
        )
        .bind(fixture.query_execution_id)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to inject stale derived execution cost")?;
        let repaired = billing
            .get_execution_cost(&fixture.state, "query_execution", fixture.query_execution_id)
            .await
            .context("failed to repair stale execution cost on read")?;
        assert_eq!(repaired.total_cost, priced.total_cost);
        assert_eq!(repaired.provider_call_count, 2);

        let failed_call_id = billing
            .reserve_execution_provider_call(&fixture.state, reserve())
            .await
            .context("failed to reserve terminal failure provider call")?;
        billing
            .finish_reserved_provider_call_without_usage(&fixture.state, failed_call_id, "failed")
            .await
            .context("failed to terminalize provider call without usage")?;

        let refreshed = billing
            .get_execution_cost(&fixture.state, "query_execution", fixture.query_execution_id)
            .await
            .context("failed to load refreshed execution cost")?;
        assert_eq!(refreshed.total_cost, priced.total_cost);
        assert_eq!(
            refreshed.provider_call_count, 3,
            "terminal provider attempts must refresh the derived execution rollup",
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn durable_dirty_generation_recovers_a_crash_before_derived_rollup() -> Result<()> {
    let fixture = BillingRollupsFixture::create_with_max_connections(4).await?;

    let result = async {
        let billing = &fixture.state.application_services.billing;
        let (provider_catalog_id, model_catalog_id) =
            openai_chat_catalog_ids(&fixture.state.persistence.postgres).await?;
        let provider_call_id = billing
            .reserve_execution_provider_call(
                &fixture.state,
                ReserveExecutionProviderCallCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    owning_execution_kind: "query_execution".to_string(),
                    owning_execution_id: fixture.query_execution_id,
                    runtime_execution_id: Some(fixture.query_rerank_runtime_execution_id),
                    runtime_task_kind: Some(RuntimeTaskKind::QueryRerank),
                    binding_id: None,
                    provider_catalog_id,
                    model_catalog_id,
                    call_kind: "query_rerank".to_string(),
                },
            )
            .await
            .context("failed to reserve durable-rollup provider call")?;
        let original = billing
            .complete_reserved_provider_call(
                &fixture.state,
                provider_call_id,
                &serde_json::json!({"input_tokens": 1000, "total_tokens": 1000}),
            )
            .await
            .context("failed to complete durable-rollup provider call")?
            .context("completed provider call should have a priced rollup")?;
        let clean_state = billing_repository::get_execution_cost_rollup_state(
            &fixture.state.persistence.postgres,
            "query_execution",
            fixture.query_execution_id,
        )
        .await?
        .context("durable rollup state missing after authoritative completion")?;
        assert_eq!(clean_state.applied_generation, clean_state.dirty_generation);

        // Model a process crash after a authoritative transaction dirtied its
        // generation but before the eager derived rebuild could run.
        sqlx::query(
            "update billing_execution_cost
             set total_cost = 0,
                 provider_call_count = 0
             where owning_execution_kind = 'query_execution'::billing_owning_execution_kind
               and owning_execution_id = $1",
        )
        .bind(fixture.query_execution_id)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to inject crash-stale derived rollup")?;
        let provider_call = billing_repository::get_provider_call_by_id(
            &fixture.state.persistence.postgres,
            provider_call_id,
        )
        .await?
        .context("durable-rollup provider call missing")?;
        billing_repository::mark_execution_cost_rollup_dirty(
            &fixture.state.persistence.postgres,
            &provider_call,
        )
        .await
        .context("failed to persist simulated dirty generation")?;

        let pending_summary = billing
            .get_library_cost_summary(&fixture.state, fixture.library_id)
            .await
            .context("a dirty generation must still answer with a typed freshness marker")?;
        assert_eq!(pending_summary.cost_rollup_freshness, CostRollupFreshness::Reconciling);
        assert_eq!(pending_summary.total_cost, None);
        assert_eq!(pending_summary.currency_code, None);

        let (first_repair, second_repair) = tokio::join!(
            ironrag_backend::services::maintenance::scheduler::repair_dirty_billing_execution_costs_once(
                &fixture.state,
                100,
            ),
            ironrag_backend::services::maintenance::scheduler::repair_dirty_billing_execution_costs_once(
                &fixture.state,
                100,
            ),
        );
        let first_repair = first_repair.context("first durable execution-cost repair failed")?;
        let second_repair = second_repair.context("second durable execution-cost repair failed")?;
        assert_eq!(first_repair.examined + second_repair.examined, 1);
        assert_eq!(first_repair.repaired + second_repair.repaired, 1);
        assert_eq!(first_repair.failed + second_repair.failed, 0);

        let repaired = billing
            .get_execution_cost(&fixture.state, "query_execution", fixture.query_execution_id)
            .await
            .context("failed to load repaired execution cost")?;
        assert_eq!(repaired.total_cost, original.total_cost);
        assert_eq!(repaired.provider_call_count, original.provider_call_count);
        let repaired_state = billing_repository::get_execution_cost_rollup_state(
            &fixture.state.persistence.postgres,
            "query_execution",
            fixture.query_execution_id,
        )
        .await?
        .context("durable rollup state missing after repair")?;
        assert_eq!(repaired_state.applied_generation, repaired_state.dirty_generation);

        let repeated =
            ironrag_backend::services::maintenance::scheduler::repair_dirty_billing_execution_costs_once(
                &fixture.state,
                100,
            )
            .await
            .context("idempotent durable repair retry failed")?;
        assert_eq!(repeated.examined, 0);
        assert_eq!(repeated.repaired, 0);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn historical_query_billing_repairs_after_conversation_retention_deletes_execution()
-> Result<()> {
    let fixture = BillingRollupsFixture::create_with_max_connections(4).await?;

    let result = async {
        let billing = &fixture.state.application_services.billing;
        let (provider_catalog_id, model_catalog_id) =
            openai_chat_catalog_ids(&fixture.state.persistence.postgres).await?;
        let provider_call_id = billing
            .reserve_execution_provider_call(
                &fixture.state,
                ReserveExecutionProviderCallCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    owning_execution_kind: "query_execution".to_string(),
                    owning_execution_id: fixture.query_execution_id,
                    runtime_execution_id: Some(fixture.query_runtime_execution_id),
                    runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer),
                    binding_id: None,
                    provider_catalog_id,
                    model_catalog_id,
                    call_kind: "query_answer".to_string(),
                },
            )
            .await
            .context("failed to reserve retained-history provider call")?;
        let original = billing
            .complete_reserved_provider_call(
                &fixture.state,
                provider_call_id,
                &serde_json::json!({"input_tokens": 1000, "total_tokens": 1000}),
            )
            .await
            .context("failed to complete retained-history provider call")?
            .context("completed provider call should have a priced rollup")?;

        let deleted = sqlx::query(
            "delete from query_conversation
             where id = (
                 select conversation_id
                 from query_execution
                 where id = $1
             )",
        )
        .bind(fixture.query_execution_id)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to model conversation retention")?;
        assert_eq!(deleted.rows_affected(), 1);
        assert!(
            query_repository::get_execution_by_id(
                &fixture.state.persistence.postgres,
                fixture.query_execution_id,
            )
            .await?
            .is_none(),
            "conversation retention must cascade the historical execution",
        );

        let provider_call = billing_repository::get_provider_call_by_id(
            &fixture.state.persistence.postgres,
            provider_call_id,
        )
        .await?
        .context("billing audit history must survive query retention")?;
        billing_repository::mark_execution_cost_rollup_dirty(
            &fixture.state.persistence.postgres,
            &provider_call,
        )
        .await
        .context("failed to model migration-created historical dirty generation")?;

        let pending = billing
            .get_library_cost_summary(&fixture.state, fixture.library_id)
            .await
            .context("historical dirty generation must answer with a freshness marker before repair")?;
        assert_eq!(pending.cost_rollup_freshness, CostRollupFreshness::Reconciling);
        assert_eq!(pending.total_cost, None);

        let repaired =
            ironrag_backend::services::maintenance::scheduler::repair_dirty_billing_execution_costs_once(
                &fixture.state,
                100,
            )
            .await
            .context("historical query billing repair failed")?;
        assert_eq!(repaired.examined, 1);
        assert_eq!(repaired.repaired, 1);
        assert_eq!(repaired.failed, 0);

        let refreshed = billing
            .get_execution_cost(&fixture.state, "query_execution", fixture.query_execution_id)
            .await
            .context("historical execution cost remained unreadable after repair")?;
        assert_eq!(refreshed.total_cost, original.total_cost);
        assert_eq!(refreshed.provider_call_count, original.provider_call_count);
        let repaired_summary = billing
            .get_library_cost_summary(&fixture.state, fixture.library_id)
            .await
            .context("historical billing cursor still blocked library reads after repair")?;
        assert_eq!(repaired_summary.cost_rollup_freshness, CostRollupFreshness::Current);
        assert_eq!(repaired_summary.total_cost, Some(original.total_cost));
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn ordinary_capture_persists_dirty_generation_before_eager_rollup() -> Result<()> {
    let fixture = BillingRollupsFixture::create_with_max_connections(4).await?;

    let result = async {
        let (provider_catalog_id, model_catalog_id) =
            openai_chat_catalog_ids(&fixture.state.persistence.postgres).await?;
        let mut transaction = fixture.state.persistence.postgres.begin().await?;
        let provider_call = billing_repository::create_provider_call(
            &mut *transaction,
            &billing_repository::NewBillingProviderCall {
                id: Uuid::now_v7(),
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                binding_id: None,
                owning_execution_kind: "query_execution",
                owning_execution_id: fixture.query_execution_id,
                runtime_execution_id: Some(fixture.query_runtime_execution_id),
                runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer.as_str()),
                provider_catalog_id,
                model_catalog_id,
                call_kind: "query_answer",
                call_state: "completed",
                completed_at: Some(Utc::now()),
            },
        )
        .await
        .context("failed to persist ordinary completed provider call")?;
        assert_eq!(provider_call.call_state, "completed");
        transaction.commit().await?;

        // This is the exact crash window between the authoritative transaction and
        // the eager derived rebuild in `capture_execution_provider_call`.
        let dirty_state = billing_repository::get_execution_cost_rollup_state(
            &fixture.state.persistence.postgres,
            "query_execution",
            fixture.query_execution_id,
        )
        .await?
        .context("ordinary capture must create a durable rollup cursor")?;
        assert!(dirty_state.applied_generation < dirty_state.dirty_generation);

        let pending = fixture
            .state
            .application_services
            .billing
            .get_library_cost_summary(&fixture.state, fixture.library_id)
            .await
            .context("scope reads must answer with a freshness marker in the capture crash window")?;
        assert_eq!(pending.cost_rollup_freshness, CostRollupFreshness::Reconciling);
        assert_eq!(pending.total_cost, None);

        let repaired =
            ironrag_backend::services::maintenance::scheduler::repair_dirty_billing_execution_costs_once(
                &fixture.state,
                100,
            )
            .await?;
        assert_eq!(repaired.examined, 1);
        assert_eq!(repaired.repaired, 1);
        assert_eq!(repaired.failed, 0);

        let clean_state = billing_repository::get_execution_cost_rollup_state(
            &fixture.state.persistence.postgres,
            "query_execution",
            fixture.query_execution_id,
        )
        .await?
        .context("ordinary capture rollup cursor disappeared after repair")?;
        assert_eq!(clean_state.applied_generation, clean_state.dirty_generation);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn scope_cost_snapshot_never_mixes_clean_health_with_a_newer_dirty_generation() -> Result<()>
{
    let fixture = BillingRollupsFixture::create_with_max_connections(4).await?;

    let result = async {
        let query_model =
            openai_catalog_model_for_purpose(&fixture.state.persistence.postgres, "query_answer")
                .await?;
        let initial = fixture
            .state
            .application_services
            .billing
            .capture_query_execution(
                &fixture.state,
                CaptureQueryExecutionBillingCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    execution_id: fixture.query_execution_id,
                    runtime_execution_id: fixture.query_runtime_execution_id,
                    binding_id: None,
                    provider_kind: "openai".to_string(),
                    model_name: query_model.model_name,
                    call_kind: "query_answer".to_string(),
                    usage_json: serde_json::json!({
                        "prompt_tokens": 1000,
                        "completion_tokens": 100,
                        "total_tokens": 1100,
                    }),
                },
            )
            .await?
            .context("initial provider call should be priced")?;

        // Hold an old, repeatable Postgres snapshot open. The repository
        // method itself is one statement in production; the explicit
        // transaction here lets the test deterministically commit a authoritative
        // mutation between two reads of that same database snapshot.
        let mut old_snapshot = fixture.state.persistence.postgres.begin().await?;
        sqlx::query("set transaction isolation level repeatable read, read only")
            .execute(&mut *old_snapshot)
            .await?;
        let before = billing_repository::get_library_cost_read_snapshot(
            &mut *old_snapshot,
            fixture.library_id,
        )
        .await?;
        assert!(!before.rollup_dirty);
        assert!(before.terminal_error_code.is_none());
        assert_eq!(before.rows.len(), 1);
        assert_eq!(before.rows[0].total_cost, initial.total_cost);
        assert_eq!(before.rows[0].provider_call_count, 1);

        let (provider_catalog_id, model_catalog_id) =
            openai_chat_catalog_ids(&fixture.state.persistence.postgres).await?;
        let provider_call = billing_repository::create_provider_call(
            &fixture.state.persistence.postgres,
            &billing_repository::NewBillingProviderCall {
                id: Uuid::now_v7(),
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                binding_id: None,
                owning_execution_kind: "query_execution",
                owning_execution_id: fixture.query_execution_id,
                runtime_execution_id: Some(fixture.query_plan_runtime_execution_id),
                runtime_task_kind: Some(RuntimeTaskKind::QueryPlan.as_str()),
                provider_catalog_id,
                model_catalog_id,
                call_kind: "query_planning",
                call_state: "completed",
                completed_at: Some(Utc::now()),
            },
        )
        .await?;
        assert_eq!(provider_call.call_state, "completed");

        // The old snapshot remains internally consistent: it sees neither the
        // new dirty generation nor a newer authoritative call paired with the old
        // scalar aggregate.
        let repeated = billing_repository::get_library_cost_read_snapshot(
            &mut *old_snapshot,
            fixture.library_id,
        )
        .await?;
        assert!(!repeated.rollup_dirty);
        assert_eq!(repeated.rows.len(), 1);
        assert_eq!(repeated.rows[0].provider_call_count, 1);
        old_snapshot.commit().await?;

        // A new statement snapshot sees the committed generation and returns
        // the blocker in the same rowset. Stale aggregates are deliberately
        // omitted, so callers cannot accidentally ignore health and expose
        // the old total.
        let current = billing_repository::get_library_cost_read_snapshot(
            &fixture.state.persistence.postgres,
            fixture.library_id,
        )
        .await?;
        assert!(current.rollup_dirty);
        assert!(current.rows.is_empty());
        let pending = fixture
            .state
            .application_services
            .billing
            .get_library_cost_summary(&fixture.state, fixture.library_id)
            .await
            .context("a dirty authoritative generation must mark the scalar scope reconciling")?;
        assert_eq!(pending.cost_rollup_freshness, CostRollupFreshness::Reconciling);
        assert_eq!(pending.total_cost, None);
        assert_eq!(pending.provider_call_count, None);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn mixed_currency_execution_becomes_terminal_without_retry_or_stale_scope_totals()
-> Result<()> {
    let fixture = BillingRollupsFixture::create_with_max_connections(4).await?;

    let result = async {
        let billing = &fixture.state.application_services.billing;
        let query_model = openai_catalog_model_for_purpose(
            &fixture.state.persistence.postgres,
            "query_answer",
        )
        .await?;
        let usage = serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 100,
            "total_tokens": 1100,
        });
        billing
            .capture_query_execution(
                &fixture.state,
                CaptureQueryExecutionBillingCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    execution_id: fixture.query_execution_id,
                    runtime_execution_id: fixture.query_runtime_execution_id,
                    binding_id: None,
                    provider_kind: "openai".to_string(),
                    model_name: query_model.model_name.clone(),
                    call_kind: "query_answer".to_string(),
                    usage_json: usage.clone(),
                },
            )
            .await
            .context("failed to capture initial USD provider call")?
            .context("initial USD provider call should be priced")?;

        let repriced = sqlx::query(
            "update ai_model_catalog model
             set currency_code = 'EUR'
             from ai_provider_catalog provider
             where provider.id = model.provider_catalog_id
               and provider.provider_kind = 'openai'
               and model.id = $1",
        )
        .bind(query_model.model_id)
        .execute(&fixture.state.persistence.postgres)
        .await?;
        assert!(repriced.rows_affected() > 0, "synthetic model prices were not repriced");

        let capture_error = billing
            .capture_query_execution(
                &fixture.state,
                CaptureQueryExecutionBillingCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    execution_id: fixture.query_execution_id,
                    runtime_execution_id: fixture.query_runtime_execution_id,
                    binding_id: None,
                    provider_kind: "openai".to_string(),
                    model_name: query_model.model_name,
                    call_kind: "query_answer".to_string(),
                    usage_json: usage,
                },
            )
            .await
            .expect_err("an execution-level scalar cannot combine USD and EUR charges");
        assert!(capture_error.to_string().contains("multiple currencies"));

        let terminal_state = billing_repository::get_execution_cost_rollup_state(
            &fixture.state.persistence.postgres,
            "query_execution",
            fixture.query_execution_id,
        )
        .await?
        .context("mixed-currency terminal rollup state is missing")?;
        assert_eq!(terminal_state.applied_generation, terminal_state.dirty_generation);
        assert_eq!(terminal_state.terminal_error_code.as_deref(), Some("mixed_currency"));

        let repeated =
            ironrag_backend::services::maintenance::scheduler::repair_dirty_billing_execution_costs_once(
                &fixture.state,
                100,
            )
            .await?;
        assert_eq!(repeated.examined, 0, "terminal work must not stay on the retry queue");

        for error in [
            billing
                .get_execution_cost(
                    &fixture.state,
                    "query_execution",
                    fixture.query_execution_id,
                )
                .await
                .expect_err("execution total must fail closed for mixed currencies"),
            billing
                .get_library_cost_summary(&fixture.state, fixture.library_id)
                .await
                .expect_err("library total must fail closed for mixed currencies"),
            billing
                .get_workspace_cost_summary(&fixture.state, fixture.workspace_id)
                .await
                .expect_err("workspace total must fail closed for mixed currencies"),
        ] {
            let message = error.to_string();
            assert!(message.contains("multiple currencies"));
            assert!(!message.contains("being reconciled"));
        }
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn scope_summaries_fail_closed_instead_of_summing_mixed_currencies() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        billing_repository::upsert_execution_cost(
            &fixture.state.persistence.postgres,
            &billing_repository::UpsertBillingExecutionCost {
                owning_execution_kind: "query_execution",
                owning_execution_id: fixture.query_execution_id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                knowledge_document_id: None,
                total_cost: Decimal::new(125, 2),
                currency_code: "USD",
                provider_call_count: 1,
                unpriced_provider_call_count: 0,
            },
        )
        .await
        .context("failed to seed USD execution rollup")?;
        billing_repository::upsert_execution_cost(
            &fixture.state.persistence.postgres,
            &billing_repository::UpsertBillingExecutionCost {
                owning_execution_kind: "ingest_attempt",
                owning_execution_id: fixture.ingest_attempt_id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                knowledge_document_id: None,
                total_cost: Decimal::new(900, 2),
                currency_code: "EUR",
                provider_call_count: 1,
                unpriced_provider_call_count: 0,
            },
        )
        .await
        .context("failed to seed EUR execution rollup")?;

        let library_error = fixture
            .state
            .application_services
            .billing
            .get_library_cost_summary(&fixture.state, fixture.library_id)
            .await
            .expect_err("a library total must never combine unlike currencies");
        assert!(library_error.to_string().contains("multiple currencies"));

        let workspace_error = fixture
            .state
            .application_services
            .billing
            .get_workspace_cost_summary(&fixture.state, fixture.workspace_id)
            .await
            .expect_err("a workspace total must never combine unlike currencies");
        assert!(workspace_error.to_string().contains("multiple currencies"));
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn dirty_library_defers_only_its_scope_while_a_clean_library_stays_current() -> Result<()> {
    let fixture = BillingRollupsFixture::create_with_max_connections(4).await?;

    let result = async {
        let second_library = fixture
            .state
            .application_services
            .catalog
            .create_library(
                &fixture.state,
                CreateLibraryCommand {
                    workspace_id: fixture.workspace_id,
                    slug: Some(format!("billing-library-b-{}", Uuid::now_v7().simple())),
                    display_name: "Billing Rollups Library B".to_string(),
                    description: None,
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create second billing library")?;

        // Library A: a committed, fully-applied rollup — exact cost.
        let clean_execution_id = Uuid::now_v7();
        let clean_total = Decimal::new(1234, 2);
        billing_repository::upsert_execution_cost(
            &fixture.state.persistence.postgres,
            &billing_repository::UpsertBillingExecutionCost {
                owning_execution_kind: "query_execution",
                owning_execution_id: clean_execution_id,
                workspace_id: fixture.workspace_id,
                library_id: fixture.library_id,
                knowledge_document_id: None,
                total_cost: clean_total,
                currency_code: "USD",
                provider_call_count: 2,
                unpriced_provider_call_count: 0,
            },
        )
        .await
        .context("failed to seed the clean library rollup")?;

        // Library B: a committed provider call whose generation is dirty, so
        // its aggregate lags. The rollup state row carries B's scope.
        let dirty_execution_id = Uuid::now_v7();
        billing_repository::upsert_execution_cost(
            &fixture.state.persistence.postgres,
            &billing_repository::UpsertBillingExecutionCost {
                owning_execution_kind: "ingest_attempt",
                owning_execution_id: dirty_execution_id,
                workspace_id: fixture.workspace_id,
                library_id: second_library.id,
                knowledge_document_id: None,
                total_cost: Decimal::new(777, 2),
                currency_code: "USD",
                provider_call_count: 1,
                unpriced_provider_call_count: 0,
            },
        )
        .await
        .context("failed to seed the dirty library rollup")?;
        sqlx::query(
            "insert into billing_execution_cost_rollup_state
             (owning_execution_kind, owning_execution_id, workspace_id, library_id,
              dirty_generation, applied_generation, dirty_at, repair_attempts,
              next_attempt_at, last_error)
             values ('ingest_attempt', $1, $2, $3, 2, 1, now(), 0, now(), null)",
        )
        .bind(dirty_execution_id)
        .bind(fixture.workspace_id)
        .bind(second_library.id)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to dirty the second library generation")?;

        let billing = &fixture.state.application_services.billing;
        let clean_library = billing
            .get_library_cost_summary(&fixture.state, fixture.library_id)
            .await
            .context("a clean library summary must remain readable")?;
        assert_eq!(clean_library.cost_rollup_freshness, CostRollupFreshness::Current);
        assert_eq!(clean_library.total_cost, Some(clean_total));

        let dirty_library = billing
            .get_library_cost_summary(&fixture.state, second_library.id)
            .await
            .context("a dirty library summary must answer with a freshness marker")?;
        assert_eq!(dirty_library.cost_rollup_freshness, CostRollupFreshness::Reconciling);
        assert_eq!(dirty_library.total_cost, None);
        assert_eq!(dirty_library.cost_coverage, None);

        // The workspace aggregate spans both libraries, so one dirty library
        // defers the whole scope: no stale A+B total is served as exact.
        let workspace = billing
            .get_workspace_cost_summary(&fixture.state, fixture.workspace_id)
            .await
            .context("a workspace with a dirty library must answer reconciling")?;
        assert_eq!(workspace.cost_rollup_freshness, CostRollupFreshness::Reconciling);
        assert_eq!(workspace.total_cost, None);

        // Repair B and the whole workspace becomes current with the exact sum.
        let repaired =
            ironrag_backend::services::maintenance::scheduler::repair_dirty_billing_execution_costs_once(
                &fixture.state,
                100,
            )
            .await
            .context("failed to repair the dirty library rollup")?;
        assert!(repaired.repaired >= 1);

        let workspace_after = billing
            .get_workspace_cost_summary(&fixture.state, fixture.workspace_id)
            .await
            .context("the workspace must become current after repair")?;
        assert_eq!(workspace_after.cost_rollup_freshness, CostRollupFreshness::Current);
        assert_eq!(workspace_after.total_cost, Some(clean_total + Decimal::new(777, 2)));
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// IRG-236: while a library rollup reconciles, a cost-dependent list read is
/// withheld (`scope_pending`) — whether its dependence is a cost sort or an
/// Intl-normalized search — while a metadata read stays available and truthful.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn document_list_scope_pending_tracks_cost_dependence() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;
    let result = async {
        let library_id = fixture.library_id;
        // Dirty this library's rollup generation.
        sqlx::query(
            "insert into billing_execution_cost_rollup_state
             (owning_execution_kind, owning_execution_id, workspace_id, library_id,
              dirty_generation, applied_generation, dirty_at, repair_attempts,
              next_attempt_at, last_error)
             values ('ingest_attempt', $1, $2, $3, 2, 1, now(), 0, now(), null)",
        )
        .bind(Uuid::now_v7())
        .bind(fixture.workspace_id)
        .bind(library_id)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to dirty the list fixture rollup")?;

        let content = &fixture.state.application_services.content;
        let command =
            |sort, search: Option<String>, normalized: Option<String>| ListDocumentsPageCommand {
                library_id,
                include_deleted: false,
                cursor: None,
                limit: 10,
                search,
                normalized_search: normalized,
                search_statuses: Vec::new(),
                filters: Default::default(),
                search_ranges: Default::default(),
                snapshot_at: Utc::now(),
                sort,
                sort_desc: true,
                status_filter: Vec::new(),
                id_filter: None,
                include_status_counts: true,
            };

        let cost_sorted = content
            .list_documents_page(&fixture.state, command(DocumentListSortColumn::Cost, None, None))
            .await?;
        assert!(cost_sorted.scope_pending, "a cost sort must be withheld while reconciling");
        assert!(cost_sorted.items.is_empty());
        assert!(cost_sorted.next_cursor.is_none());
        assert!(cost_sorted.total_count.is_none());

        let normalized_only = content
            .list_documents_page(
                &fixture.state,
                command(DocumentListSortColumn::CreatedAt, None, Some("acme".to_string())),
            )
            .await?;
        assert!(normalized_only.scope_pending, "a normalized-only search is cost-dependent");
        assert!(normalized_only.total_count.is_none());

        let metadata = content
            .list_documents_page(
                &fixture.state,
                command(DocumentListSortColumn::CreatedAt, None, None),
            )
            .await?;
        assert!(!metadata.scope_pending, "a metadata read stays available");
        assert_eq!(
            metadata.billing_rollup_freshness,
            CostRollupFreshness::Reconciling,
            "freshness describes the rollup even when the result is served"
        );
        assert_eq!(metadata.total_count, Some(0));
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn stale_provider_call_reaper_is_bounded_concurrent_and_idempotent() -> Result<()> {
    let fixture = BillingRollupsFixture::create_with_max_connections(4).await?;

    let result = async {
        let billing = &fixture.state.application_services.billing;
        let (provider_catalog_id, model_catalog_id) =
            openai_chat_catalog_ids(&fixture.state.persistence.postgres).await?;
        let reserve = || ReserveExecutionProviderCallCommand {
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            owning_execution_kind: "query_execution".to_string(),
            owning_execution_id: fixture.query_execution_id,
            runtime_execution_id: Some(fixture.query_rerank_runtime_execution_id),
            runtime_task_kind: Some(RuntimeTaskKind::QueryRerank),
            binding_id: None,
            provider_catalog_id,
            model_catalog_id,
            call_kind: "query_rerank".to_string(),
        };

        let terminal_call_id = billing
            .reserve_execution_provider_call(&fixture.state, reserve())
            .await
            .context("failed to reserve terminal control provider call")?;
        billing
            .finish_reserved_provider_call_without_usage(
                &fixture.state,
                terminal_call_id,
                "failed",
            )
            .await
            .context("failed to terminalize control provider call")?;

        let mut stale_call_ids = Vec::with_capacity(3);
        for _ in 0..3 {
            stale_call_ids.push(
                billing
                    .reserve_execution_provider_call(&fixture.state, reserve())
                    .await
                    .context("failed to reserve stale provider call")?,
            );
        }
        let fresh_call_id = billing
            .reserve_execution_provider_call(&fixture.state, reserve())
            .await
            .context("failed to reserve fresh provider call")?;

        let mut old_call_ids = stale_call_ids.clone();
        old_call_ids.push(terminal_call_id);
        sqlx::query(
            "update billing_provider_call
             set started_at = now() - interval '10 minutes'
             where id = any($1)",
        )
        .bind(&old_call_ids)
        .execute(&fixture.state.persistence.postgres)
        .await
        .context("failed to age provider-call reaper fixtures")?;

        let stale_after = std::time::Duration::from_secs(5 * 60);
        let (first_reaper, second_reaper) = tokio::join!(
            ironrag_backend::services::maintenance::scheduler::reap_stale_billing_provider_calls_once(
                &fixture.state,
                stale_after,
                2,
            ),
            ironrag_backend::services::maintenance::scheduler::reap_stale_billing_provider_calls_once(
                &fixture.state,
                stale_after,
                2,
            ),
        );
        let first_reaper = first_reaper.context("first concurrent reaper failed")?;
        let second_reaper = second_reaper.context("second concurrent reaper failed")?;
        assert!(first_reaper.reaped_provider_calls <= 2);
        assert!(second_reaper.reaped_provider_calls <= 2);
        assert_eq!(
            first_reaper.reaped_provider_calls + second_reaper.reaped_provider_calls,
            3,
            "concurrent SKIP LOCKED batches must cancel every stale reservation exactly once",
        );
        assert_eq!(
            first_reaper.rollups_refreshed + second_reaper.rollups_refreshed,
            first_reaper.affected_executions + second_reaper.affected_executions,
        );
        assert_eq!(first_reaper.rollup_failures + second_reaper.rollup_failures, 0);

        for provider_call_id in stale_call_ids {
            let row = billing_repository::get_provider_call_by_id(
                &fixture.state.persistence.postgres,
                provider_call_id,
            )
            .await?
            .context("reaped provider call missing")?;
            assert_eq!(row.call_state, "canceled");
            assert!(row.completed_at.is_some());
        }
        let terminal = billing_repository::get_provider_call_by_id(
            &fixture.state.persistence.postgres,
            terminal_call_id,
        )
        .await?
        .context("terminal control provider call missing")?;
        assert_eq!(terminal.call_state, "failed", "the reaper must not relabel terminal calls");
        let fresh = billing_repository::get_provider_call_by_id(
            &fixture.state.persistence.postgres,
            fresh_call_id,
        )
        .await?
        .context("fresh provider call missing")?;
        assert_eq!(fresh.call_state, "started", "the reaper must respect the age cutoff");

        let repeated = ironrag_backend::services::maintenance::scheduler::reap_stale_billing_provider_calls_once(
            &fixture.state,
            stale_after,
            2,
        )
        .await
        .context("idempotent reaper retry failed")?;
        assert_eq!(repeated.reaped_provider_calls, 0);
        assert_eq!(repeated.affected_executions, 0);
        assert_eq!(repeated.rollups_refreshed, 0);
        assert_eq!(repeated.rollup_failures, 0);

        let refreshed = billing
            .get_execution_cost(&fixture.state, "query_execution", fixture.query_execution_id)
            .await
            .context("failed to load reaper-refreshed execution cost")?;
        assert_eq!(
            refreshed.provider_call_count, 5,
            "the bounded unique-execution repair must include every terminal and active attempt",
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn execution_cost_returns_zero_rollup_for_ingest_attempt_without_provider_calls() -> Result<()>
{
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let billing = &fixture.state.application_services.billing;
        let zero_cost = billing
            .get_execution_cost(&fixture.state, "ingest_attempt", fixture.ingest_attempt_id)
            .await
            .context("failed to load zero-cost ingest execution")?;
        assert_eq!(zero_cost.total_cost, Decimal::ZERO);
        assert_eq!(zero_cost.provider_call_count, 0);
        assert_eq!(zero_cost.currency_code, "USD");

        let (provider_catalog_id, model_catalog_id) =
            openai_chat_catalog_ids(&fixture.state.persistence.postgres).await?;
        let failed_call_id = billing
            .reserve_execution_provider_call(
                &fixture.state,
                ReserveExecutionProviderCallCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    owning_execution_kind: "ingest_attempt".to_string(),
                    owning_execution_id: fixture.ingest_attempt_id,
                    runtime_execution_id: None,
                    runtime_task_kind: None,
                    binding_id: None,
                    provider_catalog_id,
                    model_catalog_id,
                    call_kind: "extract_graph".to_string(),
                },
            )
            .await
            .context("failed to reserve zero-usage provider attempt")?;
        billing
            .finish_reserved_provider_call_without_usage(&fixture.state, failed_call_id, "failed")
            .await
            .context("failed to terminalize zero-usage provider attempt")?;
        let failed_cost = billing
            .get_execution_cost(&fixture.state, "ingest_attempt", fixture.ingest_attempt_id)
            .await
            .context("failed to load zero-cost terminal provider attempt")?;
        assert_eq!(failed_cost.total_cost, Decimal::ZERO);
        assert_eq!(failed_cost.provider_call_count, 1);
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis"]
async fn inline_ingest_billing_capture_produces_provider_calls_and_rollup() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let billing = &fixture.state.application_services.billing;
        let graph_model =
            openai_catalog_model_for_purpose(&fixture.state.persistence.postgres, "extract_graph")
                .await?;
        let ingest_cost = billing
            .capture_ingest_attempt(
                &fixture.state,
                CaptureIngestAttemptBillingCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    attempt_id: fixture.ingest_attempt_id,
                    binding_id: None,
                    provider_kind: "openai".to_string(),
                    model_name: graph_model.model_name,
                    call_kind: "extract_graph".to_string(),
                    usage_json: serde_json::json!({
                        "prompt_tokens": 8000,
                        "completion_tokens": 2000,
                        "total_tokens": 10000,
                    }),
                },
            )
            .await
            .context("failed to capture inline ingest billing")?
            .context("inline ingest capture should return priced rollup")?;

        assert_ne!(ingest_cost.total_cost, Decimal::ZERO);
        assert_eq!(ingest_cost.provider_call_count, 1);
        assert_eq!(ingest_cost.currency_code, "USD");

        let provider_calls = billing
            .list_execution_provider_calls_page(
                &fixture.state,
                ironrag_backend::services::ops::billing::BillingExecutionPageQuery {
                    execution_kind: BillingExecutionOwnerKind::IngestAttempt,
                    execution_id: fixture.ingest_attempt_id,
                    cursor: None,
                    limit: 100,
                    include_total: false,
                },
            )
            .await
            .context("failed to list ingest attempt provider calls")?
            .items;
        assert!(!provider_calls.is_empty());
        assert!(provider_calls.iter().any(|row| row.call_kind == "extract_graph"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
