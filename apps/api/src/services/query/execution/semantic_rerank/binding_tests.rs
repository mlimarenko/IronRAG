//! Behavioural proof that semantic rerank resolves the `query_rerank` binding
//! through the real runtime resolution path.
//!
//! The sibling helper test asserts the compile-time constant only. These tests
//! configure real `ai_binding` rows in a disposable database, call the
//! production `execute_semantic_rerank`, and observe which provider the
//! recording `LlmGateway` was asked to call. Nothing here stubs
//! `resolve_active_runtime_binding`: the binding is a real row selected by the
//! production SQL.
//!
//! Ignored by default because they need a live PostgreSQL with pgvector and
//! Redis reachable through `IRONRAG_DATABASE_URL` / `IRONRAG_REDIS_URL`; each
//! test creates and drops its own temporary database. They run in the
//! integration job through the count-gated target:
//!
//! ```sh
//! make backend-test-lib-ignored
//! ```

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use anyhow::Context as _;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::*;
use crate::{
    app::{config::Settings, seed_clean_v1_ai_catalog, state::AppState},
    domains::agent_runtime::{RuntimeExecutionOwnerKind, RuntimeLifecycleState, RuntimeTaskKind},
    infra::{
        persistence::Persistence,
        repositories::{ai_repository, catalog_repository, query_repository, runtime_repository},
    },
    integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
    },
    shared::secret_encryption::SecretPurpose,
};

/// A synthetic, non-production credential value. Never a real key.
const SYNTHETIC_PROVIDER_CREDENTIAL: &str = "semantic-rerank-binding-test-credential";

/// One observed `generate` call, reduced to the identity the test cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedChat {
    provider_kind: String,
    model_name: String,
}

/// Records every `ChatRequest` the production rerank asks the gateway to issue.
#[derive(Clone, Default)]
struct RecordingGateway {
    chats: Arc<Mutex<Vec<RecordedChat>>>,
}

impl RecordingGateway {
    fn recorded(&self) -> Vec<RecordedChat> {
        // A poisoned recorder means a previous test panicked; recover the
        // observed calls rather than adding a second panic.
        self.chats.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

#[async_trait]
impl LlmGateway for RecordingGateway {
    async fn generate(&self, mut request: ChatRequest) -> anyhow::Result<ChatResponse> {
        self.chats.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(RecordedChat {
            provider_kind: request.provider_kind.clone(),
            model_name: request.model_name.clone(),
        });
        // Exactly one prepared candidate is submitted in these tests.
        Ok(ChatResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text: serde_json::json!({"scores": [{"index": 0, "score": 0.9}]}).to_string(),
            usage_json: serde_json::json!({
                "prompt_tokens": 12,
                "completion_tokens": 4,
                "total_tokens": 16,
            }),
        })
    }

    async fn embed(&self, request: EmbeddingRequest) -> anyhow::Result<EmbeddingResponse> {
        Err(anyhow!(
            "embed is not used by the semantic rerank binding proof: {}",
            request.provider_kind
        ))
    }

    async fn embed_many(
        &self,
        request: EmbeddingBatchRequest,
    ) -> anyhow::Result<EmbeddingBatchResponse> {
        Err(anyhow!(
            "embed_many is not used by the semantic rerank binding proof: {}",
            request.provider_kind
        ))
    }

    async fn vision_extract(&self, request: VisionRequest) -> anyhow::Result<VisionResponse> {
        Err(anyhow!(
            "vision_extract is not used by the semantic rerank binding proof: {}",
            request.provider_kind
        ))
    }
}

/// Disposable, freshly migrated database for one test.
struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
    /// Set once cleanup ran, so the `Drop` fallback does not repeat it.
    cleaned: AtomicBool,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> anyhow::Result<Self> {
        let mut admin_url = url::Url::parse(base_database_url)
            .context("parse semantic rerank test database url")?;
        admin_url.set_path("/postgres");
        let admin_url = admin_url.to_string();
        let name = format!("semantic_rerank_binding_{}", Uuid::now_v7().simple());

        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("connect semantic rerank test admin postgres")?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
            .execute(&admin_pool)
            .await
            .context("create semantic rerank test database")?;
        admin_pool.close().await;

        let mut database_url = url::Url::parse(base_database_url)
            .context("parse semantic rerank test database url")?;
        database_url.set_path(&format!("/{name}"));
        Ok(Self {
            name,
            admin_url,
            database_url: database_url.to_string(),
            cleaned: AtomicBool::new(false),
        })
    }

