use super::test_prelude::*;

#[test]
fn query_failure_mapping_uses_typed_code_instead_of_message_text() {
    let execution_id = Uuid::now_v7();
    let failure = QueryAnswerTaskFailure::new(
        "query_answer_failed",
        "query_binding_not_configured query_retrieval_unavailable",
    );

    assert!(matches!(
        map_query_execution_failure(&execution_id, "question", &failure),
        crate::interfaces::http::router_support::ApiError::Internal
    ));
}

#[test]
fn typed_binding_failure_maps_to_binding_code_despite_misleading_message() {
    let error = anyhow::Error::new(QueryServiceError::BindingNotConfigured {
        message: "query_provider_failed".to_string(),
    });

    assert_eq!(
        anyhow_failure_code(&error, "query_embedding_failed"),
        "query_binding_not_configured"
    );
}

#[test]
fn untyped_binding_words_do_not_change_failure_code() {
    let error = anyhow::anyhow!("query_binding_not_configured");

    assert_eq!(anyhow_failure_code(&error, "query_embedding_failed"), "query_embedding_failed");
}

#[test]
fn query_failure_mapping_does_not_expose_diagnostic_summary() {
    let execution_id = Uuid::now_v7();
    let private_diagnostic = "opaque-provider-diagnostic";
    let failure = QueryAnswerTaskFailure::new("query_provider_failed", private_diagnostic);

    let error = map_query_execution_failure(&execution_id, "private question", &failure);

    assert!(!error.to_string().contains(private_diagnostic));
    assert!(!error.to_string().contains("private question"));
}

#[test]
fn runtime_initialization_failure_does_not_expose_diagnostic_text() {
    let private_diagnostic = "runtime-initialization-sentinel-secret";
    let error = map_runtime_execution_error(
        Uuid::now_v7(),
        crate::agent_runtime::executor::RuntimeExecutionError::InvalidTaskSpec(
            private_diagnostic.to_string(),
        ),
    );

    assert!(!error.to_string().contains(private_diagnostic));
}

#[test]
fn query_terminal_failure_summary_does_not_persist_diagnostic_text() {
    let private_diagnostic = "terminal-sentinel-secret-and-private-query";
    let outcome = make_query_terminal_failure_outcome(QueryAnswerTaskFailure::new(
        "query_provider_failed",
        private_diagnostic,
    ));

    let RuntimeTerminalOutcome::Failed { summary, .. } = outcome else {
        panic!("provider failure must remain a failed terminal outcome");
    };

    assert_eq!(summary.summary_redacted.as_deref(), Some("query_provider_failed"));
    assert!(!summary.summary_redacted.as_deref().unwrap_or_default().contains(private_diagnostic));
}

#[test]
fn query_terminal_policy_failure_preserves_typed_redacted_summary() {
    let policy_summary = "operator-approved redacted policy explanation";
    let outcome = make_query_terminal_failure_outcome(QueryAnswerTaskFailure::new(
        "runtime_policy_rejected",
        policy_summary,
    ));

    let RuntimeTerminalOutcome::Canceled { summary, .. } = outcome else {
        panic!("policy rejection must remain a canceled terminal outcome");
    };

    assert_eq!(summary.summary_redacted.as_deref(), Some(policy_summary));
}

