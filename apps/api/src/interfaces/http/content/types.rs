use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domains::billing::CostRollupFreshness,
    domains::content::{
        ContentDocument, ContentDocumentHead, ContentDocumentPipelineState, ContentDocumentSummary,
        ContentMutation, ContentMutationItem, ContentRevision, ContentRevisionReadiness,
        ContentSourceAccess, DocumentReadinessSummary, WebPageProvenance,
    },
    domains::knowledge::{PreparedSegmentDetail, TypedTechnicalFact},
    infra::repositories::content_document_list_repository::{
        DocumentListFilters, DocumentListSearchRanges, FilterValues,
    },
    interfaces::http::router_support::ApiError,
    services::{
        content::{
            document_accounting::DocumentLifecycleDetail,
            service::{ContentMutationAdmission, ReprocessRevisionSource, RevisionAdmission},
        },
        ingest::web::RefetchedWebDocumentSource,
    },
};
use ironrag_contracts::documents::{DocumentReadiness, DocumentStatus};

// ============================================================================
// Document-list surface.
//
// The list response is deliberately slim: one compact row per document with
// *only* the fields the documents page actually renders (see the mapper in
// apps/web/src/pages/documents/mappers.ts). Detail-only data such as
// `readiness_summary`, `prepared_revision`, `pipeline.latest_job`,
// `technical_fact_count`, `lifecycle`, and `web_page_provenance` is NOT
// returned here — the inspector panel fetches them separately via
// /content/documents/{id}. This keeps a reference ~5 k-document payload
// under 3 MB instead of 26 MB.
// ============================================================================

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListDocumentsQuery {
    pub include_deleted: Option<bool>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub search: Option<String>,
    pub sort_by: Option<DocumentListSortKey>,
    pub sort_order: Option<DocumentListSortOrder>,
    pub include_total: Option<bool>,
    /// Comma-separated list of status buckets to keep. Accepted values:
    /// `canceled`, `failed`, `processing`, `queued`, `ready`. Empty or
    /// absent = no filter. Matches the `derived_status` column
    /// in the list CTE and the 5 status pills on the documents page.
    pub status: Option<String>,
    /// Comma-separated list of document ids to keep. Absent = no filter;
    /// an explicit empty list is rejected. Resolves documents (e.g. a deep link
    /// carrying `documentId`) through the same shared list derivation
    /// that powers the table, instead of a divergent per-id detail mapper.
    pub ids: Option<String>,
    /// Restricts the page to documents an indexing invariant is broken for.
    /// Orthogonal to `status`: a document whose chunk vectors are missing is
    /// `ready` by every `derived_status` rule, so this cannot be a status
    /// value. Absent = no filter.
    pub indexing: Option<DocumentIndexingFilter>,
    /// Strictly typed per-column grid filters as a JSON object of
    /// `DocumentColumnFilters`. Unknown keys, wrong types, malformed
    /// timestamps/decimals and oversized payloads are a 400.
    pub filters: Option<String>,
    /// Finite comma-separated derived-status buckets a literal search matches,
    /// OR'd alongside the text search over every canonical field.
    pub search_status: Option<String>,
    /// Intl-normalized variant of `search`, OR'd over the same canonical
    /// fields. The backend does not implement a language dictionary.
    pub normalized_search: Option<String>,
    /// Snapshot instant captured once for the cursor chain. Optional on the
    /// first page; on a cursor page it must equal the cursor's snapshot
    /// exactly, so a facet count and the page it opens agree. A valid RFC3339
    /// UTC instant; a mismatch is a 400.
    pub snapshot_at: Option<DateTime<Utc>>,
    /// Typed range hints as a JSON object of `DocumentSearchRanges`. OR
    /// alternatives to the text/status search; a malformed, reversed,
    /// negative, over-`i64` or oversized payload is a 400.
    pub search_ranges: Option<String>,
}

/// Indexing invariants the documents list can filter on. Each value resolves to
/// an exact document set the dashboard already counts, so the notice and the
/// list it opens cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentIndexingFilter {
    /// Ready documents the active graph projection does not cover
    /// (`graph_coverage_gap` / the `graphSparse` counter).
    GraphGap,
    /// Ready documents the active graph projection covers (the `graphReady`
    /// counter).
    GraphReady,
    /// Active-head revisions marked vector-ready that hold chunks and no chunk
    /// vectors (`vector_index_gap`).
    VectorGap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentListSortKey {
    UploadedAt,
    FileName,
    FileType,
    FileSize,
    Status,
    Cost,
    PipelineTime,
    FinishedAt,
}

