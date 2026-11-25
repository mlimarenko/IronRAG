use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use ironrag_backend::{
    domains::billing::CostCoverageStatus,
    infra::repositories::billing_repository,
    integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, VisionRequest,
        VisionResponse,
    },
};
use rust_decimal::Decimal;
use tokio::sync::Notify;

#[derive(Clone, Copy)]
enum Outcome {
    Success,
    MissingUsage,
    InvalidUsage,
    ZeroUsage,
    Failure,
    WrongDimensions,
    ChangedProfile,
    Blocked,
}

/// One mock admission the real transport would perform for a mocked gateway.
struct MockSendAdmission<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    owning_execution_kind: &'a str,
    owning_execution_id: Uuid,
    call_kind: &'a str,
    binding_id: Uuid,
    provider_catalog_id: Uuid,
    model_catalog_id: Uuid,
    model_name: &'a str,
}

/// Simulates the admission the real transport performs, so a mocked gateway can
/// participate in the same send scope a product send would use.
async fn admit_mock_send(pool: &PgPool, send: MockSendAdmission<'_>) -> Result<Uuid> {
    let MockSendAdmission {
        workspace_id,
        library_id,
        owning_execution_kind,
        owning_execution_id,
        call_kind,
        binding_id,
        provider_catalog_id,
        model_catalog_id,
        model_name,
    } = send;
    // Mirror the admission claim: the first send of a scope reuses the
    // reservation's logical id, a later send in the same scope owns a new row.
    let provider_call_id = if ironrag_backend::shared::provider_send::current_send_id().is_some() {
        Uuid::now_v7()
    } else {
        ironrag_backend::shared::provider_send::current_send_scope()
            .map(|scope| scope.logical_id())
            .unwrap_or_else(Uuid::now_v7)
    };
    let attribution = ironrag_backend::shared::provider_send::ProviderSendAttribution {
        workspace_id,
        library_id,
        owning_execution_kind: owning_execution_kind.to_string(),
        owning_execution_id,
        call_kind: call_kind.to_string(),
        binding_id: Some(binding_id),
        provider_catalog_id,
        model_catalog_id,
        runtime_execution_id: None,
        runtime_task_kind: None,
    };
    let body = serde_json::json!({"model": model_name});
    ironrag_backend::infra::repositories::billing_repository::admit_provider_send(
        pool,
        ironrag_backend::infra::repositories::billing_repository::ProviderSendRequest {
            required_scope: None,
            caller: &ironrag_backend::shared::provider_http::DispatchCaller::Unattributed,
            model: model_name,
            endpoint: "https://provider.example/v1/embeddings",
            body: &body,
            attribution: &attribution,
            provider_call_id,
        },
    )
    .await
    .map_err(|error| anyhow::anyhow!("mock admission failed: {error:?}"))?;
    ironrag_backend::shared::provider_send::publish_test_send_row(provider_call_id);
    Ok(provider_call_id)
}

struct RecordingEmbeddingGateway {
    pool: PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    binding_id: Uuid,
    provider_catalog_id: Uuid,
    model_catalog_id: Uuid,
    outcome: Outcome,
    attempts: AtomicUsize,
    dispatches: AtomicUsize,
    entered: Notify,
    response_returned: Notify,
}

#[async_trait]
impl LlmGateway for RecordingEmbeddingGateway {
    async fn generate(&self, _: ChatRequest) -> Result<ChatResponse> {
        anyhow::bail!("unexpected chat call")
    }

    async fn embed_many(&self, _: EmbeddingBatchRequest) -> Result<EmbeddingBatchResponse> {
        anyhow::bail!("unexpected embedding batch")
    }

