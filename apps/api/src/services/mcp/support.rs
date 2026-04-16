use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::state::McpMemorySettings,
    interfaces::http::{auth::AuthContext, router_support::ApiError},
    mcp_types::{
        McpMutationOperationKind, McpMutationReceiptStatus, McpReadMode,
        McpRuntimeExecutionSummary, McpRuntimeExecutionTrace,
    },
    shared::extraction::file_extract::UploadAdmissionError,
};

const EXCERPT_CONTEXT_BEFORE: usize = 80;
const EXCERPT_CONTEXT_AFTER: usize = 200;
const EXCERPT_BASE_RELEVANCE_SCORE: f64 = 0.7;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpContinuationPayload {
    pub(crate) document_id: Uuid,
    pub(crate) run_id: Uuid,
    pub(crate) latest_revision_id: Option<Uuid>,
    pub(crate) next_offset: usize,
    pub(crate) window_chars: usize,
    pub(crate) read_mode: McpReadMode,
    pub(crate) proof: String,
}

#[derive(Debug, Clone)]
pub(crate) struct NormalizedReadRequest {
    pub(crate) document_id: Uuid,
    pub(crate) read_mode: McpReadMode,
    pub(crate) start_offset: usize,
    pub(crate) window_chars: usize,
}

pub(crate) fn normalize_read_request(
    auth: &AuthContext,
    request_document_id: Option<Uuid>,
    request_mode: Option<McpReadMode>,
    request_start_offset: Option<usize>,
    request_length: Option<usize>,
    continuation_token: Option<&str>,
    default_read_window_chars: usize,
    max_read_window_chars: usize,
) -> Result<NormalizedReadRequest, ApiError> {
    if let Some(token) = continuation_token {
        let payload = decode_continuation_token(auth, token)?;
        return Ok(NormalizedReadRequest {
            document_id: payload.document_id,
            read_mode: payload.read_mode,
            start_offset: payload.next_offset,
            window_chars: payload.window_chars,
        });
    }

    let document_id = request_document_id
        .ok_or_else(|| ApiError::invalid_mcp_tool_call("documentId is required"))?;
    let read_mode = request_mode.unwrap_or(McpReadMode::Full);
    let window_chars =
        request_length.unwrap_or(default_read_window_chars).clamp(1, max_read_window_chars);

    Ok(NormalizedReadRequest {
        document_id,
        read_mode,
        start_offset: request_start_offset.unwrap_or(0),
        window_chars,
    })
}

pub(crate) fn encode_continuation_token(
    auth: &AuthContext,
    document_id: Uuid,
    run_id: Uuid,
    latest_revision_id: Option<Uuid>,
    next_offset: usize,
    window_chars: usize,
    read_mode: McpReadMode,
) -> String {
    let proof = continuation_proof(auth.token_id, document_id, run_id, next_offset, window_chars);
    let payload = McpContinuationPayload {
        document_id,
        run_id,
        latest_revision_id,
        next_offset,
        window_chars,
        read_mode,
        proof,
    };
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    URL_SAFE_NO_PAD.encode(json)
}

pub(crate) fn decode_continuation_token(
    auth: &AuthContext,
    token: &str,
) -> Result<McpContinuationPayload, ApiError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::invalid_continuation_token("invalid continuation token"))?;
    let payload: McpContinuationPayload = serde_json::from_slice(&decoded)
        .map_err(|_| ApiError::invalid_continuation_token("invalid continuation token"))?;
    let expected = continuation_proof(
        auth.token_id,
        payload.document_id,
        payload.run_id,
        payload.next_offset,
        payload.window_chars,
    );
    if payload.proof != expected {
        return Err(ApiError::invalid_continuation_token("invalid continuation token"));
    }
    Ok(payload)
}