#[derive(Debug, Clone, Copy, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentListSortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContentDocumentListItem {
    pub id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    #[schema(required = true)]
    pub file_name: Option<String>,
    #[schema(required = true)]
    pub file_type: Option<String>,
    #[schema(required = true)]
    pub file_size: Option<i64>,
    pub uploaded_at: DateTime<Utc>,
    pub document_state: String,
    /// Persisted connector identity (`content_document.external_key`).
    /// Connectors look this up via list to decide whether to upload or
    /// replace; exposed so a paginated list pass is sufficient instead
    /// of a per-document detail roundtrip.
    pub external_key: String,
    /// Status bucket derived server-side.
    pub status: DocumentStatus,
    /// Readiness bucket derived server-side.
    pub readiness: DocumentReadiness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_percent: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_finished_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_access: Option<ContentSourceAccess>,
    /// Summed cost across every billable execution attributed to this
    /// document (ingest + graph extraction). Serialized as a decimal
    /// string to avoid IEEE-754 rounding in the browser. `null` while the
    /// owning rollup reconciles — the amount is unknown, not zero — with
    /// `costCurrencyCode` also `null`.
    pub cost: Option<String>,
    pub cost_currency_code: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentListPageResponse {
    pub items: Vec<ContentDocumentListItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_count: Option<i64>,
    /// Per-bucket document counts, populated only when the caller sets
    /// `includeTotal=true`. The counts are computed from the same
    /// `derived_status` CASE expression the list CTE uses, so they stay
    /// in sync with what the pills would show if you filtered on each
    /// bucket individually. Used by the documents page filter strip to
    /// render count badges on each pill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_counts: Option<DocumentListStatusCounts>,
    /// Instant captured once for the whole cursor chain. Send it to the facets
    /// endpoint so facet counts agree with this page's pipeline durations.
    pub snapshot_at: DateTime<Utc>,
    /// Whether this query result was withheld because it reads cost while the
    /// library rollup reconciles — distinct from `cost_rollup_freshness`, which
    /// describes the rollup itself. `true` means the cost-dependent query
    /// (cost sort, cost filter, literal/normalized text search) could not be
    /// answered exactly: `items` is empty, `nextCursor` and `totalCount` are
    /// absent. A metadata read on a dirty rollup is `false` with real rows, a
    /// real total and a truthful `reconciling` freshness.
    pub scope_pending: bool,
    /// Freshness of the library billing rollup this page was read against.
    /// `reconciling` means every row's cost is withheld (`cost: null`) and a
    /// cost-dependent read would return no rows. Independent of pricing
    /// coverage; a clean library stays `current`.
    pub cost_rollup_freshness: CostRollupFreshness,
}

#[derive(Debug, Serialize, Default, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentListStatusCounts {
    pub total: i64,
    pub ready: i64,
    pub processing: i64,
    pub queued: i64,
    pub failed: i64,
    pub canceled: i64,
    pub empty: i64,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ChunksQuery {
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PreparedDataQuery {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateDocumentRequest {
    pub external_key: Option<String>,
    /// Explicit source filename, required when admitting replacement metadata.
    pub file_name: Option<String>,
    /// Optional connector-declared structural parent identity
    /// (`content_document.external_key` of the parent document). When the
    /// parent already exists in the library it is resolved at admission;
    /// otherwise the deferred resolver reconciles it later.
    pub parent_external_key: Option<String>,
    pub idempotency_key: Option<String>,
    pub content_source_kind: Option<String>,
    pub checksum: Option<String>,
    pub mime_type: Option<String>,
    pub byte_size: Option<i64>,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub document_hint: Option<String>,
    pub storage_key: Option<String>,
}

/// Multipart form for a file-backed document admission.
///
/// The names intentionally remain snake_case because multipart field names are
/// protocol literals parsed directly by `parse_upload_multipart`.
#[derive(Debug, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CreateDocumentMultipartRequest {
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
    pub external_key: Option<String>,
    pub parent_external_key: Option<String>,
    pub idempotency_key: Option<String>,
    pub title: Option<String>,
    pub document_hint: Option<String>,
}

/// Discriminates the two JSON revision shapes on `POST .../revisions`. File
/// replacement is a third, mutually exclusive shape carried over
/// `multipart/form-data` instead — see `create_revision`'s content
/// negotiation, mirroring `create_document`'s pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RevisionMode {
    Append,
    Replace,
}