    async fn vision_extract(&self, _: VisionRequest) -> Result<VisionResponse> {
        anyhow::bail!("unexpected vision call")
    }

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        let search_id: Uuid = sqlx::query_scalar(
            "select id from billing_knowledge_search where workspace_id=$1 and library_id=$2 order by id desc limit 1",
        )
        .bind(self.workspace_id)
        .bind(self.library_id)
        .fetch_one(&self.pool)
        .await?;
        admit_mock_send(
            &self.pool,
            MockSendAdmission {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                owning_execution_kind: "knowledge_search",
                owning_execution_id: search_id,
                call_kind: "knowledge_search_embedding",
                binding_id: self.binding_id,
                provider_catalog_id: self.provider_catalog_id,
                model_catalog_id: self.model_catalog_id,
                model_name: &request.model_name,
            },
        )
        .await?;
        // Observe admission independently at the real HTTP provider I/O boundary.
        let reserved: i64 = sqlx::query_scalar(
            "select count(*) from billing_provider_call c
             join billing_knowledge_search s on s.id=c.owning_execution_id
             where c.owning_execution_kind='knowledge_search'
               and c.call_kind='knowledge_search_embedding' and c.call_state='started'
               and c.workspace_id=$1 and c.library_id=$2
               and s.workspace_id=c.workspace_id and s.library_id=c.library_id
               and c.runtime_execution_id is null and c.runtime_task_kind is null",
        )
        .bind(self.workspace_id)
        .bind(self.library_id)
        .fetch_one(&self.pool)
        .await?;
        assert_eq!(
            reserved, 1,
            "knowledge search provider I/O requires a durable scoped reservation"
        );
        let call_id = ironrag_backend::observability::current_provider_call_id()
            .context("embedding must carry the reserved provider-call identity")?;
        let attributed: bool = sqlx::query_scalar(
            "select exists(select 1 from billing_provider_call c
             join ai_binding b on b.id=c.binding_id
             join ai_model_catalog m on m.id=c.model_catalog_id
             join ai_provider_catalog p on p.id=c.provider_catalog_id
             where c.id=$1 and c.workspace_id=$2 and c.library_id=$3
               and b.binding_purpose='embed_chunk'
               and m.model_name=$4 and p.provider_kind=$5)",
        )
        .bind(Uuid::parse_str(&call_id)?)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(&request.model_name)
        .bind(&request.provider_kind)
        .fetch_one(&self.pool)
        .await?;
        assert!(attributed, "observability identity must select the actual binding/model/scope");

        if matches!(self.outcome, Outcome::Failure) {
            ironrag_backend::integrations::retry::with_retry(
                || async {
                    self.dispatches.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(
                        ironrag_backend::integrations::retry::ProviderCallError::http_status(
                            "synthetic",
                            reqwest::StatusCode::SERVICE_UNAVAILABLE,
                            &reqwest::header::HeaderMap::new(),
                            "synthetic embedding transport failure",
                        ),
                    )
                },
                ironrag_backend::integrations::retry::RetryPolicy::idempotent_provider_call(
                    2,
                    Duration::ZERO,
                    Duration::ZERO,
                ),
            )
            .await?;
        }
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if matches!(self.outcome, Outcome::Blocked) {
            std::future::pending::<()>().await;
        }
        if matches!(self.outcome, Outcome::ChangedProfile) {
            sqlx::query(
                "update ai_binding set extra_parameters_json=extra_parameters_json || '{\"dimensions\":4}'::jsonb
                 where library_id=$1 and binding_purpose='embed_chunk'",
            )
            .bind(self.library_id)
            .execute(&self.pool)
            .await?;
        }
        let usage_json = match self.outcome {
            Outcome::MissingUsage => json!({}),
            Outcome::InvalidUsage => json!({"prompt_tokens": -1}),
            Outcome::ZeroUsage => json!({"prompt_tokens": 0}),
            _ => json!({"prompt_tokens": 17, "total_tokens": 17}),
        };
        let embedding = if matches!(self.outcome, Outcome::WrongDimensions) {
            vec![0.9, 0.8]
        } else {
            vec![0.9, 0.8, 0.7]
        };
        self.response_returned.notify_one();
        Ok(EmbeddingResponse {
            provider_kind: request.provider_kind.clone(),
            model_name: request.model_name.clone(),
            dimensions: embedding.len(),
            embedding,
            usage_json,
        })
    }
}

async fn configure_accounting(
    fixture: &mut KnowledgeSearchHttpFixture,
    outcome: Outcome,
) -> Result<Arc<RecordingEmbeddingGateway>> {
    let binding = fixture
        .state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(
            &fixture.state,
            fixture.library_id,
            AiBindingPurpose::EmbedChunk,
        )
        .await?
        .context("fixture embedding binding")?;
    sqlx::query("update ai_model_catalog set currency_code='USD', input_price_per_1m=1, cached_input_price_per_1m=3, output_price_per_1m=2, long_context_threshold_tokens=null, long_context_input_price_per_1m=null, long_context_cached_input_price_per_1m=null, long_context_output_price_per_1m=null where id=$1")
        .bind(binding.model_catalog_id).execute(&fixture.state.persistence.postgres).await?;
    let gateway = Arc::new(RecordingEmbeddingGateway {
        pool: fixture.state.persistence.postgres.clone(),
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        binding_id: binding.binding_id,
        provider_catalog_id: binding.provider_catalog_id,
        model_catalog_id: binding.model_catalog_id,
        outcome,
        attempts: AtomicUsize::new(0),
        dispatches: AtomicUsize::new(0),
        entered: Notify::new(),
        response_returned: Notify::new(),
    });
    fixture.state.llm_gateway = gateway.clone();
    Ok(gateway)
}