pub(crate) fn continuation_proof(
    token_id: Uuid,
    document_id: Uuid,
    run_id: Uuid,
    next_offset: usize,
    window_chars: usize,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token_id.as_bytes());
    hasher.update(document_id.as_bytes());
    hasher.update(run_id.as_bytes());
    hasher.update(next_offset.to_string().as_bytes());
    hasher.update(window_chars.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

pub(crate) fn normalize_upload_idempotency_key(
    idempotency_key: Option<&str>,
    library_id: Uuid,
    document_index: usize,
    payload_identity: &str,
) -> String {
    match idempotency_key.map(str::trim).filter(|value| !value.is_empty()) {
        Some(base) => format!("mcp:upload:{library_id}:{document_index}:{base}"),
        None => format!("mcp:upload:{library_id}:{payload_identity}"),
    }
}

pub(crate) fn normalize_document_idempotency_key(
    idempotency_key: Option<&str>,
    document_id: Uuid,
    operation_kind: &McpMutationOperationKind,
    payload_identity: &str,
) -> String {
    let operation = operation_kind_key(operation_kind);
    match idempotency_key.map(str::trim).filter(|value| !value.is_empty()) {
        Some(base) => format!("mcp:{operation}:{document_id}:{base}"),
        None => format!("mcp:{operation}:{document_id}:{payload_identity}"),
    }
}

pub(crate) fn hash_upload_payload(
    file_name: &str,
    mime_type: Option<&str>,
    title: Option<&str>,
    file_bytes: &[u8],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(file_name.as_bytes());
    hasher.update(mime_type.unwrap_or_default().as_bytes());
    hasher.update(title.unwrap_or_default().as_bytes());
    hasher.update(file_bytes);
    hex::encode(hasher.finalize())
}

pub(crate) fn hash_append_payload(appended_text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(appended_text.as_bytes());
    hex::encode(hasher.finalize())
}

pub(crate) fn hash_replace_payload(
    file_name: &str,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(file_name.as_bytes());
    hasher.update(mime_type.unwrap_or_default().as_bytes());
    hasher.update(file_bytes);
    hex::encode(hasher.finalize())
}

pub(crate) fn validate_mcp_upload_file_size(
    settings: &McpMemorySettings,
    file_name: &str,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> Result<(), ApiError> {
    let file_size_bytes = u64::try_from(file_bytes.len()).unwrap_or(u64::MAX);
    if file_size_bytes > settings.max_upload_file_bytes() {
        return Err(ApiError::from_upload_admission(UploadAdmissionError::file_too_large(
            file_name,
            mime_type,
            file_size_bytes,
            settings.upload_max_size_mb,
        )));
    }
    Ok(())
}

pub(crate) fn validate_mcp_upload_batch_size(
    settings: &McpMemorySettings,
    total_upload_bytes: u64,
) -> Result<(), ApiError> {
    if total_upload_bytes > settings.max_upload_batch_bytes() {
        return Err(ApiError::from_upload_admission(UploadAdmissionError::upload_batch_too_large(
            total_upload_bytes,
            settings.upload_max_size_mb,
        )));
    }
    Ok(())
}

pub(crate) fn operation_kind_key(operation_kind: &McpMutationOperationKind) -> &'static str {
    match operation_kind {
        McpMutationOperationKind::Upload => "upload",
        McpMutationOperationKind::Append => "append",
        McpMutationOperationKind::Replace => "replace",
    }
}

pub(crate) fn parse_mutation_operation_kind(
    value: &str,
) -> Result<McpMutationOperationKind, ApiError> {
    match value {
        "upload" => Ok(McpMutationOperationKind::Upload),
        "append" => Ok(McpMutationOperationKind::Append),
        "replace" => Ok(McpMutationOperationKind::Replace),
        _ => Err(ApiError::Internal),
    }
}

pub(crate) fn map_content_mutation_status_to_receipt_status(
    mutation_state: &str,
) -> McpMutationReceiptStatus {
    match mutation_state {
        "accepted" => McpMutationReceiptStatus::Accepted,
        "running" => McpMutationReceiptStatus::Processing,
        "applied" => McpMutationReceiptStatus::Ready,
        "failed" | "conflicted" | "canceled" => McpMutationReceiptStatus::Failed,
        _ => McpMutationReceiptStatus::Accepted,
    }
}

pub(crate) fn saturating_rank(index: usize) -> i32 {
    i32::try_from(index.saturating_add(1)).unwrap_or(i32::MAX)
}

pub(crate) fn char_slice(text: &str, start_offset: usize, window_chars: usize) -> String {
    text.chars().skip(start_offset).take(window_chars).collect()
}

pub(crate) fn payload_identity_from_source_uri(source_uri: Option<&str>) -> Option<String> {
    source_uri
        .and_then(|value| {
            value.strip_prefix("mcp://payload/").or_else(|| value.strip_prefix("inline://payload/"))
        })
        .map(ToString::to_string)
}

pub(crate) fn describe_runtime_execution_summary(execution: &McpRuntimeExecutionSummary) -> String {
    let policy_suffix = if execution.policy_summary.reject_count > 0
        || execution.policy_summary.terminate_count > 0
    {
        format!(
            " Policy interventions: {} rejected, {} terminated.",
            execution.policy_summary.reject_count, execution.policy_summary.terminate_count
        )
    } else {
        String::new()
    };
    match (execution.lifecycle_state, execution.active_stage) {
        (crate::domains::agent_runtime::RuntimeLifecycleState::Running, Some(active_stage)) => {
            format!(
                "Runtime execution {} is running in stage {}.{}",
                execution.runtime_execution_id,
                canonical_runtime_value(&active_stage),
                policy_suffix
            )
        }
        (
            crate::domains::agent_runtime::RuntimeLifecycleState::Completed
            | crate::domains::agent_runtime::RuntimeLifecycleState::Recovered,
            Some(active_stage),
        ) => format!(
            "Runtime execution {} finished in state {} after stage {}.{}",
            execution.runtime_execution_id,
            canonical_runtime_value(&execution.lifecycle_state),
            canonical_runtime_value(&active_stage),
            policy_suffix
        ),
        _ => format!(
            "Runtime execution {} is {}.{}",
            execution.runtime_execution_id,
            canonical_runtime_value(&execution.lifecycle_state),
            policy_suffix
        ),
    }
}

pub(crate) fn describe_runtime_trace_summary(trace: &McpRuntimeExecutionTrace) -> String {
    format!(
        "Runtime trace loaded for execution {} with {} stage(s), {} action(s), and {} policy decision(s).",
        trace.execution.runtime_execution_id,
        trace.stages.len(),
        trace.actions.len(),
        trace.policy_decisions.len()
    )
}

fn canonical_runtime_value<T>(value: &T) -> String
where
    T: Serialize,
{
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Slices `text[start..end]` but walks back/forward to the nearest valid
/// UTF-8 boundary so Cyrillic, CJK, or accented content does not panic
/// in the middle of a multi-byte codepoint. Used by every excerpt
/// branch below.
fn safe_substring(text: &str, start: usize, end: usize) -> &str {
    if text.is_empty() {
        return "";
    }
    let mut s = start.min(text.len());
    while s > 0 && !text.is_char_boundary(s) {
        s -= 1;
    }
    let mut e = end.min(text.len());
    if e < s {
        e = s;
    }
    while e < text.len() && !text.is_char_boundary(e) {
        e += 1;
    }
    &text[s..e]
}

pub(crate) fn preview_hit(text: &str, query_lower: &str) -> Option<(String, usize, usize, f64)> {
    if text.trim().is_empty() {
        return None;
    }
    let text_lower = text.to_lowercase();

    // 1. Exact-phrase match wins — gives the tightest excerpt around the
    //    phrase boundaries.
    if let Some(start) = text_lower.find(query_lower) {
        let end = start.saturating_add(query_lower.len());
        let excerpt_start = start.saturating_sub(80);
        let excerpt_end = end + 160;
        let slice = safe_substring(text, excerpt_start, excerpt_end);
        let score = 1.0f64 / (1.0 + start as f64);
        return Some((slice.trim().to_string(), excerpt_start, excerpt_end.min(text.len()), score));
    }

    // 2. Token fallback — the first query word that actually appears in
    //    the chunk anchors the excerpt. This covers vector hits where
    //    the full phrase is absent but a keyword is present, and stops
    //    `excerpt` from going null on semantically-similar chunks.
    for raw_word in query_lower.split_whitespace() {
        let word = raw_word.trim_matches(|c: char| !c.is_alphanumeric());
        if word.chars().count() < 3 {
            continue;
        }
        if let Some(start) = text_lower.find(word) {
            let end = start.saturating_add(word.len());
            let excerpt_start = start.saturating_sub(EXCERPT_CONTEXT_BEFORE);
            let excerpt_end = end + EXCERPT_CONTEXT_AFTER;
            let slice = safe_substring(text, excerpt_start, excerpt_end);
            let score = EXCERPT_BASE_RELEVANCE_SCORE / (1.0 + start as f64);
            return Some((
                slice.trim().to_string(),
                excerpt_start,
                excerpt_end.min(text.len()),
                score,
            ));
        }
    }

    // 3. Last resort — return the head of the chunk so the agent has at
    //    least something to decide whether to read further.
    let slice = safe_substring(text, 0, 240);
    let excerpt = slice.trim().to_string();
    if excerpt.is_empty() {
        return None;
    }
    Some((excerpt, 0, slice.len(), 0.1))
}
