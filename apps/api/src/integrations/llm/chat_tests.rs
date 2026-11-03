use std::{
    net::{IpAddr, Ipv4Addr, UdpSocket},
    sync::Arc,
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use crate::{
    domains::provider_profiles::{
        ANTHROPIC_MESSAGES_RUNTIME_KIND, GEMINI_GENERATE_CONTENT_RUNTIME_KIND, ProviderAuthScheme,
        ProviderRuntimeProfile, ProviderStructuredOutputMode, ProviderTokenLimitParameter,
    },
    integrations::{
        provider_budget::{
            ProviderBudgetConfig, ProviderBudgetError, ProviderBudgetRegistry,
            ProviderBudgetRegistryOptions, ProviderIdentity, ProviderLane, with_lane,
        },
        retry::RetryPolicy,
    },
    shared::provider_http::{ProviderHttpTransport, ProviderHttpTransportConfig},
};

use super::{
    ChatRequest, LlmGateway, UnifiedGateway,
    chat::NativeChatCall,
    request::{NativeChatRequest, native_chat_request_body},
    response::native_chat_response,
};

#[tokio::test]
async fn semantic_verification_wire_controls_remain_machine_owned() -> anyhow::Result<()> {
    let gateway =
        test_gateway(ProviderBudgetConfig::unlimited(), ProviderBudgetRegistryOptions::default());
    crate::services::query::execution::semantic_verification::assert_hostile_controls_cannot_override_gateway_body(&gateway).await
}

async fn native_chat_test_server(
    response_body: &str,
) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    // Provider URLs that explicitly allow private networks intentionally rewrite
    // loopback to the Docker host gateway. Advertise this process's routable
    // interface instead, so the fixture stays inside the test process in both a
    // container executor and an ordinary host test run.
    let route_probe =
        UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind native chat route probe");
    route_probe.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).expect("resolve native chat test route");
    let advertised_host =
        match route_probe.local_addr().expect("native chat test route address").ip() {
            IpAddr::V4(address) if !address.is_unspecified() && !address.is_loopback() => address,
            address => panic!("native chat test route must be non-loopback IPv4, got {address}"),
        };
    let listener =
        TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.expect("bind native chat server");
    let address = listener.local_addr().expect("native chat server address");
    let response_body = response_body.to_string();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept native chat request");
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 2048];
            let read = socket.read(&mut chunk).await.expect("read native chat request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or_default();
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body,
        );
        socket.write_all(response.as_bytes()).await.expect("write native chat response");
        request
    });
    (format!("http://{advertised_host}:{}", address.port()), server)
}

fn local_openai_profile() -> serde_json::Value {
    serde_json::json!({
        "_providerProfile": {
            "runtime": {
                "kind": "openai_compatible",
                "authScheme": "bearer",
                "tokenLimitParameter": "max_tokens",
                "structuredOutput": "json_schema",
                "chatPath": "/chat/completions",
                "embeddingsPath": null,
                "modelsPath": "/models"
            },
            "baseUrl": {
                "allowOverride": true,
                "requireHttps": false,
                "allowPrivateNetwork": true,
                "trimSuffixes": []
            },
            "credentials": {
                "apiKeyRequired": false,
                "baseUrlRequired": true,
                "baseUrlMode": "required",
                "validationMode": "model_list"
            }
        }
    })
}

fn test_gateway(
    config: ProviderBudgetConfig,
    options: ProviderBudgetRegistryOptions,
) -> UnifiedGateway {
    UnifiedGateway {
        transport: Arc::new(
            ProviderHttpTransport::try_new(ProviderHttpTransportConfig::llm(Duration::from_secs(
                2,
            )))
            .expect("test provider transport"),
        ),
        provider_budget: Arc::new(
            ProviderBudgetRegistry::uniform(config, options).expect("test provider budget"),
        ),
        retry_policy: RetryPolicy::no_retry(),
    }
}

#[tokio::test]
async fn openai_stream_uses_exactly_one_concurrency_permit() {
    let (base_url, server) = native_chat_test_server(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
    )
    .await;
    let gateway = test_gateway(
        ProviderBudgetConfig {
            max_outbound: 1,
            query_reserved: 0,
            ..ProviderBudgetConfig::unlimited()
        },
        ProviderBudgetRegistryOptions::default(),
    );
    let request = ChatRequest {
        provider_kind: "local-openai".to_string(),
        model_name: "sample-model".to_string(),
        prompt: "prompt".to_string(),
        api_key_override: None,
        base_url_override: Some(base_url),
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        response_format: None,
        extra_parameters_json: local_openai_profile(),
    };
    let mut deltas = Vec::new();

    let response = tokio::time::timeout(Duration::from_secs(2), async {
        gateway.generate_stream(request, &mut |delta| deltas.push(delta)).await
    })
    .await
    .expect("a single-permit stream must not deadlock")
    .expect("stream should succeed");

    server.await.expect("stream server task");
    assert_eq!(response.output_text, "ok");
    assert_eq!(deltas, ["ok"]);
    assert_eq!(response.completion_status, super::ChatCompletionStatus::Unknown);
}

