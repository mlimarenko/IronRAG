mod batch;
mod multipart;
mod snapshot;
mod source_download;
mod types;
mod web_runs;

use axum::{
    Json, Router,
    extract::{Path, Query, State, multipart::Multipart},
    routing::{get, post},
};
use uuid::Uuid;

use self::{
    batch::{batch_cancel_documents, batch_delete_documents, batch_reprocess_documents},
    multipart::{parse_replace_multipart, parse_upload_multipart, resolve_upload_external_key},
    source_download::download_document_source,
    types::{
        AppendDocumentBodyRequest, ChunkSummary, ChunksQuery, ContentDocumentDetailResponse,
        ContentDocumentListItem, ContentMutationDetailResponse, CreateDocumentRequest,
        CreateDocumentResponse, CreateMutationRequest, DocumentListCursor,
        DocumentListPageResponse, DocumentListSortKey, DocumentListSortOrder,
        DocumentListStatusCounts, EditDocumentRequest, ListDocumentsQuery, ListMutationsQuery,
        PreparedDataQuery, PreparedSegmentsPageResponse, ReprocessDocumentRequest,
        TechnicalFactsPageResponse, build_revision_metadata, decode_document_list_cursor,
        encode_document_list_cursor, map_document_summary, map_mutation_admission,
        normalize_page_window, paginate_items,
    },
    web_runs::{
        cancel_web_ingest_run, create_web_ingest_run, get_web_ingest_run,
        list_web_ingest_run_pages, list_web_ingest_runs,
    },
};
use crate::{
    app::state::AppState,
    domains::content::{ContentDocumentHead, ContentRevision},
    infra::repositories::{content_repository, content_repository::DocumentListSortColumn},
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_READ, POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ,
            POLICY_LIBRARY_WRITE, load_canonical_content_document_and_authorize,
            load_content_document_and_authorize, load_library_and_authorize,
        },
        router_support::ApiError,
    },
    services::content::service::{
        AdmitDocumentCommand, AdmitMutationCommand, AppendInlineMutationCommand,
        ContentDocumentListEntry, CreateDocumentAdmission, EditInlineMutationCommand,
        ListDocumentsPageCommand, ReplaceInlineMutationCommand, UploadInlineDocumentCommand,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/chunks", get(list_chunks))
        .route("/content/web-runs", get(list_web_ingest_runs).post(create_web_ingest_run))
        .route("/content/web-runs/{run_id}", get(get_web_ingest_run))
        .route("/content/web-runs/{run_id}/pages", get(list_web_ingest_run_pages))
        .route("/content/web-runs/{run_id}/cancel", post(cancel_web_ingest_run))
        .route("/content/documents/batch-delete", post(batch_delete_documents))
        .route("/content/documents/batch-cancel", post(batch_cancel_documents))
        .route("/content/documents/batch-reprocess", post(batch_reprocess_documents))
        .route("/content/documents", get(list_documents).post(create_document))
        .route("/content/documents/upload", axum::routing::post(upload_document))
        .route("/content/documents/{document_id}", get(get_document).delete(delete_document))
        .route("/content/documents/{document_id}/source", get(download_document_source))
        .route("/content/documents/{document_id}/append", axum::routing::post(append_document))
        .route("/content/documents/{document_id}/edit", axum::routing::post(edit_document))
        .route("/content/documents/{document_id}/replace", axum::routing::post(replace_document))
        .route("/content/documents/{document_id}/head", get(get_document_head))
        .route(
            "/content/documents/{document_id}/prepared-segments",
            get(get_document_prepared_segments),
        )
        .route(
            "/content/documents/{document_id}/technical-facts",
            get(get_document_technical_facts),
        )
        .route("/content/documents/{document_id}/reprocess", post(reprocess_document))
        .route("/content/documents/{document_id}/revisions", get(list_revisions))
        .route("/content/mutations", get(list_mutations).post(create_mutation))
        .route("/content/mutations/{mutation_id}", get(get_mutation))
        .merge(snapshot::routes())
}

