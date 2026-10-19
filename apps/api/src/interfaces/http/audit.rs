use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::audit::{
        AuditAssistantCallSummary, AuditAssistantModel, AuditEventInternalView,
        AuditEventRedactedView, AuditEventSubject,
    },
    infra::repositories::{audit_repository, iam_repository},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_MCP_AUDIT_REVIEW, POLICY_USAGE_READ, authorize_mcp_audit_review,
            load_library_and_authorize,
        },
        router_support::ApiError,
    },
    services::iam::audit::{AuditEventPage, ListAuditEventSubjectFilter, ListAuditEventsQuery},
};

const DEFAULT_AUDIT_LIMIT: u32 = 50;
const MAX_AUDIT_LIMIT: u32 = 1000;
// The facet menu is a menu, not an export: a bounded page of the most common
// values, with `hasMore` for the rest and a server-side value search.
const DEFAULT_AUDIT_FACET_LIMIT: u32 = 50;
const MAX_AUDIT_FACET_LIMIT: u32 = 100;
const MAX_AUDIT_FACET_OFFSET: u32 = 10_000;

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct AuditEventsQuery {
    /// Comma-separated actor principal ids (the actor column's own values).
    pub actor: Option<String>,
    /// Include events whose actor principal is absent (the null facet bucket).
    pub actor_null: Option<bool>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub context_bundle_id: Option<Uuid>,
    pub query_session_id: Option<Uuid>,
    pub query_execution_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    /// Comma-separated action kinds.
    pub action_kind: Option<String>,
    /// Comma-separated surface kinds.
    pub surface_kind: Option<String>,
    /// Comma-separated result kinds.
    pub result_kind: Option<String>,
    /// Comma-separated exact `createdAt` renderings (the time column's own
    /// values, round-tripped verbatim).
    pub created_at: Option<String>,
    pub search: Option<String>,
    /// JSON `AuditSearchHintsInput` derived from displayed labels, OR'd with
    /// the raw search over the whole authorized dataset.
    pub search_hints: Option<String>,
    /// Column id: `created_at`, `actor`, `action`, `surface` or `result`.
    pub sort_by: Option<String>,
    /// `asc` or `desc`.
    pub sort_order: Option<String>,
    pub limit: Option<u32>,
    /// Opaque keyset continuation token from a previous page's
    /// `nextCursor`. Absent starts from the newest event.
    pub cursor: Option<String>,
    pub internal: Option<bool>,
    pub include_assistant: Option<bool>,
}

