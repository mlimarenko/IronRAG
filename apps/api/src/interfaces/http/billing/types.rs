use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domains::billing::{BillingCharge, BillingProviderCall},
    interfaces::http::router_support::ApiError,
    services::ops::billing::DocumentCostSummary,
};

/// Caller never sees a page smaller than this unless the underlying data
/// runs out; also the default when `limit` is omitted.
pub(super) const DEFAULT_PAGE_LIMIT: u32 = 50;
/// Matches the clamp used by the other cursor-paginated list in this API
/// (`GET /v1/audit/events`'s `MAX_AUDIT_LIMIT` uses a higher bound because
/// audit rows are cheaper; billing sub-resource rows carry decimal/JSON
/// usage payloads, so this stays conservative).
pub(super) const MAX_PAGE_LIMIT: u32 = 500;

/// Shared query shape for every cursor-paginated billing GET. `cursor` is
/// opaque and only valid against the endpoint that issued it (a provider-
/// calls cursor is not interchangeable with a charges or document-costs
/// cursor). `includeTotal` opts into the otherwise-omitted expensive total
/// count, mirroring `ListDocumentsQuery.include_total` in the content
/// domain rather than the plan's repeatable `include=` parameter — no
/// other cheap-vs-expensive aggregate exists here to justify a multi-value
/// `include` switch.
#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct BillingPageQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub include_total: Option<bool>,
}

impl BillingPageQuery {
    pub(super) fn limit(&self) -> u32 {
        self.limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT)
    }

    pub(super) fn include_total(&self) -> bool {
        self.include_total.unwrap_or(false)
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingProviderCallPage {
    pub items: Vec<BillingProviderCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingChargePage {
    pub items: Vec<BillingCharge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCostPage {
    pub items: Vec<DocumentCostSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
}

// ============================================================================
// Opaque cursors.
//
// Keyset cursor (provider-calls / charges): base64(json({"t": <rfc3339>,
// "i": <uuid>})), the `(timestamp, id)` of the last row of the previous
// page — mirrors `content::types::DocumentListCursor`. Offset cursor
// (document-costs): base64(json({"o": <usize>})), since that endpoint
// paginates in-memory over one already-materialized snapshot (see
// `BillingService::list_document_costs_for_library_page`) rather than a
// DB-level keyset. Both are opaque from the client's perspective; any
// decode failure is a `BadRequest`, matching the content domain's cursor
// error handling.
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BillingKeysetCursor {
    #[serde(rename = "t")]
    at: DateTime<Utc>,
    #[serde(rename = "i")]
    id: Uuid,
}

pub(super) fn encode_keyset_cursor(at: DateTime<Utc>, id: Uuid) -> String {
    use base64::Engine;
    let json = serde_json::to_vec(&BillingKeysetCursor { at, id }).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

pub(super) fn decode_keyset_cursor(token: &str) -> Result<(DateTime<Utc>, Uuid), ApiError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::BadRequest("invalid cursor encoding".to_string()))?;
    let cursor: BillingKeysetCursor = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::BadRequest("invalid cursor payload".to_string()))?;
    Ok((cursor.at, cursor.id))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BillingOffsetCursor {
    #[serde(rename = "o")]
    offset: usize,
}

pub(super) fn encode_offset_cursor(offset: usize) -> String {
    use base64::Engine;
    let json = serde_json::to_vec(&BillingOffsetCursor { offset }).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

pub(super) fn decode_offset_cursor(token: &str) -> Result<usize, ApiError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::BadRequest("invalid cursor encoding".to_string()))?;
    let cursor: BillingOffsetCursor = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::BadRequest("invalid cursor payload".to_string()))?;
    Ok(cursor.offset)
}
