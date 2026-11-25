use std::collections::BTreeMap;

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
            ai_repository::{
                AdmittedCaller, CALLER_BINDING_FORMAT_VERSION, CallerBinding, FinancialAdmission,
                FinancialAdmissionState, FinancialModelAllowance, FinancialRequestEnvelope,
                RequiredFinancialScope,
            },
            billing_repository,
            content_document_list_repository::DocumentListSortColumn,
            query_repository, runtime_repository,
        },
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        content::service::ListDocumentsPageCommand,
        ingest::service::{AdmitIngestJobCommand, INGEST_STAGE_EMBED_CHUNK, LeaseAttemptCommand},
        ops::billing::{
            CaptureExecutionBillingCommand, CaptureIngestAttemptBillingCommand,
            CaptureQueryExecutionBillingCommand,
        },
    },
    shared::{provider_http::DispatchCaller, provider_send::ProviderSendAttribution},
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

/// The attribution of one test-owned provider call in the fixture's scope.
struct TestProviderCall<'a> {
    owning_execution_kind: &'a str,
    owning_execution_id: Uuid,
    runtime_execution_id: Option<Uuid>,
    runtime_task_kind: Option<&'a str>,
    provider_catalog_id: Uuid,
    model_catalog_id: Uuid,
    call_kind: &'a str,
}

/// Writes the one durable row a send owns, in the `started` state every
/// settlement under test expects, at the attribution the test names.
///
/// The product writer of that row is the physical-send admission; a test that
/// only exercises settlement names the row itself instead of routing a paid
/// request through an admission it does not mean to test.
async fn write_provider_call(
    fixture: &BillingRollupsFixture,
    provider_call_id: Uuid,
    call: TestProviderCall<'_>,
) -> Result<Uuid> {
    billing_repository::create_provider_call(
        &fixture.state.persistence.postgres,
        &billing_repository::NewBillingProviderCall {
            id: provider_call_id,
            workspace_id: fixture.workspace_id,
            library_id: fixture.library_id,
            binding_id: None,
            owning_execution_kind: call.owning_execution_kind,
            owning_execution_id: call.owning_execution_id,
            runtime_execution_id: call.runtime_execution_id,
            runtime_task_kind: call.runtime_task_kind,
            provider_catalog_id: call.provider_catalog_id,
            model_catalog_id: call.model_catalog_id,
            call_kind: call.call_kind,
            call_state: "started",
            completed_at: None,
        },
    )
    .await
    .with_context(|| format!("failed to write provider call {provider_call_id}"))?;
    Ok(provider_call_id)
}

