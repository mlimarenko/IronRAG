//! Read-continuation tokens for the `read_document` MCP tool: encoding a
//! resumable `(document, run, offset, window)` cursor into an opaque,
//! token-bound-proofed string, and normalizing an incoming request (either
//! a fresh `documentId`/offset pair, or a continuation token) into one
//! shape the tool handler consumes.
//!
//! Split out of the former `services/mcp/support.rs` god-file (plan
//! §6.4): this domain has exactly one caller,
//! [`super::access::documents`], and no relation to the mutation or
//! search-excerpt helpers that used to share the file.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    interfaces::http::{auth::AuthContext, router_support::ApiError},
    mcp_types::McpReadMode,
};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

fn decode_continuation_token(
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

fn continuation_proof(
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

/// Slices `text[start..end]` in char units (as opposed to byte offsets),
/// matching the char-counted `window_chars`/`next_offset` fields the
/// continuation token carries.
pub(crate) fn char_slice(text: &str, start_offset: usize, window_chars: usize) -> String {
    text.chars().skip(start_offset).take(window_chars).collect()
}
