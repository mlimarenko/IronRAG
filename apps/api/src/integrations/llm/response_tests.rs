use super::{
    ChatCompletionStatus,
    openai_compatible::extract_message_content_text,
    response::{
        native_chat_response, openai_chat_response, parse_provider_json_body,
        parse_tool_use_response, provider_usage_json,
    },
};

#[test]
fn openai_chat_completion_status_uses_protocol_signals_and_retains_paid_usage() {
    for (finish_reason, refusal, expected) in [
        (serde_json::json!("stop"), serde_json::Value::Null, ChatCompletionStatus::Complete),
        (serde_json::json!("length"), serde_json::Value::Null, ChatCompletionStatus::Truncated),
        (
            serde_json::json!("content_filter"),
            serde_json::Value::Null,
            ChatCompletionStatus::Refused,
        ),
        (serde_json::json!("stop"), serde_json::json!("declined"), ChatCompletionStatus::Refused),
        (serde_json::Value::Null, serde_json::Value::Null, ChatCompletionStatus::Unknown),
        (
            serde_json::json!("future_reason"),
            serde_json::Value::Null,
            ChatCompletionStatus::Unknown,
        ),
    ] {
        let body = serde_json::json!({
            "choices": [{"finish_reason": finish_reason, "message": {"content": "{}", "refusal": refusal}}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 2},
            "service_tier": "flex"
        });
        let (text, usage, status) = openai_chat_response(&body);
        assert_eq!(text, "{}");
        assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
        assert_eq!(status, expected);
        assert_eq!(
            usage,
            serde_json::json!({"prompt_tokens": 7, "completion_tokens": 2, "service_tier": "flex"})
        );
    }
    let (text, usage, status) = openai_chat_response(&serde_json::json!({
        "choices": [{"message": {"content": "I refuse"}}], "usage": {"completion_tokens": 2}
    }));
    assert_eq!(text, "I refuse");
    assert_eq!(status, ChatCompletionStatus::Unknown);
    assert_eq!(usage["completion_tokens"], 2);
    let (text, usage, status) = openai_chat_response(&serde_json::json!({
        "choices": [{"finish_reason": "stop", "message": {
            "content": [{"type": "refusal", "refusal": "declined"}]
        }}], "usage": {"completion_tokens": 2}
    }));
    assert!(text.is_empty());
    assert_eq!(status, ChatCompletionStatus::Refused);
    assert_eq!(usage["completion_tokens"], 2);
    assert_eq!(openai_chat_response(&serde_json::json!({})).2, ChatCompletionStatus::Unknown);
}

#[test]
fn native_chat_completion_status_uses_supported_protocol_fields() {
    use crate::domains::provider_profiles::{
        ANTHROPIC_MESSAGES_RUNTIME_KIND, GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
    };

    for (reason, expected) in [
        (Some("end_turn"), ChatCompletionStatus::Complete),
        (Some("stop_sequence"), ChatCompletionStatus::Complete),
        (Some("max_tokens"), ChatCompletionStatus::Truncated),
        (Some("model_context_window_exceeded"), ChatCompletionStatus::Truncated),
        (Some("refusal"), ChatCompletionStatus::Refused),
        (Some("tool_use"), ChatCompletionStatus::Unknown),
        (Some("future_reason"), ChatCompletionStatus::Unknown),
        (None, ChatCompletionStatus::Unknown),
    ] {
        let (text, usage, status) = native_chat_response(
            ANTHROPIC_MESSAGES_RUNTIME_KIND,
            serde_json::json!({
                "content": [{"type": "text", "text": "{}"}], "stop_reason": reason,
                "usage": {"input_tokens": 7, "output_tokens": 2}
            }),
        )
        .expect("Anthropic response");
        assert_eq!(text, "{}");
        assert_eq!(status, expected);
        assert_eq!(usage, serde_json::json!({"input_tokens": 7, "output_tokens": 2}));
    }
    for (reason, expected) in [
        (Some("STOP"), ChatCompletionStatus::Complete),
        (Some("MAX_TOKENS"), ChatCompletionStatus::Truncated),
        (Some("SAFETY"), ChatCompletionStatus::Refused),
        (Some("RECITATION"), ChatCompletionStatus::Refused),
        (Some("BLOCKLIST"), ChatCompletionStatus::Refused),
        (Some("PROHIBITED_CONTENT"), ChatCompletionStatus::Refused),
        (Some("SPII"), ChatCompletionStatus::Refused),
        (Some("IMAGE_SAFETY"), ChatCompletionStatus::Refused),
        (Some("IMAGE_PROHIBITED_CONTENT"), ChatCompletionStatus::Refused),
        (Some("FINISH_REASON_UNSPECIFIED"), ChatCompletionStatus::Unknown),
        (Some("future_reason"), ChatCompletionStatus::Unknown),
        (None, ChatCompletionStatus::Unknown),
    ] {
        let (text, usage, status) = native_chat_response(
            GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
            serde_json::json!({
                "candidates": [{"content": {"parts": [{"text": "{}"}]}, "finishReason": reason}],
                "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 2}
            }),
        )
        .expect("Gemini response");
        assert_eq!(text, "{}");
        assert_eq!(status, expected);
        assert_eq!(usage, serde_json::json!({"promptTokenCount": 7, "candidatesTokenCount": 2}));
    }
    for (reason, expected) in [
        ("SAFETY", ChatCompletionStatus::Refused),
        ("OTHER", ChatCompletionStatus::Refused),
        ("BLOCKLIST", ChatCompletionStatus::Refused),
        ("PROHIBITED_CONTENT", ChatCompletionStatus::Refused),
        ("IMAGE_SAFETY", ChatCompletionStatus::Refused),
        ("BLOCK_REASON_UNSPECIFIED", ChatCompletionStatus::Unknown),
        ("future_reason", ChatCompletionStatus::Unknown),
    ] {
        let (text, usage, status) = native_chat_response(
            GEMINI_GENERATE_CONTENT_RUNTIME_KIND,
            serde_json::json!({
                "promptFeedback": {"blockReason": reason}, "usageMetadata": {"promptTokenCount": 7}
            }),
        )
        .expect("Gemini prompt-block response");
        assert!(text.is_empty());
        assert_eq!(status, expected);
        assert_eq!(usage["promptTokenCount"], 7);
    }
    assert!(native_chat_response("unsupported", serde_json::json!({})).is_err());
}