    /// Happy-path cleanup: terminate backends and drop the database, then mark
    /// this handle cleaned so the `Drop` fallback does nothing.
    async fn drop(self) -> anyhow::Result<()> {
        cleanup_temp_database(&self.admin_url, &self.name).await?;
        self.cleaned.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// Terminates backends and drops `name`. Shared by the explicit cleanup above
/// and the panic-safe `Drop` fallback, so neither leaks
/// `semantic_rerank_binding_<uuid>`.
async fn cleanup_temp_database(admin_url: &str, name: &str) -> anyhow::Result<()> {
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url)
        .await
        .context("reconnect semantic rerank test admin postgres for cleanup")?;
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(name)
    .execute(&admin_pool)
    .await
    .context("terminate semantic rerank test database connections")?;
    sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{name}\"")))
        .execute(&admin_pool)
        .await
        .context("drop semantic rerank test database")?;
    admin_pool.close().await;
    Ok(())
}

impl Drop for TempDatabase {
    /// Best-effort cleanup for the path where a `?` returns before the explicit
    /// `drop` call. `Drop` cannot await, so a dedicated current-thread runtime
    /// performs the same terminate-and-drop off the test runtime. A no-op after
    /// `drop` already ran.
    fn drop(&mut self) {
        if self.cleaned.swap(true, Ordering::SeqCst) {
            return;
        }
        let name = self.name.clone();
        let admin_url = self.admin_url.clone();
        let cleanup = std::thread::spawn(move || {
            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build()
            {
                let _ = runtime.block_on(cleanup_temp_database(&admin_url, &name));
            }
        });
        let _ = cleanup.join();
    }
}

/// A real `AppState` over a disposable database, with the production runtime
/// resolution and billing paths intact and only the outbound provider replaced
/// by a recorder.
struct RerankFixture {
    state: AppState,
    gateway: RecordingGateway,
    database: TempDatabase,
    workspace_id: Uuid,
    library_id: Uuid,
    query_execution_id: Uuid,
    runtime_execution_id: Uuid,
}

