use axum::{Json, Router, extract::Query, extract::State, routing::get};
use ironrag_contracts::{
    admin::{
        AdminLibraryColumnFilters, AdminLibraryCostCoverage, AdminLibraryCostSearch,
        AdminLibraryCostSummary, AdminLibraryCostValue, AdminLibraryFacetColumn,
        AdminLibraryFacetValue, AdminLibraryFacets, AdminLibraryFacetsResponse,
        AdminLibraryIngestionReadiness, AdminLibraryListItem, AdminLibraryPageResponse,
        AdminLibrarySearchHints, AdminLibraryWorkspaceSummary,
    },
    ai::AiBindingPurpose,
};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::{
    app::state::AppState,
    domains::billing::CostCoverageStatus,
    infra::repositories::catalog_repository::{
        self, AdminLibraryFacetCursorValue, AdminLibraryFacetPage, AdminLibraryFacetQuery,
        AdminLibraryFacetRow, AdminLibraryFilterValues, AdminLibraryListFilters,
        AdminLibraryPageOrder, AdminLibraryPageQuery, AdminLibraryPageRow, AdminLibrarySearchScope,
    },
    interfaces::http::{auth::AuthContext, router_support::ApiError},
    services::catalog_service::INGEST_REQUIRED_BINDINGS,
};

const DEFAULT_ADMIN_LIBRARY_PAGE_LIMIT: u32 = 50;
const MAX_ADMIN_LIBRARY_PAGE_LIMIT: u32 = 100;
const MAX_ADMIN_LIBRARY_SEARCH_CHARS: usize = 200;
const MAX_ADMIN_LIBRARY_FILTER_VALUES_PER_COLUMN: usize = 200;
const MAX_ADMIN_LIBRARY_FILTERS_JSON_BYTES: usize = 64 * 1024;
const DEFAULT_ADMIN_LIBRARY_FACET_LIMIT: u32 = 50;
const MAX_ADMIN_LIBRARY_FACET_LIMIT: u32 = 200;

#[derive(Debug, Clone, Copy, Default, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminLibrarySortKey {
    #[default]
    Library,
    Workspace,
    Documents,
    Queue,
    Cost,
    Calls,
    Readiness,
    Lifecycle,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminLibrarySortOrder {
    #[default]
    Asc,
    Desc,
}

