use super::*;
use chrono::{DateTime, Utc};

use crate::domains::billing::CostRollupFreshness;

/// Primary slim paginated document list.
///
/// Response shape is `DocumentListPageResponse { items, next_cursor, total_count }`.
/// Each `items[i]` is a `ContentDocumentListItem` with only the fields the
/// documents page actually renders — status/readiness are derived server-side.
/// The inspector panel fetches full detail via `/content/documents/{id}`.
#[tracing::instrument(
    level = "info",
    name = "http.list_documents",
    skip_all,
    fields(library_id = %library_id, include_deleted, limit, document_count, elapsed_ms)
)]
#[utoipa::path(
    get,
    path = "/v1/content/libraries/{libraryId}/documents",
    tag = "content",
    operation_id = "listContentDocuments",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library that owns the document collection"),
        crate::interfaces::http::content::types::ListDocumentsQuery,
    ),
    responses(
        (status = 200, description = "Document list page", body = DocumentListPageResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
pub async fn list_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<ListDocumentsQuery>,
) -> Result<Json<DocumentListPageResponse>, ApiError> {
    const DEFAULT_LIMIT: u32 = 50;
    // Cap the page size at 1000 rows to match the largest option the
    // documents UI exposes. The previous 200 clamp silently truncated
    // every larger request — `pageSize=1000` would render "1-200 of N"
    // and batch-cancel would only ever act on the first 200 matches,
    // which was surprising to operators running bulk ops on queued
    // libraries with thousands of pending docs. 1000 rows at ~1-2 KB
    // per row is a ~1-2 MB slim response which the frontend already
    // buffers without pressure.
    const MAX_LIMIT: u32 = 1000;

    let started_at = std::time::Instant::now();
    let span = tracing::Span::current();
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    let include_deleted = query.include_deleted.unwrap_or(false);
    span.record("include_deleted", include_deleted);

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    span.record("limit", limit);

    let cursor = match query.cursor.as_deref() {
        Some(token) => Some(decode_document_list_cursor(token)?),
        None => None,
    };

    let sort = match query.sort_by {
        Some(DocumentListSortKey::UploadedAt) | None => DocumentListSortColumn::CreatedAt,
        Some(DocumentListSortKey::FileName) => DocumentListSortColumn::ExternalKey,
        Some(DocumentListSortKey::FileType) => DocumentListSortColumn::MimeType,
        Some(DocumentListSortKey::FileSize) => DocumentListSortColumn::ByteSize,
        Some(DocumentListSortKey::Status) => DocumentListSortColumn::DerivedStatus,
        Some(DocumentListSortKey::Cost) => DocumentListSortColumn::Cost,
        Some(DocumentListSortKey::PipelineTime) => DocumentListSortColumn::PipelineTime,
        Some(DocumentListSortKey::FinishedAt) => DocumentListSortColumn::FinishedAt,
    };
    // Default sort order matches the frontend: newest first.
    let sort_desc = !matches!(query.sort_order, Some(DocumentListSortOrder::Asc));
    // A cursor is only valid against the same effective sort: reject a cursor
    // produced for another column, direction, or with a primary value shaped
    // for another column before it reaches any Postgres cast.
    let effective_sort_key = query.sort_by.unwrap_or(DocumentListSortKey::UploadedAt);
    if let Some(value) = cursor.as_ref() {
        value.validate_for_sort(effective_sort_key, sort_desc)?;
    }

    // Parse the shared grid scope once: the derived-status pill (enum for the
    // page command, text for the counts), the search-status buckets, the typed
    // column filters and the typed range hints.
    let terms = parse_document_grid_terms(
        query.search_status.as_deref(),
        query.status.as_deref(),
        query.filters.as_deref(),
        query.search_ranges.as_deref(),
    )?;
    // The whole cursor chain shares one snapshot so sorting a worker-holding
    // duration is deterministic: a cursor page adopts the cursor's instant and
    // must agree with any supplied `snapshotAt`; a first page adopts the
    // supplied instant or now.
    let snapshot_at = match cursor.as_ref() {
        Some(value) => {
            if query.snapshot_at.is_some_and(|requested| requested != value.snapshot_at) {
                return Err(ApiError::BadRequest(
                    "snapshotAt does not match the list cursor".to_string(),
                ));
            }
            value.snapshot_at
        }
        None => query.snapshot_at.unwrap_or_else(Utc::now),
    };

    let id_filter =
        resolve_document_id_filter(&state, library.id, query.ids.as_deref(), query.indexing)
            .await?;

    // Per-bucket counts are computed in the page's own snapshot; the UI
    // requests them once per filter set via `includeTotal=true` and reuses the
    // cached result while paging.
    let include_status_counts = query.include_total.unwrap_or(false);

    let result = state
        .application_services
        .content
        .list_documents_page(
            &state,
            ListDocumentsPageCommand {
                library_id: library.id,
                include_deleted,
                cursor: cursor.as_ref().map(|value| {
                    crate::infra::repositories::content_document_list_repository::DocumentListPageCursor {
                        created_at: value.created_at,
                        document_id: value.document_id,
                        text_value: value.text_value.clone(),
                        number_value: value.number_value,
                        finished_value: value.finished_value,
                        value_is_null: value.value_is_null,
                    }
                }),
                limit,
                search: query.search.clone(),
                normalized_search: query.normalized_search.clone(),
                search_statuses: terms.search_statuses.clone(),
                filters: terms.filters.clone(),
                search_ranges: terms.search_ranges.clone(),
                snapshot_at,
                sort,
                sort_desc,
                status_filter: terms.statuses.clone(),
                id_filter: id_filter.clone(),
                include_status_counts,
            },
        )
        .await?;

    span.record("document_count", result.items.len());

    let next_cursor = result.next_cursor.map(|value| {
        encode_document_list_cursor(&DocumentListCursor {
            created_at: value.created_at,
            document_id: value.document_id,
            text_value: value.text_value,
            number_value: value.number_value,
            finished_value: value.finished_value,
            value_is_null: value.value_is_null,
            sort: effective_sort_key,
            sort_desc,
            snapshot_at,
        })
    });

    // Status-bucket counts were computed in the same snapshot as the page; the
    // endpoint only emits them when the caller asked (`includeTotal=true`).
    let items: Vec<ContentDocumentListItem> =
        result.items.into_iter().map(map_document_list_entry).collect();

    span.record("elapsed_ms", started_at.elapsed().as_millis() as u64);
    Ok(Json(DocumentListPageResponse {
        items,
        next_cursor,
        total_count: result.total_count,
        status_counts: result.status_counts.map(|counts| DocumentListStatusCounts {
            total: counts.total.unwrap_or_default(),
            ready: counts.ready.unwrap_or_default(),
            processing: counts.processing.unwrap_or_default(),
            queued: counts.queued.unwrap_or_default(),
            failed: counts.failed.unwrap_or_default(),
            canceled: counts.canceled.unwrap_or_default(),
            empty: counts.empty.unwrap_or_default(),
        }),
        snapshot_at,
        scope_pending: result.scope_pending,
        cost_rollup_freshness: result.billing_rollup_freshness,
    }))
}

const MAX_FILTERS_JSON_BYTES: usize = 64 * 1024;

/// Parses the `filters` query parameter: a JSON object of strictly typed
/// per-column arrays. Absent/empty = no constraints; unknown keys, wrong
/// types, malformed timestamps/decimals and oversized payloads are a 400.
pub(super) fn parse_document_column_filters(
    raw: Option<&str>,
) -> Result<
    crate::infra::repositories::content_document_list_repository::DocumentListFilters,
    ApiError,
> {
    use crate::infra::repositories::content_document_list_repository::DocumentListFilters;
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(DocumentListFilters::default());
    };
    if raw.len() > MAX_FILTERS_JSON_BYTES {
        return Err(ApiError::BadRequest(format!(
            "filters payload exceeds {MAX_FILTERS_JSON_BYTES} bytes"
        )));
    }
    let parsed: DocumentColumnFilters = serde_json::from_str(raw)
        .map_err(|error| ApiError::BadRequest(format!("invalid filters payload: {error}")))?;
    parsed.into_repository()
}