/// Canonical slim paginated document list.
///
/// Response shape is `DocumentListPageResponse { items, next_cursor, total_count }`.
/// Each `items[i]` is a `ContentDocumentListItem` with only the fields the
/// documents page actually renders — status/readiness are derived server-side.
/// The inspector panel fetches full detail via `/content/documents/{id}`.
#[tracing::instrument(
    level = "info",
    name = "http.list_documents",
    skip_all,
    fields(library_id = ?query.library_id, include_deleted, limit, document_count, elapsed_ms)
)]
async fn list_documents(
    auth: AuthContext,
    State(state): State<AppState>,
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
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    let include_deleted = query.include_deleted.unwrap_or(false);
    span.record("include_deleted", include_deleted);

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    span.record("limit", limit);

    let cursor_pair = match query.cursor.as_deref() {
        Some(token) => {
            let DocumentListCursor { created_at, document_id } =
                decode_document_list_cursor(token)?;
            Some((created_at, document_id))
        }
        None => None,
    };

    let sort = match query.sort_by {
        Some(DocumentListSortKey::UploadedAt) | None => DocumentListSortColumn::CreatedAt,
        Some(DocumentListSortKey::FileName) => DocumentListSortColumn::ExternalKey,
        Some(DocumentListSortKey::FileType) => DocumentListSortColumn::MimeType,
        Some(DocumentListSortKey::FileSize) => DocumentListSortColumn::ByteSize,
        Some(DocumentListSortKey::Status) => DocumentListSortColumn::DerivedStatus,
    };
    // Default sort order matches the frontend: newest first.
    let sort_desc = !matches!(query.sort_order, Some(DocumentListSortOrder::Asc));

    // Parse the canonical status filter: comma-separated values, each must
    // be one of the 5 derived_status buckets. An empty / absent parameter
    // means "no filter". Unknown values are rejected up front so clients
    // get a clear 400 instead of a silent no-op.
    const ALLOWED_STATUSES: &[&str] = &["canceled", "failed", "processing", "queued", "ready"];
    let status_filter: Vec<String> = match query.status.as_deref() {
        None => Vec::new(),
        Some(raw) => {
            let mut out = Vec::new();
            for token in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if !ALLOWED_STATUSES.contains(&token) {
                    return Err(ApiError::BadRequest(format!(
                        "unknown status filter value `{token}`; allowed: {}",
                        ALLOWED_STATUSES.join(", ")
                    )));
                }
                out.push(token.to_string());
            }
            out
        }
    };

    let result = state
        .canonical_services
        .content
        .list_documents_page(
            &state,
            ListDocumentsPageCommand {
                library_id: library.id,
                include_deleted,
                cursor: cursor_pair,
                limit,
                search: query.search.clone(),
                sort,
                sort_desc,
                status_filter,
            },
        )
        .await?;

    span.record("document_count", result.items.len());

    let next_cursor = result.next_cursor.map(|value| {
        encode_document_list_cursor(&DocumentListCursor {
            created_at: value.created_at,
            document_id: value.document_id,
        })
    });

    // total_count + statusCounts are both expensive on large libraries
    // (they run the same CASE derivation over every row in the library),
    // so the canonical pattern is: the UI requests them ONCE per filter
    // set by passing `includeTotal=true` on the first page, and reuses
    // the cached result while paging through. Both numbers come out of a
    // single aggregate query so there's no second round-trip.
    let (total_count, status_counts) = if query.include_total.unwrap_or(false) {
        let counts = content_repository::aggregate_document_list_status_counts(
            &state.persistence.postgres,
            library.id,
            include_deleted,
            query.search.as_deref(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        (
            Some(counts.total.unwrap_or_default()),
            Some(DocumentListStatusCounts {
                total: counts.total.unwrap_or_default(),
                ready: counts.ready.unwrap_or_default(),
                processing: counts.processing.unwrap_or_default(),
                queued: counts.queued.unwrap_or_default(),
                failed: counts.failed.unwrap_or_default(),
                canceled: counts.canceled.unwrap_or_default(),
            }),
        )
    } else {
        (None, None)
    };

    let items: Vec<ContentDocumentListItem> =
        result.items.into_iter().map(map_document_list_entry).collect();

    span.record("elapsed_ms", started_at.elapsed().as_millis() as u64);
    Ok(Json(DocumentListPageResponse { items, next_cursor, total_count, status_counts }))
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
        status: entry.status,
        readiness: entry.readiness,
        stage: entry.stage,
        processing_started_at: entry.processing_started_at,
        processing_finished_at: entry.processing_finished_at,
        failure_code: entry.failure_code,
        retryable: entry.retryable,
        source_kind: entry.source_kind,
        source_uri: entry.source_uri,
        source_access: entry.source_access,
        cost: entry.cost_total.to_string(),
        cost_currency_code: entry.cost_currency_code,
    }
}