impl AdminLibrarySortKey {
    const fn to_order(self, order: AdminLibrarySortOrder) -> AdminLibraryPageOrder {
        let column = match self {
            Self::Library => AdminLibraryFacetColumn::Library,
            Self::Workspace => AdminLibraryFacetColumn::Workspace,
            Self::Documents => AdminLibraryFacetColumn::Documents,
            Self::Queue => AdminLibraryFacetColumn::Queue,
            Self::Cost => AdminLibraryFacetColumn::Cost,
            Self::Calls => AdminLibraryFacetColumn::Calls,
            Self::Readiness => AdminLibraryFacetColumn::Readiness,
            Self::Lifecycle => AdminLibraryFacetColumn::Lifecycle,
        };
        AdminLibraryPageOrder { column, desc: matches!(order, AdminLibrarySortOrder::Desc) }
    }
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AdminLibraryListQuery {
    pub search: Option<String>,
    /// An Intl-derived canonical spelling of `search`, OR'd over the same text
    /// columns; no server-side language dictionary is involved.
    pub normalized_search: Option<String>,
    /// Typed generic search hints (`AdminLibrarySearchHints`) the client derived
    /// from the term: the numbers a formatted documents/queue/calls label
    /// spells, the exact decimals a cost label carries, and the readiness /
    /// lifecycle values whose localized label matches. Each is an OR
    /// alternative to the text search.
    pub search_hints: Option<String>,
    /// JSON object of the grid's typed per-column filters, one nullable array
    /// per column id (`library`/`workspace`/`documents`/`queue`/`cost`/`calls`/
    /// `readiness`/`lifecycle`). Absent means no constraint.
    pub filters: Option<String>,
    pub sort_by: Option<AdminLibrarySortKey>,
    pub sort_order: Option<AdminLibrarySortOrder>,
    pub offset: Option<u32>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AdminLibraryFacetQueryParams {
    /// The grid column whose distinct values are requested.
    pub facet_column: AdminLibraryFacetColumn,
    /// The same typed per-column filters the list carries; the selected column's
    /// own filter is self-excluded before counting.
    pub filters: Option<String>,
    /// The same whole-table search the list carries.
    pub search: Option<String>,
    /// The same Intl-normalized spelling the list carries.
    pub normalized_search: Option<String>,
    /// The same typed generic search hints the list carries.
    pub search_hints: Option<String>,
    /// Substring the operator typed inside this column's menu.
    pub facet_search: Option<String>,
    /// Typed hints for the selected column's displayed labels, independent of
    /// the whole-table search. Hints belonging to other columns are rejected.
    pub facet_search_hints: Option<String>,
    /// Opaque keyset cursor returned by the previous facet page.
    pub facet_cursor: Option<String>,
    pub facet_limit: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
struct AdminLibraryPageWindow {
    order: AdminLibraryPageOrder,
    offset: u32,
    limit: u32,
}

/// The owned typed search inputs the handlers hold while a repository call
/// borrows them.
#[derive(Debug, Default)]
struct AdminLibrarySearchInput {
    search: Option<String>,
    normalized_search: Option<String>,
    numbers: Vec<i64>,
    costs: Vec<AdminLibraryCostSearch>,
    unavailable: bool,
    readiness: Vec<bool>,
    lifecycle: Vec<String>,
}

impl AdminLibrarySearchInput {
    fn scope(&self) -> AdminLibrarySearchScope<'_> {
        AdminLibrarySearchScope {
            search: self.search.as_deref(),
            normalized_search: self.normalized_search.as_deref(),
            numbers: &self.numbers,
            costs: &self.costs,
            unavailable: self.unavailable,
            readiness: &self.readiness,
            lifecycle: &self.lifecycle,
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/libraries", get(list_admin_libraries))
        .route("/admin/libraries/facets", get(list_admin_library_facets))
}

fn normalize_library_search(value: Option<String>) -> Result<Option<String>, ApiError> {
    let search = value.map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
    if search.as_ref().is_some_and(|value| value.chars().count() > MAX_ADMIN_LIBRARY_SEARCH_CHARS) {
        return Err(ApiError::BadRequest("library search is too long".to_string()));
    }
    Ok(search)
}

fn convert_filter<T>(
    values: Option<Vec<Option<T>>>,
) -> Result<AdminLibraryFilterValues<T>, ApiError> {
    let Some(values) = values else {
        return Ok(AdminLibraryFilterValues::default());
    };
    if values.len() > MAX_ADMIN_LIBRARY_FILTER_VALUES_PER_COLUMN {
        return Err(ApiError::BadRequest(format!(
            "too many values in a column filter; maximum is {MAX_ADMIN_LIBRARY_FILTER_VALUES_PER_COLUMN}"
        )));
    }
    let present = !values.is_empty();
    let include_null = values.iter().any(Option::is_none);
    let values = values.into_iter().flatten().collect();
    Ok(AdminLibraryFilterValues { present, include_null, values })
}

fn convert_cost_filter(
    values: Option<Vec<Option<AdminLibraryCostValue>>>,
) -> Result<AdminLibraryFilterValues<AdminLibraryCostValue>, ApiError> {
    let Some(values) = values else {
        return Ok(AdminLibraryFilterValues::default());
    };
    if values.len() > MAX_ADMIN_LIBRARY_FILTER_VALUES_PER_COLUMN {
        return Err(ApiError::BadRequest(format!(
            "too many values in a column filter; maximum is {MAX_ADMIN_LIBRARY_FILTER_VALUES_PER_COLUMN}"
        )));
    }
    let present = !values.is_empty();
    let include_null = values.iter().any(Option::is_none);
    let mut parsed = Vec::new();
    for value in values.into_iter().flatten() {
        validate_cost_value(&value)?;
        parsed.push(value);
    }
    Ok(AdminLibraryFilterValues { present, include_null, values: parsed })
}

/// The external boundary check for one canonical cost value: the amount is a
/// real decimal that never leaves the two display fraction digits, and the
/// currency is a bare ISO 4217 code.
fn validate_cost_value(value: &AdminLibraryCostValue) -> Result<(), ApiError> {
    validate_cost_fields(
        Some(&value.amount),
        Some(&value.currency_code),
        Some(value.unpriced_call_count),
    )
}

fn validate_cost_search(value: &AdminLibraryCostSearch) -> Result<(), ApiError> {
    if value.amount.is_none()
        && value.currency_code.is_none()
        && value.coverage.is_none()
        && value.unpriced_call_count.is_none()
    {
        return Err(ApiError::BadRequest("cost search needs a displayed value".to_string()));
    }
    validate_cost_fields(
        value.amount.as_deref(),
        value.currency_code.as_deref(),
        value.unpriced_call_count,
    )
}

fn validate_cost_fields(
    amount: Option<&str>,
    currency: Option<&str>,
    unpriced: Option<i64>,
) -> Result<(), ApiError> {
    if let Some(amount) = amount {
        validate_cost_amount(amount)?;
    }
    if let Some(currency) = currency
        && (currency.len() != 3
            || !currency.chars().all(|character| character.is_ascii_uppercase()))
    {
        return Err(ApiError::BadRequest(
            "cost currency code must be a three-letter ISO code".to_string(),
        ));
    }
    if unpriced.is_some_and(|count| count < 0) {
        return Err(ApiError::BadRequest("unpriced call count must not be negative".to_string()));
    }
    Ok(())
}

fn validate_cost_amount(value: &str) -> Result<(), ApiError> {
    let amount = rust_decimal::Decimal::from_str(value)
        .map_err(|_| ApiError::BadRequest(format!("invalid cost amount `{value}`")))?;
    if amount.is_sign_negative() {
        return Err(ApiError::BadRequest("cost amount must not be negative".to_string()));
    }
    if amount.scale() > 2 {
        return Err(ApiError::BadRequest(
            "cost amount must carry at most the two display fraction digits".to_string(),
        ));
    }
    Ok(())
}

fn parse_admin_library_filters(raw: Option<&str>) -> Result<AdminLibraryListFilters, ApiError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(AdminLibraryListFilters::default());
    };
    if raw.len() > MAX_ADMIN_LIBRARY_FILTERS_JSON_BYTES {
        return Err(ApiError::BadRequest("library filters are too large".to_string()));
    }
    let parsed: AdminLibraryColumnFilters = serde_json::from_str(raw)
        .map_err(|error| ApiError::BadRequest(format!("invalid library filters: {error}")))?;
    Ok(AdminLibraryListFilters {
        library: convert_filter(parsed.library)?,
        workspace: convert_filter(parsed.workspace)?,
        documents: convert_filter(parsed.documents)?,
        queue: convert_filter(parsed.queue)?,
        cost: convert_cost_filter(parsed.cost)?,
        calls: convert_filter(parsed.calls)?,
        readiness: convert_filter(parsed.readiness)?,
        lifecycle: convert_filter(parsed.lifecycle)?,
    })
}

fn parse_admin_library_search_hints(
    raw: Option<&str>,
) -> Result<AdminLibrarySearchHints, ApiError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(AdminLibrarySearchHints::default());
    };
    if raw.len() > MAX_ADMIN_LIBRARY_FILTERS_JSON_BYTES {
        return Err(ApiError::BadRequest("library search hints are too large".to_string()));
    }
    serde_json::from_str(raw)
        .map_err(|error| ApiError::BadRequest(format!("invalid library search hints: {error}")))
}

