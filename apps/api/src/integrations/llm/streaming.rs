use anyhow::{Context, Result};

use super::extract_message_content_text;
use crate::shared::provider_http::{
    PROVIDER_STREAM_FRAME_MAX_BYTES, PROVIDER_STREAM_TOTAL_MAX_BYTES,
};

#[derive(Debug, Clone, Copy)]
struct ProviderStreamLimits {
    max_total_bytes: usize,
    max_frame_bytes: usize,
}

impl Default for ProviderStreamLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: PROVIDER_STREAM_TOTAL_MAX_BYTES,
            max_frame_bytes: PROVIDER_STREAM_FRAME_MAX_BYTES,
        }
    }
}

struct BoundedSseBuffer {
    limits: ProviderStreamLimits,
    total_bytes: usize,
    pending: Vec<u8>,
}

impl BoundedSseBuffer {
    const fn new(limits: ProviderStreamLimits) -> Self {
        Self { limits, total_bytes: 0, pending: Vec::new() }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>> {
        self.total_bytes = self
            .total_bytes
            .checked_add(chunk.len())
            .filter(|total| *total <= self.limits.max_total_bytes)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "provider stream exceeded {} byte limit",
                    self.limits.max_total_bytes
                )
            })?;
        self.pending.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some((boundary, delimiter_len)) = sse_frame_boundary(&self.pending) {
            if boundary > self.limits.max_frame_bytes {
                return Err(anyhow::anyhow!(
                    "provider stream frame exceeded {} byte limit",
                    self.limits.max_frame_bytes
                ));
            }
            let frame = String::from_utf8_lossy(&self.pending[..boundary]).into_owned();
            self.pending.drain(..boundary + delimiter_len);
            frames.push(frame);
        }
        if self.pending.len() > self.limits.max_frame_bytes {
            return Err(anyhow::anyhow!(
                "provider stream frame exceeded {} byte limit",
                self.limits.max_frame_bytes
            ));
        }
        Ok(frames)
    }

    fn finish(&mut self) -> Result<Option<String>> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        if self.pending.len() > self.limits.max_frame_bytes {
            return Err(anyhow::anyhow!(
                "provider stream frame exceeded {} byte limit",
                self.limits.max_frame_bytes
            ));
        }
        let pending = std::mem::take(&mut self.pending);
        Ok(Some(String::from_utf8_lossy(&pending).into_owned()))
    }
}

fn sse_frame_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes.windows(2).position(|window| window == b"\n\n").map(|index| (index, 2));
    let crlf = bytes.windows(4).position(|window| window == b"\r\n\r\n").map(|index| (index, 4));
    let cr = bytes.windows(2).position(|window| window == b"\r\r").map(|index| (index, 2));
    [lf, crlf, cr].into_iter().flatten().min_by_key(|(index, _)| *index)
}

