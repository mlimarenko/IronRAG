use super::*;

#[test]
fn missing_fixed_context_query_answer_binding_is_typed_configuration_failure() {
    assert!(matches!(
        missing_query_answer_binding_error(Uuid::nil()),
        QueryServiceError::BindingNotConfigured { message }
            if message.contains("query_answer")
    ));
}

#[test]
fn provider_call_record_captures_exact_resolved_binding_and_unmodified_usage() {
    let binding_id = Uuid::now_v7();
    let usage_json = serde_json::json!({});
    let binding = ResolvedRuntimeBinding {
        binding_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        binding_purpose: AiBindingPurpose::Agent,
        provider_catalog_id: Uuid::now_v7(),
        provider_kind: "provider-a".to_string(),
        provider_base_url: None,
        provider_api_style: "chat".to_string(),
        account_id: Uuid::now_v7(),
        api_key: None,
        model_catalog_id: Uuid::now_v7(),
        model_name: "model-a".to_string(),
        effective_embedding_dimensions: None,
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
    };

    let attribution = provider_call_attribution(&binding, QueryProviderCallKind::QueryAgent)
        .expect("valid agent attribution");
    let provider_call_id = Uuid::now_v7();
    let call = attribution.record(provider_call_id, usage_json.clone());

    assert_eq!(call.binding_id(), binding_id);
    assert_eq!(call.binding_purpose(), AiBindingPurpose::Agent);
    assert_eq!(call.provider().provider_kind, "provider-a");
    assert_eq!(call.provider().model_name, "model-a");
    assert_eq!(call.call_kind(), QueryProviderCallKind::QueryAgent);
    assert_eq!(call.usage_json(), &usage_json);
}

#[test]
fn provider_call_attribution_mismatch_fails_before_response_recording() {
    let binding = ResolvedRuntimeBinding {
        binding_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        binding_purpose: AiBindingPurpose::QueryAnswer,
        provider_catalog_id: Uuid::now_v7(),
        provider_kind: "provider-a".to_string(),
        provider_base_url: None,
        provider_api_style: "chat".to_string(),
        account_id: Uuid::now_v7(),
        api_key: None,
        model_catalog_id: Uuid::now_v7(),
        model_name: "model-a".to_string(),
        effective_embedding_dimensions: None,
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
    };

    let result = provider_call_attribution(&binding, QueryProviderCallKind::QueryAgent);

    assert!(matches!(result, Err(QueryServiceError::Internal(_))));
}

#[test]
fn paid_response_ledger_survives_a_later_loop_deadline() {
    let attribution = QueryProviderCallAttribution::try_new(
        Uuid::now_v7(),
        AiBindingPurpose::Agent,
        ProviderModelSelection {
            provider_kind: "provider-a".to_string(),
            model_name: "model-a".to_string(),
        },
        QueryProviderCallKind::QueryAgent,
    )
    .expect("valid agent attribution");
    let provider_call = attribution.record(Uuid::now_v7(), serde_json::json!({"output_tokens": 5}));

    let failure = AgentTurnFailure::with_loop(
        QueryServiceError::DeadlineExceeded,
        vec![provider_call.clone()],
        Vec::new(),
        agent_loop_metadata(2, Duration::from_secs(1), AgentStopReason::Deadline, 0),
    );

    assert_eq!(failure.provider_calls, vec![provider_call]);
}