#[tracing::instrument(
    level = "info",
    name = "http.list_chunks",
    skip_all,
    fields(document_id = ?query.document_id, item_count)
)]
async fn list_chunks(
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
    let head = state.canonical_services.content.get_document_head(&state, document_id).await?;
    let revision_id = head.and_then(|row| row.effective_revision_id());
    let items = match revision_id {
        Some(revision_id) => {
            state.canonical_services.content.list_chunks(&state, revision_id).await?
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
    Ok(Json(summaries))
}

#[tracing::instrument(
    level = "info",
    name = "http.create_document",
    skip_all,
    fields(library_id = ?payload.library_id)
)]
async fn create_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateDocumentRequest>,
) -> Result<Json<CreateDocumentResponse>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, payload.library_id, POLICY_LIBRARY_WRITE).await?;
    if library.workspace_id != payload.workspace_id {
        return Err(ApiError::BadRequest(
            "workspaceId does not match the target library".to_string(),
        ));
    }

    let admission = state
        .canonical_services
        .content
        .admit_document(
            &state,
            AdmitDocumentCommand {
                workspace_id: payload.workspace_id,
                library_id: payload.library_id,
                external_key: payload.external_key.clone(),
                file_name: None,
                idempotency_key: payload.idempotency_key.clone(),
                created_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: build_revision_metadata(&payload)?,
            },
        )
        .await?;
    let CreateDocumentAdmission { document, mutation } = admission;
    Ok(Json(CreateDocumentResponse {
        document: map_document_summary(document),
        mutation: map_mutation_admission(mutation),
    }))
}

#[tracing::instrument(level = "info", name = "http.upload_document", skip_all)]
async fn upload_document(
    auth: AuthContext,
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<Json<CreateDocumentResponse>, ApiError> {
    auth.require_any_scope(POLICY_DOCUMENTS_WRITE)?;
    let payload = parse_upload_multipart(&state, multipart).await?;
    let library =
        load_library_and_authorize(&auth, &state, payload.library_id, POLICY_LIBRARY_WRITE).await?;
    let response = state
        .canonical_services
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
                title: payload.title.or(Some(payload.file_name.clone())),
                file_name: payload.file_name,
                mime_type: payload.mime_type,
                file_bytes: payload.file_bytes,
            },
        )
        .await?;
    let CreateDocumentAdmission { document, mutation } = response;
    Ok(Json(CreateDocumentResponse {
        document: map_document_summary(document),
        mutation: map_mutation_admission(mutation),
    }))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_document",
    skip_all,
    fields(document_id = %document_id)
)]
async fn get_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<ContentDocumentDetailResponse>, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let summary = state.canonical_services.content.get_document(&state, document_id).await?;
    let lifecycle = crate::services::content::document_accounting::load_document_lifecycle(
        &state,
        summary.document.workspace_id,
        summary.document.library_id,
        summary.document.id,
    )
    .await
    .ok();
    let mut response = map_document_summary(summary);
    response.lifecycle = lifecycle;
    Ok(Json(response))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_document_head",
    skip_all,
    fields(document_id = %document_id)
)]
async fn get_document_head(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<ContentDocumentHead>, ApiError> {
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let head = state.canonical_services.content.get_document_head(&state, document_id).await?;
    head.map(Json).ok_or_else(|| ApiError::resource_not_found("document_head", document_id))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_document_prepared_segments",
    skip_all,
    fields(document_id = %document_id, item_count)
)]
async fn get_document_prepared_segments(
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
                .canonical_services
                .content
                .list_prepared_segments_page(&state, revision_id, offset, limit)
                .await?
        }
        None => (Vec::new(), 0),
    };
    span.record("item_count", items.len());
    Ok(Json(PreparedSegmentsPageResponse { document_id, revision_id, total, offset, limit, items }))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_document_technical_facts",
    skip_all,
    fields(document_id = %document_id, item_count)
)]
async fn get_document_technical_facts(
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
            state.canonical_services.content.list_technical_facts(&state, revision_id).await?
        }
        None => Vec::new(),
    };
    let total = items.len();
    span.record("item_count", total);
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
async fn delete_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    let admission = state
        .canonical_services
        .content
        .admit_mutation(
            &state,
            AdmitMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                operation_kind: "delete".to_string(),
                idempotency_key: None,
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

#[tracing::instrument(
    level = "info",
    name = "http.append_document",
    skip_all,
    fields(document_id = %document_id)
)]
async fn append_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Json(payload): Json<AppendDocumentBodyRequest>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    let admission = state
        .canonical_services
        .content
        .append_inline_mutation(
            &state,
            AppendInlineMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                idempotency_key: payload.idempotency_key,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                appended_text: payload.appended_text,
            },
        )
        .await?;
    Ok(Json(map_mutation_admission(admission)))
}