const MAX_SEARCH_RANGES_JSON_BYTES: usize = 8 * 1024;

/// The cost facet's fail-closed decision, composed from the shared billing
/// readability guard and the whole-scope mixed-currency probe. One function so
/// the owning handler and its unit test share the exact rule.
pub(super) fn ensure_cost_facet_readable(
    library_id: Uuid,
    rollup_dirty: bool,
    terminal_error_code: Option<&str>,
    has_null_cost: bool,
) -> Result<(), ApiError> {
    crate::services::ops::billing::ensure_billing_rollup_readable(
        "library",
        library_id,
        rollup_dirty,
        terminal_error_code,
    )?;
    if has_null_cost {
        return Err(ApiError::Conflict(format!(
            "library {library_id} has billing costs in multiple currencies; request per-currency charges"
        )));
    }
    Ok(())
}

/// Facet-local typed hints projected onto the selected column.
pub(super) struct FacetSearchHints {
    pub statuses: Vec<String>,
    pub range: Option<(String, String)>,
}

/// Projects the facet-local typed hints onto the selected column and rejects a
/// hint that does not apply to it with a 400, so a wrong-field hint never
/// silently matches nothing.
pub(super) fn facet_search_hints(
    column: DocumentFacetColumn,
    ranges: crate::infra::repositories::content_document_list_repository::DocumentListSearchRanges,
    statuses: Vec<String>,
) -> Result<FacetSearchHints, ApiError> {
    let present = u8::from(ranges.file_size.is_some())
        + u8::from(ranges.pipeline_time.is_some())
        + u8::from(ranges.date.is_some());
    if present > 1 {
        return Err(ApiError::BadRequest("facetSearchRanges accepts a single range".to_string()));
    }
    let range = if let Some((min, max)) = ranges.file_size {
        if column != DocumentFacetColumn::FileSize {
            return Err(ApiError::BadRequest(
                "facetSearchRanges.fileSize applies only to the file_size facet".to_string(),
            ));
        }
        Some((min.to_string(), max.to_string()))
    } else if let Some((min, max)) = ranges.pipeline_time {
        if column != DocumentFacetColumn::PipelineTime {
            return Err(ApiError::BadRequest(
                "facetSearchRanges.pipelineTime applies only to the pipeline_time facet"
                    .to_string(),
            ));
        }
        Some((min.to_string(), max.to_string()))
    } else if let Some((min, max)) = ranges.date {
        if !matches!(column, DocumentFacetColumn::UploadedAt | DocumentFacetColumn::FinishedAt) {
            return Err(ApiError::BadRequest(
                "facetSearchRanges.date applies only to the uploaded_at or finished_at facet"
                    .to_string(),
            ));
        }
        Some((min.to_rfc3339(), max.to_rfc3339()))
    } else {
        None
    };
    if !statuses.is_empty() && column != DocumentFacetColumn::Status {
        return Err(ApiError::BadRequest(
            "facetSearchStatus applies only to the status facet".to_string(),
        ));
    }
    Ok(FacetSearchHints { statuses, range })
}

