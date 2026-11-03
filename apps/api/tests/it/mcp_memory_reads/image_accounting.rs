use super::*;
use anyhow::{Result, bail};
use async_trait::async_trait;
use ironrag_backend::{
    infra::repositories::{ai_repository, billing_repository},
    integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
    },
    shared::secret_encryption::SecretPurpose,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy)]
enum Outcome {
    Success,
    Empty,
    Failure,
    Unknown,
    Partial,
    ZeroUsage,
    FullyCached,
    Blocked,
}

struct RecordingVisionGateway {
    outcome: Outcome,
    attempts: AtomicUsize,
    dispatches: AtomicUsize,
    entered: Notify,
    response_returned: Notify,
    pool: sqlx::PgPool,
}

#[async_trait]
impl LlmGateway for RecordingVisionGateway {
    async fn generate(&self, _: ChatRequest) -> Result<ChatResponse> {
        bail!("unexpected chat call")
    }
    async fn embed(&self, _: EmbeddingRequest) -> Result<EmbeddingResponse> {
        bail!("unexpected embedding call")
    }
    async fn embed_many(&self, _: EmbeddingBatchRequest) -> Result<EmbeddingBatchResponse> {
        bail!("unexpected embedding batch")
    }
    async fn vision_extract(&self, request: VisionRequest) -> Result<VisionResponse> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        // Admission is observed from a separate DB connection at the I/O boundary.
        let reserved: i64 = sqlx::query_scalar(
            "select count(*) from billing_provider_call where call_state='started'",
        )
        .fetch_one(&self.pool)
        .await?;
        assert_eq!(reserved, 1, "native image provider I/O must have a durable reservation");
        let call_id = ironrag_backend::observability::current_provider_call_id()
            .context("provider attempt must carry its durable event identity")?;
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from billing_provider_call where id=$1)")
                .bind(Uuid::parse_str(&call_id)?)
                .fetch_one(&self.pool)
                .await?;
        assert!(exists);
        if matches!(self.outcome, Outcome::Failure) {
            // Exercise the actual retry owner used by UnifiedGateway. A single
            // native reservation must not hide two transport dispatches.
            ironrag_backend::integrations::retry::with_retry(
                || async {
                    self.dispatches.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(
                        ironrag_backend::integrations::retry::ProviderCallError::http_status(
                            "synthetic",
                            reqwest::StatusCode::SERVICE_UNAVAILABLE,
                            &reqwest::header::HeaderMap::new(),
                            "synthetic provider failure with unknown liability",
                        ),
                    )
                },
                ironrag_backend::integrations::retry::RetryPolicy::idempotent_provider_call(
                    2,
                    std::time::Duration::ZERO,
                    std::time::Duration::ZERO,
                ),
            )
            .await?;
        }
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if matches!(self.outcome, Outcome::Blocked) {
            std::future::pending::<()>().await;
        }
        let usage_json = match self.outcome {
            Outcome::Unknown => json!({}),
            Outcome::Partial => json!({"prompt_tokens": 17}),
            Outcome::ZeroUsage => json!({"prompt_tokens": 0, "completion_tokens": 0}),
            Outcome::FullyCached => {
                json!({"prompt_tokens": 17, "completion_tokens": 5, "prompt_tokens_details":{"cached_tokens":17}})
            }
            _ => json!({"prompt_tokens": 17, "completion_tokens": 5, "total_tokens":22}),
        };
        self.response_returned.notify_one();
        Ok(VisionResponse {
            provider_kind: request.provider_kind.clone(),
            model_name: request.model_name.clone(),
            output_text: if matches!(self.outcome, Outcome::Empty) {
                " "
            } else {
                " A separate visual description. "
            }
            .into(),
            usage_json,
        })
    }
}

