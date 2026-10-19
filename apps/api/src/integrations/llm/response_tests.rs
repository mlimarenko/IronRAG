use super::{
    openai_compatible::extract_message_content_text,
    response::{parse_provider_json_body, parse_tool_use_response, provider_usage_json},
};

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