async fn assert_receipt(
    fixture: &KnowledgeSearchHttpFixture,
    call_state: &str,
    input_tokens: Option<i64>,
) -> Result<()> {
    let (call_id, owner_id) = assert_single_receipt_owner(fixture, call_state).await?;
    let unpriced =
        assert_receipt_usage(fixture, call_id, owner_id, call_state, input_tokens).await?;
    assert_receipt_cost_rollup(fixture, owner_id, call_state, unpriced, input_tokens).await
}

/// The durable call row a settled knowledge search must own: exactly one provider
/// call and one knowledge-search owner, in the expected state.
async fn assert_single_receipt_owner(
    fixture: &KnowledgeSearchHttpFixture,
    call_state: &str,
) -> Result<(Uuid, Uuid)> {
    let pool = &fixture.state.persistence.postgres;
    let calls: i64 =
        sqlx::query_scalar("select count(*) from billing_provider_call").fetch_one(pool).await?;
    let owners: i64 =
        sqlx::query_scalar("select count(*) from billing_knowledge_search").fetch_one(pool).await?;
    assert_eq!((calls, owners), (1, 1));
    let (call_id, owner_id, actual_state): (Uuid, Uuid, String) = sqlx::query_as(
        "select id, owning_execution_id, call_state::text from billing_provider_call
         where library_id=$1 and owning_execution_kind='knowledge_search'",
    )
    .bind(fixture.library_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(actual_state, call_state);
    Ok((call_id, owner_id))
}

/// The priced usage rows of that call, returning the unpriced-call count.
async fn assert_receipt_usage(
    fixture: &KnowledgeSearchHttpFixture,
    call_id: Uuid,
    owner_id: Uuid,
    call_state: &str,
    input_tokens: Option<i64>,
) -> Result<i64> {
    let pool = &fixture.state.persistence.postgres;
    let usage: Vec<(String, Decimal, Decimal)> = sqlx::query_as(
        "select u.usage_kind::text, u.quantity, c.total_price from billing_usage u
         join billing_charge c on c.usage_id=u.id where u.provider_call_id=$1",
    )
    .bind(call_id)
    .fetch_all(pool)
    .await?;
    let expected = input_tokens
        .map(|tokens| {
            vec![(
                "prompt_tokens".to_string(),
                Decimal::from(tokens),
                Decimal::from(tokens) / Decimal::from(1_000_000),
            )]
        })
        .unwrap_or_default();
    assert_eq!(usage, expected, "embedding must price observed input without inventing output");
    let unpriced = billing_repository::count_unpriced_provider_calls_by_execution(
        pool,
        "knowledge_search",
        owner_id,
    )
    .await?;
    assert_eq!(unpriced, i64::from(input_tokens.is_none() || call_state != "completed"));
    Ok(unpriced)
}

/// The derived cost rollup must count only a settled observation.
async fn assert_receipt_cost_rollup(
    fixture: &KnowledgeSearchHttpFixture,
    owner_id: Uuid,
    call_state: &str,
    unpriced: i64,
    input_tokens: Option<i64>,
) -> Result<()> {
    let cost = fixture
        .state
        .application_services
        .billing
        .roll_up_execution_cost(&fixture.state, "knowledge_search", owner_id)
        .await?
        .context("knowledge search cost")?;
    // The derived rollup counts settled observations, and only the `completed`
    // state is one. A `failed` or `canceled` reservation keeps its durable ledger
    // row and its unknown liability in `unpriced_provider_call_count`; it must be
    // neither counted as a settled call nor removed from the count.
    let settled = i64::from(call_state == "completed");
    assert_eq!(
        i64::from(cost.provider_call_count),
        settled,
        "a {call_state} attempt is not a settled provider call",
    );
    assert_eq!(i64::from(cost.unpriced_provider_call_count), unpriced);
    if settled == 0 {
        assert_eq!(
            cost.cost_coverage,
            CostCoverageStatus::Unknown,
            "an unsettled attempt is unknown liability, never a measured zero",
        );
    }
    assert_eq!(
        cost.total_cost,
        input_tokens
            .map_or(Decimal::ZERO, |tokens| { Decimal::from(tokens) / Decimal::from(1_000_000) })
    );
    Ok(())
}

async fn check_http_outcome(
    outcome: Outcome,
    status: StatusCode,
    input: Option<i64>,
) -> Result<()> {
    Box::pin(async move {
        let mut fixture = Box::pin(KnowledgeSearchHttpFixture::create()).await?;
        let result = async {
            let gateway = configure_accounting(&mut fixture, outcome).await?;
            let response = fixture.app().oneshot(fixture.search_request("orion", 5)?).await?;
            assert_eq!(response.status(), status);
            assert_eq!(gateway.attempts.load(Ordering::SeqCst), 1);
            assert_eq!(
                gateway.dispatches.load(Ordering::SeqCst),
                1,
                "one reservation cannot hide retries"
            );
            assert_receipt(
                &fixture,
                if matches!(outcome, Outcome::Failure) { "failed" } else { "completed" },
                input,
            )
            .await
        }
        .await;
        fixture.cleanup().await?;
        result
    })
    .await
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_reserves_before_dispatch_and_prices_input() -> Result<()> {
    check_http_outcome(Outcome::Success, StatusCode::OK, Some(17)).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_missing_usage_is_unknown() -> Result<()> {
    check_http_outcome(Outcome::MissingUsage, StatusCode::OK, None).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_invalid_usage_is_unknown() -> Result<()> {
    check_http_outcome(Outcome::InvalidUsage, StatusCode::OK, None).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_explicit_zero_is_priced() -> Result<()> {
    check_http_outcome(Outcome::ZeroUsage, StatusCode::OK, Some(0)).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_failure_is_durable_without_replay() -> Result<()> {
    check_http_outcome(Outcome::Failure, StatusCode::BAD_GATEWAY, None).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_dimension_rejection_retains_paid_usage() -> Result<()> {
    check_http_outcome(Outcome::WrongDimensions, StatusCode::INTERNAL_SERVER_ERROR, Some(17)).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_profile_rejection_retains_paid_usage() -> Result<()> {
    check_http_outcome(Outcome::ChangedProfile, StatusCode::CONFLICT, Some(17)).await
}

struct IngestBatchGateway {
    pool: PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    attempt_id: Uuid,
    binding_id: Uuid,
    provider_catalog_id: Uuid,
    model_catalog_id: Uuid,
    calls: std::sync::Mutex<Vec<Uuid>>,
    fail_second: bool,
    dimensions: usize,
}

#[async_trait]
impl LlmGateway for IngestBatchGateway {
    async fn generate(&self, _: ChatRequest) -> Result<ChatResponse> {
        anyhow::bail!("unexpected chat call")
    }

    async fn embed(&self, _: EmbeddingRequest) -> Result<EmbeddingResponse> {
        anyhow::bail!("unexpected single embedding")
    }

    async fn vision_extract(&self, _: VisionRequest) -> Result<VisionResponse> {
        anyhow::bail!("unexpected vision call")
    }

    async fn embed_many(&self, request: EmbeddingBatchRequest) -> Result<EmbeddingBatchResponse> {
        admit_mock_send(
            &self.pool,
            MockSendAdmission {
                workspace_id: self.workspace_id,
                library_id: self.library_id,
                owning_execution_kind: "ingest_attempt",
                owning_execution_id: self.attempt_id,
                call_kind: "embed_chunk",
                binding_id: self.binding_id,
                provider_catalog_id: self.provider_catalog_id,
                model_catalog_id: self.model_catalog_id,
                model_name: &request.model_name,
            },
        )
        .await?;
        let id = Uuid::parse_str(
            &ironrag_backend::observability::current_provider_call_id()
                .context("batch must carry its reserved provider-call ID")?,
        )?;
        let started: bool = sqlx::query_scalar(
            "select exists(select 1 from billing_provider_call
             where id=$1 and workspace_id=$2 and library_id=$3
             and owning_execution_kind='ingest_attempt' and owning_execution_id=$4
             and call_kind='embed_chunk' and call_state='started')",
        )
        .bind(id)
        .bind(self.workspace_id)
        .bind(self.library_id)
        .bind(self.attempt_id)
        .fetch_one(&self.pool)
        .await?;
        assert!(started, "the exact batch reservation must precede provider dispatch");
        let previous = self.calls.lock().unwrap().last().copied();
        if let Some(previous) = previous {
            let completed: bool = sqlx::query_scalar(
                "select exists(select 1 from billing_provider_call
                 where id=$1 and call_state='completed')",
            )
            .bind(previous)
            .fetch_one(&self.pool)
            .await?;
            assert!(completed, "the dimension-learning batch must settle before the next batch");
            let tokens: i64 = sqlx::query_scalar(
                "select quantity::bigint from billing_usage
                 where provider_call_id=$1 and usage_kind='prompt_tokens'",
            )
            .bind(previous)
            .fetch_one(&self.pool)
            .await?;
            assert_eq!(tokens, 17);
        }
        let ordinal = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(id);
            calls.len()
        };
        if self.fail_second && ordinal == 2 {
            return Err(ironrag_backend::integrations::retry::ProviderCallError::protocol(
                "synthetic batch failure",
            )
            .into());
        }
        let tokens = if ordinal == 1 { 17 } else { 29 };
        Ok(EmbeddingBatchResponse {
            provider_kind: request.provider_kind.clone(),
            model_name: request.model_name.clone(),
            dimensions: self.dimensions,
            embeddings: vec![vec![0.5; self.dimensions]; request.inputs.len()],
            usage_json: json!({"prompt_tokens": tokens, "total_tokens": tokens}),
        })
    }
}

async fn prepare_ingest_batch_fixture(
    fixture: &mut KnowledgeSearchHttpFixture,
    fail_second: bool,
) -> Result<(SearchSeedIds, Uuid, i64, Arc<IngestBatchGateway>)> {
    let ids = SearchSeedIds::new();
    seed_search_document_chunk(
        &fixture.state,
        fixture.workspace_id,
        fixture.library_id,
        &ids,
        Utc::now(),
    )
    .await?;
    let template = fixture
        .state
        .document_store
        .list_chunks_by_revision(ids.revision_id)
        .await?
        .into_iter()
        .next()
        .context("seed chunk")?;
    for index in 1..17 {
        let mut chunk = template.clone();
        chunk.chunk_id = Uuid::now_v7();
        chunk.chunk_index = index;
        chunk.content_text = format!("batch evidence {index}");
        chunk.normalized_text = chunk.content_text.clone();
        fixture.state.document_store.upsert_chunk(&chunk).await?;
    }
    let job_id = Uuid::now_v7();
    let attempt_id = Uuid::now_v7();
    sqlx::query(
        "insert into ingest_job
         (id, workspace_id, library_id, knowledge_revision_id, job_kind, queue_state,
          queue_leased_at, queue_lease_token, queue_lease_owner)
         values ($1,$2,$3,$4,'content_mutation','leased',now(),$5,'batch-test')",
    )
    .bind(job_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(ids.revision_id)
    .bind(format!("batch-lease-{job_id}"))
    .execute(&fixture.state.persistence.postgres)
    .await?;
    sqlx::query(
        "insert into ingest_attempt (id,job_id,attempt_number,attempt_state,lease_token,heartbeat_at)
         values ($1,$2,1,'leased',$3,now())",
    ).bind(attempt_id).bind(job_id).bind(format!("batch-attempt-{attempt_id}"))
    .execute(&fixture.state.persistence.postgres).await?;
    let source_version = repositories::get_library_source_truth_version(
        &fixture.state.persistence.postgres,
        fixture.library_id,
    )
    .await?;
    fixture.state.settings.ingestion_embedding_parallelism = 1;
    let binding = fixture
        .state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(
            &fixture.state,
            fixture.library_id,
            AiBindingPurpose::EmbedChunk,
        )
        .await?
        .context("fixture embedding binding")?;
    let dimensions = usize::try_from(
        binding.effective_embedding_dimensions.context("fixture embedding dimension")?.get(),
    )?;
    let gateway = Arc::new(IngestBatchGateway {
        pool: fixture.state.persistence.postgres.clone(),
        workspace_id: fixture.workspace_id,
        library_id: fixture.library_id,
        attempt_id,
        binding_id: binding.binding_id,
        provider_catalog_id: binding.provider_catalog_id,
        model_catalog_id: binding.model_catalog_id,
        calls: std::sync::Mutex::new(Vec::new()),
        fail_second,
        dimensions,
    });
    fixture.state.llm_gateway = gateway.clone();
    Ok((ids, attempt_id, source_version, gateway))
}

async fn run_ingest_batch_case(fail_second: bool, refuse_reservation: bool) -> Result<()> {
    use ironrag_backend::services::query::search::EmbedRevisionChunksInput;
    use tokio_util::sync::CancellationToken;

    let mut fixture = Box::pin(KnowledgeSearchHttpFixture::create_with_sources(false)).await?;
    let result = async {
        let (ids, attempt_id, source_version, gateway) =
            prepare_ingest_batch_fixture(&mut fixture, fail_second).await?;
        let submitted_attempt = if refuse_reservation { Uuid::now_v7() } else { attempt_id };
        let cancellation = CancellationToken::new();
        let result = fixture.state.application_services.search.embed_chunks_for_revision(
            EmbedRevisionChunksInput {
                state: &fixture.state,
                library_id: fixture.library_id,
                revision_id: ids.revision_id,
                attempt_id: submitted_attempt,
                vector_write_source_truth_version: source_version,
                cancellation_token: &cancellation,
            },
        ).await;
        let calls = gateway.calls.lock().unwrap().clone();
        if refuse_reservation {
            let error = result.expect_err("an unknown attempt must not reserve");
            assert_eq!(error.kind(), "QueryServiceError::NotFound");
            assert!(error.to_string().contains("ingest_attempt"));
            assert!(calls.is_empty(), "reservation refusal must precede every dispatch");
            let rows: i64 = sqlx::query_scalar(
                "select count(*) from billing_provider_call where owning_execution_id=$1",
            )
            .bind(submitted_attempt)
            .fetch_one(&fixture.state.persistence.postgres)
            .await?;
            assert_eq!(rows, 0);
            return Ok(());
        }
        assert_eq!(calls.len(), 2, "dimension learning and remainder require distinct calls");
        assert_ne!(calls[0], calls[1]);
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "select id, call_state::text from billing_provider_call
             where owning_execution_kind='ingest_attempt' and owning_execution_id=$1",
        ).bind(attempt_id).fetch_all(&fixture.state.persistence.postgres).await?;
        assert!(rows.contains(&(calls[0], "completed".to_string())));
        let tokens: i64 = sqlx::query_scalar(
            "select quantity::bigint from billing_usage where provider_call_id=$1 and usage_kind='prompt_tokens'",
        ).bind(calls[0]).fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(tokens, 17);
        if fail_second {
            let error = result.expect_err("the second batch fails");
            assert_eq!(error.provider_call_id(), Some(calls[1]));
            assert!(error.provider_failure().is_some());
            assert!(rows.contains(&(calls[1], "failed".to_string())));
        } else {
            let outcome = result.expect("both batches complete before success");
            assert_eq!(outcome.chunks_embedded, 17);
            assert!(rows.contains(&(calls[1], "completed".to_string())));
            let tokens: i64 = sqlx::query_scalar(
                "select quantity::bigint from billing_usage where provider_call_id=$1 and usage_kind='prompt_tokens'",
            ).bind(calls[1]).fetch_one(&fixture.state.persistence.postgres).await?;
            assert_eq!(tokens, 29);
        }
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with vector extensions and redis"]
async fn ingest_batch_accounting_reserves_and_settles_each_real_batch() -> Result<()> {
    Box::pin(run_ingest_batch_case(false, false)).await
}

#[tokio::test]
#[ignore = "requires local postgres with vector extensions and redis"]
async fn ingest_batch_accounting_preserves_prior_receipt_and_failed_identity() -> Result<()> {
    Box::pin(run_ingest_batch_case(true, false)).await
}

#[tokio::test]
#[ignore = "requires local postgres with vector extensions and redis"]
async fn ingest_batch_accounting_reservation_refusal_never_dispatches() -> Result<()> {
    Box::pin(run_ingest_batch_case(false, true)).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_authorization_denial_does_not_admit_spend() -> Result<()> {
    let mut fixture = Box::pin(KnowledgeSearchHttpFixture::create()).await?;
    let result = async {
        let gateway = configure_accounting(&mut fixture, Outcome::Success).await?;
        let mut request = fixture.search_request("orion", 5)?;
        request.headers_mut().remove(header::AUTHORIZATION);
        assert_eq!(fixture.app().oneshot(request).await?.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(gateway.attempts.load(Ordering::SeqCst), 0);
        let owners: i64 = sqlx::query_scalar("select count(*) from billing_knowledge_search")
            .fetch_one(&fixture.state.persistence.postgres)
            .await?;
        assert_eq!(owners, 0);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_empty_inventory_stays_lexical_without_admission() -> Result<()>
{
    let mut fixture = Box::pin(KnowledgeSearchHttpFixture::create_with_sources(false)).await?;
    let result = async {
        let gateway = configure_accounting(&mut fixture, Outcome::Success).await?;
        let body = fixture.search_document_hit("orion").await?;
        assert_eq!(body["embeddingProviderKind"], json!("lexical_only"));
        assert_eq!(gateway.attempts.load(Ordering::SeqCst), 0);
        let owners: i64 = sqlx::query_scalar("select count(*) from billing_knowledge_search")
            .fetch_one(&fixture.state.persistence.postgres)
            .await?;
        assert_eq!(owners, 0);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_unindexed_profile_does_not_admit_spend() -> Result<()> {
    let mut fixture = Box::pin(KnowledgeSearchHttpFixture::create()).await?;
    let result = async {
        let gateway = configure_accounting(&mut fixture, Outcome::Success).await?;
        // Vectors remain indexed for the old profile, never for this new one.
        sqlx::query("update ai_binding set extra_parameters_json=extra_parameters_json || '{\"dimensions\":4}'::jsonb where library_id=$1 and binding_purpose='embed_chunk'")
            .bind(fixture.library_id)
            .execute(&fixture.state.persistence.postgres)
            .await?;
        let response = fixture.app().oneshot(fixture.search_request("orion", 5)?).await?;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(gateway.attempts.load(Ordering::SeqCst), 0);
        let owners: i64 = sqlx::query_scalar("select count(*) from billing_knowledge_search")
            .fetch_one(&fixture.state.persistence.postgres)
            .await?;
        assert_eq!(owners, 0);
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

async fn check_canceled_request(after_response: bool) -> Result<()> {
    Box::pin(async move {
    let mut fixture = Box::pin(KnowledgeSearchHttpFixture::create()).await?;
    let result = async {
        let gateway = configure_accounting(&mut fixture, if after_response { Outcome::Success } else { Outcome::Blocked }).await?;
        let pool = &fixture.state.persistence.postgres;
        sqlx::raw_sql("create function irg311_terminal_notice() returns trigger language plpgsql as $$ begin perform pg_notify('irg311_terminal',new.id::text); return new; end $$; create trigger irg311_terminal_notice after update on billing_provider_call for each row when (new.call_state in ('completed','canceled')) execute function irg311_terminal_notice();")
            .execute(pool).await?;
        let mut listener = sqlx::postgres::PgListener::connect_with(pool).await?;
        listener.listen("irg311_terminal").await?;
        // Hold usage persistence after the gateway returns, then cancel the
        // caller. The shared reservation must own and reconcile that receipt.
        let mut blocker = pool.begin().await?;
        if after_response {
            sqlx::query("lock table billing_usage in access exclusive mode")
                .execute(&mut *blocker).await?;
        }
        let request = tokio::spawn(fixture.app().oneshot(fixture.search_request("orion", 5)?));
        timeout(SEARCH_WAIT_TIMEOUT, async {
            if after_response {
                gateway.response_returned.notified().await;
            } else {
                gateway.entered.notified().await;
            }
        }).await?;
        request.abort();
        assert!(request.await.expect_err("request must be canceled").is_cancelled());
        blocker.rollback().await?;
        timeout(SEARCH_WAIT_TIMEOUT, listener.recv()).await??;
        assert_eq!(gateway.dispatches.load(Ordering::SeqCst), 1);
        assert_receipt(&fixture, if after_response { "completed" } else { "canceled" }, after_response.then_some(17)).await
    }.await;
    fixture.cleanup().await?;
    result
    }).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_cancellation_before_response_is_unknown() -> Result<()> {
    check_canceled_request(false).await
}

#[tokio::test]
#[ignore = "requires local postgres and redis services"]
async fn knowledge_search_embedding_cancellation_after_response_retains_usage() -> Result<()> {
    check_canceled_request(true).await
}