async fn configure_vision(
    fixture: &mut McpReadFixture,
    outcome: Outcome,
) -> Result<Arc<RecordingVisionGateway>> {
    let pool = &fixture.state.persistence.postgres;
    let provider = ai_repository::list_provider_catalog(pool)
        .await?
        .into_iter()
        .find(|row| row.provider_kind == "openai")
        .context("seeded provider")?;
    let model = ai_repository::list_model_catalog(pool, Some(provider.id))
        .await?
        .into_iter()
        .find(|row| row.allowed_binding_purposes.iter().any(|purpose| purpose == "extract_text"))
        .context("seeded vision model")?;
    let account_id = Uuid::now_v7();
    // Synthetic prices make full, partial and explicit-zero receipts observable
    // independently of changes to the seeded catalog's real model prices.
    sqlx::query("update ai_model_catalog set currency_code='USD', input_price_per_1m=1, cached_input_price_per_1m=3, output_price_per_1m=2, long_context_threshold_tokens=null, long_context_input_price_per_1m=null, long_context_cached_input_price_per_1m=null, long_context_output_price_per_1m=null where id=$1")
        .bind(model.id).execute(pool).await?;
    let encrypted = fixture.state.credential_cipher.encrypt(
        SecretPurpose::AiAccountApiKey,
        account_id,
        "synthetic-irg315-not-a-provider-key",
    )?;
    let account = ai_repository::create_account(
        pool,
        ai_repository::NewAiAccount {
            account_id,
            scope_kind: "workspace",
            workspace_id: Some(fixture.workspace_id),
            library_id: None,
            provider_catalog_id: provider.id,
            label: "IRG-315 recording fake only",
            base_url: None,
            created_by_principal_id: None,
        },
        Some(&encrypted),
    )
    .await?;
    ai_repository::create_binding(
        pool,
        ai_repository::CreateAiBindingInput {
            scope_kind: "library",
            workspace_id: Some(fixture.workspace_id),
            library_id: Some(fixture.library_id),
            binding_purpose: "extract_text",
            account_id: account.id,
            provider_catalog_id: provider.id,
            model_catalog_id: model.id,
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: json!({}),
            binding_state: "active",
            updated_by_principal_id: None,
        },
    )
    .await?;
    let gateway = Arc::new(RecordingVisionGateway {
        outcome,
        attempts: AtomicUsize::new(0),
        dispatches: AtomicUsize::new(0),
        entered: Notify::new(),
        response_returned: Notify::new(),
        pool: pool.clone(),
    });
    fixture.state.llm_gateway = gateway.clone();
    Ok(gateway)
}

async fn image_document(fixture: &McpReadFixture) -> Result<(Uuid, Uuid)> {
    let key = format!("irg315/{}/source.png", Uuid::now_v7());
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(64, 64).write_to(&mut bytes, image::ImageFormat::Png)?;
    fixture.state.content_storage.write_revision_source_raw(&key, bytes.get_ref()).await?;
    fixture
        .create_source_document_state(
            "irg315-image",
            "ready",
            Some("canonical OCR text"),
            "image/png",
            Some(&key),
        )
        .await
}

async fn calls(
    fixture: &McpReadFixture,
) -> Result<Vec<billing_repository::BillingProviderCallRow>> {
    Ok(sqlx::query_as("select id, workspace_id, library_id, binding_id, owning_execution_kind::text as owning_execution_kind, owning_execution_id, runtime_execution_id, runtime_task_kind::text as runtime_task_kind, provider_catalog_id, model_catalog_id, call_kind, call_state::text as call_state, started_at, completed_at from billing_provider_call order by started_at")
        .fetch_all(&fixture.state.persistence.postgres).await?)
}