#[tokio::test]
async fn query_runtime_stage_failure_summary_does_not_persist_diagnostic_text() {
    let registry = RuntimeTaskRegistry::default().register_task::<QueryAnswerTask>();
    let policy: Arc<dyn RuntimePolicy> =
        Arc::new(DefaultRuntimePolicy::new(2_000, DefaultRuntimePolicyRules::default()));
    let hooks: Arc<dyn RuntimeHooks> = Arc::new(NoopRuntimeHooks);
    let executor = RuntimeExecutor::new(registry, policy, hooks);
    let request = RuntimeTaskRequest::<QueryAnswerTask>::new(
        QueryAnswerTaskInput {
            query_execution_id: Uuid::now_v7(),
            question: "stage-sentinel-private-query".to_string(),
            prompt_history_text: None,
            grounded_context_text: String::new(),
        },
        RuntimeExecutionOwner::query_execution(Uuid::now_v7()),
    );
    let mut session = executor.seed_session(&request).await.expect("seed runtime session");
    let private_diagnostic = "stage-sentinel-secret-and-private-query";
    let failure = QueryAnswerTaskFailure::new("query_provider_failed", private_diagnostic);

    record_query_runtime_stage(
        &executor,
        &mut session,
        RuntimeStageKind::Answer,
        false,
        Some(&failure),
        Some(chrono::Utc::now()),
    );

    let stage = session.trace.stages.last().expect("failed stage record");
    assert_eq!(stage.failure_summary_redacted.as_deref(), Some("query_provider_failed"));
    assert!(
        !stage.failure_summary_redacted.as_deref().unwrap_or_default().contains(private_diagnostic)
    );
}

#[test]
fn provider_failure_detail_survives_to_the_redacted_summary() {
    let headers = reqwest::header::HeaderMap::new();
    let body = r#"{"error":{"message":"secret-body-diagnostic","code":"insufficient_quota"}}"#;
    let error = anyhow::Error::new(crate::integrations::retry::provider_http_status_error(
        "openai",
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        &headers,
        body,
    ));

    let failure = map_runtime_answer_query_failure(
        RuntimeAnswerQueryFailure { error, provider_calls: Vec::new() },
        "query_answer_failed",
    );
    assert_eq!(
        failure.detail_redacted.as_deref(),
        Some("provider=openai http_status=429 quota_exhausted")
    );

    let outcome = make_query_terminal_failure_outcome(failure);
    let RuntimeTerminalOutcome::Failed { summary, .. } = outcome else {
        panic!("provider failure must be terminal-failed");
    };
    let summary_redacted = summary.summary_redacted.expect("redacted summary");
    assert_eq!(
        summary_redacted,
        "query_provider_failed: provider=openai http_status=429 quota_exhausted"
    );
    assert!(!summary_redacted.contains("secret-body-diagnostic"));
}

#[test]
fn provider_detail_survives_the_service_enum_flattening() {
    let headers = reqwest::header::HeaderMap::new();
    let body = r#"{"error":{"code":"insufficient_quota","message":"secret"}}"#;
    let provider_error = crate::integrations::retry::provider_http_status_error(
        "openai",
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        &headers,
        body,
    );
    let error = anyhow::Error::new(QueryServiceError::from(provider_error));

    assert_eq!(
        anyhow_failure_code(&error, "query_answer_failed"),
        "query_provider_quota_exhausted"
    );
    assert_eq!(
        redacted_failure_detail(&error).as_deref(),
        Some("provider=openai http_status=429 quota_exhausted")
    );
}

#[test]
fn redacted_failure_detail_descends_into_internal_wrappers() {
    let inner = anyhow::Error::new(crate::integrations::retry::ProviderCallError::protocol(
        "private-protocol-diagnostic",
    ));
    let error = anyhow::Error::new(QueryServiceError::Internal(inner));

    assert_eq!(redacted_failure_detail(&error).as_deref(), Some("protocol"));
}

#[test]
fn redacted_failure_detail_uses_service_unavailable_kind_token() {
    let error = anyhow::Error::new(ApiError::service_unavailable(
        "private-tool-diagnostic",
        "invalid_mcp_tool_call",
    ));

    assert_eq!(redacted_failure_detail(&error).as_deref(), Some("invalid_mcp_tool_call"));
}