/// Parses the `searchRanges` query parameter: a JSON object of half-open typed
/// ranges that are OR alternatives to the text/status search. Unknown fields,
/// non-integer / out-of-`i64` / negative / reversed bounds, malformed dates and
/// oversized payloads are a 400.
pub(super) fn parse_search_ranges(
    raw: Option<&str>,
) -> Result<
    crate::infra::repositories::content_document_list_repository::DocumentListSearchRanges,
    ApiError,
> {
    use crate::infra::repositories::content_document_list_repository::DocumentListSearchRanges;
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(DocumentListSearchRanges::default());
    };
    if raw.len() > MAX_SEARCH_RANGES_JSON_BYTES {
        return Err(ApiError::BadRequest(format!(
            "searchRanges payload exceeds {MAX_SEARCH_RANGES_JSON_BYTES} bytes"
        )));
    }
    let parsed: DocumentSearchRanges = serde_json::from_str(raw)
        .map_err(|error| ApiError::BadRequest(format!("invalid searchRanges payload: {error}")))?;
    parsed.into_repository()
}

/// The parsed text/status/filter/range scope shared by the list page, its
/// counts and the cost-facet guard: one owner so they cannot disagree.
struct DocumentGridTerms {
    search_statuses: Vec<String>,
    statuses: Vec<DocumentStatus>,
    status_filter: Vec<String>,
    filters: crate::infra::repositories::content_document_list_repository::DocumentListFilters,
    search_ranges:
        crate::infra::repositories::content_document_list_repository::DocumentListSearchRanges,
}

fn parse_document_grid_terms(
    search_status: Option<&str>,
    status: Option<&str>,
    filters: Option<&str>,
    search_ranges: Option<&str>,
) -> Result<DocumentGridTerms, ApiError> {
    fn status_strings(raw: &str) -> Result<Vec<String>, ApiError> {
        Ok(parse_document_status_filter(raw)?
            .into_iter()
            .map(|status| status.as_str().to_string())
            .collect())
    }
    let statuses = match status {
        None => Vec::new(),
        Some(raw) => parse_document_status_filter(raw)?,
    };
    Ok(DocumentGridTerms {
        search_statuses: match search_status {
            None => Vec::new(),
            Some(raw) => status_strings(raw)?,
        },
        status_filter: statuses.iter().map(|status| status.as_str().to_string()).collect(),
        statuses,
        filters: parse_document_column_filters(filters)?,
        search_ranges: parse_search_ranges(search_ranges)?,
    })
}

/// The library/text/scope inputs of one grid count query, grouped so the
/// builder takes a single cohesive value. `terms` owns the parsed
/// filter/range scope that the repository query borrows from.
struct DocumentGridCountScope<'a> {
    library_id: Uuid,
    include_deleted: bool,
    search: Option<&'a str>,
    normalized_search: Option<&'a str>,
    terms: &'a DocumentGridTerms,
    id_filter: Option<&'a [Uuid]>,
    snapshot_at: DateTime<Utc>,
}

/// The count/cost-guard query for an already-parsed grid scope.
fn grid_count_query(
    scope: DocumentGridCountScope<'_>,
) -> content_document_list_repository::DocumentListCountQuery<'_> {
    content_document_list_repository::DocumentListCountQuery {
        library_id: scope.library_id,
        include_deleted: scope.include_deleted,
        search: scope.search,
        normalized_search: scope.normalized_search,
        search_statuses: &scope.terms.search_statuses,
        filters: &scope.terms.filters,
        search_ranges: &scope.terms.search_ranges,
        status_filter: &scope.terms.status_filter,
        id_filter: scope.id_filter,
        snapshot_at: scope.snapshot_at,
    }
}