async fn count_provider_calls(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<i64> {
    let count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from billing_provider_call
         where owning_execution_kind = $1::billing_owning_execution_kind
           and owning_execution_id = $2",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_one(postgres)
    .await
    .context("failed to count provider call rows")?;
    Ok(count)
}

/// The grant the product reads back from the provider row, as typed data.
fn decode_admission(value: serde_json::Value) -> Result<FinancialAdmission> {
    Ok(serde_json::from_value(value)?)
}

/// The attribution of one physical send of the fixture's query answer call.
fn send_attribution(
    fixture: &BillingRollupsFixture,
    provider_catalog_id: Uuid,
    model_catalog_id: Uuid,
) -> ProviderSendAttribution {
    ProviderSendAttribution {
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        owning_execution_kind: "query_execution".to_string(),
        owning_execution_id: fixture.query_execution_id,
        call_kind: "query_answer".to_string(),
        binding_id: None,
        provider_catalog_id,
        model_catalog_id,
        runtime_execution_id: Some(fixture.query_runtime_execution_id),
        runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer.as_str()),
    }
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

/// Publishes an active chat admission for the synthetic catalog and returns the
/// scope a physical send must be admitted under.
async fn publish_synthetic_chat_admission(
    postgres: &PgPool,
    catalog: &SyntheticDisjointPricingCatalog,
    endpoint: &str,
    mutation_id: Uuid,
    principal_id: Uuid,
) -> Result<(RequiredFinancialScope, Uuid)> {
    let mut admission = FinancialAdmission {
        version: 1,
        provider_id: catalog.provider_catalog_id,
        scope_id: Uuid::now_v7(),
        policy_fingerprint: String::new(),
        cutoff: Utc::now() + chrono::Duration::hours(1),
        state: FinancialAdmissionState::Active,
        grant_microusd: 20,
        global_limit: 2,
        global_consumed: 0,
        models: BTreeMap::from([(
            catalog.model_name.clone(),
            FinancialModelAllowance {
                endpoint: endpoint.to_string(),
                envelope: FinancialRequestEnvelope::Chat {
                    allowed_service_tiers: vec!["default".to_string()],
                    max_input_bytes: 4096,
                    max_output_tokens: 1024,
                    max_input_tokens: 100_000,
                    media: None,
                },
                attempt_microusd: 10,
                limit: 2,
                consumed: 0,
            },
        )]),
        caller_binding: Some(CallerBinding {
            format_version: CALLER_BINDING_FORMAT_VERSION,
            generation: 1,
            callers: vec![AdmittedCaller::MutationOrigin { mutation_id, principal_id }],
        }),
        history: Vec::new(),
    };
    admission.policy_fingerprint = admission.immutable_fingerprint()?;
    let required_scope = RequiredFinancialScope {
        provider_id: admission.provider_id,
        scope_id: admission.scope_id,
        policy_fingerprint: admission.policy_fingerprint.clone(),
    };
    let published = sqlx::query(
        "update ai_provider_catalog
         set capability_flags_json =
             jsonb_set(capability_flags_json, '{financialAdmission}', $2::jsonb, true)
         where id = $1",
    )
    .bind(catalog.provider_catalog_id)
    .bind(serde_json::to_value(&admission)?)
    .execute(postgres)
    .await?;
    assert_eq!(published.rows_affected(), 1);
    Ok((required_scope, catalog.provider_catalog_id))
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
            let provider_call_id = write_provider_call(
                &fixture,
                Uuid::now_v7(),
                TestProviderCall {
                    owning_execution_kind: "query_execution",
                    owning_execution_id: fixture.query_execution_id,
                    runtime_execution_id: Some(fixture.query_runtime_execution_id),
                    runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer.as_str()),
                    provider_catalog_id: catalog.provider_catalog_id,
                    model_catalog_id: catalog.model_catalog_id,
                    call_kind: "query_answer",
                },
            )
            .await
            .with_context(|| {
                format!("failed to write provider call at {request_input_tokens} request tokens")
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
        let rerank_provider_call_id = write_provider_call(
            &fixture,
            Uuid::now_v7(),
            TestProviderCall {
                owning_execution_kind: "query_execution",
                owning_execution_id: fixture.query_execution_id,
                runtime_execution_id: Some(fixture.query_rerank_runtime_execution_id),
                runtime_task_kind: Some(RuntimeTaskKind::QueryRerank.as_str()),
                provider_catalog_id,
                model_catalog_id,
                call_kind: "query_rerank",
            },
        )
        .await
        .context("failed to write rerank billing row")?;
        let reserved = billing_repository::get_provider_call_by_id(
            &fixture.state.persistence.postgres,
            rerank_provider_call_id,
        )
        .await?
        .context("rerank provider call missing")?;
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
        let call = || TestProviderCall {
            owning_execution_kind: "query_execution",
            owning_execution_id: fixture.query_execution_id,
            runtime_execution_id: Some(fixture.query_rerank_runtime_execution_id),
            runtime_task_kind: Some(RuntimeTaskKind::QueryRerank.as_str()),
            provider_catalog_id,
            model_catalog_id,
            call_kind: "query_rerank",
        };

        let precommit_failure_id = Uuid::now_v7();
        write_provider_call(&fixture, precommit_failure_id, call())
            .await
            .context("failed to write the pre-commit fault event")?;
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
        write_provider_call(&fixture, transient_failure_id, call())
            .await
            .context("failed to write the bounded-retry fault event")?;
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
async fn distinct_provider_calls_serialize_rollup_and_terminal_failures_refresh_count() -> Result<()>
{
    let fixture = BillingRollupsFixture::create_with_max_connections(4).await?;

    let result = async {
        let billing = &fixture.state.application_services.billing;
        let (provider_catalog_id, model_catalog_id) =
            openai_chat_catalog_ids(&fixture.state.persistence.postgres).await?;
        let call = || TestProviderCall {
            owning_execution_kind: "query_execution",
            owning_execution_id: fixture.query_execution_id,
            runtime_execution_id: Some(fixture.query_rerank_runtime_execution_id),
            runtime_task_kind: Some(RuntimeTaskKind::QueryRerank.as_str()),
            provider_catalog_id,
            model_catalog_id,
            call_kind: "query_rerank",
        };
        let first_call_id = write_provider_call(&fixture, Uuid::now_v7(), call())
            .await
            .context("failed to write first concurrent provider call")?;
        let second_call_id = write_provider_call(&fixture, Uuid::now_v7(), call())
            .await
            .context("failed to write second concurrent provider call")?;
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

        let failed_call_id = write_provider_call(&fixture, Uuid::now_v7(), call())
            .await
            .context("failed to write terminal failure provider call")?;
        billing
            .finish_reserved_provider_call_without_usage(&fixture.state, failed_call_id, "failed")
            .await
            .context("failed to terminalize provider call without usage")?;

        let refreshed = billing
            .get_execution_cost(&fixture.state, "query_execution", fixture.query_execution_id)
            .await
            .context("failed to load refreshed execution cost")?;
        assert_eq!(refreshed.total_cost, priced.total_cost);
        // The terminal attempt is durable but never settled: it leaves the
        // settled count at two and appears as one unknown liability. The
        // refreshed value proves the derived rollup was rebuilt from the ledger
        // instead of serving the stale row injected above.
        assert_eq!(
            refreshed.provider_call_count, 2,
            "a terminal provider call without usage is not a settled provider call",
        );
        assert_eq!(
            refreshed.unpriced_provider_call_count, 1,
            "terminal provider attempts must refresh the derived execution rollup and keep the unknown liability",
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
        let provider_call_id = write_provider_call(
            &fixture,
            Uuid::now_v7(),
            TestProviderCall {
                owning_execution_kind: "query_execution",
                owning_execution_id: fixture.query_execution_id,
                runtime_execution_id: Some(fixture.query_rerank_runtime_execution_id),
                runtime_task_kind: Some(RuntimeTaskKind::QueryRerank.as_str()),
                provider_catalog_id,
                model_catalog_id,
                call_kind: "query_rerank",
            },
        )
        .await
        .context("failed to write durable-rollup provider call")?;
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
        let provider_call_id = write_provider_call(
            &fixture,
            Uuid::now_v7(),
            TestProviderCall {
                owning_execution_kind: "query_execution",
                owning_execution_id: fixture.query_execution_id,
                runtime_execution_id: Some(fixture.query_runtime_execution_id),
                runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer.as_str()),
                provider_catalog_id,
                model_catalog_id,
                call_kind: "query_answer",
            },
        )
        .await
        .context("failed to write retained-history provider call")?;
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
        let call = || TestProviderCall {
            owning_execution_kind: "query_execution",
            owning_execution_id: fixture.query_execution_id,
            runtime_execution_id: Some(fixture.query_rerank_runtime_execution_id),
            runtime_task_kind: Some(RuntimeTaskKind::QueryRerank.as_str()),
            provider_catalog_id,
            model_catalog_id,
            call_kind: "query_rerank",
        };

        let terminal_call_id = write_provider_call(&fixture, Uuid::now_v7(), call())
            .await
            .context("failed to write terminal control provider call")?;
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
                write_provider_call(&fixture, Uuid::now_v7(), call())
                    .await
                    .context("failed to write stale provider call")?,
            );
        }
        let fresh_call_id = write_provider_call(&fixture, Uuid::now_v7(), call())
            .await
            .context("failed to write fresh provider call")?;

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
        // No attempt completed, so the settled count stays zero, while every one
        // of the five reservations stays durable as unknown liability. That
        // split is what proves the bounded unique-execution repair covered the
        // terminal control call, the three reaped reservations and the fresh
        // active one instead of reporting an empty rollup.
        assert_eq!(
            refreshed.provider_call_count, 0,
            "no reservation completed, so no call is settled",
        );
        assert_eq!(
            refreshed.unpriced_provider_call_count, 5,
            "the bounded unique-execution repair must keep every terminal and active attempt as unpriced liability",
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn execution_cost_requires_measured_provider_call_evidence() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let billing = &fixture.state.application_services.billing;
        // An execution with no provider call has no cost evidence: the read is an
        // explicit absence, never a fabricated 0 / "USD" / complete row.
        let missing = billing
            .get_execution_cost(&fixture.state, "ingest_attempt", fixture.ingest_attempt_id)
            .await;
        assert!(
            matches!(
                missing,
                Err(ironrag_backend::interfaces::http::router_support::ApiError::NotFound(_))
            ),
            "a missing provider-call observation must not be served as a measured zero"
        );

        let (provider_catalog_id, model_catalog_id) =
            openai_chat_catalog_ids(&fixture.state.persistence.postgres).await?;
        let failed_call_id = write_provider_call(
            &fixture,
            Uuid::now_v7(),
            TestProviderCall {
                owning_execution_kind: "ingest_attempt",
                owning_execution_id: fixture.ingest_attempt_id,
                runtime_execution_id: None,
                runtime_task_kind: None,
                provider_catalog_id,
                model_catalog_id,
                call_kind: "extract_graph",
            },
        )
        .await
        .context("failed to write zero-usage provider attempt")?;
        billing
            .finish_reserved_provider_call_without_usage(&fixture.state, failed_call_id, "failed")
            .await
            .context("failed to terminalize zero-usage provider attempt")?;
        let failed_cost = billing
            .get_execution_cost(&fixture.state, "ingest_attempt", fixture.ingest_attempt_id)
            .await
            .context("failed to load the terminal provider attempt")?;
        assert_eq!(
            failed_cost.provider_call_count, 0,
            "a failed reservation is not a settled call"
        );
        assert_eq!(failed_cost.unpriced_provider_call_count, 1);
        assert_eq!(
            failed_cost.cost_coverage,
            ironrag_backend::domains::billing::CostCoverageStatus::Unknown
        );
        assert_eq!(failed_cost.total_cost, Decimal::ZERO);

        // A completed call whose payload carries no priced usage was never priced
        // at all. Its absence of charges must keep the execution unknown instead
        // of turning it into a complete zero (this is the generic call kind, not
        // one of the two kinds with a declared mandatory-usage shape).
        let completed_call_id = write_provider_call(
            &fixture,
            Uuid::now_v7(),
            TestProviderCall {
                owning_execution_kind: "ingest_attempt",
                owning_execution_id: fixture.ingest_attempt_id,
                runtime_execution_id: None,
                runtime_task_kind: None,
                provider_catalog_id,
                model_catalog_id,
                call_kind: "extract_graph",
            },
        )
        .await
        .context("failed to write the usage-less provider attempt")?;
        billing
            .complete_reserved_provider_call_deferred_rollup(
                &fixture.state,
                completed_call_id,
                &serde_json::json!({}),
            )
            .await
            .context("failed to complete the usage-less provider attempt")?;
        let incomplete = billing
            .get_execution_cost(&fixture.state, "ingest_attempt", fixture.ingest_attempt_id)
            .await
            .context("failed to load the usage-less provider attempt")?;
        assert_eq!(incomplete.provider_call_count, 1);
        assert_eq!(
            incomplete.unpriced_provider_call_count, 2,
            "a completed call without any priced usage stays unpriced"
        );
        assert_eq!(
            incomplete.cost_coverage,
            ironrag_backend::domains::billing::CostCoverageStatus::Unknown
        );
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

/// Two ordinary sends of the same logical call each own one durable row.
///
/// Without a required scope the send admits no counter debit, so the only
/// durable effect of a send is its own `billing_provider_call` row. Two sends
/// therefore leave two distinct rows with the same attribution, which is the
/// unit the ledger and the counter share.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn two_ordinary_sends_write_one_row_each() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let (provider_catalog_id, model_catalog_id) = openai_chat_catalog_ids(postgres).await?;
        let attribution = send_attribution(&fixture, provider_catalog_id, model_catalog_id);
        let caller = DispatchCaller::MutationOrigin {
            mutation_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
        };
        let body = serde_json::json!({"model": "unused-without-a-required-scope"});

        let mut send_ids = Vec::with_capacity(2);
        for _ in 0..2 {
            let requested_id = Uuid::now_v7();
            let admitted = billing_repository::admit_provider_send(
                postgres,
                billing_repository::ProviderSendRequest {
                    required_scope: None,
                    caller: &caller,
                    model: "unused-without-a-required-scope",
                    endpoint: "https://provider.example/v1/chat/completions",
                    body: &body,
                    attribution: &attribution,
                    provider_call_id: requested_id,
                },
            )
            .await
            .map_err(|error| anyhow::anyhow!("ordinary send admission refused: {error:?}"))?;
            assert_eq!(admitted.provider_call_id, requested_id);
            assert_eq!(admitted.scope_id, None, "an ordinary send debits no incident scope",);
            send_ids.push(admitted.provider_call_id);
        }
        assert_ne!(send_ids[0], send_ids[1], "each send owns its own row identity");
        assert_eq!(
            count_provider_calls(postgres, "query_execution", fixture.query_execution_id).await?,
            2,
            "two admitted sends write exactly two rows",
        );

        for provider_call_id in send_ids {
            let row = billing_repository::get_provider_call_by_id(postgres, provider_call_id)
                .await?
                .context("admitted send row is missing")?;
            assert_eq!(row.call_state, "started");
            assert_eq!(row.owning_execution_kind, "query_execution");
            assert_eq!(row.owning_execution_id, fixture.query_execution_id);
            assert_eq!(row.call_kind, "query_answer");
            assert_eq!(row.workspace_id, fixture.workspace_id);
            assert_eq!(row.library_id, fixture.library_id);
        }
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// An attribution outside the owning execution's workspace or library is
/// refused before anything is written.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn attribution_outside_the_owning_execution_scope_writes_no_row() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let (provider_catalog_id, model_catalog_id) = openai_chat_catalog_ids(postgres).await?;
        let attribution = send_attribution(&fixture, provider_catalog_id, model_catalog_id);
        let caller = DispatchCaller::MutationOrigin {
            mutation_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
        };
        let body = serde_json::json!({"model": "unused-without-a-required-scope"});

        let mut wrong_workspace = attribution.clone();
        wrong_workspace.workspace_id = Uuid::now_v7();
        let mut wrong_library = attribution.clone();
        wrong_library.library_id = Uuid::now_v7();

        for (label, refused) in [("workspace", wrong_workspace), ("library", wrong_library)] {
            let requested_id = Uuid::now_v7();
            let outcome = billing_repository::admit_provider_send(
                postgres,
                billing_repository::ProviderSendRequest {
                    required_scope: None,
                    caller: &caller,
                    model: "unused-without-a-required-scope",
                    endpoint: "https://provider.example/v1/chat/completions",
                    body: &body,
                    attribution: &refused,
                    provider_call_id: requested_id,
                },
            )
            .await;
            assert!(
                matches!(outcome, Err(billing_repository::ProviderSendAdmissionError::Attribution)),
                "a {label} outside the owning execution must be refused as an attribution mismatch",
            );
            assert!(
                billing_repository::get_provider_call_by_id(postgres, requested_id)
                    .await?
                    .is_none(),
                "a refused {label} attribution writes no row",
            );
        }
        assert_eq!(
            count_provider_calls(postgres, "query_execution", fixture.query_execution_id).await?,
            0,
            "no refused attempt leaves a row behind",
        );

        // The untouched attribution proves the refusals above are about the
        // mismatch and not about a fixture that cannot admit at all.
        billing_repository::admit_provider_send(
            postgres,
            billing_repository::ProviderSendRequest {
                required_scope: None,
                caller: &caller,
                model: "unused-without-a-required-scope",
                endpoint: "https://provider.example/v1/chat/completions",
                body: &body,
                attribution: &attribution,
                provider_call_id: Uuid::now_v7(),
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("the matching attribution must be admitted: {error:?}"))?;
        assert_eq!(
            count_provider_calls(postgres, "query_execution", fixture.query_execution_id).await?,
            1,
            "the matching attribution writes its one row",
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// A send whose runtime execution belongs to another owner is refused, so the
/// row cannot inherit a foreign owner, task kind or attribution.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn runtime_execution_from_another_owner_writes_no_row() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let (provider_catalog_id, model_catalog_id) = openai_chat_catalog_ids(postgres).await?;
        let caller = DispatchCaller::MutationOrigin {
            mutation_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
        };
        let body = serde_json::json!({"model": "unused-without-a-required-scope"});
        let endpoint = "https://provider.example/v1/chat/completions";

        // The owning execution is the fixture's ingest attempt, while the named
        // runtime execution still belongs to the query execution.
        let mut foreign_runtime = send_attribution(&fixture, provider_catalog_id, model_catalog_id);
        foreign_runtime.owning_execution_kind = "ingest_attempt".to_string();
        foreign_runtime.owning_execution_id = fixture.ingest_attempt_id;
        foreign_runtime.call_kind = "extract_graph".to_string();

        let requested_id = Uuid::now_v7();
        let outcome = billing_repository::admit_provider_send(
            postgres,
            billing_repository::ProviderSendRequest {
                required_scope: None,
                caller: &caller,
                model: "unused-without-a-required-scope",
                endpoint,
                body: &body,
                attribution: &foreign_runtime,
                provider_call_id: requested_id,
            },
        )
        .await;
        assert!(
            matches!(outcome, Err(billing_repository::ProviderSendAdmissionError::Attribution)),
            "a runtime execution of another owner must be refused as an attribution mismatch",
        );
        assert!(
            billing_repository::get_provider_call_by_id(postgres, requested_id).await?.is_none(),
            "a refused runtime attribution writes no row",
        );
        assert_eq!(
            count_provider_calls(postgres, "ingest_attempt", fixture.ingest_attempt_id).await?,
            0,
        );

        // The same owner without a runtime half is admitted: the refusal above
        // was the foreign runtime execution, not the owner itself.
        let mut owned = foreign_runtime.clone();
        owned.runtime_execution_id = None;
        owned.runtime_task_kind = None;
        billing_repository::admit_provider_send(
            postgres,
            billing_repository::ProviderSendRequest {
                required_scope: None,
                caller: &caller,
                model: "unused-without-a-required-scope",
                endpoint,
                body: &body,
                attribution: &owned,
                provider_call_id: Uuid::now_v7(),
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("the owned attribution must be admitted: {error:?}"))?;
        assert_eq!(
            count_provider_calls(postgres, "ingest_attempt", fixture.ingest_attempt_id).await?,
            1,
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// One admitted send under a real financial scope writes one row and debits
/// exactly one model unit and one global unit in that same transaction.
///
/// The scope is the durable grant the product reads, written on the model's own
/// provider row, so the counter and the row are observed after a real admission
/// rather than through a test-only shortcut.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn admitted_send_debits_one_model_and_one_global_unit_with_its_row() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let catalog = create_synthetic_disjoint_pricing_catalog(postgres).await?;
        let model_name = catalog.model_name.clone();
        let endpoint = "https://provider.example/v1/chat/completions";
        let mutation_id = Uuid::now_v7();
        let principal_id = Uuid::now_v7();
        let caller = DispatchCaller::MutationOrigin { mutation_id, principal_id };

        let mut admission = FinancialAdmission {
            version: 1,
            provider_id: catalog.provider_catalog_id,
            scope_id: Uuid::now_v7(),
            policy_fingerprint: String::new(),
            cutoff: Utc::now() + chrono::Duration::hours(1),
            state: FinancialAdmissionState::Active,
            grant_microusd: 20,
            global_limit: 2,
            global_consumed: 0,
            models: BTreeMap::from([(
                model_name.clone(),
                FinancialModelAllowance {
                    endpoint: endpoint.to_string(),
                    envelope: FinancialRequestEnvelope::Chat {
                        allowed_service_tiers: vec!["default".to_string()],
                        max_input_bytes: 4096,
                        max_output_tokens: 1024,
                        max_input_tokens: 100_000,
                        media: None,
                    },
                    attempt_microusd: 10,
                    limit: 2,
                    consumed: 0,
                },
            )]),
            caller_binding: Some(CallerBinding {
                format_version: CALLER_BINDING_FORMAT_VERSION,
                generation: 1,
                callers: vec![AdmittedCaller::MutationOrigin { mutation_id, principal_id }],
            }),
            history: Vec::new(),
        };
        admission.policy_fingerprint = admission.immutable_fingerprint()?;
        let required_scope = RequiredFinancialScope {
            provider_id: admission.provider_id,
            scope_id: admission.scope_id,
            policy_fingerprint: admission.policy_fingerprint.clone(),
        };
        let published = sqlx::query(
            "update ai_provider_catalog
             set capability_flags_json =
                 jsonb_set(capability_flags_json, '{financialAdmission}', $2::jsonb, true)
             where id = $1",
        )
        .bind(catalog.provider_catalog_id)
        .bind(serde_json::to_value(&admission)?)
        .execute(postgres)
        .await
        .context("failed to publish the fixture financial admission")?;
        assert_eq!(published.rows_affected(), 1);

        let body = serde_json::json!({
            "model": model_name.clone(),
            "messages": [{"role": "user", "content": "billing admission"}],
            "service_tier": "default",
            "n": 1,
            "max_completion_tokens": 16,
        });
        let attribution =
            send_attribution(&fixture, catalog.provider_catalog_id, catalog.model_catalog_id);
        let provider_call_id = Uuid::now_v7();
        let admitted = billing_repository::admit_provider_send(
            postgres,
            billing_repository::ProviderSendRequest {
                required_scope: Some(&required_scope),
                caller: &caller,
                model: &model_name,
                endpoint,
                body: &body,
                attribution: &attribution,
                provider_call_id,
            },
        )
        .await
        .map_err(|error| anyhow::anyhow!("scoped send admission refused: {error:?}"))?;
        assert_eq!(admitted.provider_call_id, provider_call_id);
        assert_eq!(admitted.scope_id, Some(required_scope.scope_id));

        let row = billing_repository::get_provider_call_by_id(postgres, provider_call_id)
            .await?
            .context("the admitted send row is missing")?;
        assert_eq!(row.call_state, "started");
        assert_eq!(row.model_catalog_id, catalog.model_catalog_id);
        assert_eq!(
            count_provider_calls(postgres, "query_execution", fixture.query_execution_id).await?,
            1,
            "one admitted send writes exactly one row",
        );

        let stored: Option<serde_json::Value> = sqlx::query_scalar(
            "select capability_flags_json -> 'financialAdmission'
             from ai_provider_catalog
             where id = $1",
        )
        .bind(catalog.provider_catalog_id)
        .fetch_one(postgres)
        .await?;
        let stored: FinancialAdmission =
            decode_admission(stored.context("financial admission is missing")?)?;
        assert_eq!(stored.global_consumed, 1, "one send debits one global unit");
        assert_eq!(
            stored.models.get(&model_name).map(|allowance| allowance.consumed),
            Some(1),
            "one send debits one unit of its own model",
        );

        // A refused attribution changes neither the row count nor the counter.
        let mut refused = attribution.clone();
        refused.owning_execution_id = fixture.ingest_attempt_id;
        let refused_id = Uuid::now_v7();
        let outcome = billing_repository::admit_provider_send(
            postgres,
            billing_repository::ProviderSendRequest {
                required_scope: Some(&required_scope),
                caller: &caller,
                model: &model_name,
                endpoint,
                body: &body,
                attribution: &refused,
                provider_call_id: refused_id,
            },
        )
        .await;
        assert!(
            matches!(outcome, Err(billing_repository::ProviderSendAdmissionError::Attribution)),
            "an attribution of another owner is refused before the counter moves",
        );
        assert!(
            billing_repository::get_provider_call_by_id(postgres, refused_id).await?.is_none(),
            "a refused send writes no row",
        );
        assert_eq!(
            count_provider_calls(postgres, "query_execution", fixture.query_execution_id).await?,
            1,
        );
        let after_refusal: Option<serde_json::Value> = sqlx::query_scalar(
            "select capability_flags_json -> 'financialAdmission'
             from ai_provider_catalog
             where id = $1",
        )
        .bind(catalog.provider_catalog_id)
        .fetch_one(postgres)
        .await?;
        let after_refusal =
            decode_admission(after_refusal.context("the financial admission disappeared")?)?;
        assert_eq!(after_refusal.global_consumed, 1, "a refused send debits nothing");
        assert_eq!(
            after_refusal.models.get(&model_name).map(|allowance| allowance.consumed),
            Some(1),
        );
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

/// A logical call that is retried once at the transport boundary owns two
/// durable rows and two admission debits, with the superseded attempt already
/// terminal before the retry is admitted.
#[tokio::test]
#[ignore = "requires local postgres with required extensions; provider HTTP is local and synthetic"]
async fn a_retried_physical_send_owns_two_rows_and_two_debits() -> Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use ironrag_backend::{
        integrations::{
            provider_admission::FinancialDispatchAdmission,
            retry::{ProviderCallError, RetryPolicy, with_retry},
        },
        shared::{
            provider_http::{
                ProviderDispatchError, ProviderSurface, send_provider_http_request,
                test_runtime_binding_authority, with_test_dispatch_caller,
            },
            provider_send::{ProviderSendScope, with_provider_send},
        },
    };

    let fixture = BillingRollupsFixture::create().await?;

    let route_probe = std::net::UdpSocket::bind("0.0.0.0:0")?;
    route_probe.connect("192.0.2.1:9")?;
    let host = route_probe.local_addr()?.ip();
    let listener = tokio::net::TcpListener::bind((host, 0)).await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let dispatches = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&dispatches);
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            let counter = Arc::clone(&counter);
            async move {
                let attempt = counter.fetch_add(1, Ordering::SeqCst);
                let (status, body) = if attempt == 0 {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        serde_json::json!({"error": {"message": "synthetic retry"}}),
                    )
                } else {
                    (
                        axum::http::StatusCode::OK,
                        serde_json::json!({
                            "id": "synthetic",
                            "object": "chat.completion",
                            "choices": [{
                                "index": 0,
                                "message": {"role": "assistant", "content": "ok"},
                                "finish_reason": "stop"
                            }],
                            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
                        }),
                    )
                };
                (status, axum::Json(body))
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let catalog = create_synthetic_disjoint_pricing_catalog(postgres).await?;
        let endpoint = format!("{base_url}/v1/chat/completions");
        let mutation_id = Uuid::now_v7();
        let principal_id = Uuid::now_v7();
        let caller = DispatchCaller::MutationOrigin { mutation_id, principal_id };

        let mut admission = FinancialAdmission {
            version: 1,
            provider_id: catalog.provider_catalog_id,
            scope_id: Uuid::now_v7(),
            policy_fingerprint: String::new(),
            cutoff: Utc::now() + chrono::Duration::hours(1),
            state: FinancialAdmissionState::Active,
            grant_microusd: 20,
            global_limit: 2,
            global_consumed: 0,
            models: BTreeMap::from([(
                catalog.model_name.clone(),
                FinancialModelAllowance {
                    endpoint: endpoint.clone(),
                    envelope: FinancialRequestEnvelope::Chat {
                        allowed_service_tiers: vec!["default".to_string()],
                        max_input_bytes: 4096,
                        max_output_tokens: 1024,
                        max_input_tokens: 100_000,
                        media: None,
                    },
                    attempt_microusd: 10,
                    limit: 2,
                    consumed: 0,
                },
            )]),
            caller_binding: Some(CallerBinding {
                format_version: CALLER_BINDING_FORMAT_VERSION,
                generation: 1,
                callers: vec![AdmittedCaller::MutationOrigin { mutation_id, principal_id }],
            }),
            history: Vec::new(),
        };
        admission.policy_fingerprint = admission.immutable_fingerprint()?;
        let required_scope = RequiredFinancialScope {
            provider_id: admission.provider_id,
            scope_id: admission.scope_id,
            policy_fingerprint: admission.policy_fingerprint.clone(),
        };
        sqlx::query(
            "update ai_provider_catalog
             set capability_flags_json =
                 jsonb_set(capability_flags_json, '{financialAdmission}', $2::jsonb, true)
             where id = $1",
        )
        .bind(catalog.provider_catalog_id)
        .bind(serde_json::to_value(&admission)?)
        .execute(postgres)
        .await?;

        let attribution =
            send_attribution(&fixture, catalog.provider_catalog_id, catalog.model_catalog_id);
        let scope = ProviderSendScope::new(attribution, Uuid::now_v7());
        let authority = test_runtime_binding_authority(
            catalog.provider_kind.clone(),
            catalog.provider_catalog_id,
            catalog.model_name.clone(),
            base_url.clone(),
        );
        // The attested context copies the ambient caller, so it must be built
        // with the same caller the stored admission binds.
        let context = with_test_dispatch_caller(caller, async {
            authority.attest(ProviderSurface::Chat, None)
        })
        .await;
        let body = serde_json::json!({
            "model": catalog.model_name.clone(),
            "messages": [{"role": "user", "content": "retry boundary"}],
            "service_tier": "default",
            "n": 1,
            "max_completion_tokens": 16,
        });
        let url = reqwest::Url::parse(&endpoint)?;
        let admission = FinancialDispatchAdmission::new(
            postgres.clone(),
            Some(required_scope.clone()),
            true,
        );

        let retried = with_test_dispatch_caller(
            caller,
            with_provider_send(scope.clone(), async {
                with_retry(
                    || async {
                        let request = reqwest::Client::new().post(url.clone()).json(&body);
                        let send = send_provider_http_request(
                            request,
                            &context,
                            &admission,
                            "chat",
                        )
                        .await
                        .map_err(|error| match error {
                            ProviderDispatchError::Refused(refusal) => {
                                ProviderCallError::admission(
                                    ironrag_backend::integrations::provider_budget::ProviderBudgetError::Dispatch(refusal),
                                )
                            }
                            ProviderDispatchError::Build => {
                                ProviderCallError::protocol("synthetic request could not be built")
                            }
                            ProviderDispatchError::Transport(source) => {
                                ProviderCallError::transport("synthetic transport", source)
                            }
                        })?;
                        let (response, mut permit) = send.into_parts();
                        if !response.status().is_success() {
                            let status = response.status();
                            // The superseded send must be terminal before the
                            // retry is admitted.
                            permit.fail_send();
                            return Err(ProviderCallError::http_status(
                                "synthetic",
                                status,
                                &reqwest::header::HeaderMap::new(),
                                "synthetic retryable status",
                            ));
                        }
                        permit.complete_send();
                        Ok(())
                    },
                    RetryPolicy::idempotent_provider_call(
                        2,
                        std::time::Duration::ZERO,
                        std::time::Duration::ZERO,
                    ),
                )
                .await
            }),
        )
        .await;
        retried?;

        assert_eq!(
            dispatches.load(Ordering::SeqCst),
            2,
            "the retry must reach the transport twice"
        );
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "select id, call_state::text from billing_provider_call
             where owning_execution_kind='query_execution' and owning_execution_id=$1
             order by started_at",
        )
        .bind(fixture.query_execution_id)
        .fetch_all(postgres)
        .await?;
        assert_eq!(rows.len(), 2, "each physical send owns exactly one row");
        assert_eq!(rows[0].1, "failed", "the 503 attempt is terminal before the retry");

        let second = scope.current_send_id().context("the accepted send row must be published")?;
        assert_eq!(rows[1].0, second, "the accepted row is the one being settled");
        fixture
            .state
            .application_services
            .billing
            .complete_reserved_provider_call_deferred_rollup_with_retry(
                &fixture.state,
                second,
                &serde_json::json!({
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15,
                }),
            )
            .await?;

        let states: Vec<String> = sqlx::query_scalar(
            "select call_state::text from billing_provider_call
             where owning_execution_kind='query_execution' and owning_execution_id=$1
             order by call_state::text",
        )
        .bind(fixture.query_execution_id)
        .fetch_all(postgres)
        .await?;
        assert_eq!(states, vec!["completed".to_string(), "failed".to_string()]);

        let stored = decode_admission(
            sqlx::query_scalar(
                "select capability_flags_json -> 'financialAdmission'
                 from ai_provider_catalog where id = $1",
            )
            .bind(catalog.provider_catalog_id)
            .fetch_one(postgres)
            .await?,
        )?;
        assert_eq!(stored.global_consumed, 2, "two admitted sends debit twice");
        assert_eq!(stored.models[&catalog.model_name].consumed, 2);
        Ok(())
    }
    .await;

    server.abort();
    fixture.cleanup().await?;
    result
}

/// A reply that is accepted but never settled stays attributable to the caller's
/// guard: the transport neither cancels the row nor claims usage, and the guard's
/// own terminalization cancels exactly that row without usage or charge.
#[tokio::test]
#[ignore = "requires local postgres with required extensions; provider HTTP is local and synthetic"]
async fn a_received_but_unsettled_send_terminalizes_without_usage() -> Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use ironrag_backend::{
        integrations::provider_admission::FinancialDispatchAdmission,
        shared::{
            provider_http::{
                ProviderSurface, send_provider_http_request, test_runtime_binding_authority,
                with_test_dispatch_caller,
            },
            provider_send::{ProviderSendScope, with_provider_send},
        },
    };

    let fixture = BillingRollupsFixture::create().await?;

    let route_probe = std::net::UdpSocket::bind("0.0.0.0:0")?;
    route_probe.connect("192.0.2.1:9")?;
    let host = route_probe.local_addr()?.ip();
    let listener = tokio::net::TcpListener::bind((host, 0)).await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let dispatches = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&dispatches);
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({
                        "id": "synthetic",
                        "object": "chat.completion",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "ok"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
                    })),
                )
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let catalog = create_synthetic_disjoint_pricing_catalog(postgres).await?;
        let endpoint = format!("{base_url}/v1/chat/completions");
        let mutation_id = Uuid::now_v7();
        let principal_id = Uuid::now_v7();
        let caller = DispatchCaller::MutationOrigin { mutation_id, principal_id };
        let (required_scope, provider_catalog_id) = publish_synthetic_chat_admission(
            postgres,
            &catalog,
            &endpoint,
            mutation_id,
            principal_id,
        )
        .await?;

        let attribution = send_attribution(&fixture, provider_catalog_id, catalog.model_catalog_id);
        let scope = ProviderSendScope::new(attribution, Uuid::now_v7());
        let authority = test_runtime_binding_authority(
            catalog.provider_kind.clone(),
            provider_catalog_id,
            catalog.model_name.clone(),
            base_url.clone(),
        );
        // The attested context copies the ambient caller, so it must be built
        // with the same caller the stored admission binds.
        let context = with_test_dispatch_caller(caller, async {
            authority.attest(ProviderSurface::Chat, None)
        })
        .await;
        let body = serde_json::json!({
            "model": catalog.model_name.clone(),
            "messages": [{"role": "user", "content": "handoff"}],
            "service_tier": "default",
            "n": 1,
            "max_completion_tokens": 16,
        });
        let url = reqwest::Url::parse(&endpoint)?;
        let admission = FinancialDispatchAdmission::new(
            postgres.clone(),
            Some(required_scope.clone()),
            true,
        );
        let accepted = with_test_dispatch_caller(
            caller,
            with_provider_send(scope.clone(), async {
                let request = reqwest::Client::new().post(url).json(&body);
                let send =
                    send_provider_http_request(request, &context, &admission, "chat").await?;
                Ok::<_, ironrag_backend::shared::provider_http::ProviderDispatchError>(
                    send.into_parts(),
                )
            }),
        )
        .await
        .expect("the admitted send reaches the provider");
        let (response, mut permit) = accepted;
        // The reply is received and accepted. The transport hands the row to the
        // caller's settlement guard, so the permit must not cancel it and must
        // not claim usage.
        permit.complete_send();
        drop(permit);
        drop(response);

        let row_id = scope
            .current_send_id()
            .context("the accepted send row must be published")?;
        let state = sqlx::query_scalar::<_, String>(
            "select call_state::text from billing_provider_call where id=$1",
        )
        .bind(row_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(state, "started", "an accepted reply waits for the caller's settlement");
        // The caller guard owns the row from here: when it is dropped without a
        // settlement, it cancels exactly this row and records no usage.
        fixture
            .state
            .application_services
            .billing
            .finish_reserved_provider_call_without_usage(&fixture.state, row_id, "canceled")
            .await?;
        let state = sqlx::query_scalar::<_, String>(
            "select call_state::text from billing_provider_call where id=$1",
        )
        .bind(row_id)
        .fetch_one(postgres)
        .await?;
        assert_eq!(state, "canceled", "the caller guard terminalizes the row without usage");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("select count(*) from billing_usage where provider_call_id=$1")
                .bind(row_id)
                .fetch_one(postgres)
                .await?,
            0,
            "no usage may be invented",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("select count(*) from billing_charge c join billing_usage u on u.id=c.usage_id where u.provider_call_id=$1")
                .bind(row_id)
                .fetch_one(postgres)
                .await?,
            0,
            "no charge may be invented",
        );
        assert_eq!(dispatches.load(Ordering::SeqCst), 1, "a canceled handoff never retries");
        Ok(())
    }
    .await;

    server.abort();
    fixture.cleanup().await?;
    result
}