#[tracing::instrument(
    level = "info",
    name = "http.edit_document",
    skip_all,
    fields(document_id = %document_id)
)]
async fn edit_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Json(payload): Json<EditDocumentRequest>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    let admission = state
        .canonical_services
        .content
        .edit_inline_mutation(
            &state,
            EditInlineMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                idempotency_key: payload.idempotency_key,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                markdown: payload.markdown,
            },
        )
        .await?;
    Ok(Json(map_mutation_admission(admission)))
}

#[tracing::instrument(
    level = "info",
    name = "http.replace_document",
    skip_all,
    fields(document_id = %document_id)
)]
async fn replace_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    multipart: Multipart,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    let payload = parse_replace_multipart(&state, multipart).await?;
    let admission = state
        .canonical_services
        .content
        .replace_inline_mutation(
            &state,
            ReplaceInlineMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                idempotency_key: payload.idempotency_key,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                file_name: payload.file_name,
                mime_type: payload.mime_type,
                file_bytes: payload.file_bytes,
            },
        )
        .await?;
    Ok(Json(map_mutation_admission(admission)))
}

#[tracing::instrument(
    level = "info",
    name = "http.list_revisions",
    skip_all,
    fields(document_id = %document_id, item_count)
)]
async fn list_revisions(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<Vec<ContentRevision>>, ApiError> {
    let span = tracing::Span::current();
    let _ = load_content_document_and_authorize(&auth, &state, document_id, POLICY_DOCUMENTS_READ)
        .await?;
    let revisions = state.canonical_services.content.list_revisions(&state, document_id).await?;
    span.record("item_count", revisions.len());
    Ok(Json(revisions))
}

#[tracing::instrument(
    level = "info",
    name = "http.create_mutation",
    skip_all,
    fields(document_id = ?payload.document_id, library_id = ?payload.library_id)
)]
async fn create_mutation(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateMutationRequest>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let document = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        payload.document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    if document.workspace_id != payload.workspace_id || document.library_id != payload.library_id {
        return Err(ApiError::BadRequest(
            "workspaceId or libraryId does not match the target document".to_string(),
        ));
    }

    let admission = state
        .canonical_services
        .content
        .admit_mutation(
            &state,
            AdmitMutationCommand {
                workspace_id: payload.workspace_id,
                library_id: payload.library_id,
                document_id: document.id,
                operation_kind: payload.operation_kind.clone(),
                idempotency_key: payload.idempotency_key.clone(),
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: build_revision_metadata(&CreateDocumentRequest {
                    workspace_id: payload.workspace_id,
                    library_id: payload.library_id,
                    external_key: None,
                    idempotency_key: payload.idempotency_key.clone(),
                    content_source_kind: payload.content_source_kind.clone(),
                    checksum: payload.checksum.clone(),
                    mime_type: payload.mime_type.clone(),
                    byte_size: payload.byte_size,
                    title: payload.title.clone(),
                    language_code: payload.language_code.clone(),
                    source_uri: payload.source_uri.clone(),
                    storage_key: payload.storage_key.clone(),
                })?,
                parent_async_operation_id: None,
            },
        )
        .await?;
    Ok(Json(map_mutation_admission(admission)))
}

#[tracing::instrument(
    level = "info",
    name = "http.list_mutations",
    skip_all,
    fields(library_id = ?query.library_id, item_count)
)]
async fn list_mutations(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListMutationsQuery>,
) -> Result<Json<Vec<ContentMutationDetailResponse>>, ApiError> {
    let span = tracing::Span::current();
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_READ).await?;
    let admissions = state
        .canonical_services
        .content
        .list_mutation_admissions(&state, library.workspace_id, library.id)
        .await?;
    let items: Vec<_> = admissions.into_iter().map(map_mutation_admission).collect();
    span.record("item_count", items.len());
    Ok(Json(items))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_mutation",
    skip_all,
    fields(mutation_id = %mutation_id)
)]
async fn get_mutation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(mutation_id): Path<Uuid>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let admission =
        state.canonical_services.content.get_mutation_admission(&state, mutation_id).await?;
    let mutation = &admission.mutation;
    let library =
        load_library_and_authorize(&auth, &state, mutation.library_id, POLICY_LIBRARY_READ).await?;
    if library.workspace_id != mutation.workspace_id {
        return Err(ApiError::Unauthorized);
    }
    Ok(Json(map_mutation_admission(admission)))
}