fn convert_hint_values<T: Clone>(values: Option<Vec<T>>) -> Result<Vec<T>, ApiError> {
    let Some(values) = values else {
        return Ok(Vec::new());
    };
    if values.len() > MAX_ADMIN_LIBRARY_FILTER_VALUES_PER_COLUMN {
        return Err(ApiError::BadRequest(format!(
            "too many values in a search hint; maximum is {MAX_ADMIN_LIBRARY_FILTER_VALUES_PER_COLUMN}"
        )));
    }
    Ok(values)
}

fn normalize_admin_library_search(
    search: Option<String>,
    normalized_search: Option<String>,
    hints: Option<String>,
) -> Result<AdminLibrarySearchInput, ApiError> {
    let hints = parse_admin_library_search_hints(hints.as_deref())?;
    let costs = convert_hint_values(hints.costs)?;
    for value in &costs {
        validate_cost_search(value)?;
    }
    Ok(AdminLibrarySearchInput {
        search: normalize_library_search(search)?,
        normalized_search: normalize_library_search(normalized_search)?,
        numbers: convert_hint_values(hints.numbers)?,
        costs,
        unavailable: hints.unavailable.unwrap_or(false),
        readiness: convert_hint_values(hints.readiness)?,
        lifecycle: convert_hint_values(hints.lifecycle)?,
    })
}

fn normalize_admin_library_page(
    query: AdminLibraryListQuery,
) -> Result<(AdminLibraryPageWindow, AdminLibraryListFilters, AdminLibrarySearchInput), ApiError> {
    let search =
        normalize_admin_library_search(query.search, query.normalized_search, query.search_hints)?;
    let filters = parse_admin_library_filters(query.filters.as_deref())?;
    Ok((
        AdminLibraryPageWindow {
            order: query.sort_by.unwrap_or_default().to_order(query.sort_order.unwrap_or_default()),
            offset: query.offset.unwrap_or_default(),
            limit: query
                .limit
                .unwrap_or(DEFAULT_ADMIN_LIBRARY_PAGE_LIMIT)
                .clamp(1, MAX_ADMIN_LIBRARY_PAGE_LIMIT),
        },
        filters,
        search,
    ))
}

fn normalize_facet_hints(
    column: AdminLibraryFacetColumn,
    raw: Option<String>,
) -> Result<AdminLibrarySearchInput, ApiError> {
    let hints = normalize_admin_library_search(None, None, raw)?;
    let numbers_allowed = matches!(
        column,
        AdminLibraryFacetColumn::Documents
            | AdminLibraryFacetColumn::Queue
            | AdminLibraryFacetColumn::Calls
    );
    let unavailable_allowed = numbers_allowed || matches!(column, AdminLibraryFacetColumn::Cost);
    let wrong_column = (!hints.numbers.is_empty() && !numbers_allowed)
        || (!hints.costs.is_empty() && !matches!(column, AdminLibraryFacetColumn::Cost))
        || (!hints.readiness.is_empty() && !matches!(column, AdminLibraryFacetColumn::Readiness))
        || (!hints.lifecycle.is_empty() && !matches!(column, AdminLibraryFacetColumn::Lifecycle))
        || (hints.unavailable && !unavailable_allowed);
    if wrong_column {
        return Err(ApiError::BadRequest(
            "facet search hints must belong to the requested column".to_string(),
        ));
    }
    Ok(hints)
}

fn map_missing_binding_purposes(values: Vec<String>) -> Result<Vec<AiBindingPurpose>, ApiError> {
    values
        .into_iter()
        .map(|value| value.parse::<AiBindingPurpose>().map_err(|_| ApiError::Internal))
        .collect()
}

struct AdminLibraryCostSummarySource {
    cost_blocked: bool,
    currency_count: i64,
    total_cost: rust_decimal::Decimal,
    currency_code: String,
    document_count: i64,
    provider_call_count: i64,
    unpriced_provider_call_count: i64,
}

