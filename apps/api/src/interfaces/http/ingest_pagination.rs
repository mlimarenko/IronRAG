//! Shared pagination vocabulary for the ingest job and attempt list surfaces.
//!
//! Both the REST handlers in [`super::ingestion`] and the MCP ingest
//! diagnostics tools page the same two keysets, so the cursor shapes, the
//! accepted `queue_state` filter values and the cursor codec live here once.
//! The public REST contract (field names and error strings) is unchanged by
//! the move.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::router_support::ApiError;

/// Accepted `queue_state` filter values, in the order the error message lists
/// them.
pub(crate) const INGEST_QUEUE_STATE_VALUES: &[&str] =
    &["queued", "leased", "completed", "failed", "canceled", "paused"];

/// Parses a comma-separated `status` filter into its accepted states.
///
/// Empty or blank input yields an empty filter (no filtering).
pub(crate) fn parse_ingest_queue_state_filter(raw: &str) -> Result<Vec<String>, ApiError> {
    let mut out = Vec::new();
    for token in raw.split(',').map(str::trim).filter(|value| !value.is_empty()) {
        if !INGEST_QUEUE_STATE_VALUES.contains(&token) {
            return Err(ApiError::BadRequest(format!(
                "unknown status filter value `{token}`; allowed: {}",
                INGEST_QUEUE_STATE_VALUES.join(", ")
            )));
        }
        out.push(token.to_string());
    }
    Ok(out)
}

/// Keyset cursor for the ingest job list: `queued_at` plus the job id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct IngestJobListCursor {
    pub(crate) queued_at: chrono::DateTime<chrono::Utc>,
    pub(crate) job_id: Uuid,
}

/// Keyset cursor for one job's attempt history: `attempt_number` plus the
/// attempt id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct IngestAttemptListCursor {
    pub(crate) attempt_number: i32,
    pub(crate) attempt_id: Uuid,
}

// ============================================================================
// Opaque cursors for the ingest jobs/attempts list surfaces.
//
// Same shape as the primary `/v1/content/libraries/{libraryId}/documents`
// cursor: base64(json(...)), opaque from the client's perspective, only
// valid against the server version that produced it. Any decode failure is
// surfaced as a `BadRequest` — callers are expected to drop the cursor and
// start from the top instead of pretending the page succeeded.
// ============================================================================

pub(crate) fn encode_page_cursor<T: Serialize>(cursor: &T) -> String {
    use base64::Engine;
    // Both cursor shapes below are plain structs of infallibly-serializable
    // fields (`DateTime<Utc>` / `Uuid` / `i32`), so `to_vec` can only fail on
    // I/O errors which `Vec<u8>` never produces. Fall back to an empty token
    // on the (unreachable) error path so the caller keeps paginating rather
    // than panicking on the hot path.
    let json = serde_json::to_vec(cursor).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

pub(crate) fn decode_page_cursor<T: serde::de::DeserializeOwned>(
    token: &str,
) -> Result<T, ApiError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::BadRequest("invalid cursor encoding".to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::BadRequest("invalid cursor payload".to_string()))
}
