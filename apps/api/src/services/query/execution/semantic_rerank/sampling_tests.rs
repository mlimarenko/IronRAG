use super::*;

fn binding(
    temperature: Option<f64>,
    top_p: Option<f64>,
) -> crate::services::ai_catalog_service::ResolvedRuntimeBinding {
    crate::services::ai_catalog_service::ResolvedRuntimeBinding {
        binding_id: Uuid::now_v7(),
        workspace_id: Uuid::nil(),
        library_id: Uuid::nil(),
        binding_purpose: AiBindingPurpose::QueryCompile,
        provider_catalog_id: Uuid::now_v7(),
        provider_kind: "openai".to_string(),
        provider_base_url: None,
        provider_api_style: "openai".to_string(),
        account_id: Uuid::now_v7(),
        api_key: Some("test-key".to_string()),
        model_catalog_id: Uuid::now_v7(),
        model_name: "gpt-5.6-luna".to_string(),
        effective_embedding_dimensions: None,
        system_prompt: None,
        temperature,
        top_p,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
    }
}

/// A binding whose merged provider profile declares the typed reasoning
/// policy. This is the shape `merge_provider_runtime_profile` writes for every
/// resolved binding.
fn binding_with_provider_reasoning_policy(
    reasoning: &str,
    temperature: Option<f64>,
    top_p: Option<f64>,
) -> crate::services::ai_catalog_service::ResolvedRuntimeBinding {
    let mut binding = binding(temperature, top_p);
    binding.extra_parameters_json = serde_json::json!({
        "_providerProfile": { "requestPolicy": { "reasoning": reasoning } },
    });
    binding
}

fn prepared_request() -> PreparedSemanticRerankRequest {
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
        SemanticRerankPolicy::bounded(500, 1, 100, 500),
    )
    .expect("candidate evidence should produce a request")
}

#[test]
fn semantic_request_preserves_binding_sampling_configuration() {
    let prepared = prepared_request();
    for (temperature, top_p) in [(None, None), (Some(0.35), Some(0.8))] {
        let binding = binding(temperature, top_p);
        let request = build_semantic_chat_request(&binding, &prepared);

        assert_eq!(request.temperature, binding.temperature);
        assert_eq!(request.top_p, binding.top_p);
        assert_eq!(request.system_prompt.as_deref(), Some(SEMANTIC_RERANK_SYSTEM_PROMPT));
        assert!(request.response_format.is_some());
    }
}

#[test]
fn semantic_request_disables_reasoning_from_the_typed_provider_policy() {
    let prepared = prepared_request();
    let mut binding = binding_with_provider_reasoning_policy("disable_with_none", None, None);
    binding.extra_parameters_json["reasoning_effort"] = serde_json::json!("high");
    binding.extra_parameters_json["provider_hint"] = serde_json::json!("keep");
    let request = build_semantic_chat_request(&binding, &prepared);

    assert_eq!(
        request.extra_parameters_json.get("reasoning_effort"),
        Some(&serde_json::json!("none")),
        "the answer-only rerank budget requires reasoning to stay off"
    );
    assert_eq!(
        request.extra_parameters_json.get("provider_hint"),
        Some(&serde_json::json!("keep")),
        "the reasoning override must not drop unrelated binding parameters"
    );
    assert_eq!(
        request.max_output_tokens_override,
        Some(semantic_rerank_max_output_tokens(1, true))
    );
}

#[test]
fn semantic_request_forwards_reasoning_when_the_typed_policy_forwards_it() {
    // The provider identity is the same as the disabling case: only the typed
    // policy differs, so the decision must not come from a provider-name table.
    let prepared = prepared_request();
    let mut binding = binding_with_provider_reasoning_policy("forward", None, None);
    binding.extra_parameters_json["reasoning_effort"] = serde_json::json!("high");
    let request = build_semantic_chat_request(&binding, &prepared);

    assert_eq!(
        request.extra_parameters_json.get("reasoning_effort"),
        Some(&serde_json::json!("high")),
        "a forwarding policy keeps the binding's own reasoning parameters"
    );
    assert_eq!(
        request.max_output_tokens_override,
        Some(semantic_rerank_max_output_tokens(1, false))
    );
}

#[test]
fn semantic_request_applies_the_typed_policy_for_any_provider_identity() {
    // An arbitrary non-OpenAI provider whose typed policy requires the control
    // must still get it: nothing here routes on the provider or model name.
    let prepared = prepared_request();
    let mut binding = binding_with_provider_reasoning_policy("disable_with_none", None, None);
    binding.provider_kind = "provider-nebula".to_string();
    let request = build_semantic_chat_request(&binding, &prepared);

    assert_eq!(
        request.extra_parameters_json.get("reasoning_effort"),
        Some(&serde_json::json!("none")),
        "the typed provider policy is the only reasoning-control authority"
    );
    assert_eq!(
        request.max_output_tokens_override,
        Some(semantic_rerank_max_output_tokens(1, true))
    );
}

#[test]
fn semantic_request_reserves_the_answer_share_when_reasoning_stays_possible() {
    assert_eq!(semantic_rerank_max_output_tokens(1, true), 88);
    assert_eq!(semantic_rerank_max_output_tokens(1, false), 176);
    assert_eq!(semantic_rerank_max_output_tokens(16, true), 448);
    assert_eq!(semantic_rerank_max_output_tokens(16, false), 896);
    assert_eq!(semantic_rerank_max_output_tokens(32, false), 1_664);
    assert_eq!(semantic_rerank_max_output_tokens(usize::MAX, true), 2_048);
    assert_eq!(semantic_rerank_max_output_tokens(usize::MAX, false), 2_048);
}

#[test]
fn semantic_request_tolerates_non_object_binding_parameters() {
    let prepared = prepared_request();
    let mut binding = binding(None, None);
    binding.extra_parameters_json = serde_json::Value::Null;
    let request = build_semantic_chat_request(&binding, &prepared);

    assert!(
        request.extra_parameters_json.get("reasoning_effort").is_none(),
        "a binding with no typed provider policy must not invent a reasoning override"
    );
    assert_eq!(
        request.max_output_tokens_override,
        Some(semantic_rerank_max_output_tokens(1, false)),
        "an unknown provider policy keeps the doubled budget"
    );
}
