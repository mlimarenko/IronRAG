use anyhow::{Context, Result};
use std::time::Duration;

use super::extract_message_content_text;

pub(super) fn consume_openai_compatible_stream_frame(
    frame: &str,
    output_text: &mut String,
    usage_json: &mut serde_json::Value,
    on_delta: &mut (dyn FnMut(String) + Send),
) -> Result<bool> {
    if frame.trim().is_empty() || frame.starts_with(':') {
        return Ok(false);
    }

    let mut data_lines = Vec::new();
    for raw_line in frame.split('\n') {
        let line = raw_line.trim_end();
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start());
        }
    }

    if data_lines.is_empty() {
        return Ok(false);
    }

    let mut payload_text = String::new();
    for (index, line) in data_lines.iter().enumerate() {
        if index > 0 {
            payload_text.push('\n');
        }
        payload_text.push_str(line);
    }
    if payload_text.trim() == "[DONE]" {
        return Ok(true);
    }

    let payload: serde_json::Value = serde_json::from_str(&payload_text)
        .context("failed to parse upstream streaming payload as json")?;
    let delta = extract_stream_delta_text(&payload);
    if !delta.is_empty() {
        output_text.push_str(&delta);
        on_delta(delta);
    }
    if let Some(usage) = payload.get("usage").filter(|value| !value.is_null()) {
        *usage_json = usage.clone();
    }
    Ok(false)
}

pub(super) async fn drain_openai_compatible_stream(
    mut response: reqwest::Response,
    on_delta: &mut (dyn FnMut(String) + Send),
) -> Result<(String, serde_json::Value)> {
    let mut output_text = String::new();
    let mut usage_json = serde_json::json!({});
    let mut buffer = String::new();

    while let Some(chunk) = response.chunk().await? {
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        if buffer.contains('\r') {
            buffer = buffer.replace("\r\n", "\n").replace('\r', "\n");
        }
        while let Some(boundary) = buffer.find("\n\n") {
            let frame = buffer[..boundary].to_string();
            buffer = buffer[boundary + 2..].to_string();
            if consume_openai_compatible_stream_frame(
                &frame,
                &mut output_text,
                &mut usage_json,
                on_delta,
            )? {
                return Ok((output_text, usage_json));
            }
        }
    }

    if !buffer.trim().is_empty() {
        let _ = consume_openai_compatible_stream_frame(
            &buffer,
            &mut output_text,
            &mut usage_json,
            on_delta,
        )?;
    }

    Ok((output_text, usage_json))
}

fn extract_stream_delta_text(payload: &serde_json::Value) -> String {
    let Some(choices) = payload.get("choices").and_then(serde_json::Value::as_array) else {
        return String::new();
    };

    let mut rendered = String::new();
    for value in choices.iter().filter_map(|choice| {
        choice
            .get("delta")
            .and_then(|delta| delta.get("content"))
            .map(extract_message_content_text)
            .filter(|value| !value.is_empty())
    }) {
        rendered.push_str(&value);
    }
    rendered
}

pub(super) const fn is_retryable_upstream_status(status_code: u16) -> bool {
    matches!(
        status_code,
        408 | 409 | 425 | 429 | 500 | 502 | 503 | 504 | 520 | 521 | 522 | 523 | 524 | 529
    )
}

pub(super) fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    error.is_timeout()
        || error.is_connect()
        || is_retryable_transport_error_text(&error.to_string())
}

pub(super) fn is_retryable_transport_error_text(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("connection closed before message completed")
        || normalized.contains("connection reset")
        || normalized.contains("broken pipe")
        || normalized.contains("unexpected eof")
        || normalized.contains("http2")
        || normalized.contains("sendrequest")
        || normalized.contains("error sending request")
}

/// Fixed canonical backoff schedule for retryable LLM provider failures
/// (timeouts, 4xx transient, 5xx). Each entry is the delay to wait *after*
/// the N-th failed attempt before the next retry. After exhausting the
/// schedule the caller surfaces the final error. Total worst-case backoff:
/// 1 + 3 + 10 + 30 + 90 = 134 seconds across 5 retries, covering typical
/// provider warm-up, rate-limit windows, and long transient outages.
const TRANSPORT_RETRY_SCHEDULE_SECS: &[u64] = &[1, 3, 10, 30, 90];

pub(super) fn transport_retry_delay(_base_delay_ms: u64, attempt: usize) -> Duration {
    let idx = if attempt == 0 { 0 } else { attempt - 1 };
    let idx = idx.min(TRANSPORT_RETRY_SCHEDULE_SECS.len() - 1);
    Duration::from_secs(TRANSPORT_RETRY_SCHEDULE_SECS[idx])
}