async fn cancel_native_read(
    fixture: &McpReadFixture,
    token: &str,
    session_id: &str,
    request_id: &str,
) -> Result<()> {
    let response = mcp_tool_call_support::post_session_json(
        fixture.app(), "/v1/mcp", token, session_id,
        json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":request_id}}),
    ).await?;
    assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis; recording fake makes no provider requests"]
async fn native_image_reads_account_success_failure_and_unknown_usage() -> Result<()> {
    for outcome in [
        Outcome::Success,
        Outcome::Empty,
        Outcome::Failure,
        Outcome::Unknown,
        Outcome::Partial,
        Outcome::ZeroUsage,
        Outcome::FullyCached,
    ] {
        let mut fixture = Box::pin(McpReadFixture::create(Settings::from_env()?)).await?;
        let result: Result<()> = async {
            let gateway = configure_vision(&mut fixture, outcome).await?;
            let (document, revision) = image_document(&fixture).await?;
            let token = fixture.bearer_token(&["documents:read"], "irg315-authorized").await?;
            // An ordinary Markdown document never dispatches a provider.
            let (markdown, _) = fixture.create_source_document_state("irg315-markdown", "ready", Some("# Plain Markdown"), "text/markdown", None).await?;
            let response = fixture.mcp_tool_call(&token, "read_document", json!({"documentId":markdown})).await?;
            assert_eq!(response["result"]["isError"], false);
            assert_eq!(gateway.attempts.load(Ordering::SeqCst), 0);
            assert!(calls(&fixture).await?.is_empty());
            let foreign = catalog_repository::create_workspace(&fixture.state.persistence.postgres, &format!("irg315-foreign-{}", Uuid::now_v7().simple()), "Foreign workspace", None).await?;
            let foreign_token = iam_token_support::mint_api_token(&fixture.state.persistence.postgres, Some(foreign.id), "workspace", "irg315-foreign", &["documents:read"]).await?;
            let response = fixture.mcp_tool_call(&foreign_token.plaintext, "read_document", json!({"documentId":document})).await?;
            assert!(response.get("error").is_some() || response["result"]["isError"] == true);
            assert_eq!(gateway.attempts.load(Ordering::SeqCst), 0);
            assert!(calls(&fixture).await?.is_empty());
            // Scope and tenant denial must happen before admission or provider I/O.
            let denied = fixture.bearer_token(&["audit:read"], "irg315-denied").await?;
            let response = fixture.mcp_tool_call(&denied, "read_document", json!({"documentId":document})).await?;
            assert!(response.get("error").is_some() || response["result"]["isError"] == true);
            assert_eq!(gateway.attempts.load(Ordering::SeqCst), 0);
            assert!(calls(&fixture).await?.is_empty());
            let response = fixture.mcp_tool_call(&token, "read_document", json!({"documentId":document,"mode":"full"})).await?;
            assert_eq!(gateway.attempts.load(Ordering::SeqCst), 1, "native read response: {response}");
            assert_eq!(gateway.dispatches.load(Ordering::SeqCst), 1, "native image transport must not replay");
            assert_eq!(response["result"]["isError"], false);
            let payload = &response["result"]["structuredContent"];
            assert_eq!(payload["latestRevisionId"], json!(revision));
            assert_eq!(payload["mimeType"], "image/png");
            assert_eq!(payload["sourceUri"], "irg315-image.txt");
            assert_eq!(payload["content"], "canonical OCR text");
            assert_eq!(payload["sliceStartOffset"], 0);
            assert_eq!(payload["sliceEndOffset"], 18);
            assert_eq!(payload["totalContentLength"], 18);
            if matches!(outcome, Outcome::Failure | Outcome::Empty) { assert!(payload["visualDescription"].is_null()); }
            else { assert_eq!(payload["visualDescription"], "A separate visual description."); }
            let rows = calls(&fixture).await?;
            assert_eq!(rows.len(), 1, "one durable event per native image attempt");
            let call = &rows[0];
            assert_eq!(call.workspace_id, fixture.workspace_id);
            assert_eq!(call.library_id, fixture.library_id);
            assert_eq!(call.call_state, if matches!(outcome, Outcome::Failure) {"failed"} else {"completed"});
            let usage: Vec<(String, rust_decimal::Decimal)> = sqlx::query_as("select usage_kind::text, quantity from billing_usage where provider_call_id=$1 order by usage_kind")
                .bind(call.id).fetch_all(&fixture.state.persistence.postgres).await?;
            let expected_usage: Vec<(String, rust_decimal::Decimal)> = match outcome {
                Outcome::Success | Outcome::Empty => vec![("completion_tokens".to_owned(), 5.into()), ("prompt_tokens".to_owned(), 17.into())],
                Outcome::Partial => vec![("prompt_tokens".to_owned(), 17.into())],
                Outcome::ZeroUsage => vec![("completion_tokens".to_owned(), 0.into()), ("prompt_tokens".to_owned(), 0.into())],
                Outcome::FullyCached => vec![("cached_input_tokens".to_owned(), 17.into()), ("completion_tokens".to_owned(), 5.into()), ("prompt_tokens".to_owned(), 0.into())],
                _ => Vec::new(),
            };
            assert_eq!(usage, expected_usage, "exact returned dimensions must survive; absent usage is not zero");
            let cost = fixture.state.application_services.billing.roll_up_execution_cost(&fixture.state, &call.owning_execution_kind, call.owning_execution_id).await?.context("durable cost coverage")?;
            assert_eq!(cost.provider_call_count, 1);
            let attributed_document: Uuid = sqlx::query_scalar("select knowledge_document_id from billing_execution_cost where owning_execution_id=$1")
                .bind(call.owning_execution_id).fetch_one(&fixture.state.persistence.postgres).await?;
            assert_eq!(attributed_document, document);
            let fully_known = matches!(outcome, Outcome::Success | Outcome::Empty | Outcome::ZeroUsage | Outcome::FullyCached);
            assert_eq!(cost.unpriced_provider_call_count, i32::from(!fully_known));
            let expected_cost = match outcome {
                Outcome::Success | Outcome::Empty => rust_decimal::Decimal::new(27, 6),
                Outcome::Partial => rust_decimal::Decimal::new(17, 6),
                Outcome::FullyCached => rust_decimal::Decimal::new(61, 6),
                _ => rust_decimal::Decimal::ZERO,
            };
            assert_eq!(cost.currency_code, "USD");
            assert_eq!(cost.total_cost, expected_cost);
            let charge_count: i64 = sqlx::query_scalar("select count(*) from billing_charge c join billing_usage u on u.id=c.usage_id where u.provider_call_id=$1")
                .bind(call.id).fetch_one(&fixture.state.persistence.postgres).await?;
            assert_eq!(charge_count, i64::try_from(expected_usage.len())?);
            Ok(())
        }.await;
        fixture.cleanup().await?;
        result?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis; recording fake makes no provider requests"]
async fn native_image_admission_failure_prevents_provider_io() -> Result<()> {
    let mut fixture = Box::pin(McpReadFixture::create(Settings::from_env()?)).await?;
    let result: Result<()> = async {
        let gateway = configure_vision(&mut fixture, Outcome::Success).await?;
        let (document, _) = image_document(&fixture).await?;
        let token = fixture.bearer_token(&["documents:read"], "irg315-admission-denial").await?;
        sqlx::raw_sql("create function irg315_deny_provider_admission() returns trigger language plpgsql as $$ begin raise exception 'IRG-315 synthetic admission denial'; end $$; create trigger irg315_deny_provider_admission before insert on billing_provider_call for each row execute function irg315_deny_provider_admission();")
            .execute(&fixture.state.persistence.postgres).await?;
        let response = fixture.mcp_tool_call(&token, "read_document", json!({"documentId":document})).await?;
        assert!(response.get("error").is_some() || response["result"]["isError"] == true);
        assert_eq!(gateway.attempts.load(Ordering::SeqCst), 0);
        assert!(calls(&fixture).await?.is_empty());
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis; recording fake makes no provider requests"]
async fn native_image_cancellation_retains_unknown_liability() -> Result<()> {
    let mut fixture = Box::pin(McpReadFixture::create(Settings::from_env()?)).await?;
    let result: Result<()> = async {
        let gateway = configure_vision(&mut fixture, Outcome::Blocked).await?;
        let (document, _) = image_document(&fixture).await?;
        let mut terminal = sqlx::postgres::PgListener::connect(&fixture.temp_database.database_url).await?;
        terminal.listen("irg315_call_terminal").await?;
        sqlx::raw_sql("create function irg315_notify_cancel() returns trigger language plpgsql as $$ begin perform pg_notify('irg315_call_terminal', new.id::text); return new; end $$; create trigger irg315_notify_cancel after update on billing_provider_call for each row when (new.call_state='canceled') execute function irg315_notify_cancel();")
            .execute(&fixture.state.persistence.postgres).await?;
        let token = fixture.bearer_token(&["documents:read"], "irg315-cancel").await?;
        let session = mcp_tool_call_support::initialize_session(fixture.app(), "/v1/mcp", &token, "init").await?;
        let mut read = Box::pin(mcp_tool_call_support::post_session_json(fixture.app(), "/v1/mcp", &token, &session.id,
            json!({"jsonrpc":"2.0", "id":"irg315-cancel", "method":"tools/call", "params":{"name":"read_document", "arguments":{"documentId":document}}})));
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            tokio::select! {
                result = &mut read => panic!("blocked provider returned: {result:?}"),
                () = gateway.entered.notified() => (),
            }
        }).await?;
        // Disconnecting HTTP does not cancel a durable MCP tool. Use the
        // protocol cancellation owner and drain its response instead.
        cancel_native_read(&fixture, &token, &session.id, "irg315-cancel").await?;
        let response = tokio::time::timeout(std::time::Duration::from_secs(20), &mut read).await??;
        // Cancellation closes the native reply channel; JSON dispatch maps
        // that terminal transport outcome to HTTP 202 (jsonrpc.rs).
        assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
        drop(read);
        // Await the committed transition, not an arbitrary delay or DB poll.
        let notification = tokio::time::timeout(std::time::Duration::from_secs(20), terminal.recv()).await??;
        let rows = calls(&fixture).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(notification.payload(), rows[0].id.to_string());
        assert_eq!(rows[0].call_state, "canceled");
        let cost = fixture.state.application_services.billing.roll_up_execution_cost(&fixture.state, &rows[0].owning_execution_kind, rows[0].owning_execution_id).await?.context("canceled cost coverage")?;
        assert_eq!(cost.provider_call_count, 1);
        assert_eq!(cost.unpriced_provider_call_count, 1);
        let quantities: i64 = sqlx::query_scalar("select count(*) from billing_usage").fetch_one(&fixture.state.persistence.postgres).await?;
        assert_eq!(quantities, 0);
        assert_eq!(gateway.attempts.load(Ordering::SeqCst), 1);
        mcp_tool_call_support::terminate_session(fixture.app(), "/v1/mcp", &token, &session.id).await?;
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires isolated postgres and redis; recording fake makes no provider requests"]
async fn native_image_cancellation_after_receipt_preserves_returned_usage() -> Result<()> {
    let mut fixture = Box::pin(McpReadFixture::create(Settings::from_env()?)).await?;
    let result: Result<()> = async {
        let gateway = configure_vision(&mut fixture, Outcome::Success).await?;
        let (document, _) = image_document(&fixture).await?;
        let token = fixture.bearer_token(&["documents:read"], "irg315-paid-receipt-cancel").await?;
        let mut terminal = sqlx::postgres::PgListener::connect(&fixture.temp_database.database_url).await?;
        terminal.listen("irg315_receipt_committed").await?;
        sqlx::raw_sql("create function irg315_hold_completion() returns trigger language plpgsql as $$ begin perform pg_advisory_xact_lock(315315); perform pg_notify('irg315_receipt_committed', new.id::text); return new; end $$; create trigger irg315_hold_completion before update on billing_provider_call for each row when (new.call_state='completed') execute function irg315_hold_completion();")
            .execute(&fixture.state.persistence.postgres).await?;
        let mut blocker = fixture.state.persistence.postgres.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(315315)").execute(&mut *blocker).await?;
        let session = mcp_tool_call_support::initialize_session(fixture.app(), "/v1/mcp", &token, "init").await?;
        let mut read = Box::pin(mcp_tool_call_support::post_session_json(fixture.app(), "/v1/mcp", &token, &session.id,
            json!({"jsonrpc":"2.0", "id":"irg315-receipt", "method":"tools/call", "params":{"name":"read_document", "arguments":{"documentId":document}}})));
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            tokio::select! {
                result = &mut read => panic!("blocked completion returned: {result:?}"),
                () = gateway.response_returned.notified() => (),
            }
        }).await?;
        cancel_native_read(&fixture, &token, &session.id, "irg315-receipt").await?;
        let response = tokio::time::timeout(std::time::Duration::from_secs(20), &mut read).await??;
        assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
        drop(read);
        blocker.commit().await?;
        let notification = tokio::time::timeout(std::time::Duration::from_secs(20), terminal.recv()).await??;
        let rows = calls(&fixture).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(notification.payload(), rows[0].id.to_string());
        assert_eq!(rows[0].call_state, "completed", "returned usage must never downgrade to canceled");
        let usage: Vec<(String, rust_decimal::Decimal)> = sqlx::query_as("select usage_kind::text, quantity from billing_usage where provider_call_id=$1 order by usage_kind")
            .bind(rows[0].id).fetch_all(&fixture.state.persistence.postgres).await?;
        assert_eq!(usage, vec![("completion_tokens".into(), 5.into()), ("prompt_tokens".into(), 17.into())]);
        assert_eq!(gateway.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(gateway.dispatches.load(Ordering::SeqCst), 1);
        mcp_tool_call_support::terminate_session(fixture.app(), "/v1/mcp", &token, &session.id).await?;
        Ok(())
    }.await;
    fixture.cleanup().await?;
    result
}