fn validate_stream_content_length(
    response: &reqwest::Response,
    limits: ProviderStreamLimits,
) -> Result<()> {
    if response.content_length().is_some_and(|length| length > limits.max_total_bytes as u64) {
        anyhow::bail!("provider stream Content-Length exceeds configured limit");
    }
    Ok(())
}

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
    let limits = ProviderStreamLimits::default();
    validate_stream_content_length(&response, limits)?;
    let mut output_text = String::new();
    let mut usage_json = serde_json::json!({});
    let mut buffer = BoundedSseBuffer::new(limits);

    while let Some(chunk) = response.chunk().await? {
        for frame in buffer.push(&chunk)? {
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

    if let Some(frame) = buffer.finish()?.filter(|frame| !frame.trim().is_empty()) {
        let _ = consume_openai_compatible_stream_frame(
            &frame,
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

/// Accumulates a streaming tool-use response. Each SSE frame from an
/// OpenAI-compatible provider can carry one of:
///   * `delta.content` — assistant text tokens (forwarded live);
///   * `delta.tool_calls[]` — partial tool-call records (id, name
///     appear once, `arguments` arrives as a stream of string chunks
///     that must be concatenated);
///   * `usage` — final usage stats;
///   * `finish_reason` — "stop" / "`tool_calls`" / "length".
///
/// Tool-call arguments are assembled across frames before returning to
/// the caller. Text tokens are pushed out immediately through
/// `on_text_delta` so a caller that streams text can render partial
/// answers.
#[derive(Default)]
pub(super) struct ToolUseStreamState {
    pub(super) output_text: String,
    pub(super) finish_reason: Option<String>,
    pub(super) usage_json: serde_json::Value,
    /// Indexed by the `tool_calls[].index` field from the provider.
    /// Entries get filled progressively: identity fields (id, name)
    /// arrive once near the start, `arguments` accumulates chunks.
    pub(super) tool_calls: std::collections::BTreeMap<usize, PartialToolCall>,
}

#[derive(Default)]
pub(super) struct PartialToolCall {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) arguments_json: String,
}

impl ToolUseStreamState {
    pub(super) fn finalize(
        self,
    ) -> (String, Option<String>, serde_json::Value, Vec<super::ChatToolCall>) {
        let tool_calls = self
            .tool_calls
            .into_values()
            .map(|partial| super::ChatToolCall {
                id: partial.id,
                name: partial.name,
                arguments_json: partial.arguments_json,
            })
            .collect();
        (self.output_text, self.finish_reason, self.usage_json, tool_calls)
    }
}

pub(super) fn consume_tool_use_stream_frame(
    frame: &str,
    state: &mut ToolUseStreamState,
    on_text_delta: &mut (dyn FnMut(String) + Send),
) -> Result<bool> {
    let Some(payload_text) = parse_sse_payload(frame)? else {
        return Ok(false);
    };
    if payload_text.trim() == "[DONE]" {
        return Ok(true);
    }

    let payload: serde_json::Value = serde_json::from_str(&payload_text)
        .context("failed to parse upstream tool-use streaming payload as json")?;
    update_tool_use_state(&payload, state, on_text_delta);
    Ok(false)
}

fn parse_sse_payload(frame: &str) -> Result<Option<String>> {
    if frame.trim().is_empty() || frame.starts_with(':') {
        return Ok(None);
    }

    let data_lines = frame
        .split('\n')
        .map(str::trim_end)
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>();
    if data_lines.is_empty() {
        return Ok(None);
    }

    Ok(Some(data_lines.join("\n")))
}

fn update_tool_use_state(
    payload: &serde_json::Value,
    state: &mut ToolUseStreamState,
    on_text_delta: &mut (dyn FnMut(String) + Send),
) {
    if let Some(usage) = payload.get("usage").filter(|value| !value.is_null()) {
        state.usage_json = usage.clone();
    }

    let Some(choices) = payload.get("choices").and_then(serde_json::Value::as_array) else {
        return;
    };
    for choice in choices {
        update_tool_use_choice(choice, state, on_text_delta);
    }
}

fn update_tool_use_choice(
    choice: &serde_json::Value,
    state: &mut ToolUseStreamState,
    on_text_delta: &mut (dyn FnMut(String) + Send),
) {
    if let Some(reason) = choice.get("finish_reason").and_then(|value| value.as_str()) {
        state.finish_reason = Some(reason.to_string());
    }
    let Some(delta) = choice.get("delta") else {
        return;
    };
    forward_text_delta(delta, state, on_text_delta);
    if let Some(tool_chunks) = delta.get("tool_calls").and_then(serde_json::Value::as_array) {
        for chunk in tool_chunks {
            accumulate_tool_call_chunk(chunk, state);
        }
    }
}

fn forward_text_delta(
    delta: &serde_json::Value,
    state: &mut ToolUseStreamState,
    on_text_delta: &mut (dyn FnMut(String) + Send),
) {
    let Some(content) = delta.get("content") else {
        return;
    };
    let chunk = extract_message_content_text(content);
    if chunk.is_empty() {
        return;
    }
    state.output_text.push_str(&chunk);
    on_text_delta(chunk);
}

fn accumulate_tool_call_chunk(chunk: &serde_json::Value, state: &mut ToolUseStreamState) {
    let Some(index) = chunk.get("index").and_then(serde_json::Value::as_u64) else {
        return;
    };
    let entry = state.tool_calls.entry(index as usize).or_default();
    if let Some(id) = chunk.get("id").and_then(|value| value.as_str())
        && entry.id.is_empty()
    {
        entry.id = id.to_string();
    }
    let Some(function) = chunk.get("function") else {
        return;
    };
    if let Some(name) = function.get("name").and_then(|value| value.as_str())
        && entry.name.is_empty()
    {
        entry.name = name.to_string();
    }
    if let Some(arguments) = function.get("arguments").and_then(|value| value.as_str()) {
        entry.arguments_json.push_str(arguments);
    }
}

pub(super) async fn drain_tool_use_stream(
    mut response: reqwest::Response,
    on_text_delta: &mut (dyn FnMut(String) + Send),
) -> Result<ToolUseStreamState> {
    let limits = ProviderStreamLimits::default();
    validate_stream_content_length(&response, limits)?;
    let mut state = ToolUseStreamState::default();
    let mut buffer = BoundedSseBuffer::new(limits);
    while let Some(chunk) = response.chunk().await? {
        for frame in buffer.push(&chunk)? {
            if consume_tool_use_stream_frame(&frame, &mut state, on_text_delta)? {
                return Ok(state);
            }
        }
    }
    if let Some(frame) = buffer.finish()?.filter(|frame| !frame.trim().is_empty()) {
        let _ = consume_tool_use_stream_frame(&frame, &mut state, on_text_delta)?;
    }
    Ok(state)
}

#[cfg(test)]
mod transport_limit_tests {
    use super::{BoundedSseBuffer, ProviderStreamLimits};

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

        let error =
            buffer.push(b"data: 3\n\n").expect_err("cumulative stream cap must be enforced");

        assert!(error.to_string().contains("stream"));
    }
}