fn map_admin_library_cost_summary(
    source: AdminLibraryCostSummarySource,
) -> Option<AdminLibraryCostSummary> {
    (!source.cost_blocked && source.currency_count <= 1).then(|| AdminLibraryCostSummary {
        total_cost: source.total_cost.to_string(),
        currency_code: source.currency_code,
        document_count: source.document_count,
        provider_call_count: source.provider_call_count,
        unpriced_provider_call_count: source.unpriced_provider_call_count,
        cost_coverage: match CostCoverageStatus::from_call_counts(
            source.provider_call_count,
            source.unpriced_provider_call_count,
        ) {
            CostCoverageStatus::Complete => AdminLibraryCostCoverage::Complete,
            CostCoverageStatus::Partial => AdminLibraryCostCoverage::Partial,
            CostCoverageStatus::Unknown => AdminLibraryCostCoverage::Unknown,
        },
    })
}

fn map_admin_library_item(row: AdminLibraryPageRow) -> Result<AdminLibraryListItem, ApiError> {
    let missing_binding_purposes = map_missing_binding_purposes(row.missing_binding_purposes)?;
    let cost_summary = map_admin_library_cost_summary(AdminLibraryCostSummarySource {
        cost_blocked: row.cost_blocked,
        currency_count: row.currency_count,
        total_cost: row.total_cost,
        currency_code: row.currency_code,
        document_count: row.document_count,
        provider_call_count: row.provider_call_count,
        unpriced_provider_call_count: row.unpriced_provider_call_count,
    });
    let cost_value = cost_summary
        .as_ref()
        .filter(|summary| summary.cost_coverage != AdminLibraryCostCoverage::Unknown)
        .and_then(|summary| {
            let coverage = summary.cost_coverage;
            rounded_display_amount(&summary.total_cost).map(|amount| AdminLibraryCostValue {
                amount,
                currency_code: summary.currency_code.clone(),
                coverage,
                unpriced_call_count: summary.unpriced_provider_call_count,
            })
        });
    Ok(AdminLibraryListItem {
        id: row.id,
        slug: row.slug,
        display_name: row.display_name,
        lifecycle_state: row.lifecycle_state,
        include_document_hint_in_mcp_answers: row.include_document_hint_in_mcp_answers,
        workspace: AdminLibraryWorkspaceSummary {
            id: row.workspace_id,
            slug: row.workspace_slug,
            display_name: row.workspace_display_name,
        },
        ingestion_readiness: AdminLibraryIngestionReadiness {
            ready: missing_binding_purposes.is_empty(),
            missing_binding_purposes,
        },
        queued_job_count: row.queued_job_count,
        cost_summary,
        cost_value,
    })
}

/// Round a decimal source string to the two display fraction digits, half-up,
/// without ever going through a float.
fn rounded_display_amount(value: &str) -> Option<String> {
    use rust_decimal::RoundingStrategy;
    let amount = rust_decimal::Decimal::from_str(value).ok()?;
    Some(amount.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero).to_string())
}