#[tokio::test]
async fn chat_gateway_preserves_paid_completion_outcomes_in_json_and_stream_responses() {
    use super::ChatCompletionStatus;

    let gateway =
        test_gateway(ProviderBudgetConfig::unlimited(), ProviderBudgetRegistryOptions::default());
    let usage = serde_json::json!({"prompt_tokens": 7, "completion_tokens": 2});
    let expected_usage =
        serde_json::json!({"prompt_tokens": 7, "completion_tokens": 2, "service_tier": "flex"});
    for (reason, refusal, expected_status) in [
        (Some("stop"), None, ChatCompletionStatus::Complete),
        (Some("length"), None, ChatCompletionStatus::Truncated),
        (Some("content_filter"), None, ChatCompletionStatus::Refused),
        (Some("stop"), Some("declined"), ChatCompletionStatus::Refused),
        (None, None, ChatCompletionStatus::Unknown),
        (Some("future_reason"), None, ChatCompletionStatus::Unknown),
    ] {
        let content =
            if refusal.is_some() { serde_json::Value::Null } else { serde_json::json!("{}") };
        let expected_text = if refusal.is_some() { "" } else { "{}" };
        let body = serde_json::json!({
            "choices": [{"message": {"content": content, "refusal": refusal}, "finish_reason": reason}],
            "usage": usage, "service_tier": "flex"
        });
        let (base_url, server) = native_chat_test_server(&body.to_string()).await;
        let response = gateway
            .generate(completion_test_request(base_url))
            .await
            .expect("paid response must return text, status and usage");
        server.await.expect("JSON server task");
        assert_eq!(response.output_text, expected_text);
        assert_eq!(response.completion_status, expected_status);
        assert_eq!(response.usage_json, expected_usage);

        let delta =
            serde_json::json!({"choices": [{"delta": {"content": content, "refusal": refusal}}]});
        let finish = serde_json::json!({"choices": [{"delta": {}, "finish_reason": reason}]});
        let counters = serde_json::json!({"choices": [], "usage": usage, "service_tier": "flex"});
        // The unknown future reason also exercises EOF without a DONE marker
        // and an unterminated final usage frame.
        let ending = if reason == Some("future_reason") { "" } else { "\n\ndata: [DONE]\n\n" };
        let stream = format!("data: {delta}\n\ndata: {finish}\n\ndata: {counters}{ending}");
        let (base_url, server) = native_chat_test_server(&stream).await;
        let mut emitted = String::new();
        let response = gateway
            .generate_stream(completion_test_request(base_url), &mut |delta| {
                emitted.push_str(&delta)
            })
            .await
            .expect("paid stream must retain completion outcome");
        server.await.expect("stream server task");
        assert_eq!(response.output_text, expected_text);
        assert_eq!(emitted, expected_text);
        assert_eq!(response.completion_status, expected_status);
        assert_eq!(response.usage_json, expected_usage);
    }
}

fn completion_test_request(base_url: String) -> ChatRequest {
    ChatRequest {
        provider_kind: "local-openai".to_string(),
        model_name: "sample-model".to_string(),
        prompt: "prompt".to_string(),
        api_key_override: None,
        base_url_override: Some(base_url),
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        response_format: None,
        extra_parameters_json: local_openai_profile(),
    }
}

#[tokio::test]
async fn rejected_concurrency_admission_refunds_the_rate_token() {
    let gateway = test_gateway(
        ProviderBudgetConfig {
            max_outbound: 1,
            query_reserved: 0,
            requests_per_second: 1,
            burst: 1,
        },
        ProviderBudgetRegistryOptions {
            acquire_timeout: Duration::from_millis(20),
            ..ProviderBudgetRegistryOptions::default()
        },
    );
    let provider_kind = "local-openai";
    let base_url = "http://localhost:12345";
    let identity = ProviderIdentity::new(provider_kind, base_url);
    let _held = gateway.provider_budget.acquire(&identity, ProviderLane::Ingest).await.unwrap();

    let error =
        with_lane(ProviderLane::Query, gateway.begin_provider_attempt(provider_kind, base_url))
            .await
            .expect_err("saturated query admission must time out");
    assert_eq!(
        error.admission_error(),
        Some(&ProviderBudgetError::AcquireTimeout { timeout_ms: 20 })
    );

    let token = tokio::time::timeout(
        Duration::from_millis(20),
        gateway.provider_budget.throttle(&identity),
    )
    .await
    .expect("rejected admission must return its consumed rate token")
    .expect("refunded token must be immediately available");
    // The refunded token is a real reservation: commit it rather than dropping
    // the must-use guard, which would refund it a second time.
    token.commit();
}