/// A failed admission commit never dispatches and leaves neither a row nor a
/// debit. The deferred constraint trigger fails the commit after the writes, so
/// the whole unit rolls back.
#[tokio::test]
#[ignore = "requires local postgres with required extensions; provider HTTP is local and synthetic"]
async fn a_failed_admission_commit_dispatches_nothing_and_writes_nothing() -> Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use ironrag_backend::{
        integrations::provider_admission::FinancialDispatchAdmission,
        shared::{
            provider_http::{
                ProviderSurface, send_provider_http_request, test_runtime_binding_authority,
                with_test_dispatch_caller,
            },
            provider_send::{ProviderSendScope, with_provider_send},
        },
    };

    let fixture = BillingRollupsFixture::create().await?;

    let route_probe = std::net::UdpSocket::bind("0.0.0.0:0")?;
    route_probe.connect("192.0.2.1:9")?;
    let host = route_probe.local_addr()?.ip();
    let listener = tokio::net::TcpListener::bind((host, 0)).await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let dispatches = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&dispatches);
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({"id": "synthetic", "choices": []})),
                )
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let catalog = create_synthetic_disjoint_pricing_catalog(postgres).await?;
        let endpoint = format!("{base_url}/v1/chat/completions");
        let mutation_id = Uuid::now_v7();
        let principal_id = Uuid::now_v7();
        let caller = DispatchCaller::MutationOrigin { mutation_id, principal_id };
        let (required_scope, provider_catalog_id) = publish_synthetic_chat_admission(
            postgres,
            &catalog,
            &endpoint,
            mutation_id,
            principal_id,
        )
        .await?;

        sqlx::query(
            "create function test_fail_admission_commit() returns trigger
             language plpgsql
             as $$
             begin
                 raise exception 'synthetic admission commit failure';
             end;
             $$",
        )
        .execute(postgres)
        .await?;
        sqlx::query(
            "create constraint trigger test_fail_admission_commit
             after update on ai_provider_catalog
             deferrable initially deferred
             for each row execute function test_fail_admission_commit()",
        )
        .execute(postgres)
        .await?;

        let attribution = send_attribution(&fixture, provider_catalog_id, catalog.model_catalog_id);
        let scope = ProviderSendScope::new(attribution, Uuid::now_v7());
        let authority = test_runtime_binding_authority(
            catalog.provider_kind.clone(),
            provider_catalog_id,
            catalog.model_name.clone(),
            base_url.clone(),
        );
        // The attested context copies the ambient caller, so it must be built
        // with the same caller the stored admission binds.
        let context = with_test_dispatch_caller(caller, async {
            authority.attest(ProviderSurface::Chat, None)
        })
        .await;
        let body = serde_json::json!({
            "model": catalog.model_name.clone(),
            "messages": [{"role": "user", "content": "commit failure"}],
            "service_tier": "default",
            "n": 1,
            "max_completion_tokens": 16,
        });
        let url = reqwest::Url::parse(&endpoint)?;
        let admission =
            FinancialDispatchAdmission::new(postgres.clone(), Some(required_scope.clone()), true);
        let refused = with_test_dispatch_caller(
            caller,
            with_provider_send(scope.clone(), async {
                let request = reqwest::Client::new().post(url).json(&body);
                send_provider_http_request(request, &context, &admission, "chat").await
            }),
        )
        .await;
        match refused {
            Err(ironrag_backend::shared::provider_http::ProviderDispatchError::Refused(
                ironrag_backend::shared::provider_http::ProviderDispatchRefusal::Admission,
            )) => {}
            Err(other) => panic!(
                "a failed admission commit must surface the native admission refusal, got {other:?}"
            ),
            Ok(_) => panic!("a failed admission commit must not produce a dispatchable response"),
        }
        assert_eq!(dispatches.load(Ordering::SeqCst), 0, "no provider request leaves the process");
        assert_eq!(scope.current_send_id(), None, "no row was published");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*) from billing_provider_call where owning_execution_id=$1",
            )
            .bind(fixture.query_execution_id)
            .fetch_one(postgres)
            .await?,
            0,
            "the rolled-back admission writes no row",
        );
        let stored = decode_admission(
            sqlx::query_scalar(
                "select capability_flags_json -> 'financialAdmission'
                 from ai_provider_catalog where id = $1",
            )
            .bind(provider_catalog_id)
            .fetch_one(postgres)
            .await?,
        )?;
        assert_eq!(stored.global_consumed, 0, "the rolled-back admission debits nothing");
        assert_eq!(stored.models[&catalog.model_name].consumed, 0);
        Ok(())
    }
    .await;

    server.abort();
    fixture.cleanup().await?;
    result
}

