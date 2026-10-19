use super::*;

async fn resolve_sample_graph_extraction(
    gateway: &FakeGateway,
    max_provider_attempts: usize,
) -> std::result::Result<ResolvedGraphExtraction, GraphExtractionFailureOutcome> {
    resolve_graph_extraction_with_gateway(
        gateway,
        GraphExtractionLoopRequest {
            extraction_recovery: &ExtractionRecoveryService,
            provider_failure_classification: &crate::services::ops::provider_failure::ProviderFailureClassificationService::default(),
            runtime_binding: &sample_runtime_binding(),
            request: &sample_request(),
            recovery_enabled: true,
            max_provider_attempts,
            provider_timeout_retry_limit: 1,
        },
        &CancellationToken::new(),
    )
    .await
}

#[test]
fn graph_terminal_failure_summary_does_not_persist_diagnostic_text() {
    let private_diagnostic = "graph-terminal-sentinel-secret-and-private-content";
    let outcome = make_graph_terminal_failure_outcome(GraphExtractionTaskFailure {
        code: "graph_extract_failed".to_string(),
        summary: private_diagnostic.to_string(),
    });

    let RuntimeTerminalOutcome::Failed { summary, .. } = outcome else {
        panic!("graph extraction failure must remain a failed terminal outcome");
    };

    assert_eq!(summary.summary_redacted.as_deref(), Some("graph_extract_failed"));
    assert!(!summary.summary_redacted.as_deref().unwrap_or_default().contains(private_diagnostic));
}

#[test]
fn graph_terminal_policy_failure_does_not_trust_untyped_task_summary() {
    let private_diagnostic = "forged-policy-sentinel-secret";
    let outcome = make_graph_terminal_failure_outcome(GraphExtractionTaskFailure {
        code: "runtime_policy_rejected".to_string(),
        summary: private_diagnostic.to_string(),
    });

    let RuntimeTerminalOutcome::Canceled { summary, .. } = outcome else {
        panic!("graph policy rejection must remain a canceled terminal outcome");
    };

    assert_eq!(summary.summary_redacted.as_deref(), Some("runtime_policy_rejected"));
    assert!(!summary.summary_redacted.as_deref().unwrap_or_default().contains(private_diagnostic));
}

#[tokio::test]
async fn graph_runtime_stage_failure_summary_does_not_persist_diagnostic_text() {
    let registry = RuntimeTaskRegistry::default().register_task::<GraphExtractTask>();
    let policy: Arc<dyn RuntimePolicy> =
        Arc::new(DefaultRuntimePolicy::new(2_000, DefaultRuntimePolicyRules::default()));
    let hooks: Arc<dyn RuntimeHooks> = Arc::new(NoopRuntimeHooks);
    let executor = RuntimeExecutor::new(registry, policy, hooks);
    let request = RuntimeTaskRequest::<GraphExtractTask>::new(
        GraphExtractTaskInput {
            library_id: uuid::Uuid::now_v7(),
            document_id: uuid::Uuid::now_v7(),
            chunk_id: uuid::Uuid::now_v7(),
            revision_id: None,
            normalized_text: "stage-sentinel-private-content".to_string(),
            technical_facts: Vec::new(),
        },
        RuntimeExecutionOwner::graph_extraction_attempt(uuid::Uuid::now_v7()),
    );
    let mut session = executor.seed_session(&request).await.expect("seed graph runtime session");
    let private_diagnostic = "graph-stage-sentinel-secret-and-private-content";
    let failure = GraphExtractionTaskFailure {
        code: "graph_extract_failed".to_string(),
        summary: private_diagnostic.to_string(),
    };

    record_graph_runtime_stage(
        &executor,
        &mut session,
        GraphRuntimeStageUpdate {
            stage_kind: RuntimeStageKind::ExtractGraph,
            stage_state: RuntimeStageState::Failed,
            deterministic: false,
            failure: Some(&failure),
            started_at: Some(chrono::Utc::now()),
        },
    );

    let stage = session.trace.stages.last().expect("failed graph stage record");
    assert_eq!(stage.failure_summary_redacted.as_deref(), Some("graph_extract_failed"));
    assert!(
        !stage.failure_summary_redacted.as_deref().unwrap_or_default().contains(private_diagnostic)
    );
}