#[test]
fn extracts_plain_string_content() {
    let value = serde_json::json!("ok");
    assert_eq!(extract_message_content_text(&value), "ok");
}

#[test]
fn extracts_text_from_content_parts() {
    let value = serde_json::json!([
        {"type": "text", "text": "hello"},
        {"type": "text", "text": {"value": "world"}}
    ]);
    assert_eq!(extract_message_content_text(&value), "hello\nworld");
}

#[test]
fn parses_provider_json_from_utf8_bytes_without_charset_roundtrip() {
    let body = b"{\"value\":\"\xd0\xa1\xd1\x82\xd1\x80\xd0\xbe\xd0\xba\xd0\xb0\"}";
    let latin1_misdecoded = body.iter().map(|byte| char::from(*byte)).collect::<String>();
    assert!(latin1_misdecoded.contains('\u{00d0}'));

    let parsed =
        parse_provider_json_body(body, "provider-alpha", "chat").expect("body is UTF-8 JSON");

    assert_eq!(parsed["value"], "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}");
}

#[test]
fn parse_tool_use_response_returns_final_text_when_tool_calls_are_absent() {
    let body = serde_json::json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": "Use the grounded answer fallback."
            }
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 5}
    });

    let (output_text, tool_calls, finish_reason, usage_json, reasoning_content) =
        parse_tool_use_response(&body).expect("tool-call-less response should parse");

    assert_eq!(output_text, "Use the grounded answer fallback.");
    assert!(tool_calls.is_empty());
    assert_eq!(finish_reason.as_deref(), Some("stop"));
    assert_eq!(usage_json["completion_tokens"], 5);
    assert!(reasoning_content.is_none());
}

#[test]
fn real_tool_response_reasoning_roundtrips_without_becoming_answer_text() {
    let body = serde_json::json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "content": null,
                "reasoning_content": "private reasoning\nwith exact whitespace  ",
                "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "search_documents", "arguments": "{\"query\":\"value\"}"}
                }]
            }
        }]
    });
    let (text, calls, _, _, reasoning) = parse_tool_use_response(&body).unwrap();
    assert!(text.is_empty());
    let assistant = super::ChatMessage::assistant_with_reasoning_and_tool_calls(reasoning, calls);
    let messages =
        [assistant, super::ChatMessage::tool_result("call_1", "search_documents", "evidence")];
    let wire = serde_json::to_value(
        messages
            .iter()
            .map(super::openai_compatible::OpenAiCompatibleToolUseMessage::from)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(wire[0]["reasoning_content"], body["choices"][0]["message"]["reasoning_content"]);
    assert_eq!(wire[0]["tool_calls"], body["choices"][0]["message"]["tool_calls"]);
    assert!(wire[0].get("content").is_none());
    assert!(wire[1].get("reasoning_content").is_none());
    assert_eq!(wire[1]["content"], "evidence");

    let final_body = serde_json::json!({"choices": [{
        "finish_reason": "stop", "message": {
            "content": "Public answer [chunk:123]",
            "reasoning_content": "more private reasoning"
        }
    }]});
    let (answer, calls, _, _, reasoning) = parse_tool_use_response(&final_body).unwrap();
    assert_eq!(answer, "Public answer [chunk:123]");
    assert!(calls.is_empty());
    assert_eq!(reasoning.as_deref(), Some("more private reasoning"));
}

#[test]
fn usage_json_keeps_the_actual_service_tier_from_the_response() {
    let body = serde_json::json!({
        "usage": {"prompt_tokens": 10, "completion_tokens": 2},
        "service_tier": "flex"
    });

    let usage_json = provider_usage_json(&body);

    assert_eq!(usage_json["service_tier"], "flex");
    assert_eq!(usage_json["prompt_tokens"], 10);
    assert_eq!(usage_json["completion_tokens"], 2);
}

#[test]
fn tool_use_response_usage_keeps_the_actual_service_tier() {
    let body = serde_json::json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": "ok", "tool_calls": []}
        }],
        "usage": {"prompt_tokens": 3, "completion_tokens": 5},
        "service_tier": "flex"
    });

    let (_, _, _, usage_json, _) =
        parse_tool_use_response(&body).expect("service-tiered response should parse");

    assert_eq!(usage_json["service_tier"], "flex");
    assert_eq!(usage_json["prompt_tokens"], 3);
}
