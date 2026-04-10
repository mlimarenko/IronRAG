use std::sync::Arc;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    domains::agent_runtime::{RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimeTaskKind},
    infra::{
        arangodb::client::ArangoClient,
        persistence::Persistence,
        repositories::{query_repository, runtime_repository},
    },
    services::{
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
        ingest::service::{AdmitIngestJobCommand, LeaseAttemptCommand},
        ops::billing::{
            CaptureExecutionBillingCommand, CaptureIngestAttemptBillingCommand,
            CaptureQueryExecutionBillingCommand,
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
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
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
    query_runtime_execution_id: Uuid,
    ingest_attempt_id: Uuid,
}

impl BillingRollupsFixture {
    async fn create() -> Result<Self> {
        let settings =
            Settings::from_env().context("failed to load settings for billing_rollups test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&temp_database.database_url)
            .await
            .context("failed to connect billing_rollups postgres")?;

        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("failed to apply canonical baseline migrations for billing_rollups")?;

        let state = build_test_state(settings, postgres)?;
        let workspace = state
            .canonical_services
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
            .canonical_services
            .catalog
            .create_library(
                &state,
                CreateLibraryCommand {
                    workspace_id: workspace.id,
                    slug: Some(format!("billing-library-{}", Uuid::now_v7().simple())),
                    display_name: "Billing Rollups Library".to_string(),
                    description: Some("canonical billing rollup test fixture".to_string()),
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
        let runtime_execution_id = Uuid::now_v7();
        runtime_repository::create_runtime_execution(
            &state.persistence.postgres,
            &runtime_repository::NewRuntimeExecution {
                id: runtime_execution_id,
                owner_kind: RuntimeExecutionOwnerKind::QueryExecution.as_str(),
                owner_id: execution_id,
                task_kind: RuntimeTaskKind::QueryAnswer.as_str(),
                surface_kind: "rest",
                contract_name: "query_answer",
                contract_version: "1",
                lifecycle_state: RuntimeLifecycleState::Running.as_str(),
                active_stage: None,
                turn_budget: 4,
                turn_count: 1,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
            },
        )
        .await
        .context("failed to create billing runtime execution")?;
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
                runtime_execution_id,
                query_text: "How much did this execution cost?",
                failure_code: None,
            },
        )
        .await
        .context("failed to create query execution")?;

        let ingest_job = state
            .canonical_services
            .ingest
            .admit_job(
                &state,
                AdmitIngestJobCommand {
                    workspace_id: workspace.id,
                    library_id: library.id,
                    mutation_id: None,
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
            .canonical_services
            .ingest
            .lease_attempt(
                &state,
                LeaseAttemptCommand {
                    job_id: ingest_job.id,
                    worker_principal_id: None,
                    lease_token: Some("billing-rollup-lease".to_string()),
                    knowledge_generation_id: None,
                    current_stage: Some("embedding_chunks".to_string()),
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
            query_runtime_execution_id: runtime_execution_id,
            ingest_attempt_id: ingest_attempt.id,
        })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await
    }
}

fn build_test_state(settings: Settings, postgres: PgPool) -> Result<AppState> {
    let persistence = Persistence {
        postgres,
        redis: redis::Client::open(settings.redis_url.clone())
            .context("failed to create redis client for billing_rollups test state")?,
    };
    let arango_client = Arc::new(ArangoClient::from_settings(&settings)?);
    AppState::from_dependencies(settings, persistence, arango_client)
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
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_billing_rollups_cover_query_and_ingest_executions() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let billing = &fixture.state.canonical_services.billing;

        let unpriced = billing
            .capture_execution_provider_call(
                &fixture.state,
                CaptureExecutionBillingCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    owning_execution_kind: "query_execution".to_string(),
                    owning_execution_id: fixture.query_execution_id,
                    runtime_execution_id: Some(fixture.query_runtime_execution_id),
                    runtime_task_kind: Some(RuntimeTaskKind::QueryPlan),
                    binding_id: None,
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5.4-mini".to_string(),
                    call_kind: "query_planning".to_string(),
                    usage_json: serde_json::json!({}),
                },
            )
            .await
            .context("failed to capture unpriced planning provider call")?;
        assert!(unpriced.is_none());

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
                    model_name: "gpt-5.4".to_string(),
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
        assert_eq!(query_cost.total_cost, Decimal::new(25, 3));
        assert_eq!(query_cost.provider_call_count, 2);

        let query_rollup = billing
            .capture_execution_provider_call(
                &fixture.state,
                CaptureExecutionBillingCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    owning_execution_kind: "query_execution".to_string(),
                    owning_execution_id: fixture.query_execution_id,
                    runtime_execution_id: Some(fixture.query_runtime_execution_id),
                    runtime_task_kind: Some(RuntimeTaskKind::QueryRerank),
                    binding_id: None,
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5.4-mini".to_string(),
                    call_kind: "query_rerank".to_string(),
                    usage_json: serde_json::json!({
                        "input_tokens": 2000,
                        "total_tokens": 2000,
                    }),
                },
            )
            .await
            .context("failed to capture rerank billing")?
            .context("query execution cost should stay priced")?;
        assert_eq!(query_rollup.total_cost, Decimal::new(255, 4));
        assert_eq!(query_rollup.provider_call_count, 3);

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
                    call_kind: "embed_chunk_batch".to_string(),
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
            .list_execution_provider_calls(
                &fixture.state,
                "query_execution",
                fixture.query_execution_id,
            )
            .await
            .context("failed to list query execution provider calls")?;
        provider_calls.extend(
            billing
                .list_execution_provider_calls(
                    &fixture.state,
                    "ingest_attempt",
                    fixture.ingest_attempt_id,
                )
                .await
                .context("failed to list ingest execution provider calls")?,
        );
        assert_eq!(provider_calls.len(), 4);
        assert!(provider_calls.iter().any(|row| row.call_kind == "query_planning"));
        assert!(provider_calls.iter().any(|row| row.call_kind == "query_answer"));
        assert!(provider_calls.iter().any(|row| row.call_kind == "query_rerank"));
        assert!(provider_calls.iter().any(|row| row.call_kind == "embed_chunk_batch"));

        let mut charges = billing
            .list_execution_charges(&fixture.state, "query_execution", fixture.query_execution_id)
            .await
            .context("failed to list query execution charges")?;
        charges.extend(
            billing
                .list_execution_charges(&fixture.state, "ingest_attempt", fixture.ingest_attempt_id)
                .await
                .context("failed to list ingest execution charges")?,
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
        assert_eq!(stored_query_cost.total_cost, Decimal::new(255, 4));
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
#[ignore = "requires local postgres with canonical extensions"]
async fn execution_cost_returns_zero_rollup_for_ingest_attempt_without_provider_calls() -> Result<()>
{
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let billing = &fixture.state.canonical_services.billing;
        let zero_cost = billing
            .get_execution_cost(&fixture.state, "ingest_attempt", fixture.ingest_attempt_id)
            .await
            .context("failed to load zero-cost ingest execution")?;
        assert_eq!(zero_cost.total_cost, Decimal::ZERO);
        assert_eq!(zero_cost.provider_call_count, 0);
        assert_eq!(zero_cost.currency_code, "USD");
        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango"]
async fn inline_ingest_billing_capture_produces_provider_calls_and_rollup() -> Result<()> {
    let fixture = BillingRollupsFixture::create().await?;

    let result = async {
        let billing = &fixture.state.canonical_services.billing;
        let ingest_cost = billing
            .capture_ingest_attempt(
                &fixture.state,
                CaptureIngestAttemptBillingCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    attempt_id: fixture.ingest_attempt_id,
                    binding_id: None,
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5.4-mini".to_string(),
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
            .list_execution_provider_calls(
                &fixture.state,
                "ingest_attempt",
                fixture.ingest_attempt_id,
            )
            .await
            .context("failed to list ingest attempt provider calls")?;
        assert!(!provider_calls.is_empty());
        assert!(provider_calls.iter().any(|row| row.call_kind == "extract_graph"));

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
