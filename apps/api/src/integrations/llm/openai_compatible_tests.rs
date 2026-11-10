use crate::domains::provider_profiles::{
    ProviderAuthScheme, ProviderStructuredOutputMode, ProviderTokenLimitParameter,
};

use super::{
    ChatToolDef, UnifiedGateway,
    openai_compatible::{
        OpenAiCompatibleMessage, OpenAiCompatibleMessageContent, OpenAiCompatibleRequest,
        OpenAiCompatibleToolDef, OpenAiCompatibleToolUseChatRequest,
    },
    request::{provider_response_format, provider_system_prompt},
};

fn openai_test_request(extra_parameters_json: &serde_json::Value) -> OpenAiCompatibleRequest<'_> {
    OpenAiCompatibleRequest {
        provider_kind: "openai",
        api_key: Some("test"),
        base_url: "https://api.openai.com/v1",
        auth_scheme: ProviderAuthScheme::Bearer,
        chat_path: "/chat/completions".to_string(),
        model_name: "gpt-5.4-mini",
        messages: vec![OpenAiCompatibleMessage {
            role: "user".to_string(),
            content: OpenAiCompatibleMessageContent::Text("hello".to_string()),
        }],
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens: None,
        token_limit_parameter: ProviderTokenLimitParameter::MaxCompletionTokens,
        response_format: None,
        extra_parameters_json,
        stream: false,
        dispatch: None,
    }
}

#[test]
fn serializes_openai_compatible_chat_request_as_valid_json() {
    let extra_parameters_json = serde_json::json!({});
    let body =
        openai_test_request(&extra_parameters_json).body().expect("request body should serialize");
    let value: serde_json::Value =
        serde_json::from_slice(&body).expect("serialized body should stay valid json");
    assert_eq!(value.get("model").and_then(serde_json::Value::as_str), Some("gpt-5.4-mini"));
    assert_eq!(
        value
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("content"))
            .and_then(serde_json::Value::as_str),
        Some("hello"),
    );
}

#[test]
fn financial_chat_service_tier_defaults_without_overriding_explicit_tier() {
    for (extra, expected) in
        [(serde_json::json!({}), "default"), (serde_json::json!({"service_tier": "flex"}), "flex")]
    {
        let body = openai_test_request(&extra).body().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["service_tier"], expected);
    }
}

#[test]
fn financial_chat_count_defaults_without_overriding_explicit_count() {
    for (extra, expected) in [(serde_json::json!({}), 1), (serde_json::json!({"n": 2}), 2)] {
        let body = openai_test_request(&extra).body().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["n"], expected);
    }
    assert!(openai_test_request(&serde_json::json!(null)).body().is_err());
}

#[test]
fn default_chat_parameters_is_idempotent_and_preserves_explicit_values() {
    let mut empty = serde_json::Map::new();
    super::openai_compatible::default_chat_parameters(&mut empty);
    assert_eq!(empty["service_tier"], "default");
    assert_eq!(empty["n"], 1);

    let mut explicit = serde_json::Map::from_iter([
        ("service_tier".to_string(), serde_json::json!("flex")),
        ("n".to_string(), serde_json::json!(2)),
    ]);
    super::openai_compatible::default_chat_parameters(&mut explicit);
    assert_eq!(explicit["service_tier"], "flex");
    assert_eq!(explicit["n"], 2);
}

#[test]
fn serializes_response_format_when_schema_is_requested() {
    let response_format = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "graph_extraction",
            "strict": true,
            "schema": {"type": "object"}
        }
    });
    let extra_parameters_json = serde_json::json!({});
    let mut request = openai_test_request(&extra_parameters_json);
    request.response_format = Some(&response_format);
    let body = request.body().expect("request body should serialize");
    let value: serde_json::Value =
        serde_json::from_slice(&body).expect("serialized body should stay valid json");
    assert_eq!(
        value
            .get("response_format")
            .and_then(|item| item.get("type"))
            .and_then(serde_json::Value::as_str),
        Some("json_schema"),
    );
}

#[test]
fn tool_use_request_omits_empty_tools_and_choice() {
    let payload = OpenAiCompatibleToolUseChatRequest {
        model: "provider-alpha-tool-model",
        messages: vec![],
        tools: vec![],
        temperature: None,
        top_p: None,
        max_completion_tokens: None,
        max_tokens: Some(16),
        tool_choice: None,
        stream: false,
        extra: serde_json::json!({}),
    };
    let value = serde_json::to_value(payload).expect("tool-use request should serialize to JSON");

    assert!(value.get("tools").is_none());
    assert!(value.get("tool_choice").is_none());
}