/// Split a comma-separated query parameter into its non-empty, trimmed parts.
fn split_csv(raw: Option<&str>) -> Vec<String> {
    raw.map(|value| {
        value
            .split(',')
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// Resolve the requested ordering, rejecting an unknown column or direction
/// instead of silently falling back to the default.
fn parse_audit_sort(
    sort_by: Option<&str>,
    sort_order: Option<&str>,
) -> Result<audit_repository::AuditSort, ApiError> {
    let default = audit_repository::AuditSort::default();
    let key = match sort_by {
        None => default.key,
        Some(raw) => audit_repository::AuditSortKey::parse(raw)
            .ok_or_else(|| ApiError::BadRequest(format!("unknown sortBy `{raw}`")))?,
    };
    let order = match sort_order {
        None => default.order,
        Some(raw) => audit_repository::AuditSortOrder::parse(raw)
            .ok_or_else(|| ApiError::BadRequest(format!("unknown sortOrder `{raw}`")))?,
    };
    Ok(audit_repository::AuditSort { key, order })
}

/// A displayed date/time interval, with an exclusive upper bound.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditCreatedAtRangeInput {
    pub min: chrono::DateTime<chrono::Utc>,
    pub max: chrono::DateTime<chrono::Utc>,
}

/// One canonical hint contract for global and column-local facet searches.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AuditSearchHintsInput {
    pub action_kinds: Vec<String>,
    pub surface_kinds: Vec<String>,
    pub result_kinds: Vec<String>,
    pub actor_null: bool,
    pub created_at_range: Option<AuditCreatedAtRangeInput>,
}

fn audit_search_hints(raw: Option<&str>) -> Result<audit_repository::AuditSearchHints, ApiError> {
    let Some(raw) = raw else {
        return Ok(audit_repository::AuditSearchHints::default());
    };
    if raw.len() > 32_768 {
        return Err(ApiError::BadRequest("audit search hints are too large".to_string()));
    }
    let hints: AuditSearchHintsInput = serde_json::from_str(raw)
        .map_err(|_| ApiError::BadRequest("invalid audit search hints".to_string()))?;
    for values in [&hints.action_kinds, &hints.surface_kinds, &hints.result_kinds] {
        if values.len() > 200 || values.iter().any(|value| value.is_empty() || value.len() > 128) {
            return Err(ApiError::BadRequest(
                "audit search hint values exceed their bounds".to_string(),
            ));
        }
    }
    if hints.created_at_range.as_ref().is_some_and(|range| range.min >= range.max) {
        return Err(ApiError::BadRequest(
            "audit search date interval must be increasing".to_string(),
        ));
    }
    let (created_at_from, created_at_to) =
        hints.created_at_range.map_or((None, None), |range| (Some(range.min), Some(range.max)));
    Ok(audit_repository::AuditSearchHints {
        action_kinds: hints.action_kinds,
        surface_kinds: hints.surface_kinds,
        result_kinds: hints.result_kinds,
        actor_null: hints.actor_null,
        created_at_from,
        created_at_to,
    })
}

fn audit_facet_search_hints(
    column: audit_repository::AuditFacetColumn,
    raw: Option<&str>,
) -> Result<audit_repository::AuditSearchHints, ApiError> {
    use audit_repository::AuditFacetColumn;
    let hints = audit_search_hints(raw)?;
    if (!hints.action_kinds.is_empty() && column != AuditFacetColumn::Action)
        || (!hints.surface_kinds.is_empty() && column != AuditFacetColumn::Surface)
        || (!hints.result_kinds.is_empty() && column != AuditFacetColumn::Result)
        || (hints.actor_null && column != AuditFacetColumn::Actor)
        || (hints.created_at_from.is_some() && column != AuditFacetColumn::CreatedAt)
    {
        return Err(ApiError::BadRequest(
            "facet hints must belong to the requested audit column".to_string(),
        ));
    }
    Ok(hints)
}

/// Pair the selected columns with their values; a count mismatch or an unknown
/// column is a `BadRequest`, never a silently ignored label request.
fn audit_selected_values(
    columns: Option<&str>,
    values: Option<&str>,
) -> Result<Vec<(audit_repository::AuditFacetColumn, String)>, ApiError> {
    let columns = split_csv(columns);
    let values = split_csv(values);
    if columns.is_empty() && values.is_empty() {
        return Ok(Vec::new());
    }
    if columns.len() != values.len() {
        return Err(ApiError::BadRequest(
            "selectedValueColumn and selectedValue must have the same length".to_string(),
        ));
    }
    if columns.len() > 200 || values.iter().any(|value| value.len() > 128) {
        return Err(ApiError::BadRequest("selected audit values exceed their bounds".to_string()));
    }
    columns
        .iter()
        .zip(values)
        .map(|(column, value)| {
            audit_repository::AuditFacetColumn::parse(column)
                .map(|parsed| (parsed, value))
                .ok_or_else(|| {
                    ApiError::BadRequest(format!("unknown selectedValueColumn `{column}`"))
                })
        })
        .collect()
}

fn audit_multi_filters(
    actor: Option<&str>,
    actor_null: Option<bool>,
    action_kind: Option<&str>,
    surface_kind: Option<&str>,
    result_kind: Option<&str>,
    created_at: Option<&str>,
) -> audit_repository::AuditEventMultiFilters {
    audit_repository::AuditEventMultiFilters {
        actor_principal_ids: split_csv(actor),
        actor_include_null: actor_null.unwrap_or(false),
        action_kinds: split_csv(action_kind),
        surface_kinds: split_csv(surface_kind),
        result_kinds: split_csv(result_kind),
        created_at_texts: split_csv(created_at),
    }
}

// ============================================================================
// Opaque cursor for /v1/audit/events keyset pagination.
//
// The cursor is base64(json({"k": <sortBy>, "o": <sortOrder>, "v": <value>,
// "i": "<uuid>"})). It pins the ordering it was minted under: a cursor
// decoded against a different `sortBy`/`sortOrder` is a `BadRequest` rather
// than a silently wrong page. `v` is absent when the ordered column was NULL.
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
struct AuditEventListCursor {
    #[serde(rename = "k")]
    sort_by: String,
    #[serde(rename = "o")]
    sort_order: String,
    #[serde(rename = "v", default, skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(rename = "i")]
    id: Uuid,
}

fn encode_audit_event_cursor(cursor: &AuditEventListCursor) -> String {
    use base64::Engine;
    let json = serde_json::to_vec(cursor).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

fn decode_audit_event_cursor(
    token: &str,
    sort: audit_repository::AuditSort,
) -> Result<audit_repository::AuditListCursorValue, ApiError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::BadRequest("invalid cursor encoding".to_string()))?;
    let decoded: AuditEventListCursor = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::BadRequest("invalid cursor payload".to_string()))?;
    if decoded.sort_by != sort.key.as_str() || decoded.sort_order != sort.order.as_str() {
        return Err(ApiError::BadRequest("cursor does not match the requested sort".to_string()));
    }
    let value = match decoded.value {
        None => None,
        Some(raw) if sort.key.is_time() => {
            let timestamp = chrono::DateTime::parse_from_rfc3339(&raw)
                .map_err(|_| ApiError::BadRequest("invalid cursor timestamp".to_string()))?
                .with_timezone(&chrono::Utc);
            Some(audit_repository::AuditCursorValue::Time(timestamp))
        }
        Some(raw) => Some(audit_repository::AuditCursorValue::Text(raw)),
    };
    Ok(audit_repository::AuditListCursorValue { value, id: decoded.id })
}