#[test]
fn untyped_failures_keep_the_bare_code_summary() {
    let failure = map_runtime_answer_query_failure(
        RuntimeAnswerQueryFailure {
            error: anyhow::anyhow!("private free-form diagnostic"),
            provider_calls: Vec::new(),
        },
        "query_answer_failed",
    );
    assert_eq!(failure.detail_redacted, None);

    let outcome = make_query_terminal_failure_outcome(failure);
    let RuntimeTerminalOutcome::Failed { summary, .. } = outcome else {
        panic!("untyped failure must be terminal-failed");
    };
    assert_eq!(summary.summary_redacted.as_deref(), Some("query_answer_failed"));
}

#[test]
fn query_failure_mapping_preserves_explicit_binding_code() {
    let execution_id = Uuid::now_v7();
    let failure = QueryAnswerTaskFailure::new("query_binding_not_configured", "opaque");

    assert!(matches!(
        map_query_execution_failure(&execution_id, "question", &failure),
        crate::interfaces::http::router_support::ApiError::ServiceUnavailable {
            kind: "query_binding_not_configured",
            ..
        }
    ));
}

#[test]
fn query_failure_mapping_preserves_explicit_retrieval_code() {
    let execution_id = Uuid::now_v7();
    let failure = QueryAnswerTaskFailure::new("query_retrieval_unavailable", "opaque");

    assert!(matches!(
        map_query_execution_failure(&execution_id, "question", &failure),
        crate::interfaces::http::router_support::ApiError::ServiceUnavailable {
            kind: "query_retrieval_unavailable",
            ..
        }
    ));
}

#[test]
fn query_failure_mapping_honors_explicit_deadline_code() {
    let execution_id = Uuid::now_v7();
    let failure = QueryAnswerTaskFailure::new("query_deadline_exceeded", "opaque");

    assert!(matches!(
        map_query_execution_failure(&execution_id, "question", &failure),
        crate::interfaces::http::router_support::ApiError::GatewayTimeout { .. }
    ));
}

fn provider_call(
    binding_id: Uuid,
    binding_purpose: AiBindingPurpose,
    provider_kind: &str,
    model_name: &str,
    call_kind: QueryProviderCallKind,
    usage_json: serde_json::Value,
) -> QueryProviderCall {
    QueryProviderCallAttribution::try_new(
        binding_id,
        binding_purpose,
        crate::domains::provider_profiles::ProviderModelSelection {
            provider_kind: provider_kind.to_string(),
            model_name: model_name.to_string(),
        },
        call_kind,
    )
    .expect("test provider attribution must be valid")
    .record(Uuid::now_v7(), usage_json)
}

#[test]
fn provider_call_keeps_the_preallocated_billing_event_id() {
    let provider_call = provider_call(
        Uuid::now_v7(),
        AiBindingPurpose::Agent,
        "agent-provider",
        "agent-model",
        QueryProviderCallKind::QueryAgent,
        serde_json::json!({"output_tokens": 5}),
    );

    assert_ne!(provider_call.provider_call_id(), Uuid::nil());
    assert_eq!(provider_call.clone().provider_call_id(), provider_call.provider_call_id());
}

#[test]
fn every_terminal_outcome_preserves_the_precompleted_provider_event_identity() {
    let provider_call = provider_call(
        Uuid::now_v7(),
        AiBindingPurpose::QueryAnswer,
        "answer-provider",
        "answer-model",
        QueryProviderCallKind::QueryAnswer,
        serde_json::json!({"input_tokens": 3}),
    );
    let provider_call_id = provider_call.provider_call_id();
    let completed = RuntimeTerminalOutcome::Completed {
        success: QueryAnswerTaskSuccess {
            answer_text: "completed".to_string(),
            provider_calls: vec![provider_call.clone()],
        },
    };
    let recovered = RuntimeTerminalOutcome::Recovered {
        success: QueryAnswerTaskSuccess {
            answer_text: "recovered".to_string(),
            provider_calls: vec![provider_call.clone()],
        },
        recovery: RuntimeRecoveryOutcome { attempts: 1, summary_redacted: None },
    };
    let summary = RuntimeFailureSummary {
        code: "neutral_failure".to_string(),
        summary_redacted: Some("neutral_failure".to_string()),
    };
    let failed = RuntimeTerminalOutcome::Failed {
        failure: QueryAnswerTaskFailure::new("neutral_failure", "opaque")
            .with_provider_calls(vec![provider_call.clone()]),
        summary: summary.clone(),
    };
    let canceled = RuntimeTerminalOutcome::Canceled {
        failure: QueryAnswerTaskFailure::new("runtime_policy_rejected", "redacted")
            .with_provider_calls(vec![provider_call]),
        summary,
    };

    for outcome in [completed, recovered, failed, canceled] {
        let calls = match outcome {
            RuntimeTerminalOutcome::Completed { success }
            | RuntimeTerminalOutcome::Recovered { success, .. } => success.provider_calls,
            RuntimeTerminalOutcome::Failed { failure, .. }
            | RuntimeTerminalOutcome::Canceled { failure, .. } => failure.provider_calls,
        };
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].provider_call_id(), provider_call_id);
    }
}