/// Distinct values of one grid column, keyset-paginated on the canonical
/// value. Self-excludes the selected column's own filter and applies every
/// other filter/search/scope predicate over the same base CTE as the list, so
/// a facet count and the page it opens cannot disagree.
#[tracing::instrument(
    level = "info",
    name = "http.list_document_facets",
    skip_all,
    fields(library_id = %library_id, facet_column = ?query.facet_column, value_count)
)]
#[utoipa::path(
    get,
    path = "/v1/content/libraries/{libraryId}/documents/facets",
    tag = "content",
    operation_id = "listContentDocumentFacets",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library that owns the document collection"),
        crate::interfaces::http::content::types::ListDocumentFacetsQuery,
    ),
    responses(
        (status = 200, description = "Distinct facet values for one grid column", body = DocumentFacetsResponse),
        (status = 400, description = "Invalid filters or facet cursor payload"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
pub async fn list_document_facets(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<ListDocumentFacetsQuery>,
) -> Result<Json<DocumentFacetsResponse>, ApiError> {
    const DEFAULT_FACET_LIMIT: u32 = 50;
    const MAX_FACET_LIMIT: u32 = 200;

    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    let include_deleted = query.include_deleted.unwrap_or(false);
    let facet_limit = query.facet_limit.unwrap_or(DEFAULT_FACET_LIMIT).clamp(1, MAX_FACET_LIMIT);
    let column = query.facet_column;

    let terms = parse_document_grid_terms(
        query.search_status.as_deref(),
        query.status.as_deref(),
        query.filters.as_deref(),
        query.search_ranges.as_deref(),
    )?;
    // Facet-local typed hints: validated against the selected column before the
    // cost guard so a wrong-field hint is a clear 400.
    let facet_ranges = parse_search_ranges(query.facet_search_ranges.as_deref())?;
    let facet_statuses: Vec<String> = match query.facet_search_status.as_deref() {
        None => Vec::new(),
        Some(raw) => parse_document_status_filter(raw)?
            .into_iter()
            .map(|status| status.as_str().to_string())
            .collect(),
    };
    let hints = facet_search_hints(column, facet_ranges, facet_statuses)?;
    let facet_search_statuses = hints.statuses;
    let facet_search_range = hints.range;
    let decoded_cursor = match query.facet_cursor.as_deref() {
        Some(token) => Some(decode_document_facet_cursor(token)?),
        None => None,
    };
    // A cursor page reuses the cursor's snapshot and must agree with any
    // supplied `snapshotAt`; a first page adopts the supplied instant or now.
    let snapshot_at = match &decoded_cursor {
        Some(cursor) => {
            if cursor.column != column {
                return Err(ApiError::BadRequest(
                    "facetCursor column does not match facetColumn".to_string(),
                ));
            }
            if query.snapshot_at.is_some_and(|requested| requested != cursor.snapshot_at) {
                return Err(ApiError::BadRequest(
                    "snapshotAt does not match the facet cursor".to_string(),
                ));
            }
            cursor.snapshot_at
        }
        None => query.snapshot_at.unwrap_or_else(Utc::now),
    };
    let cursor_value = match &decoded_cursor {
        Some(cursor) => Some(cursor.to_repository()?),
        None => None,
    };
    let id_filter =
        resolve_document_id_filter(&state, library.id, query.ids.as_deref(), query.indexing)
            .await?;

    // Any facet whose scope genuinely reads cost — the facet column is `cost`,
    // or an effective `cost` filter or literal/normalized search is present —
    // must not be resolved from stale costs. `document_scope_reads_cost` is the
    // one owner of that decision shared with the list path; it sees the facet
    // column so a `cost` facet is cost-dependent even though it self-excludes
    // its own filter. The scope aggregate and the facet page are read in one
    // REPEATABLE READ snapshot, so the health marker describes the same
    // generation as the values and is truthful for a non-cost facet too.
    let facet_cost_dependent = content_document_list_repository::document_scope_reads_cost(
        None,
        query.search.as_deref(),
        query.normalized_search.as_deref(),
        &terms.filters,
        Some(column.to_repository()),
    );
    let content_document_list_repository::DocumentFacetRead {
        page: facet_page,
        rollup_dirty,
        terminal_error_code,
        has_null_cost,
    } = content_document_list_repository::read_document_facet_page(
        &state.persistence.postgres,
        content_document_list_repository::DocumentListFacetQuery {
            library_id: library.id,
            include_deleted,
            column: column.to_repository(),
            search: query.search.as_deref(),
            normalized_search: query.normalized_search.as_deref(),
            search_statuses: &terms.search_statuses,
            facet_search: query.facet_search.as_deref(),
            facet_search_statuses: &facet_search_statuses,
            facet_search_range,
            filters: &terms.filters,
            search_ranges: &terms.search_ranges,
            status_filter: &terms.status_filter,
            snapshot_at,
            cursor: cursor_value.as_ref(),
            limit: facet_limit,
            id_filter: id_filter.as_deref(),
        },
        grid_count_query(DocumentGridCountScope {
            library_id: library.id,
            include_deleted,
            search: query.search.as_deref(),
            normalized_search: query.normalized_search.as_deref(),
            terms: &terms,
            id_filter: id_filter.as_deref(),
            snapshot_at,
        }),
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    // The conflict a mixed-currency document raises on the list page must fire
    // here before pagination, even when that document's NULL cost group would
    // otherwise land on a later facet page: otherwise this page advertises
    // "clean" cost values the list can later 409. The scan runs over the whole
    // authorized, filtered scope, not the returned page. Only the `cost` facet
    // is affected by mixed currency; a terminal-but-clean scope keeps its 500.
    if column == DocumentFacetColumn::Cost {
        ensure_cost_facet_readable(
            library.id,
            rollup_dirty,
            terminal_error_code.as_deref(),
            has_null_cost,
        )?;
    }
    let cost_rollup_freshness = CostRollupFreshness::from_rollup_dirty(rollup_dirty);
    // A cost-dependent facet cannot serve a distribution while the rollup
    // reconciles: its values or counts would be over stale costs. It answers
    // pending — empty values, no continuation — while a non-cost facet stays
    // available and truthfully reconciling.
    let scope_pending = rollup_dirty && facet_cost_dependent;

    let (values, next_cursor) = if scope_pending {
        (Vec::new(), None)
    } else {
        let values: Vec<DocumentFacetValue> =
            facet_page.rows.iter().map(|row| facet_value_for_column(column, row)).collect();
        let next_cursor = match facet_page.rows.last() {
            Some(last) if facet_page.has_more => Some(encode_document_facet_cursor(
                &facet_cursor_from_row(snapshot_at, column, last),
            )),
            _ => None,
        };
        (values, next_cursor)
    };

    let span = tracing::Span::current();
    span.record("value_count", values.len());
    Ok(Json(DocumentFacetsResponse {
        column,
        values,
        next_cursor,
        snapshot_at,
        scope_pending,
        cost_rollup_freshness,
    }))
}

fn facet_value_for_column(
    column: DocumentFacetColumn,
    row: &content_document_list_repository::DocumentFacetRow,
) -> DocumentFacetValue {
    let value = match column {
        DocumentFacetColumn::FileName
        | DocumentFacetColumn::FileType
        | DocumentFacetColumn::Status => match &row.value_text {
            Some(text) => FacetValue::Text(text.clone()),
            None => FacetValue::Null(()),
        },
        DocumentFacetColumn::FileSize | DocumentFacetColumn::PipelineTime => {
            match row.value_number {
                Some(number) => FacetValue::Number(number),
                None => FacetValue::Null(()),
            }
        }
        DocumentFacetColumn::UploadedAt | DocumentFacetColumn::FinishedAt => {
            match row.value_timestamp {
                Some(timestamp) => {
                    FacetValue::Text(timestamp.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
                }
                None => FacetValue::Null(()),
            }
        }
        DocumentFacetColumn::Cost => match &row.value_decimal {
            Some(decimal) => FacetValue::Text(decimal.to_string()),
            None => FacetValue::Null(()),
        },
    };
    DocumentFacetValue { value, count: row.facet_count }
}

fn facet_cursor_from_row(
    snapshot_at: DateTime<Utc>,
    column: DocumentFacetColumn,
    row: &content_document_list_repository::DocumentFacetRow,
) -> DocumentFacetCursor {
    let mut cursor = DocumentFacetCursor {
        snapshot_at,
        column,
        value: None,
        number_value: None,
        value_is_null: false,
    };
    match column {
        DocumentFacetColumn::FileName
        | DocumentFacetColumn::FileType
        | DocumentFacetColumn::Status => {
            cursor.value = row.value_text.clone();
            cursor.value_is_null = row.value_text.is_none();
        }
        DocumentFacetColumn::FileSize | DocumentFacetColumn::PipelineTime => {
            cursor.number_value = row.value_number;
            cursor.value_is_null = row.value_number.is_none();
        }
        DocumentFacetColumn::UploadedAt | DocumentFacetColumn::FinishedAt => {
            cursor.value = row
                .value_timestamp
                .map(|timestamp| timestamp.to_rfc3339_opts(chrono::SecondsFormat::Micros, true));
            cursor.value_is_null = row.value_timestamp.is_none();
        }
        DocumentFacetColumn::Cost => {
            cursor.value = row.value_decimal.map(|decimal| decimal.to_string());
            cursor.value_is_null = row.value_decimal.is_none();
        }
    }
    cursor
}

fn map_document_list_entry(entry: ContentDocumentListEntry) -> ContentDocumentListItem {
    ContentDocumentListItem {
        id: entry.id,
        library_id: entry.library_id,
        workspace_id: entry.workspace_id,
        file_name: entry.file_name,
        file_type: entry.file_type,
        file_size: entry.file_size,
        uploaded_at: entry.uploaded_at,
        document_state: entry.document_state,
        external_key: entry.external_key,
        status: entry.status,
        readiness: entry.readiness,
        stage: entry.stage,
        progress_percent: entry.progress_percent,
        processing_started_at: entry.processing_started_at,
        processing_finished_at: entry.processing_finished_at,
        failure_code: entry.failure_code,
        failure_message: entry.failure_message,
        retryable: entry.retryable,
        source_kind: entry.source_kind,
        source_uri: entry.source_uri,
        document_hint: entry.document_hint,
        source_access: entry.source_access,
        cost: entry.cost_total.map(|total| total.to_string()),
        cost_currency_code: entry.cost_currency_code,
    }
}

pub(super) fn parse_document_status_filter(raw: &str) -> Result<Vec<DocumentStatus>, ApiError> {
    let mut out = Vec::new();
    for token in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let status = match token {
            "canceled" => DocumentStatus::Canceled,
            "empty" => DocumentStatus::Empty,
            "failed" => DocumentStatus::Failed,
            "processing" => DocumentStatus::Processing,
            "queued" => DocumentStatus::Queued,
            "ready" => DocumentStatus::Ready,
            _ => {
                return Err(ApiError::BadRequest(format!(
                    "unknown status filter value `{token}`; allowed: canceled, empty, failed, processing, queued, ready"
                )));
            }
        };
        out.push(status);
    }
    Ok(out)
}

/// Which documents this page may contain: `None` for every document,
/// `Some(ids)` for exactly those — the empty set included.
///
/// Two independent sources can narrow the page and they compose by
/// intersection: `ids`, a deep link (e.g. the Graph view's "open document"
/// action carrying `documentId`) resolving a specific document through the
/// same shared list derivation that powers the table, so the inspector opens
/// even when the target is off the loaded page; and `indexing`, a broken
/// indexing invariant the server resolves to a document set of its own.
///
/// `indexing` resolving to nothing must produce an empty page. It is the case
/// the filter exists for — an operator following the `vector_index_gap` notice
/// after the gap has been reprocessed away — and answering it with the whole
/// library would repeat the defect the filter was added to fix.
///
/// An explicit `ids` parameter must name at least one document. An empty
/// parameter is rejected rather than silently expanding to the whole library.
async fn resolve_document_id_filter(
    state: &AppState,
    library_id: Uuid,
    raw_ids: Option<&str>,
    indexing: Option<DocumentIndexingFilter>,
) -> Result<Option<Vec<Uuid>>, ApiError> {
    let explicit = match raw_ids {
        None => None,
        Some(raw) => Some(parse_document_id_filter(raw)?),
    };
    let indexed = match indexing {
        None => None,
        Some(DocumentIndexingFilter::VectorGap) => Some(
            state
                .document_store
                .list_documents_missing_chunk_vectors(library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
        ),
        Some(DocumentIndexingFilter::GraphGap) => Some(
            crate::infra::repositories::content_document_list_repository::list_library_documents_with_graph_gap(
                &state.persistence.postgres,
                library_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
        ),
        Some(DocumentIndexingFilter::GraphReady) => Some(
            crate::infra::repositories::content_document_list_repository::list_library_documents_graph_ready(
                &state.persistence.postgres,
                library_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
        ),
    };
    Ok(compose_document_id_filter(explicit, indexed))
}

/// Two independent narrowings compose by intersection. `None` on both sides is
/// the only way to end up with no filter at all — an empty set from either
/// source stays an empty set, and reaches the query as an empty page.
pub(super) fn compose_document_id_filter(
    explicit: Option<Vec<Uuid>>,
    indexed: Option<Vec<Uuid>>,
) -> Option<Vec<Uuid>> {
    match (explicit, indexed) {
        (None, None) => None,
        (Some(ids), None) | (None, Some(ids)) => Some(ids),
        (Some(explicit), Some(indexed)) => {
            let eligible: std::collections::HashSet<Uuid> = indexed.into_iter().collect();
            Some(explicit.into_iter().filter(|id| eligible.contains(id)).collect())
        }
    }
}

pub(super) fn parse_document_id_filter(raw: &str) -> Result<Vec<Uuid>, ApiError> {
    let mut out = Vec::new();
    for token in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let id = Uuid::parse_str(token).map_err(|_| {
            ApiError::BadRequest(format!("invalid document id `{token}` in ids filter"))
        })?;
        out.push(id);
    }
    if out.is_empty() {
        return Err(ApiError::BadRequest("ids filter must name at least one document".to_owned()));
    }
    Ok(out)
}

#[tracing::instrument(
    level = "info",
    name = "http.list_chunks",
    skip_all,
    fields(document_id = ?query.document_id, item_count)
)]
#[utoipa::path(
    get,
    path = "/v1/chunks",
    tag = "content",
    operation_id = "listChunks",
    params(crate::interfaces::http::content::types::ChunksQuery),
    responses(
        (status = 200, description = "Chunks for the requested revision", body = [ChunkSummary]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
    ),
)]
pub async fn list_chunks(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ChunksQuery>,
) -> Result<Json<Vec<ChunkSummary>>, ApiError> {
    let span = tracing::Span::current();
    auth.require_any_scope(POLICY_DOCUMENTS_READ)?;

    let document_id =
        query.document_id.ok_or_else(|| ApiError::BadRequest("documentId is required".into()))?;
    let document =
        load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
            .await?;
    let revision_id = resolve_readable_revision_id(&state, document_id).await?;
    let items = match revision_id {
        Some(revision_id) => {
            state.application_services.content.list_chunks(&state, revision_id).await?
        }
        None => Vec::new(),
    };

    let summaries: Vec<ChunkSummary> = items
        .into_iter()
        .map(|chunk| ChunkSummary {
            id: chunk.id,
            document_id,
            library_id: document.library_id,
            ordinal: chunk.chunk_index,
            content: chunk.normalized_text,
            token_count: chunk.token_count,
        })
        .collect();
    span.record("item_count", summaries.len());
    require_revision_still_current(&state, document_id, revision_id).await?;
    Ok(Json(summaries))
}

/// Creates a document in the target library. Content-negotiated on
/// `Content-Type`: `application/json` admits metadata/inline-text documents;
/// `multipart/form-data` admits a file upload. Both bodies converge on the same
/// `CreateDocumentAdmission` result and the same 201 + Location response —
/// there is one creation door, not two.
#[tracing::instrument(level = "info", name = "http.create_document", skip_all, fields(library_id = %library_id))]
#[utoipa::path(
    post,
    path = "/v1/content/libraries/{libraryId}/documents",
    tag = "content",
    operation_id = "createContentDocument",
    params(("libraryId" = uuid::Uuid, Path, description = "Library that owns the new document")),
    request_body(
        content(
            (CreateDocumentRequest = "application/json"),
            (CreateDocumentMultipartRequest = "multipart/form-data"),
        ),
        description = "JSON body for a metadata/inline-text document, OR multipart/form-data for a file upload",
    ),
    responses(
        (status = 201, description = "Newly created document", body = CreateDocumentResponse),
        (status = 400, description = "Invalid request payload"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
        (status = 409, description = "An active document with this external key already exists"),
        (status = 415, description = "Content-Type is neither application/json nor multipart/form-data"),
    ),
)]
pub async fn create_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    let CreateDocumentAdmission { document, mutation } = if content_type
        .starts_with("multipart/form-data")
    {
        let multipart = Multipart::from_request(request, &state)
            .await
            .map_err(|error| ApiError::BadRequest(format!("invalid multipart body: {error}")))?;
        let payload = parse_upload_multipart(&state, multipart).await?;
        state
            .application_services
            .content
            .upload_inline_document(
                &state,
                UploadInlineDocumentCommand {
                    workspace_id: library.workspace_id,
                    library_id: library.id,
                    external_key: resolve_upload_external_key(
                        payload.external_key.clone(),
                        &payload.file_name,
                    ),
                    idempotency_key: payload.idempotency_key.clone(),
                    requested_by_principal_id: Some(auth.principal_id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    title: payload.title.or_else(|| Some(payload.file_name.clone())),
                    document_hint: payload.document_hint,
                    file_name: payload.file_name,
                    mime_type: payload.mime_type,
                    file_bytes: payload.file_bytes,
                    parent_external_key: payload.parent_external_key,
                },
            )
            .await?
    } else if content_type.starts_with("application/json") {
        const MAX_JSON_BODY_BYTES: usize = 25 * 1024 * 1024;
        let bytes = axum::body::to_bytes(request.into_body(), MAX_JSON_BODY_BYTES).await.map_err(
            |error| ApiError::BadRequest(format!("failed to read request body: {error}")),
        )?;
        let payload: CreateDocumentRequest = serde_json::from_slice(&bytes)
            .map_err(|error| ApiError::BadRequest(format!("invalid JSON body: {error}")))?;
        state
            .application_services
            .content
            .admit_document(
                &state,
                AdmitDocumentCommand {
                    workspace_id: library.workspace_id,
                    library_id: library.id,
                    external_key: payload.external_key.clone(),
                    file_name: payload.file_name.clone(),
                    idempotency_key: payload.idempotency_key.clone(),
                    created_by_principal_id: Some(auth.principal_id),
                    request_surface: "rest".to_string(),
                    source_identity: None,
                    revision: build_revision_metadata(&payload)?,
                    parent_external_key: payload.parent_external_key.clone(),
                },
            )
            .await?
    } else {
        return Err(ApiError::unsupported_media_type(format!(
            "Content-Type must be application/json or multipart/form-data, got `{content_type}`"
        )));
    };

    let location = format!("/v1/content/documents/{}", document.document.id);
    let body = CreateDocumentResponse {
        document: map_document_summary(document),
        mutation: map_mutation_admission(mutation),
    };
    let mut response = (axum::http::StatusCode::CREATED, Json(body)).into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(&location) {
        response.headers_mut().insert(axum::http::header::LOCATION, value);
    }
    Ok(response)
}

#[tracing::instrument(
    level = "info",
    name = "http.get_document",
    skip_all,
    fields(document_id = %document_id)
)]
#[utoipa::path(
    get,
    path = "/v1/content/documents/{documentId}",
    tag = "content",
    operation_id = "getContentDocument",
    params(("documentId" = uuid::Uuid, Path, description = "Document identifier")),
    responses(
        (status = 200, description = "Content document detail", body = ContentDocumentDetailResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
        (status = 404, description = "Document not found"),
    ),
)]
pub async fn get_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<ContentDocumentDetailResponse>, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let summary = state.application_services.content.get_document(&state, document_id).await?;
    let revision_id = resolve_readable_revision_id(&state, document_id).await?;
    if summary.active_revision.as_ref().map(|revision| revision.id) != revision_id {
        return Err(ApiError::resource_not_found("document", document_id));
    }
    current_document_response(&state, summary, revision_id).await
}