/// The server committed the row and the debit, but the admission reply was
/// lost. Admission must report failure, no provider request may leave, and the
/// committed row must be reconciled to a terminal unknown-usage state with its
/// original attribution and debit intact.
#[tokio::test]
#[ignore = "requires local postgres with required extensions; provider HTTP is local and synthetic"]
async fn a_lost_commit_ack_keeps_the_row_and_debit_without_dispatch() -> Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use ironrag_backend::{
        integrations::provider_admission::FinancialDispatchAdmission,
        shared::{
            provider_http::{
                ProviderSurface, send_provider_http_request, test_runtime_binding_authority,
                with_test_dispatch_caller,
            },
            provider_send::{ProviderSendScope, with_provider_send},
        },
    };

    let fixture = BillingRollupsFixture::create().await?;

    let route_probe = std::net::UdpSocket::bind("0.0.0.0:0")?;
    route_probe.connect("192.0.2.1:9")?;
    let host = route_probe.local_addr()?.ip();
    let listener = tokio::net::TcpListener::bind((host, 0)).await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let dispatches = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&dispatches);
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::OK,
                    axum::Json(serde_json::json!({"id": "synthetic", "choices": []})),
                )
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let catalog = create_synthetic_disjoint_pricing_catalog(postgres).await?;
        let endpoint = format!("{base_url}/v1/chat/completions");
        let mutation_id = Uuid::now_v7();
        let principal_id = Uuid::now_v7();
        let caller = DispatchCaller::MutationOrigin { mutation_id, principal_id };
        let (required_scope, provider_catalog_id) = publish_synthetic_chat_admission(
            postgres,
            &catalog,
            &endpoint,
            mutation_id,
            principal_id,
        )
        .await?;

        let attribution = send_attribution(&fixture, provider_catalog_id, catalog.model_catalog_id);
        let scope = ProviderSendScope::new(attribution.clone(), Uuid::now_v7());
        // The first send of a scope reuses the logical id as its row id.
        ironrag_backend::infra::repositories::billing_repository::arm_lost_commit_ack(
            scope.logical_id(),
        );
        let authority = test_runtime_binding_authority(
            catalog.provider_kind.clone(),
            provider_catalog_id,
            catalog.model_name.clone(),
            base_url.clone(),
        );
        // The attested context copies the ambient caller, so it must be built
        // with the same caller the stored admission binds.
        let context = with_test_dispatch_caller(caller, async {
            authority.attest(ProviderSurface::Chat, None)
        })
        .await;
        let body = serde_json::json!({
            "model": catalog.model_name.clone(),
            "messages": [{"role": "user", "content": "lost ack"}],
            "service_tier": "default",
            "n": 1,
            "max_completion_tokens": 16,
        });
        let url = reqwest::Url::parse(&endpoint)?;
        let admission =
            FinancialDispatchAdmission::new(postgres.clone(), Some(required_scope.clone()), true);
        let refused = with_test_dispatch_caller(
            caller,
            with_provider_send(scope.clone(), async {
                let request = reqwest::Client::new().post(url).json(&body);
                send_provider_http_request(request, &context, &admission, "chat").await
            }),
        )
        .await;
        assert!(refused.is_err(), "a lost acknowledgement surfaces as an admission failure");
        assert_eq!(dispatches.load(Ordering::SeqCst), 0, "no provider request leaves the process");
        assert_eq!(scope.current_send_id(), None, "no current row is published to the caller");

        let row = billing_repository::get_provider_call_by_id(postgres, scope.logical_id())
            .await?
            .context("the committed row must remain durable")?;
        assert_eq!(row.call_state, "canceled", "the committed row is terminal without usage");
        assert_eq!(row.owning_execution_kind, attribution.owning_execution_kind);
        assert_eq!(row.owning_execution_id, attribution.owning_execution_id);
        assert_eq!(row.call_kind, attribution.call_kind);
        assert_eq!(row.binding_id, attribution.binding_id);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*) from billing_usage where provider_call_id=$1"
            )
            .bind(row.id)
            .fetch_one(postgres)
            .await?,
            0,
            "a lost acknowledgement records no usage",
        );

        let stored = decode_admission(
            sqlx::query_scalar(
                "select capability_flags_json -> 'financialAdmission'
                 from ai_provider_catalog where id = $1",
            )
            .bind(provider_catalog_id)
            .fetch_one(postgres)
            .await?,
        )?;
        assert_eq!(stored.global_consumed, 1, "the committed debit is preserved exactly once");
        assert_eq!(stored.models[&catalog.model_name].consumed, 1);

        // An unrelated logical call keeps its own empty scope.
        let other = ProviderSendScope::new(attribution, Uuid::now_v7());
        assert_eq!(other.current_send_id(), None);
        assert!(
            sqlx::query_scalar::<_, i64>(
                "select count(*) from billing_provider_call where call_state='started'",
            )
            .fetch_one(postgres)
            .await?
                == 0,
            "no admitted row is left unsettled",
        );
        Ok(())
    }
    .await;

    server.abort();
    fixture.cleanup().await?;
    result
}