#[tokio::test]
async fn retries_after_terminal_parse_failure_and_aggregates_usage() {
    let gateway = FakeGateway {
        responses: Mutex::new(vec![
            Ok(ChatResponse {
                provider_kind: "provider-alpha".to_string(),
                model_name: "alpha-chat-mini".to_string(),
                output_text: "this is not json".to_string(),
                usage_json: serde_json::json!({
                    "prompt_tokens": 11,
                    "completion_tokens": 4,
                    "total_tokens": 15,
                }),
            }),
            Ok(ChatResponse {
                provider_kind: "provider-alpha".to_string(),
                model_name: "alpha-chat-mini".to_string(),
                output_text: r#"{"entities":["Provider Alpha"],"relations":[]}"#.to_string(),
                usage_json: serde_json::json!({
                    "prompt_tokens": 7,
                    "completion_tokens": 3,
                    "total_tokens": 10,
                }),
            }),
        ]),
    };

    let resolved =
        resolve_sample_graph_extraction(&gateway, 2).await.expect("retry should recover");

    assert_eq!(resolved.recovery.provider_attempt_count, 2);
    assert_eq!(resolved.recovery.reask_count, 1);
    assert_eq!(resolved.usage_json.get("call_count").and_then(serde_json::Value::as_u64), Some(2));
    assert_eq!(
        resolved.usage_json.get("total_tokens").and_then(serde_json::Value::as_i64),
        Some(25)
    );
    let raw_output_json = build_raw_output_json(GraphExtractionRawOutput {
        output_text: &resolved.output_text,
        usage_json: resolved.usage_json.clone(),
        lifecycle: &resolved.lifecycle,
        recovery: &resolved.recovery,
        recovery_summary: &resolved.recovery_summary,
        usage_calls: &resolved.usage_calls,
        extraction_contract_hash: &graph_extraction_contract_hash(
            &sample_request(),
            &sample_runtime_binding(),
            GraphEntityTypeClassifierIdentity::Off,
            256 * 1024,
        ),
    });
    let provider_calls = raw_output_json
        .get("provider_calls")
        .and_then(serde_json::Value::as_array)
        .expect("provider calls are persisted");
    assert_eq!(provider_calls.len(), 2);
    assert!(
        provider_calls[0]
            .get("timing")
            .and_then(|value| value.get("elapsed_ms"))
            .and_then(serde_json::Value::as_i64)
            .is_some()
    );
}

#[tokio::test]
async fn retries_upstream_protocol_failures_as_transient_provider_errors() {
    let source = serde_json::Error::io(std::io::Error::other("opaque response"));
    let gateway = FakeGateway {
        responses: Mutex::new(vec![
            Err(anyhow::Error::new(ProviderCallError::json("opaque", source))
                .context("provider boundary")),
            Ok(ChatResponse {
                provider_kind: "provider-alpha".to_string(),
                model_name: "alpha-chat-mini".to_string(),
                output_text: r#"{"entities":["Provider Alpha"],"relations":[]}"#.to_string(),
                usage_json: serde_json::json!({
                    "prompt_tokens": 9,
                    "completion_tokens": 3,
                    "total_tokens": 12,
                }),
            }),
        ]),
    };

    let resolved = resolve_sample_graph_extraction(&gateway, 2)
        .await
        .expect("upstream protocol failure should retry");

    assert_eq!(resolved.recovery.provider_attempt_count, 2);
    assert_eq!(
        resolved.provider_failure.as_ref().map(|detail| detail.failure_class.clone()),
        Some(RuntimeProviderFailureClass::RecoveredAfterRetry)
    );
    assert_eq!(
        resolved.recovery_attempts.first().map(|attempt| attempt.trigger_reason.as_str()),
        Some("upstream_protocol_failure")
    );
}