impl RerankFixture {
    async fn create() -> anyhow::Result<Self> {
        let mut settings =
            Settings::from_env().context("load settings for semantic rerank test")?;
        let database = TempDatabase::create(&settings.database_url).await?;
        settings.database_url = database.database_url.clone();
        settings.credential_master_key = Some(STANDARD.encode([41_u8; 32]));

        let postgres = PgPoolOptions::new()
            .max_connections(4)
            .connect(&settings.database_url)
            .await
            .context("connect semantic rerank test postgres")?;
        sqlx::migrate!("./migrations")
            .run(&postgres)
            .await
            .context("apply migrations for semantic rerank test")?;
        seed_clean_v1_ai_catalog(&postgres)
            .await
            .context("seed clean-v1 AI catalog for semantic rerank test")?;

        let redis = redis::Client::open(settings.redis_url.clone())
            .context("create redis client for semantic rerank test state")?;
        let persistence = Persistence::for_tests(postgres, redis);
        let mut state = AppState::from_dependencies(settings, persistence)
            .context("build AppState for semantic rerank test")?;
        let gateway = RecordingGateway::default();
        state.llm_gateway = Arc::new(gateway.clone());

        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &format!("semantic-rerank-workspace-{suffix}"),
            "Semantic Rerank Workspace",
            None,
        )
        .await
        .context("create semantic rerank workspace")?;
        let library = catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            &format!("semantic-rerank-library-{suffix}"),
            "Semantic Rerank Library",
            Some("binding resolution proof fixture"),
            None,
        )
        .await
        .context("create semantic rerank library")?;

        // A durable query execution is required by the billing reservation that
        // gates the provider call, so the rerank must resolve real attribution.
        let conversation = query_repository::create_conversation(
            &state.persistence.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: workspace.id,
                library_id: library.id,
                created_by_principal_id: None,
                title: Some("Semantic rerank binding proof"),
                conversation_state: "active",
                request_surface: "ui",
            },
            5,
        )
        .await
        .context("create semantic rerank conversation")?;
        let query_execution_id = Uuid::now_v7();
        let runtime_execution_id = Uuid::now_v7();
        runtime_repository::create_runtime_execution(
            &state.persistence.postgres,
            &runtime_repository::NewRuntimeExecution {
                id: runtime_execution_id,
                owner_kind: RuntimeExecutionOwnerKind::QueryExecution.as_str(),
                owner_id: query_execution_id,
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
                parent_execution_id: None,
            },
        )
        .await
        .context("create semantic rerank runtime execution")?;
        query_repository::create_execution(
            &state.persistence.postgres,
            &query_repository::NewQueryExecution {
                execution_id: query_execution_id,
                context_bundle_id: Uuid::now_v7(),
                workspace_id: workspace.id,
                library_id: library.id,
                conversation_id: conversation.id,
                request_turn_id: None,
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id,
                query_text: "rank evidence",
                failure_code: None,
            },
        )
        .await
        .context("create semantic rerank query execution")?;

        Ok(Self {
            state,
            gateway,
            database,
            workspace_id: workspace.id,
            library_id: library.id,
            query_execution_id,
            runtime_execution_id,
        })
    }

    /// Inserts one real `ai_binding` row through the production repository,
    /// binding the given seeded provider/model to the requested purpose.
    async fn add_binding(
        &self,
        binding_purpose: &str,
        provider_kind: &str,
        model_name: &str,
    ) -> anyhow::Result<()> {
        let pool = &self.state.persistence.postgres;
        let provider = ai_repository::list_provider_catalog(pool)
            .await
            .context("list provider catalog for semantic rerank test")?
            .into_iter()
            .find(|row| row.provider_kind == provider_kind)
            .with_context(|| format!("seeded provider {provider_kind} was not found"))?;
        let model = ai_repository::list_model_catalog(pool, Some(provider.id))
            .await
            .context("list model catalog for semantic rerank test")?
            .into_iter()
            .find(|row| row.model_name == model_name)
            .with_context(|| {
                format!("seeded model {model_name} was not found for provider {provider_kind}")
            })?;

        let account_id = Uuid::now_v7();
        let encrypted_api_key = self.state.credential_cipher.encrypt(
            SecretPurpose::AiAccountApiKey,
            account_id,
            SYNTHETIC_PROVIDER_CREDENTIAL,
        )?;
        let account = ai_repository::create_account(
            pool,
            ai_repository::NewAiAccount {
                account_id,
                scope_kind: "workspace",
                workspace_id: Some(self.workspace_id),
                library_id: None,
                provider_catalog_id: provider.id,
                label: &format!("semantic-rerank-{binding_purpose}-credential"),
                base_url: None,
                created_by_principal_id: None,
            },
            Some(&encrypted_api_key),
        )
        .await
        .context("create semantic rerank provider account")?;

        ai_repository::create_binding(
            pool,
            ai_repository::CreateAiBindingInput {
                scope_kind: "library",
                workspace_id: Some(self.workspace_id),
                library_id: Some(self.library_id),
                binding_purpose,
                account_id: account.id,
                provider_catalog_id: provider.id,
                model_catalog_id: model.id,
                system_prompt: None,
                temperature: None,
                top_p: None,
                max_output_tokens_override: None,
                extra_parameters_json: serde_json::json!({}),
                binding_state: "active",
                updated_by_principal_id: None,
            },
        )
        .await
        .context("create semantic rerank binding")?;
        Ok(())
    }

    fn execution_context(&self) -> SemanticRerankExecutionContext {
        SemanticRerankExecutionContext {
            workspace_id: self.workspace_id,
            query_execution_id: self.query_execution_id,
            runtime_execution_id: self.runtime_execution_id,
        }
    }

    fn prepared_request(&self) -> anyhow::Result<PreparedSemanticRerankRequest> {
        let chunks = [RerankCandidate {
            id: "chunk-a".to_string(),
            text: "candidate evidence".to_string(),
            score: Some(0.5),
        }];
        prepare_semantic_rerank_request(
            "rank evidence",
            &[],
            &[],
            &chunks,
            SemanticRerankPolicy::bounded(5_000, 4, 1_200, 4_000),
        )
        .context("candidate evidence should produce a prepared rerank request")
    }

    async fn cleanup(self) -> anyhow::Result<()> {
        self.state.persistence.postgres.close().await;
        self.database.drop().await
    }
}