#[tracing::instrument(
    level = "info",
    name = "http.patch_document_metadata",
    skip_all,
    fields(document_id = %document_id)
)]
#[utoipa::path(
    patch,
    path = "/v1/content/documents/{documentId}",
    tag = "content",
    operation_id = "patchContentDocumentMetadata",
    params(("documentId" = uuid::Uuid, Path, description = "Document identifier")),
    request_body = PatchDocumentMetadataRequest,
    responses(
        (status = 200, description = "Updated content document detail", body = ContentDocumentDetailResponse),
        (status = 409, description = "Protected metadata requires an accepted replacement intent"),
        (status = 400, description = "Patch payload is invalid"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
        (status = 404, description = "Document not found"),
    ),
)]
pub async fn patch_document_metadata(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Json(payload): Json<PatchDocumentMetadataRequest>,
) -> Result<Json<ContentDocumentDetailResponse>, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_WRITE)
        .await?;
    let document_hint = normalize_optional_document_hint(payload.document_hint)?;
    let summary = state
        .application_services
        .content
        .update_active_revision_document_hint(&state, document_id, document_hint)
        .await?;
    let revision_id = summary.active_revision.as_ref().map(|revision| revision.id);
    current_document_response(&state, summary, revision_id).await
}

async fn current_document_response(
    state: &AppState,
    summary: crate::domains::content::ContentDocumentSummary,
    revision_id: Option<Uuid>,
) -> Result<Json<ContentDocumentDetailResponse>, ApiError> {
    let document_id = summary.document.id;
    let lifecycle = crate::services::content::document_accounting::load_document_lifecycle(
        state,
        summary.document.workspace_id,
        summary.document.library_id,
        document_id,
    )
    .await
    .ok();
    let mut response = map_document_summary(summary);
    response.lifecycle = lifecycle;
    require_revision_still_current(state, document_id, revision_id).await?;
    Ok(Json(response))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_document_head",
    skip_all,
    fields(document_id = %document_id)
)]
#[utoipa::path(
    get,
    path = "/v1/content/documents/{documentId}/head",
    tag = "content",
    operation_id = "getContentDocumentHead",
    params(("documentId" = uuid::Uuid, Path, description = "Document identifier")),
    responses(
        (status = 200, description = "Active head revision summary for the document", body = ContentDocumentHead),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
        (status = 404, description = "Document or head revision not found"),
    ),
)]
pub async fn get_document_head(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<ContentDocumentHead>, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let mut head =
        state.application_services.content.get_document_head(&state, document_id).await?;
    let revision_id = resolve_readable_revision_id(&state, document_id).await?;
    if head.as_ref().and_then(|row| row.readable_revision_id) != revision_id {
        return Err(ApiError::resource_not_found("document_head", document_id));
    }
    if let Some(head) = head.as_mut() {
        head.active_revision_id = revision_id;
    }
    require_revision_still_current(&state, document_id, revision_id).await?;
    head.map(Json).ok_or_else(|| ApiError::resource_not_found("document_head", document_id))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_document_prepared_segments",
    skip_all,
    fields(document_id = %document_id, item_count)
)]
#[utoipa::path(
    get,
    path = "/v1/content/documents/{documentId}/prepared-segments",
    tag = "content",
    operation_id = "listContentPreparedSegments",
    params(
        ("documentId" = uuid::Uuid, Path, description = "Document identifier"),
        crate::interfaces::http::content::types::PreparedDataQuery,
    ),
    responses(
        (status = 200, description = "Prepared segments for the document", body = PreparedSegmentsPageResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
        (status = 404, description = "Document not found"),
    ),
)]
pub async fn get_document_prepared_segments(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Query(query): Query<PreparedDataQuery>,
) -> Result<Json<PreparedSegmentsPageResponse>, ApiError> {
    let span = tracing::Span::current();
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let revision_id = resolve_readable_revision_id(&state, document_id).await?;
    let (offset, limit) = normalize_page_window(query.offset, query.limit);
    let (items, total) = match revision_id {
        Some(revision_id) => {
            state
                .application_services
                .content
                .list_prepared_segments_page(&state, revision_id, offset, limit)
                .await?
        }
        None => (Vec::new(), 0),
    };
    span.record("item_count", items.len());
    require_revision_still_current(&state, document_id, revision_id).await?;
    Ok(Json(PreparedSegmentsPageResponse { document_id, revision_id, total, offset, limit, items }))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_document_technical_facts",
    skip_all,
    fields(document_id = %document_id, item_count)
)]
#[utoipa::path(
    get,
    path = "/v1/content/documents/{documentId}/technical-facts",
    tag = "content",
    operation_id = "listContentTechnicalFacts",
    params(
        ("documentId" = uuid::Uuid, Path, description = "Document identifier"),
        crate::interfaces::http::content::types::PreparedDataQuery,
    ),
    responses(
        (status = 200, description = "Technical facts extracted from the document", body = TechnicalFactsPageResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
        (status = 404, description = "Document not found"),
    ),
)]
pub async fn get_document_technical_facts(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Query(query): Query<PreparedDataQuery>,
) -> Result<Json<TechnicalFactsPageResponse>, ApiError> {
    let span = tracing::Span::current();
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let revision_id = resolve_readable_revision_id(&state, document_id).await?;
    let (offset, limit) = normalize_page_window(query.offset, query.limit);
    let items = match revision_id {
        Some(revision_id) => {
            state.application_services.content.list_technical_facts(&state, revision_id).await?
        }
        None => Vec::new(),
    };
    let total = items.len();
    span.record("item_count", total);
    require_revision_still_current(&state, document_id, revision_id).await?;
    Ok(Json(TechnicalFactsPageResponse {
        document_id,
        revision_id,
        total,
        offset,
        limit,
        items: paginate_items(items, offset, limit),
    }))
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_document",
    skip_all,
    fields(document_id = %document_id)
)]
#[utoipa::path(
    delete,
    path = "/v1/content/documents/{documentId}",
    tag = "content",
    operation_id = "deleteContentDocument",
    params(
        ("documentId" = uuid::Uuid, Path, description = "Document identifier"),
        ("Idempotency-Key" = Option<String>, Header, description = "Stable caller key for exact-target deletion replay"),
    ),
    responses(
        (status = 200, description = "Mutation that captures the deletion", body = ContentMutationDetailResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the document"),
        (status = 404, description = "Document not found"),
        (status = 409, description = "Idempotency key belongs to a different deletion request"),
    ),
)]
pub async fn delete_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let idempotency_key = headers
        .get("Idempotency-Key")
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|raw| types::normalize_optional_text(raw.to_owned()))
                .ok_or_else(|| {
                    ApiError::BadRequest("Idempotency-Key must be nonblank text".to_owned())
                })
        })
        .transpose()?;
    let document =
        load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_WRITE)
            .await?;
    let admission = state
        .application_services
        .content
        .admit_mutation(
            &state,
            AdmitMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                operation_kind: "delete".to_string(),
                idempotency_key,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: None,
                parent_async_operation_id: None,
            },
        )
        .await?;
    Ok(Json(map_mutation_admission(admission)))
}

async fn resolve_readable_revision_id(
    state: &AppState,
    document_id: Uuid,
) -> Result<Option<Uuid>, ApiError> {
    crate::infra::repositories::content_fingerprint_repository::get_current_revision_id(
        &state.persistence.postgres,
        document_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "document visibility lookup failed"))
}

async fn require_revision_still_current(
    state: &AppState,
    document_id: Uuid,
    revision_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let Some(revision_id) = revision_id else {
        return Err(ApiError::resource_not_found("document", document_id));
    };
    if !crate::infra::repositories::content_fingerprint_repository::revision_is_current(
        &state.persistence.postgres,
        document_id,
        revision_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "document visibility lookup failed"))?
    {
        return Err(ApiError::resource_not_found("document", document_id));
    }
    Ok(())
}