#[tokio::test]
async fn retries_transient_upstream_rejections_as_provider_errors() {
    let gateway = FakeGateway {
        responses: Mutex::new(vec![
            Err(anyhow::Error::new(ProviderCallError::http_status(
                "provider-alpha",
                reqwest::StatusCode::SERVICE_UNAVAILABLE,
                &reqwest::header::HeaderMap::new(),
                "opaque",
            ))),
            Ok(ChatResponse {
                provider_kind: "provider-alpha".to_string(),
                model_name: "alpha-chat-mini".to_string(),
                output_text: r#"{"entities":["Provider Alpha"],"relations":[]}"#.to_string(),
                usage_json: serde_json::json!({
                    "prompt_tokens": 11,
                    "completion_tokens": 4,
                    "total_tokens": 15,
                }),
            }),
        ]),
    };

    let resolved = resolve_sample_graph_extraction(&gateway, 2)
        .await
        .expect("transient upstream rejection should retry");

    assert_eq!(resolved.recovery.provider_attempt_count, 2);
    assert_eq!(
        resolved.provider_failure.as_ref().map(|detail| detail.failure_class.clone()),
        Some(RuntimeProviderFailureClass::RecoveredAfterRetry)
    );
    assert_eq!(
        resolved.recovery_attempts.first().map(|attempt| attempt.trigger_reason.as_str()),
        Some("upstream_transient_rejection")
    );
}

#[tokio::test]
async fn misleading_untyped_provider_text_is_terminal_without_retry() {
    let gateway = FakeGateway {
        responses: Mutex::new(vec![
            Err(anyhow::anyhow!("provider request failed status=503 timeout connection reset")),
            Ok(ChatResponse {
                provider_kind: "provider-alpha".to_string(),
                model_name: "alpha-chat-mini".to_string(),
                output_text: r#"{"entities":["Provider Alpha"],"relations":[]}"#.to_string(),
                usage_json: serde_json::json!({}),
            }),
        ]),
    };

    let failure = resolve_sample_graph_extraction(&gateway, 2)
        .await
        .expect_err("untyped text must not trigger provider retry");

    assert!(failure.provider_failure.is_none());
    assert_eq!(gateway.responses.lock().expect("lock fake responses").len(), 1);
}

#[tokio::test]
async fn fails_after_retry_exhaustion_with_recovery_trace() {
    let gateway = FakeGateway {
        responses: Mutex::new(vec![
            Ok(ChatResponse {
                provider_kind: "provider-alpha".to_string(),
                model_name: "alpha-chat-mini".to_string(),
                output_text: "broken payload".to_string(),
                usage_json: serde_json::json!({ "prompt_tokens": 5 }),
            }),
            Ok(ChatResponse {
                provider_kind: "provider-alpha".to_string(),
                model_name: "alpha-chat-mini".to_string(),
                output_text: "still broken".to_string(),
                usage_json: serde_json::json!({ "prompt_tokens": 6 }),
            }),
        ]),
    };

    let failure = resolve_sample_graph_extraction(&gateway, 2)
        .await
        .expect_err("malformed output should fail after retry exhaustion");

    assert!(failure.error_message.contains("after 2 provider attempt(s)"));
    assert_eq!(
        failure.provider_failure.as_ref().map(|detail| detail.failure_class.clone()),
        Some(RuntimeProviderFailureClass::InvalidModelOutput)
    );
}

#[tokio::test]
async fn records_retry_after_on_a_graph_provider_rate_limit() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::RETRY_AFTER, "120".parse().expect("retry-after header value"));
    let gateway = FakeGateway {
        responses: Mutex::new(vec![Err(anyhow::Error::new(ProviderCallError::http_status(
            "provider-alpha",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            &headers,
            "opaque",
        )))]),
    };

    let failure = resolve_sample_graph_extraction(&gateway, 1)
        .await
        .expect_err("a 429 with no retry budget must be a terminal provider failure");

    let details =
        failure.provider_failure.as_ref().expect("the provider failure snapshot must be recorded");
    assert_eq!(details.failure_class, RuntimeProviderFailureClass::UpstreamRejection);
    assert_eq!(details.upstream_status.as_deref(), Some("429"));
    assert_eq!(
        details.retry_after_seconds,
        Some(120),
        "the advertised Retry-After must survive into the failure snapshot"
    );
}

#[test]
fn provider_usage_payload_keeps_provider_metadata() {
    let usage = build_provider_usage_json(
        "provider-alpha",
        "alpha-chat-mini",
        serde_json::json!({
            "prompt_tokens": 21,
            "completion_tokens": 9,
        }),
    );

    assert_eq!(
        usage.get("provider_kind").and_then(serde_json::Value::as_str),
        Some("provider-alpha")
    );
    assert_eq!(
        usage.get("model_name").and_then(serde_json::Value::as_str),
        Some("alpha-chat-mini")
    );
    assert_eq!(usage.get("prompt_tokens").and_then(serde_json::Value::as_i64), Some(21));
}