#[test]
fn tool_use_request_includes_tools_and_choice_when_present() {
    let def = ChatToolDef {
        name: "lookup".to_string(),
        description: "Lookup structured facts".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"}
            },
            "required": ["id"]
        }),
    };
    let payload = OpenAiCompatibleToolUseChatRequest {
        model: "provider-alpha-tool-model",
        messages: vec![],
        tools: vec![OpenAiCompatibleToolDef::from(&def)],
        temperature: None,
        top_p: None,
        max_completion_tokens: Some(16),
        max_tokens: None,
        tool_choice: Some("auto"),
        stream: false,
        extra: serde_json::json!({}),
    };
    let value = serde_json::to_value(payload).expect("tool-use request should serialize to JSON");

    assert_eq!(value.get("tools").and_then(serde_json::Value::as_array).map(Vec::len), Some(1));
    assert_eq!(value.get("tool_choice").and_then(serde_json::Value::as_str), Some("auto"));
}

#[test]
fn prompt_only_json_object_request_body_has_no_response_format() {
    let requested = serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "graph_extraction",
            "strict": true,
            "schema": {"type": "object"}
        }
    });
    let response_format = provider_response_format(
        "provider-alpha",
        Some(&requested),
        ProviderStructuredOutputMode::PromptOnlyJsonObject,
    )
    .expect("prompt-only response format should resolve");
    let system_prompt = provider_system_prompt(
        "provider-alpha",
        Some("Base prompt"),
        Some(&requested),
        ProviderStructuredOutputMode::PromptOnlyJsonObject,
    )
    .expect("prompt-only system prompt should resolve");
    let extra_parameters_json = serde_json::json!({});
    let mut request = openai_test_request(&extra_parameters_json);
    request.provider_kind = "provider-alpha";
    request.base_url = "https://example.invalid/v1";
    request.model_name = "provider-model";
    request.system_prompt = system_prompt.as_deref();
    request.response_format = response_format.as_ref();
    let body = request.body().expect("request body should serialize");
    let value: serde_json::Value =
        serde_json::from_slice(&body).expect("serialized body should stay valid json");

    assert!(value.get("response_format").is_none());
    assert_eq!(
        value
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("content"))
            .and_then(serde_json::Value::as_str)
            .map(|text| text.contains("JSON Schema:")),
        Some(true),
    );
}

#[test]
fn serializes_openai_token_limit_as_max_completion_tokens() {
    let extra_parameters_json = serde_json::json!({});
    let mut request = openai_test_request(&extra_parameters_json);
    request.max_output_tokens = Some(16);
    let body = request.body().expect("request body should serialize");
    let value: serde_json::Value =
        serde_json::from_slice(&body).expect("serialized body should stay valid json");
    assert_eq!(value.get("max_completion_tokens").and_then(serde_json::Value::as_i64), Some(16),);
    assert!(value.get("max_tokens").is_none());
}

#[test]
fn serializes_bounded_vision_controls_for_openai() {
    let vision = super::VisionRequest {
        provider_kind: "openai".into(),
        model_name: "gpt-5.4-mini".into(),
        prompt: "fixture".into(),
        image_bytes: vec![],
        mime_type: "image/png".into(),
        api_key_override: None,
        base_url_override: None,
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: Some(128_000),
        extra_parameters_json: serde_json::json!({"n": 1, "service_tier": "flex"}),
        dispatch: None,
    };
    let extra = UnifiedGateway::upstream_extra_parameters(&vision.extra_parameters_json, None);
    let mut request = openai_test_request(&extra);
    request.max_output_tokens = vision.max_output_tokens_override;
    let body = request.body().expect("serialize vision controls");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid request JSON");
    assert_eq!(value["n"], 1);
    assert_eq!(value["service_tier"], "flex");
    assert_eq!(value["max_completion_tokens"], 128_000);
    assert!(value.get("max_tokens").is_none());
}

#[test]
fn serializes_non_openai_token_limit_as_max_tokens() {
    let extra_parameters_json = serde_json::json!({});
    let mut request = openai_test_request(&extra_parameters_json);
    request.provider_kind = "deepseek";
    request.base_url = "https://example.invalid/v1";
    request.model_name = "deepseek-chat";
    request.max_output_tokens = Some(16);
    request.token_limit_parameter = ProviderTokenLimitParameter::MaxTokens;
    let body = request.body().expect("request body should serialize");
    let value: serde_json::Value =
        serde_json::from_slice(&body).expect("serialized body should stay valid json");
    assert_eq!(value.get("max_tokens").and_then(serde_json::Value::as_i64), Some(16),);
    assert!(value.get("max_completion_tokens").is_none());
}

#[test]
fn keeps_reasoning_effort_extra_parameter_on_the_provider_body() {
    let extra_parameters_json = serde_json::json!({
        "reasoning_effort": "none",
        "_providerProfile": { "runtime": {} },
        "temperature": 0.3,
    });
    let extra = UnifiedGateway::upstream_extra_parameters(&extra_parameters_json, None);
    assert_eq!(extra.get("reasoning_effort"), Some(&serde_json::json!("none")));
    assert!(extra.get("_providerProfile").is_none());
    assert!(extra.get("temperature").is_none());

    let body = openai_test_request(&extra).body().expect("request body should serialize");
    let value: serde_json::Value =
        serde_json::from_slice(&body).expect("serialized body should stay valid json");
    assert_eq!(value.get("reasoning_effort").and_then(serde_json::Value::as_str), Some("none"));
}
