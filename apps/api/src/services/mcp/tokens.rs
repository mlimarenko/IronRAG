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
    mcp_types::{McpReadDocumentRequest, McpReadMode},
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
    pub(crate) expected_revision_id: Option<Uuid>,
    pub(crate) read_mode: McpReadMode,
    pub(crate) start_offset: usize,
    pub(crate) window_chars: usize,
}

/// Char-window bounds the `read_document` tool applies when the caller does
/// not pin an explicit `length`. Carried together so normalization takes the
/// request and one settings value rather than two loose `usize` arguments.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReadWindowBounds {
    pub(crate) default_chars: usize,
    pub(crate) max_chars: usize,
}

/// Resumable cursor fields the continuation token encodes. Grouped so the
/// token builder takes one cohesive value plus the authorizing context.
#[derive(Debug, Clone)]
pub(crate) struct McpReadCursor {
    pub(crate) document_id: Uuid,
    pub(crate) run_id: Uuid,
    pub(crate) latest_revision_id: Option<Uuid>,
    pub(crate) next_offset: usize,
    pub(crate) window_chars: usize,
    pub(crate) read_mode: McpReadMode,
}

pub(crate) fn normalize_read_request(
    auth: &AuthContext,
    request: &McpReadDocumentRequest,
    window: ReadWindowBounds,
) -> Result<NormalizedReadRequest, ApiError> {
    if let Some(token) = request.continuation_token.as_deref() {
        let payload = decode_continuation_token(auth, token)?;
        if request.document_id.is_some_and(|document_id| document_id != payload.document_id) {
            return Err(ApiError::invalid_continuation_token(
                "continuation document does not match",
            ));
        }
        return Ok(NormalizedReadRequest {
            document_id: payload.document_id,
            expected_revision_id: Some(payload.run_id),
            read_mode: payload.read_mode,
            start_offset: payload.next_offset,
            window_chars: payload.window_chars,
        });
    }

    let document_id = request
        .document_id
        .ok_or_else(|| ApiError::invalid_mcp_tool_call("documentId is required"))?;
    let read_mode = request.mode.clone().unwrap_or(McpReadMode::Full);
    let window_chars = request.length.unwrap_or(window.default_chars).clamp(1, window.max_chars);

    Ok(NormalizedReadRequest {
        document_id,
        expected_revision_id: None,
        read_mode,
        start_offset: request.start_offset.unwrap_or(0),
        window_chars,
    })
}

pub(crate) fn encode_continuation_token(auth: &AuthContext, cursor: McpReadCursor) -> String {
    let proof = continuation_proof(
        auth.token_id,
        cursor.document_id,
        cursor.run_id,
        cursor.next_offset,
        cursor.window_chars,
    );
    let payload = McpContinuationPayload {
        document_id: cursor.document_id,
        run_id: cursor.run_id,
        latest_revision_id: cursor.latest_revision_id,
        next_offset: cursor.next_offset,
        window_chars: cursor.window_chars,
        read_mode: cursor.read_mode,
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