/// (a) A configured `query_compile` binding must not satisfy the rerank: the
/// rerank resolves only `query_rerank`, so with no such row it fails with
/// `MissingBinding` before any provider call.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn semantic_rerank_does_not_fall_back_to_the_compile_binding() -> anyhow::Result<()> {
    let fixture = RerankFixture::create().await?;
    fixture.add_binding("query_compile", "openai", "gpt-6-astra").await?;

    let attempt = execute_semantic_rerank(
        &fixture.state,
        fixture.library_id,
        fixture.execution_context(),
        fixture.prepared_request()?,
        SemanticRerankPolicy::bounded(5_000, 4, 1_200, 4_000),
    )
    .await;

    assert_eq!(
        attempt,
        SemanticRerankAttempt::Failed {
            failure: SemanticRerankFailure::MissingBinding,
            prepared_candidate_count: 1,
        },
        "a query_compile binding must never satisfy the query_rerank resolution",
    );
    assert!(
        fixture.gateway.recorded().is_empty(),
        "the rerank must not invoke any provider when only a query_compile binding exists",
    );

    fixture.cleanup().await
}

/// (b) With a real `query_rerank` binding, the production path reaches the
/// provider call and the gateway observes exactly that binding's provider and
/// model, not the `query_compile` binding configured beside it.
#[tokio::test]
#[ignore = "requires local postgres service"]
async fn semantic_rerank_invokes_the_provider_tied_to_the_rerank_binding() -> anyhow::Result<()> {
    let fixture = RerankFixture::create().await?;
    fixture.add_binding("query_compile", "openai", "gpt-6-astra").await?;
    fixture.add_binding("query_rerank", "deepseek", "deepseek-v4-pro").await?;

    let attempt = execute_semantic_rerank(
        &fixture.state,
        fixture.library_id,
        fixture.execution_context(),
        fixture.prepared_request()?,
        SemanticRerankPolicy::bounded(5_000, 4, 1_200, 4_000),
    )
    .await;

    let recorded = fixture.gateway.recorded();
    assert_eq!(
        recorded,
        vec![RecordedChat {
            provider_kind: "deepseek".to_string(),
            model_name: "deepseek-v4-pro".to_string(),
        }],
        "the rerank must call the provider tied to its own query_rerank binding",
    );
    assert!(
        !matches!(
            attempt,
            SemanticRerankAttempt::Failed { failure: SemanticRerankFailure::MissingBinding, .. }
        ),
        "the query_rerank binding was resolved, so the rerank must not report a missing binding",
    );

    fixture.cleanup().await
}