/// JSON body for `POST /v1/content/documents/{documentId}/revisions`.
/// `mode = "append"` requires `appendedText`; `mode = "replace"` requires
/// `markdown`. Replacing the underlying file bytes instead of markdown text
/// uses `multipart/form-data` on the same endpoint, not this shape.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRevisionRequest {
    /// Required for append; the exact immutable source revision.
    pub base_revision_id: Option<Uuid>,
    pub mode: RevisionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub appended_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    pub idempotency_key: Option<String>,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub document_hint: Option<String>,
    pub visibility_intent_id: Option<Uuid>,
}

/// Multipart form for a file-backed document revision.
#[derive(Debug, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CreateRevisionMultipartRequest {
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
    pub idempotency_key: Option<String>,
    pub document_hint: Option<String>,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub visibility_intent_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReprocessDocumentRequest {
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatchDocumentMetadataRequest {
    pub document_hint: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContentDocumentDetailResponse {
    pub document: ContentDocument,
    #[schema(required = true)]
    pub file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_access: Option<ContentSourceAccess>,
    pub head: Option<ContentDocumentHead>,
    pub active_revision: Option<ContentRevision>,
    pub readiness: Option<ContentRevisionReadiness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness_summary: Option<DocumentReadinessSummary>,
    // Full prepared revision (~4 MB on PDF docs) is not returned here —
    // the inspector only needs the two count fields below. Use the
    // paginated `/content/documents/{id}/prepared-segments` endpoint
    // for the actual blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepared_segment_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub technical_fact_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_page_provenance: Option<WebPageProvenance>,
    pub pipeline: ContentDocumentPipelineState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<DocumentLifecycleDetail>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContentMutationDetailResponse {
    pub mutation: ContentMutation,
    pub items: Vec<ContentMutationItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_operation_id: Option<Uuid>,
    /// Durable admission visibility policy of this mutation. Always emitted so
    /// a client can never treat the field as absent and silently assume
    /// readability; unknown values are a decode error, not a default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_revision_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_fingerprint: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateDocumentResponse {
    pub document: ContentDocumentDetailResponse,
    pub mutation: ContentMutationDetailResponse,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChunkSummary {
    pub id: Uuid,
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub ordinal: i32,
    pub content: String,
    pub token_count: Option<i32>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSegmentsPageResponse {
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub items: Vec<PreparedSegmentDetail>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalFactsPageResponse {
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub items: Vec<TypedTechnicalFact>,
}

pub(super) fn map_document_summary(
    summary: ContentDocumentSummary,
) -> ContentDocumentDetailResponse {
    let prepared_segment_count =
        summary.prepared_revision.as_ref().map(|revision| revision.block_count);
    let technical_fact_count =
        summary.prepared_revision.as_ref().map(|revision| revision.typed_fact_count);
    // `prepared_revision` is intentionally not copied onto the response
    // — see the field removal above.
    ContentDocumentDetailResponse {
        document: summary.document,
        file_name: summary.file_name,
        source_access: summary.source_access,
        head: summary.head,
        active_revision: summary.active_revision,
        readiness: summary.readiness,
        readiness_summary: summary.readiness_summary,
        prepared_segment_count,
        technical_fact_count,
        web_page_provenance: summary.web_page_provenance,
        pipeline: summary.pipeline,
        lifecycle: None,
    }
}

pub(super) fn build_revision_metadata(
    payload: &CreateDocumentRequest,
) -> Result<Option<RevisionAdmission>, ApiError> {
    let checksum = payload.checksum.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let mime_type = payload.mime_type.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let byte_size = payload.byte_size;

    match (checksum, mime_type, byte_size) {
        (None, None, None) => Ok(None),
        (Some(checksum), Some(mime_type), Some(byte_size)) => Ok(Some(RevisionAdmission {
            content_source_kind: payload
                .content_source_kind
                .clone()
                .unwrap_or_else(|| "upload".to_string()),
            replacement: ironrag_contracts::documents::ReplacementIdentity {
                file_name: payload.file_name.clone().ok_or_else(|| {
                    ApiError::BadRequest(
                        "fileName is required for a complete replacement".to_owned(),
                    )
                })?,
                checksum: checksum.to_string(),
                mime_type: mime_type.to_string(),
                byte_size,
                title: payload.title.clone(),
                language_code: payload.language_code.clone(),
                source_uri: payload.source_uri.clone(),
                document_hint: payload.document_hint.clone(),
            },
            visibility_intent_id: None,
            base_revision_id: None,
            storage_key: payload.storage_key.clone(),
        })),
        _ => Err(ApiError::BadRequest(
            "checksum, mimeType, and byteSize must be provided together".to_string(),
        )),
    }
}

pub(super) fn build_reprocess_revision_metadata(
    active_revision: &ContentRevision,
    source: ReprocessRevisionSource,
) -> RevisionAdmission {
    RevisionAdmission {
        content_source_kind: active_revision.content_source_kind.clone(),
        replacement: ironrag_contracts::documents::ReplacementIdentity {
            file_name: source.file_name,
            checksum: source.checksum,
            mime_type: source.mime_type,
            byte_size: source.byte_size,
            title: source.title,
            language_code: active_revision.language_code.clone(),
            source_uri: source.source_uri,
            document_hint: active_revision.document_hint.clone(),
        },
        visibility_intent_id: None,
        base_revision_id: None,
        storage_key: Some(source.storage_key),
    }
}

/// Builds revision metadata for the web-retry path, where the caller just
/// re-fetched the source URL and wants the new mutation to reference the
/// fresh blob instead of the previous capture. `checksum`, `byte_size`,
/// `storage_key` and `mime_type` come from the live fetch; the rest (title,
/// language, and the `source_uri` itself) is carried forward from the previous
/// revision so downstream identity normalization stays stable across retries.
pub(super) fn build_web_refetch_revision_metadata(
    active_revision: &ContentRevision,
    refetched: RefetchedWebDocumentSource,
) -> RevisionAdmission {
    RevisionAdmission {
        content_source_kind: active_revision.content_source_kind.clone(),
        replacement: ironrag_contracts::documents::ReplacementIdentity {
            file_name: refetched.file_name,
            checksum: refetched.checksum,
            mime_type: refetched.mime_type.unwrap_or_else(|| active_revision.mime_type.clone()),
            byte_size: refetched.byte_size,
            title: active_revision.title.clone(),
            language_code: active_revision.language_code.clone(),
            source_uri: active_revision.source_uri.clone(),
            document_hint: active_revision.document_hint.clone(),
        },
        visibility_intent_id: None,
        base_revision_id: None,
        storage_key: Some(refetched.storage_key),
    }
}

pub(super) fn map_mutation_admission(
    admission: ContentMutationAdmission,
) -> ContentMutationDetailResponse {
    ContentMutationDetailResponse {
        mutation: admission.mutation,
        items: admission.items,
        job_id: admission.job_id,
        async_operation_id: admission.async_operation_id,
        operation_status: admission.operation_status,
        result_revision_id: admission.result_revision_id,
        result_checksum: admission.result_checksum,
        request_fingerprint: admission.request_fingerprint,
    }
}

pub(super) fn normalize_page_window(offset: Option<usize>, limit: Option<usize>) -> (usize, usize) {
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(100).clamp(1, 500);
    (offset, limit)
}

pub(super) fn paginate_items<T>(items: Vec<T>, offset: usize, limit: usize) -> Vec<T> {
    items.into_iter().skip(offset).take(limit).collect()
}

pub(crate) fn normalize_optional_text(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(crate) fn normalize_optional_document_hint(
    value: Option<String>,
) -> Result<Option<String>, ApiError> {
    let Some(value) = value.and_then(normalize_optional_text) else {
        return Ok(None);
    };
    if value.chars().count() > 1024 {
        return Err(ApiError::BadRequest(
            "document_hint must be at most 1024 characters".to_string(),
        ));
    }
    Ok(Some(value))
}

// ============================================================================
// Opaque cursor for /v1/content/documents keyset pagination.
//
// The cursor is base64(json) and carries the stable ordering tuple for the
// active server-side sort: primary value/null bucket plus created_at and id.
// It is opaque from the client's perspective and only valid against the
// server version that produced it. Any decode failure is surfaced as a
// `BadRequest` — callers are expected to drop the cursor and start from
// the top instead of pretending the page succeeded.
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DocumentListCursor {
    #[serde(rename = "t")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "i")]
    pub document_id: Uuid,
    #[serde(rename = "s", default, skip_serializing_if = "Option::is_none")]
    pub text_value: Option<String>,
    #[serde(rename = "n", default, skip_serializing_if = "Option::is_none")]
    pub number_value: Option<i64>,
    #[serde(rename = "f", default, skip_serializing_if = "Option::is_none")]
    pub finished_value: Option<DateTime<Utc>>,
    #[serde(rename = "z", default)]
    pub value_is_null: bool,
    /// Sort key this cursor was produced for. A cursor is only valid for the
    /// same effective sort.
    #[serde(rename = "k")]
    pub sort: DocumentListSortKey,
    /// Sort direction this cursor was produced for.
    #[serde(rename = "d")]
    pub sort_desc: bool,
    /// Snapshot instant the page was computed against; carried so subsequent
    /// pages (and the facets endpoint) reuse the exact same clock.
    #[serde(rename = "q")]
    pub snapshot_at: DateTime<Utc>,
}

fn cursor_mismatch() -> ApiError {
    ApiError::BadRequest("cursor value does not match the requested sort".to_string())
}

impl DocumentListCursor {
    /// Reject a cursor that was produced for a different sort, or whose primary
    /// value has the wrong shape for the requested sort — before any Postgres
    /// cast can turn a malformed payload into a 500.
    pub(super) fn validate_for_sort(
        &self,
        sort: DocumentListSortKey,
        sort_desc: bool,
    ) -> Result<(), ApiError> {
        if self.sort != sort || self.sort_desc != sort_desc {
            return Err(ApiError::BadRequest(
                "cursor does not match the requested sort".to_string(),
            ));
        }
        if matches!(sort, DocumentListSortKey::Cost) {
            return self.validate_cost_value();
        }
        if self.has_wrong_shape(sort) {
            return Err(cursor_mismatch());
        }
        Ok(())
    }

    /// Which primary column may carry a value for this sort. A cursor whose
    /// primary field does not match the requested sort is rejected before the
    /// value ever reaches a Postgres cast.
    fn has_wrong_shape(&self, sort: DocumentListSortKey) -> bool {
        match sort {
            DocumentListSortKey::UploadedAt => self.uploaded_at_shape_wrong(),
            DocumentListSortKey::FileName
            | DocumentListSortKey::FileType
            | DocumentListSortKey::Status => self.text_shape_wrong(),
            DocumentListSortKey::FileSize | DocumentListSortKey::PipelineTime => {
                self.number_shape_wrong()
            }
            DocumentListSortKey::FinishedAt => self.finished_shape_wrong(),
            // Cost carries a decimal string and is validated separately.
            DocumentListSortKey::Cost => false,
        }
    }

    fn uploaded_at_shape_wrong(&self) -> bool {
        self.value_is_null
            || self.text_value.is_some()
            || self.number_value.is_some()
            || self.finished_value.is_some()
    }

    fn text_shape_wrong(&self) -> bool {
        self.number_value.is_some()
            || self.finished_value.is_some()
            || (!self.value_is_null && self.text_value.is_none())
    }

    fn number_shape_wrong(&self) -> bool {
        self.text_value.is_some()
            || self.finished_value.is_some()
            || (!self.value_is_null && self.number_value.is_none())
    }

    fn finished_shape_wrong(&self) -> bool {
        self.text_value.is_some()
            || self.number_value.is_some()
            || (!self.value_is_null && self.finished_value.is_none())
    }

    /// A cost cursor carries its exact decimal text; the caller has already
    /// ruled out the numeric and timestamp columns. The text must parse as a
    /// decimal before it can be cast.
    fn validate_cost_value(&self) -> Result<(), ApiError> {
        if self.number_value.is_some() || self.finished_value.is_some() {
            return Err(cursor_mismatch());
        }
        if self.value_is_null {
            return Ok(());
        }
        let raw = self.text_value.as_deref().ok_or_else(cursor_mismatch)?;
        <rust_decimal::Decimal as std::str::FromStr>::from_str(raw)
            .map_err(|_| cursor_mismatch())?;
        Ok(())
    }
}

pub(super) fn encode_document_list_cursor(cursor: &DocumentListCursor) -> String {
    use base64::Engine;
    // `DocumentListCursor` is a plain struct of `DateTime<Utc>` + `Uuid`, both
    // of which have infallible `Serialize` impls — to_vec can only fail on
    // I/O errors which `Vec<u8>` never produces. A failure here would mean a
    // serde_json upstream regression, which is far out of scope for a cursor
    // encoder; fall back to an empty token so the caller keeps paginating
    // rather than panicking on the hot path.
    let json = serde_json::to_vec(cursor).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

pub(super) fn decode_document_list_cursor(token: &str) -> Result<DocumentListCursor, ApiError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::BadRequest("invalid cursor encoding".to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::BadRequest("invalid cursor payload".to_string()))
}

// ============================================================================
// Strictly typed per-column grid filters.
//
// The wire form is a JSON object (`filters` query parameter). Each column is a
// nullable array: an absent key, whole-key `null` or `[]` means "no
// constraint"; a `null` element selects the column's own NULL bucket; several
// values are OR within the column and AND across columns.
// ============================================================================

#[derive(Debug, Clone, Default, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentColumnFilters {
    pub file_name: Option<Vec<Option<String>>>,
    pub file_type: Option<Vec<Option<String>>>,
    pub file_size: Option<Vec<Option<i64>>>,
    pub uploaded_at: Option<Vec<Option<String>>>,
    pub status: Option<Vec<Option<DocumentStatus>>>,
    pub cost: Option<Vec<Option<String>>>,
    pub pipeline_time: Option<Vec<Option<i64>>>,
    pub finished_at: Option<Vec<Option<String>>>,
}

const MAX_FILTER_VALUES_PER_COLUMN: usize = 200;

fn filter_scalar_values<T>(values: Option<Vec<Option<T>>>) -> Result<FilterValues<T>, ApiError> {
    let Some(values) = values else {
        return Ok(FilterValues::default());
    };
    if values.len() > MAX_FILTER_VALUES_PER_COLUMN {
        return Err(ApiError::BadRequest(format!(
            "too many values in a column filter; maximum is {MAX_FILTER_VALUES_PER_COLUMN}"
        )));
    }
    let present = !values.is_empty();
    let include_null = values.iter().any(Option::is_none);
    let values = values.into_iter().flatten().collect();
    Ok(FilterValues { present, include_null, values })
}

fn filter_timestamp_values(
    values: Option<Vec<Option<String>>>,
) -> Result<FilterValues<DateTime<Utc>>, ApiError> {
    let Some(values) = values else {
        return Ok(FilterValues::default());
    };
    if values.len() > MAX_FILTER_VALUES_PER_COLUMN {
        return Err(ApiError::BadRequest(format!(
            "too many values in a column filter; maximum is {MAX_FILTER_VALUES_PER_COLUMN}"
        )));
    }
    let present = !values.is_empty();
    let include_null = values.iter().any(Option::is_none);
    let mut parsed = Vec::new();
    for value in values.into_iter().flatten() {
        let timestamp = DateTime::parse_from_rfc3339(&value)
            .map_err(|_| {
                ApiError::BadRequest(format!("invalid RFC3339 timestamp `{value}` in filters"))
            })?
            .with_timezone(&Utc);
        parsed.push(timestamp);
    }
    Ok(FilterValues { present, include_null, values: parsed })
}

fn filter_decimal_values(
    values: Option<Vec<Option<String>>>,
) -> Result<FilterValues<rust_decimal::Decimal>, ApiError> {
    let Some(values) = values else {
        return Ok(FilterValues::default());
    };
    if values.len() > MAX_FILTER_VALUES_PER_COLUMN {
        return Err(ApiError::BadRequest(format!(
            "too many values in a column filter; maximum is {MAX_FILTER_VALUES_PER_COLUMN}"
        )));
    }
    let present = !values.is_empty();
    let include_null = values.iter().any(Option::is_none);
    let mut parsed = Vec::new();
    for value in values.into_iter().flatten() {
        let decimal =
            <rust_decimal::Decimal as std::str::FromStr>::from_str(&value).map_err(|_| {
                ApiError::BadRequest(format!("invalid decimal cost `{value}` in filters"))
            })?;
        parsed.push(decimal);
    }
    Ok(FilterValues { present, include_null, values: parsed })
}

impl DocumentColumnFilters {
    pub(super) fn into_repository(self) -> Result<DocumentListFilters, ApiError> {
        let status = self.status.map(|values| {
            values
                .into_iter()
                .map(|value| value.map(|status| status.as_str().to_string()))
                .collect()
        });
        Ok(DocumentListFilters {
            file_name: filter_scalar_values(self.file_name)?,
            file_type: filter_scalar_values(self.file_type)?,
            file_size: filter_scalar_values(self.file_size)?,
            uploaded_at: filter_timestamp_values(self.uploaded_at)?,
            status: filter_scalar_values(status)?,
            cost: filter_decimal_values(self.cost)?,
            pipeline_time: filter_scalar_values(self.pipeline_time)?,
            finished_at: filter_timestamp_values(self.finished_at)?,
        })
    }
}

// ============================================================================
// Typed search ranges.
//
// An optional JSON object (`searchRanges` query parameter). Each present range
// is an OR alternative to the literal / normalized / status search terms —
// never an AND with them — while the grid's column filters and the status/id
// scope remain ANDed. Bounds are half-open `[min, max)`.
// ============================================================================

#[derive(Debug, Clone, Default, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentSearchRanges {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<DocumentIntRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_time: Option<DocumentIntRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<DocumentDateRange>,
}

/// Integer bound, inclusive lower / exclusive upper. Both endpoints are
/// nonnegative and `min <= max`; a value outside `i64` fails deserialization.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentIntRange {
    pub min: i64,
    pub max: i64,
}

/// RFC3339 UTC bound, inclusive lower / exclusive upper, `min <= max`.
#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentDateRange {
    pub min: String,
    pub max: String,
}

fn parse_rfc3339_bound(value: &str, field: &str) -> Result<DateTime<Utc>, ApiError> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| ApiError::BadRequest(format!("invalid RFC3339 {field} bound `{value}`")))
}

impl DocumentSearchRanges {
    pub(super) fn into_repository(self) -> Result<DocumentListSearchRanges, ApiError> {
        fn int_range(range: DocumentIntRange, field: &str) -> Result<(i64, i64), ApiError> {
            if range.min < 0 || range.max < 0 {
                return Err(ApiError::BadRequest(format!(
                    "{field} range bounds must be nonnegative"
                )));
            }
            if range.min > range.max {
                return Err(ApiError::BadRequest(format!("{field} range is reversed (min > max)")));
            }
            Ok((range.min, range.max))
        }
        let date = match self.date {
            None => None,
            Some(range) => {
                let min = parse_rfc3339_bound(&range.min, "date")?;
                let max = parse_rfc3339_bound(&range.max, "date")?;
                if min > max {
                    return Err(ApiError::BadRequest(
                        "date range is reversed (min > max)".to_string(),
                    ));
                }
                Some((min, max))
            }
        };
        Ok(DocumentListSearchRanges {
            file_size: self.file_size.map(|range| int_range(range, "fileSize")).transpose()?,
            pipeline_time: self
                .pipeline_time
                .map(|range| int_range(range, "pipelineTime"))
                .transpose()?,
            date,
        })
    }
}

// ============================================================================
// Facet surface for the documents data grid.
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFacetColumn {
    UploadedAt,
    FileName,
    FileType,
    FileSize,
    Status,
    Cost,
    PipelineTime,
    FinishedAt,
}

impl DocumentFacetColumn {
    pub(super) const fn to_repository(
        self,
    ) -> crate::infra::repositories::content_document_list_repository::DocumentFacetColumn {
        use crate::infra::repositories::content_document_list_repository::DocumentFacetColumn as Repo;
        match self {
            Self::UploadedAt => Repo::UploadedAt,
            Self::FileName => Repo::FileName,
            Self::FileType => Repo::FileType,
            Self::FileSize => Repo::FileSize,
            Self::Status => Repo::Status,
            Self::Cost => Repo::Cost,
            Self::PipelineTime => Repo::PipelineTime,
            Self::FinishedAt => Repo::FinishedAt,
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListDocumentFacetsQuery {
    pub facet_column: DocumentFacetColumn,
    pub include_deleted: Option<bool>,
    pub filters: Option<String>,
    pub search: Option<String>,
    pub search_status: Option<String>,
    pub normalized_search: Option<String>,
    /// The list page's status pill, forwarded verbatim so the facet keeps the
    /// same pill scope while still self-excluding the grid's own `status`
    /// column filter.
    pub status: Option<String>,
    pub ids: Option<String>,
    pub indexing: Option<DocumentIndexingFilter>,
    /// Snapshot instant. Optional on the first page; on a cursor page it must
    /// equal the cursor's snapshot exactly.
    pub snapshot_at: Option<DateTime<Utc>>,
    /// Typed range hints (same canonical `DocumentSearchRanges` as the list).
    pub search_ranges: Option<String>,
    /// Typed hint that matches this facet column's OWN value (OR with
    /// `facetSearch`): Size/Duration/Uploaded/Finished ranges.
    pub facet_search_ranges: Option<String>,
    /// Exact derived-status buckets a Status-facet value hint matches.
    pub facet_search_status: Option<String>,
    pub facet_search: Option<String>,
    pub facet_cursor: Option<String>,
    pub facet_limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum FacetValue {
    Text(String),
    Number(i64),
    Null(()),
}

impl utoipa::PartialSchema for FacetValue {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, Schema, SchemaType, Type};
        utoipa::openapi::RefOr::T(Schema::Object(
            ObjectBuilder::new()
                .schema_type(SchemaType::from_iter([Type::String, Type::Integer, Type::Null]))
                .build(),
        ))
    }
}

impl utoipa::ToSchema for FacetValue {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("FacetValue")
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFacetValue {
    pub value: FacetValue,
    pub count: i64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFacetsResponse {
    pub column: DocumentFacetColumn,
    pub values: Vec<DocumentFacetValue>,
    /// `null` when the last page has been reached.
    pub next_cursor: Option<String>,
    pub snapshot_at: DateTime<Utc>,
    /// Whether this facet result was withheld because it reads cost while the
    /// library rollup reconciles. `true` means the cost-dependent facet could
    /// not be answered exactly — the facet column is `cost`, or the scope
    /// carries an effective cost filter or literal/normalized search — so
    /// `values` is empty and `nextCursor` is absent. A non-cost facet on a
    /// dirty rollup is `false` with real values and a truthful reconciling
    /// freshness.
    pub scope_pending: bool,
    /// Freshness of the library billing rollup. `reconciling` means the facet
    /// could not be resolved exactly if it reads cost: `values` is empty and
    /// `nextCursor` is absent (not "no values"), so a cost-dependent facet is
    /// never an arbitrary stale distribution. Independent of pricing coverage.
    pub cost_rollup_freshness: CostRollupFreshness,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentFacetCursor {
    #[serde(rename = "q")]
    pub snapshot_at: DateTime<Utc>,
    #[serde(rename = "c")]
    pub column: DocumentFacetColumn,
    #[serde(rename = "v", default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(rename = "n", default, skip_serializing_if = "Option::is_none")]
    pub number_value: Option<i64>,
    #[serde(rename = "z", default)]
    pub value_is_null: bool,
}

fn facet_cursor_mismatch() -> ApiError {
    ApiError::BadRequest("facet cursor value does not match its column".to_string())
}

impl DocumentFacetCursor {
    pub(super) fn to_repository(
        &self,
    ) -> Result<
        crate::infra::repositories::content_document_list_repository::DocumentFacetCursorValue,
        ApiError,
    > {
        use crate::infra::repositories::content_document_list_repository::DocumentFacetCursorValue;
        if self.value_is_null {
            return Ok(DocumentFacetCursorValue::Null);
        }
        match self.column {
            DocumentFacetColumn::FileName
            | DocumentFacetColumn::FileType
            | DocumentFacetColumn::Status => self
                .value
                .clone()
                .map(DocumentFacetCursorValue::Text)
                .ok_or_else(facet_cursor_mismatch),
            DocumentFacetColumn::FileSize | DocumentFacetColumn::PipelineTime => self
                .number_value
                .map(DocumentFacetCursorValue::Number)
                .ok_or_else(facet_cursor_mismatch),
            DocumentFacetColumn::UploadedAt | DocumentFacetColumn::FinishedAt => {
                let value = self.value.as_deref().ok_or_else(facet_cursor_mismatch)?;
                let timestamp = DateTime::parse_from_rfc3339(value)
                    .map_err(|_| facet_cursor_mismatch())?
                    .with_timezone(&Utc);
                Ok(DocumentFacetCursorValue::Timestamp(timestamp))
            }
            DocumentFacetColumn::Cost => {
                let value = self.value.as_deref().ok_or_else(facet_cursor_mismatch)?;
                let decimal = <rust_decimal::Decimal as std::str::FromStr>::from_str(value)
                    .map_err(|_| facet_cursor_mismatch())?;
                Ok(DocumentFacetCursorValue::Decimal(decimal))
            }
        }
    }
}

pub(super) fn encode_document_facet_cursor(cursor: &DocumentFacetCursor) -> String {
    use base64::Engine;
    let json = serde_json::to_vec(cursor).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

pub(super) fn decode_document_facet_cursor(token: &str) -> Result<DocumentFacetCursor, ApiError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::BadRequest("invalid facet cursor encoding".to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::BadRequest("invalid facet cursor payload".to_string()))
}
