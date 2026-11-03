use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use ironrag_backend::{
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

struct RecordingEmbeddingGateway {
    pool: PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
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
    let cost = fixture
        .state
        .application_services
        .billing
        .roll_up_execution_cost(&fixture.state, "knowledge_search", owner_id)
        .await?
        .context("knowledge search cost")?;
    assert_eq!(cost.provider_call_count, 1);
    assert_eq!(i64::from(cost.unpriced_provider_call_count), unpriced);
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
