use super::{
    ChatRequest, ChatRequestSeed, EmbeddingBatchRequest, EmbeddingRequest, ToolUseRequest,
    VisionRequest,
};

fn assert_runtime_secret_is_redacted(
    value: &(impl serde::Serialize + std::fmt::Debug),
    secret: &str,
) {
    let serialized = serde_json::to_value(value).expect("request should serialize safely");
    let debug = format!("{value:?}");

    assert!(serialized.get("api_key_override").is_none());
    assert!(!serialized.to_string().contains(secret));
    assert!(!debug.contains(secret));
    assert!(debug.contains("<redacted>"));
}

#[test]
fn internal_chat_request_never_serializes_provider_credentials() {
    let request = ChatRequest {
        provider_kind: "provider-alpha".to_string(),
        model_name: "model-alpha".to_string(),
        prompt: "synthetic prompt".to_string(),
        api_key_override: Some("serialization-regression-secret".to_string()),
        base_url_override: Some("https://example.com".to_string()),
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        response_format: None,
        extra_parameters_json: serde_json::json!({}),
    };

    assert_runtime_secret_is_redacted(&request, "serialization-regression-secret");

    let seed = ChatRequestSeed {
        provider_kind: "provider-alpha".to_string(),
        model_name: "model-alpha".to_string(),
        api_key_override: Some("seed-regression-secret".to_string()),
        base_url_override: Some("https://example.com".to_string()),
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
    };
    assert_runtime_secret_is_redacted(&seed, "seed-regression-secret");

    let tool = ToolUseRequest {
        provider_kind: "provider-alpha".to_string(),
        model_name: "model-alpha".to_string(),
        api_key_override: Some("tool-regression-secret".to_string()),
        base_url_override: Some("https://example.com".to_string()),
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        messages: Vec::new(),
        tools: Vec::new(),
        extra_parameters_json: serde_json::json!({}),
        require_tool_call: false,
    };
    assert_runtime_secret_is_redacted(&tool, "tool-regression-secret");

    let embedding = EmbeddingRequest {
        provider_kind: "provider-alpha".to_string(),
        model_name: "model-alpha".to_string(),
        input: "synthetic input".to_string(),
        api_key_override: Some("embedding-regression-secret".to_string()),
        base_url_override: Some("https://example.com".to_string()),
        extra_parameters_json: serde_json::json!({}),
    };
    assert_runtime_secret_is_redacted(&embedding, "embedding-regression-secret");

    let embedding_batch = EmbeddingBatchRequest {
        provider_kind: "provider-alpha".to_string(),
        model_name: "model-alpha".to_string(),
        inputs: vec!["synthetic input".to_string()],
        api_key_override: Some("batch-regression-secret".to_string()),
        base_url_override: Some("https://example.com".to_string()),
        extra_parameters_json: serde_json::json!({}),
    };
    assert_runtime_secret_is_redacted(&embedding_batch, "batch-regression-secret");

    let vision = VisionRequest {
        provider_kind: "provider-alpha".to_string(),
        model_name: "model-alpha".to_string(),
        prompt: "synthetic prompt".to_string(),
        image_bytes: vec![0],
        mime_type: "image/png".to_string(),
        api_key_override: Some("vision-regression-secret".to_string()),
        base_url_override: Some("https://example.com".to_string()),
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({}),
    };
    assert_runtime_secret_is_redacted(&vision, "vision-regression-secret");
}