#[tokio::test]
async fn native_anthropic_chat_executes_the_bounded_http_path() {
    let (base_url, server) = native_chat_test_server(
        r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":2}}"#,
    )
    .await;
    let gateway = UnifiedGateway {
        transport: Arc::new(
            ProviderHttpTransport::try_new(ProviderHttpTransportConfig::llm(Duration::from_secs(
                2,
            )))
            .expect("native chat transport"),
        ),
        provider_budget: Arc::new(
            ProviderBudgetRegistry::uniform(
                ProviderBudgetConfig::unlimited(),
                ProviderBudgetRegistryOptions::default(),
            )
            .expect("test provider budget"),
        ),
        retry_policy: RetryPolicy::no_retry(),
    };
    let runtime = ProviderRuntimeProfile {
        kind: ANTHROPIC_MESSAGES_RUNTIME_KIND.to_string(),
        auth_scheme: ProviderAuthScheme::AnthropicApiKey,
        token_limit_parameter: ProviderTokenLimitParameter::MaxTokens,
        structured_output: ProviderStructuredOutputMode::Unsupported,
        chat_path: "/messages".to_string(),
        embeddings_path: None,
        models_path: Some("/models".to_string()),
        score_criteria: Vec::new(),
    };

    let (output, usage, _) = gateway
        .call_native_chat(NativeChatCall {
            provider_kind: "anthropic",
            runtime: &runtime,
            api_key: Some("synthetic-api-key"),
            base_url: &base_url,
            allow_private_network: true,
            model_name: "model-alpha",
            prompt: "prompt",
            system_prompt: Some("system"),
            temperature: Some(0.2),
            top_p: Some(0.9),
            max_output_tokens: Some(64),
            image: None,
        })
        .await
        .expect("native Anthropic chat call");
    let request = String::from_utf8(server.await.expect("native chat server task"))
        .expect("native chat request is utf-8");

    assert_eq!(output, "ok");
    assert_eq!(usage["input_tokens"], 2);
    assert!(request.to_ascii_lowercase().contains("anthropic-version: 2023-06-01"));
    assert!(request.contains("synthetic-api-key"));
    assert!(request.contains("\"model\":\"model-alpha\""));
}

#[test]
fn serializes_anthropic_native_request_and_parses_usage() {
    let request = native_chat_request_body(
        ANTHROPIC_MESSAGES_RUNTIME_KIND,
        NativeChatRequest {
            model_name: "model-alpha",
            prompt: "prompt",
            system_prompt: Some("system"),
            temperature: Some(0.2),
            top_p: Some(0.9),
            max_output_tokens: Some(64),
            image: None,
        },
    )
    .expect("Anthropic body should serialize");
    assert_eq!(request["model"], "model-alpha");
    assert_eq!(request["messages"][0]["role"], "user");
    assert_eq!(request["max_tokens"], 64);

    let (output, usage, completion_status) = native_chat_response(
        ANTHROPIC_MESSAGES_RUNTIME_KIND,
        serde_json::json!({"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":2}}),
    )
    .expect("Anthropic response should parse");
    assert_eq!(output, "ok");
    assert_eq!(usage["input_tokens"], 2);
    assert_eq!(completion_status, super::ChatCompletionStatus::Unknown);
}

#[test]
fn serializes_gemini_native_request_and_parses_usage() {
    let request = native_chat_request_body(
        GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
        NativeChatRequest {
            model_name: "models/model-beta",
            prompt: "prompt",
            system_prompt: Some("system"),
            temperature: Some(0.2),
            top_p: Some(0.9),
            max_output_tokens: Some(64),
            image: None,
        },
    )
    .expect("Gemini body should serialize");
    assert_eq!(request["contents"][0]["parts"][0]["text"], "prompt");
    assert_eq!(request["generationConfig"]["maxOutputTokens"], 64);

    let (output, usage, completion_status) = native_chat_response(
            GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
            serde_json::json!({"candidates":[{"content":{"parts":[{"text":"ok"}]}}],"usageMetadata":{"promptTokenCount":2}}),
        )
        .expect("Gemini response should parse");
    assert_eq!(output, "ok");
    assert_eq!(usage["promptTokenCount"], 2);
    assert_eq!(completion_status, super::ChatCompletionStatus::Unknown);
}
#[test]
fn serializes_native_vision_requests_with_provider_specific_image_fields() {
    let anthropic = native_chat_request_body(
        ANTHROPIC_MESSAGES_RUNTIME_KIND,
        NativeChatRequest {
            model_name: "model-alpha",
            prompt: "prompt",
            system_prompt: Some("  "),
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            image: Some((b"image", "image/png")),
        },
    )
    .expect("Anthropic vision body should serialize");
    assert_eq!(
        anthropic,
        serde_json::json!({
            "model": "model-alpha",
            "max_tokens": 1024,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "prompt"},
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": "aW1hZ2U="
                        }
                    }
                ]
            }]
        }),
    );

    let gemini = native_chat_request_body(
        GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
        NativeChatRequest {
            model_name: "models/model-beta",
            prompt: "prompt",
            system_prompt: Some("  "),
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            image: Some((b"image", "image/png")),
        },
    )
    .expect("Gemini vision body should serialize");
    assert_eq!(
        gemini,
        serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [
                    {"text": "prompt"},
                    {"inlineData": {"mimeType": "image/png", "data": "aW1hZ2U="}}
                ]
            }]
        }),
    );
}