#[tracing::instrument(
    level = "info",
    name = "http.reprocess_document",
    skip_all,
    fields(document_id = %document_id)
)]
async fn reprocess_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
    Json(payload): Json<ReprocessDocumentRequest>,
) -> Result<Json<ContentMutationDetailResponse>, ApiError> {
    let _ = load_canonical_content_document_and_authorize(
        &auth,
        &state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    let admission = self::batch::reprocess_single_document(
        &state,
        None,
        auth.principal_id,
        payload.idempotency_key,
        document_id,
    )
    .await?;
    Ok(Json(map_mutation_admission(admission)))
}

async fn resolve_readable_revision_id(
    state: &AppState,
    document_id: Uuid,
) -> Result<Option<Uuid>, ApiError> {
    let head = state.canonical_services.content.get_document_head(state, document_id).await?;
    Ok(head.and_then(|row| row.effective_revision_id()))
}

#[cfg(test)]
mod tests {
    use super::{batch, types};
    use crate::domains::content::{
        ContentDocument, ContentDocumentPipelineState, ContentDocumentSummary, ContentRevision,
    };
    use crate::interfaces::http::router_support::ApiError;
    use crate::services::content::service::ReprocessRevisionSource;
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn reprocess_metadata_preserves_active_revision_source_kind() {
        let revision = ContentRevision {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_number: 1,
            parent_revision_id: None,
            content_source_kind: "upload".to_string(),
            checksum: "sha256:test".to_string(),
            mime_type: "application/pdf".to_string(),
            byte_size: 636,
            title: Some("runtime-upload-check.pdf".to_string()),
            language_code: Some("ru".to_string()),
            source_uri: Some("upload://runtime-upload-check.pdf".to_string()),
            storage_key: Some("storage/runtime-upload-check.pdf".to_string()),
            created_by_principal_id: None,
            created_at: Utc::now(),
        };

        let metadata = types::build_reprocess_revision_metadata(
            &revision,
            ReprocessRevisionSource {
                checksum: revision.checksum.clone(),
                mime_type: revision.mime_type.clone(),
                byte_size: revision.byte_size,
                title: revision.title.clone(),
                source_uri: revision.source_uri.clone(),
                storage_key: revision.storage_key.clone().expect("storage key"),
            },
        );

        assert_eq!(metadata.content_source_kind, "upload");
        assert_eq!(metadata.checksum, revision.checksum);
        assert_eq!(metadata.mime_type, revision.mime_type);
        assert_eq!(metadata.byte_size, revision.byte_size);
        assert_eq!(metadata.title, revision.title);
        assert_eq!(metadata.language_code, revision.language_code);
        assert_eq!(metadata.source_uri, revision.source_uri);
        assert_eq!(metadata.storage_key, revision.storage_key);
    }

    #[test]
    fn reprocess_metadata_preserves_edited_source_storage() {
        let revision = ContentRevision {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_number: 3,
            parent_revision_id: Some(Uuid::now_v7()),
            content_source_kind: "edit".to_string(),
            checksum: "sha256:edited".to_string(),
            mime_type: "text/markdown".to_string(),
            byte_size: 128,
            title: Some("Inventory.xlsx".to_string()),
            language_code: None,
            source_uri: Some("edit://Inventory.md".to_string()),
            storage_key: Some("content/demo/Inventory.md".to_string()),
            created_by_principal_id: None,
            created_at: Utc::now(),
        };

        let metadata = types::build_reprocess_revision_metadata(
            &revision,
            ReprocessRevisionSource {
                checksum: revision.checksum.clone(),
                mime_type: revision.mime_type.clone(),
                byte_size: revision.byte_size,
                title: revision.title.clone(),
                source_uri: revision.source_uri.clone(),
                storage_key: revision.storage_key.clone().expect("storage key"),
            },
        );

        assert_eq!(metadata.content_source_kind, "edit");
        assert_eq!(metadata.mime_type, "text/markdown");
        assert_eq!(metadata.source_uri.as_deref(), Some("edit://Inventory.md"));
        assert_eq!(metadata.storage_key.as_deref(), Some("content/demo/Inventory.md"));
    }

    #[test]
    fn reprocess_metadata_can_describe_derived_text_source() {
        let revision = ContentRevision {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_number: 2,
            parent_revision_id: None,
            content_source_kind: "upload".to_string(),
            checksum: "sha256:original".to_string(),
            mime_type: "application/pdf".to_string(),
            byte_size: 4096,
            title: Some("quarterly-report.pdf".to_string()),
            language_code: None,
            source_uri: Some("upload://quarterly-report.pdf".to_string()),
            storage_key: None,
            created_by_principal_id: None,
            created_at: Utc::now(),
        };

        let derived_source_uri = format!("derived-text://{}", revision.id);
        let metadata = types::build_reprocess_revision_metadata(
            &revision,
            ReprocessRevisionSource {
                checksum: "sha256:derived".to_string(),
                mime_type: "text/plain".to_string(),
                byte_size: 128,
                title: Some("quarterly-report.txt".to_string()),
                source_uri: Some(derived_source_uri.clone()),
                storage_key: "content/derived/quarterly-report.txt".to_string(),
            },
        );

        assert_eq!(metadata.content_source_kind, "upload");
        assert_eq!(metadata.checksum, "sha256:derived");
        assert_eq!(metadata.mime_type, "text/plain");
        assert_eq!(metadata.byte_size, 128);
        assert_eq!(metadata.title.as_deref(), Some("quarterly-report.txt"));
        assert_eq!(metadata.source_uri.as_deref(), Some(derived_source_uri.as_str()));
        assert_eq!(metadata.storage_key.as_deref(), Some("content/derived/quarterly-report.txt"));
    }

    #[test]
    fn batch_document_id_limit_allows_canonical_payload_ceiling() {
        assert_eq!(batch::BATCH_MAX_DOCUMENT_IDS, 100_000);
        assert!(batch::ensure_batch_document_id_limit(batch::BATCH_MAX_DOCUMENT_IDS).is_ok());
        assert!(batch::ensure_batch_document_id_limit(1).is_ok());
    }

    #[test]
    fn batch_document_id_limit_rejects_empty_and_oversized_payloads() {
        let empty = batch::ensure_batch_document_id_limit(0)
            .expect_err("empty document id list must be rejected");
        assert!(matches!(empty, ApiError::BadRequest(_)));

        let oversized = batch::ensure_batch_document_id_limit(batch::BATCH_MAX_DOCUMENT_IDS + 1)
            .expect_err("payloads above the DoS sanity limit must fail");
        let ApiError::BadRequest(message) = oversized else {
            unreachable!("ensure_batch_document_id_limit must return bad_request on overflow");
        };
        assert!(message.contains("batch size exceeds maximum"));
    }

    #[test]
    fn map_document_summary_uses_canonical_summary_file_name() {
        let document_id = Uuid::now_v7();
        let summary = ContentDocumentSummary {
            document: ContentDocument {
                id: document_id,
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                external_key: "external-key".to_string(),
                document_state: "active".to_string(),
                created_at: Utc::now(),
            },
            file_name: "readable-revision.pdf".to_string(),
            head: None,
            active_revision: Some(ContentRevision {
                id: Uuid::now_v7(),
                document_id,
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                revision_number: 2,
                parent_revision_id: None,
                content_source_kind: "replace".to_string(),
                checksum: "checksum".to_string(),
                mime_type: "application/pdf".to_string(),
                byte_size: 128,
                title: Some("processing-replacement.pdf".to_string()),
                language_code: None,
                source_uri: Some("upload://processing-replacement.pdf".to_string()),
                storage_key: Some("content/demo".to_string()),
                created_by_principal_id: None,
                created_at: Utc::now(),
            }),
            source_access: None,
            readiness: None,
            readiness_summary: None,
            prepared_revision: None,
            web_page_provenance: None,
            pipeline: ContentDocumentPipelineState { latest_mutation: None, latest_job: None },
        };

        let response = types::map_document_summary(summary);

        assert_eq!(response.file_name, "readable-revision.pdf");
    }
}