/// Usage observed before a cancellation must survive the handoff and settle
/// exactly once: the late cleanup retry must not duplicate usage or charges.
#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn observed_usage_settles_exactly_once_after_a_cancelled_handoff() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;
    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let catalog = create_synthetic_disjoint_pricing_catalog(postgres).await?;
        let provider_call_id = Uuid::now_v7();
        write_provider_call(
            &fixture,
            provider_call_id,
            TestProviderCall {
                owning_execution_kind: "query_execution",
                owning_execution_id: fixture.query_execution_id,
                runtime_execution_id: Some(fixture.query_runtime_execution_id),
                runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer.as_str()),
                provider_catalog_id: catalog.provider_catalog_id,
                model_catalog_id: catalog.model_catalog_id,
                call_kind: "query_answer",
            },
        )
        .await?;
        let usage_json = serde_json::json!({
            "prompt_tokens": 17,
            "completion_tokens": 5,
            "total_tokens": 22,
        });
        let billing = &fixture.state.application_services.billing;
        billing
            .complete_reserved_provider_call_deferred_rollup_with_retry(
                &fixture.state,
                provider_call_id,
                &usage_json,
            )
            .await?;
        // The late cleanup of a cancelled handoff repeats the same stable
        // completion with the exact observed usage.
        billing
            .complete_reserved_provider_call_deferred_rollup_with_retry(
                &fixture.state,
                provider_call_id,
                &usage_json,
            )
            .await?;
        let row = billing_repository::get_provider_call_by_id(postgres, provider_call_id)
            .await?
            .context("the observed-usage row disappeared")?;
        assert_eq!(row.call_state, "completed");
        let usage: Vec<(String, i64)> = sqlx::query_as(
            "select usage_kind, quantity::bigint from billing_usage
             where provider_call_id=$1 order by usage_kind",
        )
        .bind(provider_call_id)
        .fetch_all(postgres)
        .await?;
        assert_eq!(
            usage,
            vec![("completion_tokens".to_string(), 5), ("prompt_tokens".to_string(), 17)],
            "the late retry must keep exactly the observed per-unit usage",
        );
        let charges: Vec<(String, i64)> = sqlx::query_as(
            "select u.usage_kind, count(*)::bigint from billing_charge c
             join billing_usage u on u.id=c.usage_id
             where u.provider_call_id=$1 group by u.usage_kind order by u.usage_kind",
        )
        .bind(provider_call_id)
        .fetch_all(postgres)
        .await?;
        assert_eq!(
            charges,
            vec![("completion_tokens".to_string(), 1), ("prompt_tokens".to_string(), 1)],
            "each priced unit is charged exactly once",
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

/// Aborting a body read that is suspended after the response headers must
/// terminalize the admitted row through its transport guard, without a retry.
#[tokio::test]
#[ignore = "requires local postgres with required extensions; provider HTTP is local and synthetic"]
async fn aborting_a_suspended_body_read_terminalizes_the_admitted_row() -> Result<()> {
    use ironrag_backend::{
        integrations::provider_admission::FinancialDispatchAdmission,
        shared::{
            provider_http::{
                ProviderSurface, send_provider_http_request, test_runtime_binding_authority,
                with_test_dispatch_caller,
            },
            provider_send::{ProviderSendScope, with_provider_send},
        },
    };

    let fixture = BillingRollupsFixture::create().await?;

    let route_probe = std::net::UdpSocket::bind("0.0.0.0:0")?;
    route_probe.connect("192.0.2.1:9")?;
    let host = route_probe.local_addr()?.ip();
    let listener = tokio::net::TcpListener::bind((host, 0)).await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|| async {
            // Headers leave immediately; the body never produces a frame, so a
            // reader suspends until its owner aborts it.
            let body = axum::body::Body::from_stream(futures::stream::pending::<
                Result<axum::body::Bytes, std::io::Error>,
            >());
            axum::response::Response::builder()
                .status(axum::http::StatusCode::OK)
                .header("content-type", "application/json")
                .body(body)
                .expect("synthetic suspended response")
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let result = async {
        let postgres = &fixture.state.persistence.postgres;
        let catalog = create_synthetic_disjoint_pricing_catalog(postgres).await?;
        let endpoint = format!("{base_url}/v1/chat/completions");
        let mutation_id = Uuid::now_v7();
        let principal_id = Uuid::now_v7();
        let caller = DispatchCaller::MutationOrigin { mutation_id, principal_id };
        let (required_scope, provider_catalog_id) = publish_synthetic_chat_admission(
            postgres,
            &catalog,
            &endpoint,
            mutation_id,
            principal_id,
        )
        .await?;

        let attribution = send_attribution(&fixture, provider_catalog_id, catalog.model_catalog_id);
        let scope = ProviderSendScope::new(attribution, Uuid::now_v7());
        let authority = test_runtime_binding_authority(
            catalog.provider_kind.clone(),
            provider_catalog_id,
            catalog.model_name.clone(),
            base_url.clone(),
        );
        // The attested context copies the ambient caller, so it must be built
        // with the same caller the stored admission binds.
        let context = with_test_dispatch_caller(caller, async {
            authority.attest(ProviderSurface::Chat, None)
        })
        .await;
        let body = serde_json::json!({
            "model": catalog.model_name.clone(),
            "messages": [{"role": "user", "content": "suspended body"}],
            "service_tier": "default",
            "n": 1,
            "max_completion_tokens": 16,
        });
        let url = reqwest::Url::parse(&endpoint)?;
        let admission = FinancialDispatchAdmission::new(
            postgres.clone(),
            Some(required_scope.clone()),
            true,
        );
        let (response, permit) = with_test_dispatch_caller(
            caller,
            with_provider_send(scope.clone(), async {
                let request = reqwest::Client::new().post(url).json(&body);
                let send =
                    send_provider_http_request(request, &context, &admission, "chat").await?;
                Ok::<_, ironrag_backend::shared::provider_http::ProviderDispatchError>(
                    send.into_parts(),
                )
            }),
        )
        .await
        .expect("the admitted send reaches the provider");

        let read = tokio::time::timeout(std::time::Duration::from_millis(150), response.bytes())
            .await;
        assert!(read.is_err(), "the body read must be suspended until the abort");
        drop(permit);

        let row_id = scope
            .current_send_id()
            .context("the admitted send row must be published")?;
        let mut state = String::new();
        for _ in 0..40 {
            state = sqlx::query_scalar("select call_state::text from billing_provider_call where id=$1")
                .bind(row_id)
                .fetch_one(postgres)
                .await?;
            if state != "started" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(state, "canceled", "the aborted body read terminalizes its own row");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("select count(*) from billing_usage where provider_call_id=$1")
                .bind(row_id)
                .fetch_one(postgres)
                .await?,
            0,
            "an aborted body read settles no usage",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select count(*) from billing_provider_call where owning_execution_id=$1 and call_state='started'",
            )
            .bind(fixture.query_execution_id)
            .fetch_one(postgres)
            .await?,
            0,
            "no second attempt or unsettled row is left behind",
        );
        Ok(())
    }
    .await;

    server.abort();
    fixture.cleanup().await?;
    result
}