fn encode_audit_page_cursor(
    sort: audit_repository::AuditSort,
    cursor: &audit_repository::AuditListCursorValue,
) -> String {
    encode_audit_event_cursor(&AuditEventListCursor {
        sort_by: sort.key.as_str().to_string(),
        sort_order: sort.order.as_str().to_string(),
        value: cursor.value.as_ref().map(audit_repository::AuditCursorValue::as_text),
        id: cursor.id,
    })
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditEventSubjectResponse {
    pub audit_event_id: Uuid,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub query_session_id: Option<Uuid>,
    pub query_execution_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub context_bundle_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditActorPrincipalResponse {
    pub id: Uuid,
    pub principal_kind: String,
    pub status: String,
    pub display_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditAssistantModelResponse {
    pub provider_kind: String,
    pub model_name: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditAssistantCallResponse {
    pub query_execution_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub models: Vec<AuditAssistantModelResponse>,
    pub total_cost: Option<Decimal>,
    pub currency_code: Option<String>,
    pub provider_call_count: i64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditEventResponse {
    pub id: Uuid,
    pub actor_principal_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_principal: Option<AuditActorPrincipalResponse>,
    pub surface_kind: String,
    pub action_kind: String,
    pub result_kind: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub redacted_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub internal_message: Option<String>,
    pub subjects: Vec<AuditEventSubjectResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_call: Option<AuditAssistantCallResponse>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditEventPageResponse {
    pub items: Vec<AuditEventResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub total: i64,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/audit/events", get(list_audit_events))
        .route("/audit/events/facets", get(list_audit_event_facets))
}

/// The one authorization path for both audit reads: the internal view is
/// system-admin only, a library is authorized and folded into its workspace, and
/// a library/workspace mismatch is refused rather than silently narrowed.
async fn resolve_audit_scope(
    auth: &AuthContext,
    state: &AppState,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    internal: bool,
) -> Result<(Option<Uuid>, Option<Uuid>), ApiError> {
    if internal && !auth.is_system_admin {
        return Err(ApiError::forbidden(
            "internal audit view requires system administrator access",
        ));
    }

    let mut workspace_filter = if auth.is_system_admin {
        workspace_id
    } else {
        authorize_mcp_audit_review(auth, workspace_id)?
    };

    let library_filter = if let Some(library_id) = library_id {
        let library =
            load_library_and_authorize(auth, state, library_id, POLICY_MCP_AUDIT_REVIEW).await?;
        if let Some(workspace_id) = workspace_filter
            && workspace_id != library.workspace_id
        {
            return Err(ApiError::BadRequest(
                "libraryId does not belong to workspaceId".to_string(),
            ));
        }
        workspace_filter = Some(library.workspace_id);
        Some(library.id)
    } else {
        None
    };

    Ok((workspace_filter, library_filter))
}

/// The one resolution of the authorized scope, the column filters and the
/// derived search hints that both audit reads share. Keeping it here means the
/// list and its facets can never disagree about which events are in view.
async fn resolve_audit_read(
    auth: &AuthContext,
    state: &AppState,
    query: AuditReadQuery<'_>,
) -> Result<
    (
        Option<Uuid>,
        Option<Uuid>,
        bool,
        audit_repository::AuditEventMultiFilters,
        audit_repository::AuditSearchHints,
    ),
    ApiError,
> {
    let internal = query.internal.unwrap_or(false);
    let (workspace_filter, library_filter) =
        resolve_audit_scope(auth, state, query.workspace_id, query.library_id, internal).await?;
    let filters = audit_multi_filters(
        query.actor,
        query.actor_null,
        query.action_kind,
        query.surface_kind,
        query.result_kind,
        query.created_at,
    );
    let search_hints = audit_search_hints(query.search_hints)?;
    Ok((workspace_filter, library_filter, internal, filters, search_hints))
}

/// The query fields both audit reads resolve identically.
struct AuditReadQuery<'a> {
    internal: Option<bool>,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    actor: Option<&'a str>,
    actor_null: Option<bool>,
    action_kind: Option<&'a str>,
    surface_kind: Option<&'a str>,
    result_kind: Option<&'a str>,
    created_at: Option<&'a str>,
    search_hints: Option<&'a str>,
}

/// Both audit query DTOs share these fields verbatim, so the read resolution
/// borrows them through one accessor rather than two copy-pasted blocks.
trait AuditReadFields {
    fn internal(&self) -> Option<bool>;
    fn workspace_id(&self) -> Option<Uuid>;
    fn library_id(&self) -> Option<Uuid>;
    fn actor(&self) -> Option<&str>;
    fn actor_null(&self) -> Option<bool>;
    fn action_kind(&self) -> Option<&str>;
    fn surface_kind(&self) -> Option<&str>;
    fn result_kind(&self) -> Option<&str>;
    fn created_at(&self) -> Option<&str>;
    fn search_hints(&self) -> Option<&str>;
}

macro_rules! audit_read_fields {
    ($type:ty) => {
        impl AuditReadFields for $type {
            fn internal(&self) -> Option<bool> {
                self.internal
            }
            fn workspace_id(&self) -> Option<Uuid> {
                self.workspace_id
            }
            fn library_id(&self) -> Option<Uuid> {
                self.library_id
            }
            fn actor(&self) -> Option<&str> {
                self.actor.as_deref()
            }
            fn actor_null(&self) -> Option<bool> {
                self.actor_null
            }
            fn action_kind(&self) -> Option<&str> {
                self.action_kind.as_deref()
            }
            fn surface_kind(&self) -> Option<&str> {
                self.surface_kind.as_deref()
            }
            fn result_kind(&self) -> Option<&str> {
                self.result_kind.as_deref()
            }
            fn created_at(&self) -> Option<&str> {
                self.created_at.as_deref()
            }
            fn search_hints(&self) -> Option<&str> {
                self.search_hints.as_deref()
            }
        }
    };
}

audit_read_fields!(AuditEventsQuery);
audit_read_fields!(AuditEventFacetsQuery);

impl<'a, T: AuditReadFields> From<&'a T> for AuditReadQuery<'a> {
    fn from(query: &'a T) -> Self {
        Self {
            internal: query.internal(),
            workspace_id: query.workspace_id(),
            library_id: query.library_id(),
            actor: query.actor(),
            actor_null: query.actor_null(),
            action_kind: query.action_kind(),
            surface_kind: query.surface_kind(),
            result_kind: query.result_kind(),
            created_at: query.created_at(),
            search_hints: query.search_hints(),
        }
    }
}

#[utoipa::path(
    get,
    path = "/v1/audit/events",
    tag = "audit",
    operation_id = "listAuditEvents",
    params(AuditEventsQuery),
    responses(
        (status = 200, description = "Audit events page", body = AuditEventPageResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the requested scope"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_audit_events",
    skip_all,
    fields(workspace_id = ?query.workspace_id, library_id = ?query.library_id, item_count)
)]
pub async fn list_audit_events(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AuditEventsQuery>,
) -> Result<Json<AuditEventPageResponse>, ApiError> {
    let span = tracing::Span::current();
    let internal = query.internal.unwrap_or(false);
    let (workspace_filter, library_filter, authorized_internal, filters, search_hints) =
        resolve_audit_read(&auth, &state, AuditReadQuery::from(&query)).await?;
    let subject_filter = ListAuditEventSubjectFilter {
        knowledge_document_id: query.knowledge_document_id,
        knowledge_revision_id: query.knowledge_revision_id,
        context_bundle_id: query.context_bundle_id,
        query_session_id: query.query_session_id,
        query_execution_id: query.query_execution_id,
        runtime_execution_id: query.runtime_execution_id,
        async_operation_id: query.async_operation_id,
    };
    let sort = parse_audit_sort(query.sort_by.as_deref(), query.sort_order.as_deref())?;
    let cursor = match query.cursor.as_deref() {
        Some(token) => Some(decode_audit_event_cursor(token, sort)?),
        None => None,
    };
    let list_query = ListAuditEventsQuery {
        workspace_id: workspace_filter,
        library_id: library_filter,
        subject_filter,
        filters,
        search: query.search.filter(|value| !value.trim().is_empty()),
        internal: authorized_internal,
        search_hints,
        sort,
        limit: i64::from(query.limit.unwrap_or(DEFAULT_AUDIT_LIMIT).clamp(1, MAX_AUDIT_LIMIT)),
        cursor,
    };

    let mut response_items = Vec::new();
    let (total, next_cursor) = if internal {
        let events =
            state.application_services.audit.list_internal_events(&state, &list_query).await?;
        let total = events.total;
        let next_cursor = events.next_cursor.clone();
        push_internal_response_items(
            &state,
            &auth,
            workspace_filter,
            library_filter,
            &mut response_items,
            events,
        )
        .await?;
        (total, next_cursor)
    } else {
        let events =
            state.application_services.audit.list_redacted_events(&state, &list_query).await?;
        let total = events.total;
        let next_cursor = events.next_cursor.clone();
        push_redacted_response_items(
            &state,
            &auth,
            workspace_filter,
            library_filter,
            &mut response_items,
            events,
        )
        .await?;
        (total, next_cursor)
    };

    attach_actor_principals(&state, &mut response_items).await?;

    if query.include_assistant.unwrap_or(false) {
        attach_assistant_call_summaries(&state, &auth, &mut response_items).await?;
    }
    span.record("item_count", response_items.len());
    let next_cursor = next_cursor.map(|cursor| encode_audit_page_cursor(sort, &cursor));
    Ok(Json(AuditEventPageResponse { items: response_items, next_cursor, total }))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditEventFacetValueResponse {
    /// The raw column value; `null` is the genuine "no actor" bucket.
    pub value: Option<String>,
    /// The label the grid renders for this value (the actor's display label,
    /// or the enum value itself).
    pub label: String,
    pub count: i64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditEventFacetsResponse {
    pub values: Vec<AuditEventFacetValueResponse>,
    pub has_more: bool,
}

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct AuditEventFacetsQuery {
    /// Column id: `actor`, `action`, `surface` or `result`.
    pub facet_column: String,
    /// Narrow the value list itself; does not affect the counts' scope.
    pub facet_search: Option<String>,
    pub facet_limit: Option<u32>,
    pub facet_offset: Option<u32>,
    /// Every other column filter, comma-separated where multi-valued.
    pub actor: Option<String>,
    pub actor_null: Option<bool>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub context_bundle_id: Option<Uuid>,
    pub query_session_id: Option<Uuid>,
    pub query_execution_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub action_kind: Option<String>,
    pub surface_kind: Option<String>,
    pub result_kind: Option<String>,
    pub created_at: Option<String>,
    pub search: Option<String>,
    /// JSON `AuditSearchHintsInput` for the whole-table term.
    pub search_hints: Option<String>,
    /// JSON `AuditSearchHintsInput` for this column's value search only.
    pub facet_search_hints: Option<String>,
    /// The selected values whose server label the caller must render, by
    /// column and value. Bounded and explicit; the pair lists are positional.
    pub selected_value_column: Option<String>,
    pub selected_value: Option<String>,
    pub internal: Option<bool>,
}

/// The values of one audit column over the whole filtered set, each column's
/// own selection excluded. The caller's scope is resolved exactly as the list
/// endpoint resolves it, so a facet never reveals a value the list would hide.
#[utoipa::path(
    get,
    path = "/v1/audit/events/facets",
    tag = "audit",
    operation_id = "listAuditEventFacets",
    params(AuditEventFacetsQuery),
    responses(
        (status = 200, description = "Facet values for one audit column", body = AuditEventFacetsResponse),
        (status = 400, description = "Unknown facet column"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the requested scope"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_audit_event_facets",
    skip_all,
    fields(
        facet_column = %query.facet_column,
        workspace_id = ?query.workspace_id,
        library_id = ?query.library_id,
        item_count
    )
)]
pub async fn list_audit_event_facets(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AuditEventFacetsQuery>,
) -> Result<Json<AuditEventFacetsResponse>, ApiError> {
    let span = tracing::Span::current();
    let (workspace_filter, library_filter, authorized_internal, filters, search_hints) =
        resolve_audit_read(&auth, &state, AuditReadQuery::from(&query)).await?;

    let facet_column =
        audit_repository::AuditFacetColumn::parse(&query.facet_column).ok_or_else(|| {
            ApiError::BadRequest(format!("unknown facetColumn `{}`", query.facet_column))
        })?;

    let mut filters = filters;
    // Self-exclusion: a column's menu lists the values the *other* filters
    // leave, so drop this column's own selection.
    match facet_column {
        audit_repository::AuditFacetColumn::Actor => {
            filters.actor_principal_ids.clear();
            filters.actor_include_null = false;
        }
        audit_repository::AuditFacetColumn::Action => filters.action_kinds.clear(),
        audit_repository::AuditFacetColumn::Surface => filters.surface_kinds.clear(),
        audit_repository::AuditFacetColumn::Result => filters.result_kinds.clear(),
        audit_repository::AuditFacetColumn::CreatedAt => filters.created_at_texts.clear(),
    }

    let selected_values = audit_selected_values(
        query.selected_value_column.as_deref(),
        query.selected_value.as_deref(),
    )?;
    if selected_values.iter().any(|(column, _)| *column != facet_column) {
        return Err(ApiError::BadRequest(
            "selected values must belong to the requested audit column".to_string(),
        ));
    }
    // A selected-value lookup resolves its complete bounded set, not one page
    // of a facet menu. The accepted set may contain up to 200 values.
    let (limit, offset) = if selected_values.is_empty() {
        let offset = query.facet_offset.unwrap_or(0);
        if offset > MAX_AUDIT_FACET_OFFSET {
            return Err(ApiError::BadRequest(format!(
                "facetOffset must not exceed {MAX_AUDIT_FACET_OFFSET}"
            )));
        }
        (
            i64::from(
                query
                    .facet_limit
                    .unwrap_or(DEFAULT_AUDIT_FACET_LIMIT)
                    .clamp(1, MAX_AUDIT_FACET_LIMIT),
            ),
            i64::from(offset),
        )
    } else {
        (
            i64::try_from(selected_values.len())
                .map_err(|_| ApiError::BadRequest("too many selected audit values".to_string()))?,
            0,
        )
    };
    let facet_query = audit_repository::AuditFacetQuery {
        workspace_id: workspace_filter,
        library_id: library_filter,
        subject_filter: audit_repository::AuditEventSubjectFilter {
            knowledge_document_id: query.knowledge_document_id,
            knowledge_revision_id: query.knowledge_revision_id,
            context_bundle_id: query.context_bundle_id,
            query_session_id: query.query_session_id,
            query_execution_id: query.query_execution_id,
            runtime_execution_id: query.runtime_execution_id,
            async_operation_id: query.async_operation_id,
        },
        filters,
        search: query.search.filter(|value| !value.trim().is_empty()),
        internal: authorized_internal,
        search_hints,
        selected_values,
        facet_column,
        facet_search: query.facet_search.filter(|value| !value.trim().is_empty()),
        facet_search_hints: audit_facet_search_hints(
            facet_column,
            query.facet_search_hints.as_deref(),
        )?,
        limit,
        offset,
    };

    let mut rows = state.application_services.audit.list_event_facets(&state, &facet_query).await?;
    let has_extra_row = rows.len() as i64 > facet_query.limit;
    if has_extra_row {
        let limit = usize::try_from(facet_query.limit).unwrap_or(usize::MAX);
        rows.truncate(limit);
    }
    // Continuation is exhausted at the offset budget. A value search still
    // resolves later values without replaying the last permitted page.
    let has_more = has_extra_row
        && facet_query.offset + facet_query.limit <= i64::from(MAX_AUDIT_FACET_OFFSET);
    span.record("item_count", rows.len());
    Ok(Json(AuditEventFacetsResponse {
        values: rows
            .into_iter()
            .map(|row| AuditEventFacetValueResponse {
                value: row.value,
                label: row.label,
                count: row.count,
            })
            .collect(),
        has_more,
    }))
}

async fn push_internal_response_items(
    state: &AppState,
    auth: &AuthContext,
    workspace_filter: Option<Uuid>,
    library_filter: Option<Uuid>,
    response_items: &mut Vec<AuditEventResponse>,
    page: AuditEventPage<AuditEventInternalView>,
) -> Result<(), ApiError> {
    for event in page.items {
        let subjects = visible_subjects(
            state,
            event.id,
            auth.is_system_admin,
            workspace_filter,
            library_filter,
        )
        .await?;
        if auth.is_system_admin || !subjects.is_empty() {
            response_items.push(map_internal_event(event, subjects));
        }
    }

    Ok(())
}

async fn push_redacted_response_items(
    state: &AppState,
    auth: &AuthContext,
    workspace_filter: Option<Uuid>,
    library_filter: Option<Uuid>,
    response_items: &mut Vec<AuditEventResponse>,
    page: AuditEventPage<AuditEventRedactedView>,
) -> Result<(), ApiError> {
    for event in page.items {
        let subjects = visible_subjects(
            state,
            event.id,
            auth.is_system_admin,
            workspace_filter,
            library_filter,
        )
        .await?;
        if auth.is_system_admin || !subjects.is_empty() {
            response_items.push(map_redacted_event(event, subjects));
        }
    }

    Ok(())
}

async fn visible_subjects(
    state: &AppState,
    audit_event_id: Uuid,
    is_system_admin: bool,
    workspace_filter: Option<Uuid>,
    library_filter: Option<Uuid>,
) -> Result<Vec<AuditEventSubjectResponse>, ApiError> {
    let subjects =
        state.application_services.audit.list_event_subjects(state, audit_event_id).await?;

    Ok(subjects
        .into_iter()
        .filter(|subject| {
            if is_system_admin {
                return true;
            }
            if let Some(library_id) = library_filter {
                return subject.library_id == Some(library_id);
            }
            if let Some(workspace_id) = workspace_filter {
                return subject.workspace_id == Some(workspace_id);
            }
            false
        })
        .map(map_subject)
        .collect())
}

async fn attach_assistant_call_summaries(
    state: &AppState,
    auth: &AuthContext,
    items: &mut [AuditEventResponse],
) -> Result<(), ApiError> {
    let query_execution_ids = items
        .iter()
        .filter(|event| event.action_kind == "query.execution.run")
        .filter_map(query_execution_id_from_event)
        .collect::<Vec<_>>();
    if query_execution_ids.is_empty() {
        return Ok(());
    }

    let summaries = state
        .application_services
        .audit
        .list_assistant_call_summaries(state, &query_execution_ids)
        .await?;

    for event in items.iter_mut().filter(|event| event.action_kind == "query.execution.run") {
        let Some(query_execution_id) = query_execution_id_from_event(event) else {
            continue;
        };
        let Some((workspace_id, library_id)) = audit_scope_from_event(event) else {
            continue;
        };
        if !auth.has_library_permission(workspace_id, library_id, POLICY_USAGE_READ) {
            continue;
        }
        if let Some(summary) = summaries.get(&query_execution_id) {
            event.assistant_call = Some(map_assistant_call(summary));
        }
    }

    Ok(())
}

fn query_execution_id_from_event(event: &AuditEventResponse) -> Option<Uuid> {
    event.subjects.iter().find_map(|subject| subject.query_execution_id)
}

fn audit_scope_from_event(event: &AuditEventResponse) -> Option<(Uuid, Uuid)> {
    event.subjects.iter().find_map(|subject| Some((subject.workspace_id?, subject.library_id?)))
}

async fn attach_actor_principals(
    state: &AppState,
    items: &mut [AuditEventResponse],
) -> Result<(), ApiError> {
    let actor_ids = items
        .iter()
        .filter_map(|event| event.actor_principal_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if actor_ids.is_empty() {
        return Ok(());
    }

    let profiles =
        iam_repository::list_principal_profiles_by_ids(&state.persistence.postgres, &actor_ids)
            .await
            .map_err(|error| {
                ApiError::internal_with_log(error, "failed to load audit actor principals")
            })?
            .into_iter()
            .map(|row| (row.id, map_actor_principal(row)))
            .collect::<HashMap<_, _>>();

    for event in items {
        if let Some(actor_id) = event.actor_principal_id
            && let Some(profile) = profiles.get(&actor_id)
        {
            event.actor_principal = Some(profile.clone());
        }
    }

    Ok(())
}

fn map_actor_principal(row: iam_repository::IamPrincipalProfileRow) -> AuditActorPrincipalResponse {
    AuditActorPrincipalResponse {
        id: row.id,
        principal_kind: row.principal_kind,
        status: row.status,
        display_label: row.display_label,
        login: row.login,
        display_name: row.display_name,
        role: row.role,
    }
}

fn map_internal_event(
    event: AuditEventInternalView,
    subjects: Vec<AuditEventSubjectResponse>,
) -> AuditEventResponse {
    AuditEventResponse {
        id: event.id,
        actor_principal_id: event.actor_principal_id,
        actor_principal: None,
        surface_kind: event.surface_kind,
        action_kind: event.action_kind,
        result_kind: event.result_kind,
        request_id: event.request_id,
        trace_id: event.trace_id,
        created_at: event.created_at,
        redacted_message: event.redacted_message,
        internal_message: event.internal_message,
        subjects,
        assistant_call: None,
    }
}

fn map_redacted_event(
    event: AuditEventRedactedView,
    subjects: Vec<AuditEventSubjectResponse>,
) -> AuditEventResponse {
    AuditEventResponse {
        id: event.id,
        actor_principal_id: event.actor_principal_id,
        actor_principal: None,
        surface_kind: event.surface_kind,
        action_kind: event.action_kind,
        result_kind: event.result_kind,
        request_id: event.request_id,
        trace_id: event.trace_id,
        created_at: event.created_at,
        redacted_message: event.redacted_message,
        internal_message: None,
        subjects,
        assistant_call: None,
    }
}

fn map_assistant_call(summary: &AuditAssistantCallSummary) -> AuditAssistantCallResponse {
    AuditAssistantCallResponse {
        query_execution_id: summary.query_execution_id,
        conversation_id: summary.conversation_id,
        runtime_execution_id: summary.runtime_execution_id,
        models: summary.models.iter().map(map_assistant_model).collect(),
        total_cost: summary.total_cost,
        currency_code: summary.currency_code.clone(),
        provider_call_count: summary.provider_call_count,
    }
}

fn map_assistant_model(model: &AuditAssistantModel) -> AuditAssistantModelResponse {
    AuditAssistantModelResponse {
        provider_kind: model.provider_kind.clone(),
        model_name: model.model_name.clone(),
    }
}

fn map_subject(subject: AuditEventSubject) -> AuditEventSubjectResponse {
    let knowledge_document_id = match subject.subject_kind.as_str() {
        "knowledge_document" => Some(subject.subject_id),
        "knowledge_revision" => subject.document_id,
        _ => None,
    };
    let knowledge_revision_id =
        (subject.subject_kind == "knowledge_revision").then_some(subject.subject_id);

    AuditEventSubjectResponse {
        audit_event_id: subject.audit_event_id,
        subject_kind: subject.subject_kind,
        subject_id: subject.subject_id,
        workspace_id: subject.workspace_id,
        library_id: subject.library_id,
        document_id: knowledge_document_id.or(subject.document_id),
        knowledge_document_id,
        knowledge_revision_id,
        query_session_id: subject.query_session_id,
        query_execution_id: subject.query_execution_id,
        runtime_execution_id: subject.runtime_execution_id,
        context_bundle_id: subject.context_bundle_id,
        async_operation_id: subject.async_operation_id,
    }
}