#[test]
fn ordinary_answer_failure_preserves_completed_provider_responses() {
    let provider_call = provider_call(
        Uuid::now_v7(),
        AiBindingPurpose::QueryAnswer,
        "answer-provider",
        "answer-model",
        QueryProviderCallKind::QueryAnswer,
        serde_json::json!({"output_tokens": 5}),
    );
    let failure = crate::services::query::execution::RuntimeAnswerQueryFailure {
        error: anyhow::anyhow!("post-response verification failed"),
        provider_calls: vec![provider_call.clone()],
    };

    let mapped = map_runtime_answer_query_failure(failure, "query_answer_failed");

    assert_eq!(mapped.code, "query_answer_failed");
    assert_eq!(mapped.provider_calls, vec![provider_call]);
}

#[test]
fn persisted_typed_outcome_is_independent_of_heavy_reference_hydration() {
    let candidate_document_id = Uuid::now_v7();
    let summary = serde_json::json!({
        "answerDisposition": "clarification",
        "answerClarification": {
            "required": true,
            "question": "Which neutral variant?",
            "answerCandidates": [{
                "label": "Variant A",
                "kind": "document",
                "confidence": 0.75,
                "provenance": {
                    "entityId": null,
                    "documentId": candidate_document_id,
                    "chunkId": null
                }
            }]
        }
    });

    // Reference hydration may time out or fail independently. The answer
    // outcome is read from the lightweight persisted bundle header.
    let (disposition, clarification) =
        persisted_execution_answer_outcome(Uuid::now_v7(), Some(&summary));

    assert_eq!(disposition, QueryAnswerDisposition::Clarification);
    assert!(clarification.required);
    assert_eq!(clarification.question.as_deref(), Some("Which neutral variant?"));
    assert_eq!(clarification.answer_candidates.len(), 1);
    assert_eq!(
        clarification.answer_candidates[0].provenance.document_id,
        Some(candidate_document_id)
    );
}

#[test]
fn missing_or_corrupt_persisted_typed_outcome_fails_closed_to_non_terminal() {
    let missing_clarification = serde_json::json!({
        "answerDisposition": "clarification"
    });
    let contradictory_clarification = serde_json::json!({
        "answerDisposition": "factual_ready",
        "answerClarification": null
    });
    for summary in [None, Some(&missing_clarification), Some(&contradictory_clarification)] {
        assert_eq!(
            persisted_execution_answer_outcome(Uuid::now_v7(), summary),
            (QueryAnswerDisposition::NonTerminal, QueryClarification::default()),
        );
    }
}

#[test]
fn agent_loop_unclassified_internal_failure_stays_the_generic_fallback() {
    let opaque = QueryServiceError::Internal(anyhow::anyhow!("provider socket reset"));

    let code = query_service_failure_code(&opaque, "query_agent_loop_failed");

    assert_eq!(code, "query_agent_loop_failed");
    assert_eq!(query_execution_api_failure(code), QueryExecutionApiFailure::Internal);
}