#[utoipa::path(
    get,
    path = "/v1/admin/libraries",
    tag = "admin",
    operation_id = "listAdminLibraries",
    params(AdminLibraryListQuery),
    responses(
        (status = 200, description = "Bounded library page for the administration console", body = AdminLibraryPageResponse),
        (status = 400, description = "Invalid search, filter or page parameters"),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_admin_libraries",
    skip_all,
    fields(item_count, total_count)
)]
pub async fn list_admin_libraries(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AdminLibraryListQuery>,
) -> Result<Json<AdminLibraryPageResponse>, ApiError> {
    if !auth.is_system_admin {
        return Err(ApiError::Unauthorized);
    }
    let (window, filters, search) = normalize_admin_library_page(query)?;
    let required_binding_purposes = required_binding_purposes();
    let (rows, facets) = catalog_repository::list_admin_library_page(
        &state.persistence.postgres,
        AdminLibraryPageQuery {
            scope: search.scope(),
            filters: &filters,
            required_binding_purposes: &required_binding_purposes,
            offset: window.offset,
            limit: window.limit,
            order: window.order,
        },
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let items = rows.into_iter().map(map_admin_library_item).collect::<Result<Vec<_>, _>>()?;
    let aggregate_cost_summary = map_admin_library_cost_summary(AdminLibraryCostSummarySource {
        cost_blocked: facets.cost_blocked,
        currency_count: facets.currency_count,
        total_cost: facets.total_cost,
        currency_code: facets.currency_code,
        document_count: facets.document_count,
        provider_call_count: facets.provider_call_count,
        unpriced_provider_call_count: facets.unpriced_provider_call_count,
    });
    tracing::Span::current().record("item_count", items.len());
    tracing::Span::current().record("total_count", facets.total_count);
    Ok(Json(AdminLibraryPageResponse {
        items,
        total_count: facets.total_count,
        facets: AdminLibraryFacets {
            workspace_count: facets.workspace_count,
            ready_count: facets.ready_count,
            active_count: facets.active_count,
        },
        aggregate_cost_summary,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/admin/libraries/facets",
    tag = "admin",
    operation_id = "listAdminLibraryFacets",
    params(AdminLibraryFacetQueryParams),
    responses(
        (status = 200, description = "Bounded distinct values for one library grid column", body = AdminLibraryFacetsResponse),
        (status = 400, description = "Invalid search, filter or facet parameters"),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_admin_library_facets",
    skip_all,
    fields(value_count)
)]
pub async fn list_admin_library_facets(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AdminLibraryFacetQueryParams>,
) -> Result<Json<AdminLibraryFacetsResponse>, ApiError> {
    if !auth.is_system_admin {
        return Err(ApiError::Unauthorized);
    }
    let column = query.facet_column;
    let search =
        normalize_admin_library_search(query.search, query.normalized_search, query.search_hints)?;
    let filters = parse_admin_library_filters(query.filters.as_deref())?;
    let facet_search = normalize_library_search(query.facet_search)?;
    let facet_hints = normalize_facet_hints(query.facet_column, query.facet_search_hints)?;
    let limit = query
        .facet_limit
        .unwrap_or(DEFAULT_ADMIN_LIBRARY_FACET_LIMIT)
        .clamp(1, MAX_ADMIN_LIBRARY_FACET_LIMIT);
    let cursor = query.facet_cursor.as_deref().map(decode_facet_cursor).transpose()?;
    if let Some(cursor) = &cursor
        && cursor.column != column
    {
        return Err(facet_cursor_mismatch());
    }
    // A cursor on the NULL bucket means the NULL group — always last — has been
    // returned; the chain is over.
    if cursor.as_ref().is_some_and(|cursor| cursor.is_null) {
        return Ok(Json(AdminLibraryFacetsResponse {
            column,
            values: Vec::new(),
            next_cursor: None,
        }));
    }
    let cursor_value = cursor.as_ref().map(cursor_value).transpose()?;
    let required_binding_purposes = required_binding_purposes();
    let page = catalog_repository::list_admin_library_facet_page(
        &state.persistence.postgres,
        AdminLibraryFacetQuery {
            scope: search.scope(),
            filters: &filters,
            required_binding_purposes: &required_binding_purposes,
            column,
            facet_search: facet_search.as_deref(),
            facet_hints: facet_hints.scope(),
            cursor: cursor_value.as_ref(),
            limit,
        },
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let next_cursor = facet_next_cursor(column, &page);
    let values = page.rows.iter().map(|row| map_facet_value(column, row)).collect::<Vec<_>>();
    tracing::Span::current().record("value_count", values.len());
    Ok(Json(AdminLibraryFacetsResponse { column, values, next_cursor }))
}

fn required_binding_purposes() -> Vec<String> {
    INGEST_REQUIRED_BINDINGS.iter().map(|purpose| purpose.as_str().to_string()).collect()
}

fn facet_next_cursor(
    column: AdminLibraryFacetColumn,
    page: &AdminLibraryFacetPage,
) -> Option<String> {
    if !page.has_more {
        return None;
    }
    page.rows.last().map(|row| encode_facet_cursor(&cursor_from_row(column, row)))
}

fn cursor_is_null(column: AdminLibraryFacetColumn, row: &AdminLibraryFacetRow) -> bool {
    match column {
        AdminLibraryFacetColumn::Library
        | AdminLibraryFacetColumn::Workspace
        | AdminLibraryFacetColumn::Lifecycle => row.value_text.is_none(),
        AdminLibraryFacetColumn::Documents
        | AdminLibraryFacetColumn::Queue
        | AdminLibraryFacetColumn::Calls => row.value_number.is_none(),
        AdminLibraryFacetColumn::Cost => row.value_decimal.is_none(),
        AdminLibraryFacetColumn::Readiness => row.value_flag.is_none(),
    }
}

fn cursor_from_row(
    column: AdminLibraryFacetColumn,
    row: &AdminLibraryFacetRow,
) -> AdminLibraryFacetCursor {
    AdminLibraryFacetCursor {
        column,
        text: row.value_text.clone(),
        number: row.value_number,
        decimal: row.value_decimal.map(|value| value.to_string()),
        flag: row.value_flag,
        currency: row.value_currency.clone(),
        coverage: row.value_coverage.clone(),
        is_null: cursor_is_null(column, row),
    }
}

fn map_facet_value(
    column: AdminLibraryFacetColumn,
    row: &AdminLibraryFacetRow,
) -> AdminLibraryFacetValue {
    AdminLibraryFacetValue {
        count: row.facet_count,
        text: row.value_text.clone(),
        number: (column != AdminLibraryFacetColumn::Cost).then_some(row.value_number).flatten(),
        decimal: row.value_decimal.map(|value| value.to_string()),
        flag: row.value_flag,
        currency_code: row.value_currency.clone(),
        coverage: row.value_coverage.as_deref().map(parse_facet_coverage),
        unpriced_call_count: (column == AdminLibraryFacetColumn::Cost)
            .then_some(row.value_number)
            .flatten(),
    }
}

fn parse_facet_coverage(value: &str) -> AdminLibraryCostCoverage {
    match value {
        "partial" => AdminLibraryCostCoverage::Partial,
        "unknown" => AdminLibraryCostCoverage::Unknown,
        _ => AdminLibraryCostCoverage::Complete,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdminLibraryFacetCursor {
    #[serde(rename = "c")]
    column: AdminLibraryFacetColumn,
    #[serde(rename = "t", default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(rename = "n", default, skip_serializing_if = "Option::is_none")]
    number: Option<i64>,
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    decimal: Option<String>,
    #[serde(rename = "f", default, skip_serializing_if = "Option::is_none")]
    flag: Option<bool>,
    #[serde(rename = "u", default, skip_serializing_if = "Option::is_none")]
    currency: Option<String>,
    #[serde(rename = "v", default, skip_serializing_if = "Option::is_none")]
    coverage: Option<String>,
    #[serde(rename = "z", default)]
    is_null: bool,
}

fn facet_cursor_mismatch() -> ApiError {
    ApiError::BadRequest("facet cursor value does not match its column".to_string())
}

fn encode_facet_cursor(cursor: &AdminLibraryFacetCursor) -> String {
    use base64::Engine;
    let bytes = serde_json::to_vec(cursor).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_facet_cursor(token: &str) -> Result<AdminLibraryFacetCursor, ApiError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::BadRequest("invalid facet cursor encoding".to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::BadRequest("invalid facet cursor payload".to_string()))
}

fn cursor_value(
    cursor: &AdminLibraryFacetCursor,
) -> Result<AdminLibraryFacetCursorValue, ApiError> {
    let shape = (
        cursor.text.is_some(),
        cursor.number.is_some(),
        cursor.decimal.is_some(),
        cursor.flag.is_some(),
        cursor.currency.is_some(),
        cursor.coverage.is_some(),
    );
    let expected = cursor_shape(cursor.column);
    if shape != expected {
        return Err(facet_cursor_mismatch());
    }
    match cursor.column {
        AdminLibraryFacetColumn::Library
        | AdminLibraryFacetColumn::Workspace
        | AdminLibraryFacetColumn::Lifecycle => cursor
            .text
            .clone()
            .map(AdminLibraryFacetCursorValue::Text)
            .ok_or_else(facet_cursor_mismatch),
        AdminLibraryFacetColumn::Documents
        | AdminLibraryFacetColumn::Queue
        | AdminLibraryFacetColumn::Calls => cursor
            .number
            .map(AdminLibraryFacetCursorValue::Number)
            .ok_or_else(facet_cursor_mismatch),
        AdminLibraryFacetColumn::Cost => cost_cursor_value(cursor),
        AdminLibraryFacetColumn::Readiness => {
            cursor.flag.map(AdminLibraryFacetCursorValue::Flag).ok_or_else(facet_cursor_mismatch)
        }
    }
}

fn cursor_shape(column: AdminLibraryFacetColumn) -> (bool, bool, bool, bool, bool, bool) {
    match column {
        AdminLibraryFacetColumn::Library
        | AdminLibraryFacetColumn::Workspace
        | AdminLibraryFacetColumn::Lifecycle => (true, false, false, false, false, false),
        AdminLibraryFacetColumn::Documents
        | AdminLibraryFacetColumn::Queue
        | AdminLibraryFacetColumn::Calls => (false, true, false, false, false, false),
        AdminLibraryFacetColumn::Cost => (false, true, true, false, true, true),
        AdminLibraryFacetColumn::Readiness => (false, false, false, true, false, false),
    }
}

fn cost_cursor_value(
    cursor: &AdminLibraryFacetCursor,
) -> Result<AdminLibraryFacetCursorValue, ApiError> {
    let currency = cursor.currency.clone().ok_or_else(facet_cursor_mismatch)?;
    let amount = cursor
        .decimal
        .as_deref()
        .map(|value| rust_decimal::Decimal::from_str(value).map_err(|_| facet_cursor_mismatch()))
        .transpose()?
        .ok_or_else(facet_cursor_mismatch)?;
    let coverage = cursor.coverage.clone().ok_or_else(facet_cursor_mismatch)?;
    let unpriced_call_count = cursor.number.ok_or_else(facet_cursor_mismatch)?;
    let value = AdminLibraryCostValue {
        amount: amount.to_string(),
        currency_code: currency.clone(),
        coverage: match coverage.as_str() {
            "complete" => AdminLibraryCostCoverage::Complete,
            "partial" => AdminLibraryCostCoverage::Partial,
            _ => return Err(facet_cursor_mismatch()),
        },
        unpriced_call_count,
    };
    validate_cost_value(&value)?;
    Ok(AdminLibraryFacetCursorValue::Cost { currency, amount, coverage, unpriced_call_count })
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;
    use uuid::Uuid;

    use super::*;
    #[test]
    fn admin_library_page_defaults_are_bounded() -> Result<(), ApiError> {
        let (window, filters, search) =
            normalize_admin_library_page(AdminLibraryListQuery::default())?;
        assert_eq!(window.limit, DEFAULT_ADMIN_LIBRARY_PAGE_LIMIT);
        assert_eq!(window.offset, 0);
        assert_eq!(
            window.order,
            AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Library, desc: false }
        );
        assert!(!filters.documents.present);
        assert!(search.search.is_none());
        assert!(search.numbers.is_empty());
        Ok(())
    }

    #[test]
    fn admin_library_page_parses_filters_and_caps_limit() -> Result<(), ApiError> {
        let query = AdminLibraryListQuery {
            search: Some("  alpha  ".to_string()),
            normalized_search: Some("alpha".to_string()),
            search_hints: Some(
                r#"{"numbers":[12],"costs":[{"amount":"1.23"}],"unavailable":true,"readiness":[true],"lifecycle":["active"]}"#
                    .to_string(),
            ),
            filters: Some(
                r#"{"library":["Documents"],"documents":[12,null],"cost":[{"amount":"1.50","currencyCode":"USD","coverage":"complete","unpricedCallCount":0}],"readiness":[true],"lifecycle":["active"],"queue":[],"workspace":["Primary"],"calls":[3]}"#
                    .to_string(),
            ),
            sort_by: Some(AdminLibrarySortKey::Queue),
            sort_order: Some(AdminLibrarySortOrder::Desc),
            offset: Some(25),
            limit: Some(u32::MAX),
        };
        let (window, filters, search) = normalize_admin_library_page(query)?;
        assert_normalized_admin_library_search(&search);
        assert_eq!(
            window.order,
            AdminLibraryPageOrder { column: AdminLibraryFacetColumn::Queue, desc: true }
        );
        assert_eq!(window.offset, 25);
        assert_eq!(window.limit, MAX_ADMIN_LIBRARY_PAGE_LIMIT);
        assert_normalized_admin_library_filters(&filters);
        Ok(())
    }

    fn assert_normalized_admin_library_search(search: &AdminLibrarySearchInput) {
        assert_eq!(search.search.as_deref(), Some("alpha"));
        assert_eq!(search.normalized_search.as_deref(), Some("alpha"));
        assert_eq!(search.numbers, vec![12]);
        assert_eq!(
            search.costs,
            vec![AdminLibraryCostSearch { amount: Some("1.23".to_string()), ..Default::default() }]
        );
        assert!(search.unavailable);
        assert_eq!(search.readiness, vec![true]);
        assert_eq!(search.lifecycle, vec!["active".to_string()]);
    }

    fn assert_normalized_admin_library_filters(filters: &AdminLibraryListFilters) {
        assert!(filters.library.present);
        assert_eq!(filters.library.values, vec!["Documents".to_string()]);
        assert!(filters.documents.include_null);
        assert_eq!(filters.documents.values, vec![12]);
        assert!(filters.readiness.present);
        assert_eq!(filters.readiness.values, vec![true]);
        assert!(!filters.queue.present);
        assert_eq!(
            filters.cost.values,
            vec![AdminLibraryCostValue {
                amount: "1.50".into(),
                currency_code: "USD".into(),
                coverage: AdminLibraryCostCoverage::Complete,
                unpriced_call_count: 0,
            }]
        );
    }

    #[test]
    fn admin_library_page_rejects_bad_search_hints() {
        let query = AdminLibraryListQuery {
            search_hints: Some(r#"{"costs":[{"amount":"not-a-decimal"}]}"#.to_string()),
            ..AdminLibraryListQuery::default()
        };
        assert!(matches!(normalize_admin_library_page(query), Err(ApiError::BadRequest(_))));
        let query = AdminLibraryListQuery {
            search_hints: Some(r#"{"nope":[1]}"#.to_string()),
            ..AdminLibraryListQuery::default()
        };
        assert!(matches!(normalize_admin_library_page(query), Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn cost_search_rejects_empty_invalid_and_legacy_values() {
        for raw in [
            r#"{"costs":[{}]}"#,
            r#"{"costs":["1.23"]}"#,
            r#"{"costs":[{"amount":"-1.23"}]}"#,
            r#"{"costs":[{"amount":"1.234"}]}"#,
            r#"{"costs":[{"currencyCode":"usd"}]}"#,
            r#"{"costs":[{"unpricedCallCount":-1}]}"#,
            r#"{"costs":[{"amount":"1.23","unknown":true}]}"#,
        ] {
            assert!(
                matches!(
                    normalize_admin_library_search(None, None, Some(raw.into())),
                    Err(ApiError::BadRequest(_))
                ),
                "must reject {raw}"
            );
        }
    }

    #[test]
    fn facet_search_hints_are_column_local() -> Result<(), ApiError> {
        let cost = r#"{"costs":[{"amount":"1.23","currencyCode":"EUR","coverage":"partial","unpricedCallCount":3}]}"#;
        let accepted = normalize_facet_hints(AdminLibraryFacetColumn::Cost, Some(cost.into()))?;
        assert_eq!(accepted.costs[0].currency_code.as_deref(), Some("EUR"));
        assert_eq!(accepted.costs[0].unpriced_call_count, Some(3));
        for column in [
            AdminLibraryFacetColumn::Library,
            AdminLibraryFacetColumn::Workspace,
            AdminLibraryFacetColumn::Calls,
            AdminLibraryFacetColumn::Documents,
            AdminLibraryFacetColumn::Queue,
            AdminLibraryFacetColumn::Readiness,
            AdminLibraryFacetColumn::Lifecycle,
        ] {
            assert!(matches!(
                normalize_facet_hints(column, Some(cost.into())),
                Err(ApiError::BadRequest(_))
            ));
        }
        for (column, raw) in [
            (AdminLibraryFacetColumn::Calls, r#"{"numbers":[1234],"unavailable":true}"#),
            (AdminLibraryFacetColumn::Cost, r#"{"unavailable":true}"#),
            (AdminLibraryFacetColumn::Readiness, r#"{"readiness":[false]}"#),
            (AdminLibraryFacetColumn::Lifecycle, r#"{"lifecycle":["archived"]}"#),
        ] {
            normalize_facet_hints(column, Some(raw.into()))?;
        }
        Ok(())
    }

    #[test]
    fn admin_library_page_rejects_unknown_filter_keys() {
        let query = AdminLibraryListQuery {
            filters: Some(r#"{"nope":[1]}"#.to_string()),
            ..AdminLibraryListQuery::default()
        };
        assert!(matches!(normalize_admin_library_page(query), Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn admin_library_page_rejects_too_many_filter_values() {
        let values = (0..=MAX_ADMIN_LIBRARY_FILTER_VALUES_PER_COLUMN)
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let query = AdminLibraryListQuery {
            filters: Some(format!(r#"{{"queue":[{values}]}}"#)),
            ..AdminLibraryListQuery::default()
        };
        assert!(matches!(normalize_admin_library_page(query), Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn admin_library_item_omits_unreadable_cost() -> Result<(), ApiError> {
        let item = map_admin_library_item(AdminLibraryPageRow {
            id: Uuid::from_u128(1),
            slug: "documents".to_string(),
            display_name: "Documents".to_string(),
            lifecycle_state: "active".to_string(),
            include_document_hint_in_mcp_answers: true,
            workspace_id: Uuid::from_u128(2),
            workspace_slug: "primary".to_string(),
            workspace_display_name: "Primary".to_string(),
            missing_binding_purposes: vec!["embed_chunk".to_string()],
            queued_job_count: 7,
            cost_blocked: true,
            currency_count: 1,
            total_cost: Decimal::ZERO,
            currency_code: "USD".to_string(),
            document_count: 0,
            provider_call_count: 0,
            unpriced_provider_call_count: 0,
            coverage_state: Some("unknown".to_string()),
        })?;
        assert!(!item.ingestion_readiness.ready);
        assert_eq!(
            item.ingestion_readiness.missing_binding_purposes,
            vec![AiBindingPurpose::EmbedChunk]
        );
        assert_eq!(item.queued_job_count, 7);
        assert!(item.cost_summary.is_none());
        assert!(item.cost_value.is_none());
        Ok(())
    }

    #[test]
    fn admin_library_item_rounds_the_canonical_cost_value() -> Result<(), ApiError> {
        let item = map_admin_library_item(AdminLibraryPageRow {
            id: Uuid::from_u128(1),
            slug: "documents".to_string(),
            display_name: "Documents".to_string(),
            lifecycle_state: "active".to_string(),
            include_document_hint_in_mcp_answers: true,
            workspace_id: Uuid::from_u128(2),
            workspace_slug: "primary".to_string(),
            workspace_display_name: "Primary".to_string(),
            missing_binding_purposes: Vec::new(),
            queued_job_count: 0,
            cost_blocked: false,
            currency_count: 1,
            total_cost: Decimal::new(12345, 4),
            currency_code: "USD".to_string(),
            document_count: 3,
            provider_call_count: 3,
            unpriced_provider_call_count: 1,
            coverage_state: Some("partial".to_string()),
        })?;
        let value = item.cost_value.expect("readable cost produces a value");
        assert_eq!(value.amount, "1.23");
        assert_eq!(value.currency_code, "USD");
        assert_eq!(value.coverage, AdminLibraryCostCoverage::Partial);
        assert_eq!(value.unpriced_call_count, 1);
        // The lossless summary keeps every digit the rollup carried.
        assert_eq!(item.cost_summary.expect("summary").total_cost, "1.2345");
        Ok(())
    }

    #[test]
    fn facet_cursor_round_trips_and_rejects_other_columns() -> Result<(), ApiError> {
        let cursor = AdminLibraryFacetCursor {
            column: AdminLibraryFacetColumn::Cost,
            text: None,
            number: Some(1),
            decimal: Some("1.50".to_string()),
            flag: None,
            currency: Some("USD".to_string()),
            coverage: Some("partial".to_string()),
            is_null: false,
        };
        let token = encode_facet_cursor(&cursor);
        let decoded = decode_facet_cursor(&token)?;
        assert_eq!(decoded.column, AdminLibraryFacetColumn::Cost);
        assert_eq!(decoded.decimal.as_deref(), Some("1.50"));
        match cursor_value(&decoded)? {
            AdminLibraryFacetCursorValue::Cost {
                currency,
                amount,
                coverage,
                unpriced_call_count,
            } => {
                assert_eq!(currency, "USD");
                assert_eq!(amount, Decimal::new(150, 2));
                assert_eq!(coverage, "partial");
                assert_eq!(unpriced_call_count, 1);
            }
            other => panic!("expected cost cursor, got {other:?}"),
        }
        let number_cursor =
            AdminLibraryFacetCursor { column: AdminLibraryFacetColumn::Documents, ..cursor };
        assert!(cursor_value(&number_cursor).is_err());
        Ok(())
    }

    #[test]
    fn admin_library_facets_row_maps_typed_cost_metadata() {
        let row = AdminLibraryFacetRow {
            value_text: None,
            value_number: Some(1),
            value_decimal: Some(Decimal::new(150, 2)),
            value_flag: None,
            value_currency: Some("USD".to_string()),
            value_coverage: Some("partial".to_string()),
            facet_count: 4,
        };
        let value = map_facet_value(AdminLibraryFacetColumn::Cost, &row);
        assert_eq!(value.count, 4);
        assert_eq!(value.decimal.as_deref(), Some("1.50"));
        assert_eq!(value.number, None);
        assert_eq!(value.unpriced_call_count, Some(1));
        assert_eq!(value.currency_code.as_deref(), Some("USD"));
        assert_eq!(value.coverage, Some(AdminLibraryCostCoverage::Partial));
        assert!(value.text.is_none());
        assert!(value.flag.is_none());
        assert!(!cursor_is_null(AdminLibraryFacetColumn::Cost, &row));
        let null_row = AdminLibraryFacetRow::default();
        assert!(cursor_is_null(AdminLibraryFacetColumn::Cost, &null_row));
    }
}
