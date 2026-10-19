use super::streaming::{
    BoundedSseBuffer, ProviderStreamLimits, ToolUseStreamState,
    consume_openai_compatible_stream_frame, consume_tool_use_stream_frame,
};

#[test]
fn consumes_stream_delta_frames() {
    let mut output_text = String::new();
    let mut usage_json = serde_json::json!({});
    let mut emitted = String::new();
    let done = consume_openai_compatible_stream_frame(
        r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#,
        &mut output_text,
        &mut usage_json,
        &mut |delta| emitted.push_str(&delta),
    )
    .expect("stream frame should parse");
    assert!(!done);
    assert_eq!(output_text, "Hello");
    assert_eq!(emitted, "Hello");
    assert_eq!(usage_json, serde_json::json!({}));
}

#[test]
fn marks_done_for_done_frame() {
    let mut output_text = String::new();
    let mut usage_json = serde_json::json!({});
    let done = consume_openai_compatible_stream_frame(
        "data: [DONE]",
        &mut output_text,
        &mut usage_json,
        &mut |_delta| {},
    )
    .expect("done frame should parse");
    assert!(done);
}

#[test]
fn streamed_usage_keeps_the_actual_service_tier_across_frames() {
    let mut output_text = String::new();
    let mut usage_json = serde_json::json!({});

    // The tier may arrive on an early delta frame before any usage counters.
    consume_openai_compatible_stream_frame(
        r#"data: {"service_tier":"flex","choices":[{"delta":{"content":"Hi"}}]}"#,
        &mut output_text,
        &mut usage_json,
        &mut |_delta| {},
    )
    .expect("tier frame should parse");
    // The final frame carries the usage counters and repeats the tier.
    consume_openai_compatible_stream_frame(
        r#"data: {"service_tier":"flex","choices":[],"usage":{"prompt_tokens":7,"completion_tokens":2}}"#,
        &mut output_text,
        &mut usage_json,
        &mut |_delta| {},
    )
    .expect("usage frame should parse");

    assert_eq!(usage_json["service_tier"], "flex");
    assert_eq!(usage_json["prompt_tokens"], 7);
    assert_eq!(usage_json["completion_tokens"], 2);
}

#[test]
fn tool_use_stream_usage_keeps_the_actual_service_tier() {
    let mut state = ToolUseStreamState::default();

    consume_tool_use_stream_frame(
        r#"data: {"service_tier":"flex","choices":[],"usage":{"prompt_tokens":4,"completion_tokens":1}}"#,
        &mut state,
        &mut |_delta| {},
    )
    .expect("tool-use usage frame should parse");

    assert_eq!(state.usage_json["service_tier"], "flex");
    assert_eq!(state.usage_json["prompt_tokens"], 4);
}

#[test]
fn rejects_a_single_oversized_sse_frame_before_json_parsing() {
    let limits = ProviderStreamLimits { max_total_bytes: 64, max_frame_bytes: 16 };
    let mut buffer = BoundedSseBuffer::new(limits);

    let error = buffer
        .push(b"data: 12345678901234567")
        .expect_err("unterminated oversized frame must be rejected");

    assert!(error.to_string().contains("frame"));
}

#[test]
fn rejects_stream_bytes_above_the_total_cap_even_across_small_frames() {
    let limits = ProviderStreamLimits { max_total_bytes: 24, max_frame_bytes: 16 };
    let mut buffer = BoundedSseBuffer::new(limits);
    buffer.push(b"data: 1\n\n").expect("first frame fits");
    buffer.push(b"data: 2\n\n").expect("second frame fits");

    let error = buffer.push(b"data: 3\n\n").expect_err("cumulative stream cap must be enforced");

    assert!(error.to_string().contains("stream"));
}
