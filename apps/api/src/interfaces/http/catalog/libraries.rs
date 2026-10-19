use super::mapping::lifecycle_state_label;
use super::*;

/// One successful library-scoped REST audit event. The subject is always the
/// audited library, so only the action, the human summaries and the owning
/// workspace vary between handlers.
struct LibraryAuditEvent {
    request_id: Option<axum::Extension<RequestId>>,
    action_kind: &'static str,
    redacted_message: String,
    internal_message: String,
    workspace_id: Uuid,
    library_id: Uuid,
}

/// Records one successful library-scoped REST audit event through the canonical
/// emitter, framing the audited library as the single audit subject.
async fn record_library_audit_event(
    state: &AppState,
    actor_principal_id: Uuid,
    event: LibraryAuditEvent,
) {
    let LibraryAuditEvent {
        request_id,
        action_kind,
        redacted_message,
        internal_message,
        workspace_id,
        library_id,
    } = event;
    record_rest_audit_event(
        state,
        actor_principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: action_kind.to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(redacted_message),
            internal_message: Some(internal_message),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "library".to_string(),
                subject_id: library_id,
                workspace_id: Some(workspace_id),
                library_id: Some(library_id),
                document_id: None,
            }],
        },
    )
    .await;
}

#[utoipa::path(
    get,
    path = "/v1/catalog/workspaces/{workspaceId}/libraries",
    tag = "catalog",
    operation_id = "listCatalogLibraries",
    params(
        ("workspaceId" = uuid::Uuid, Path, description = "Workspace identifier"),
        CatalogLibraryListQuery,
    ),
    responses(
        (status = 200, description = "Bounded library page visible to the caller in this workspace", body = CatalogLibraryPageResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the workspace"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_libraries",
    skip_all,
    fields(workspace_id = %workspace_id, item_count)
)]
pub async fn list_libraries(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<CatalogLibraryListQuery>,
) -> Result<Json<CatalogLibraryPageResponse>, ApiError> {
    let span = tracing::Span::current();
    authorize_workspace_discovery(&auth, workspace_id)?;
    let window = normalize_catalog_library_page(query)?;
    let visible_library_ids = library_discovery_ids(&auth, workspace_id);
    let page = CatalogService
        .search_library_page(
            &state,
            SearchCatalogLibraryPage {
                workspace_id,
                search: window.search.as_deref(),
                visible_library_ids: visible_library_ids.as_deref(),
                pinned_library_id: None,
                offset: window.offset,
                limit: window.limit,
            },
        )
        .await?;
    let items: Vec<_> =
        cap_catalog_library_page(page.items, window.limit).into_iter().map(map_library).collect();
    span.record("item_count", items.len());
    Ok(Json(CatalogLibraryPageResponse {
        items,
        total_count: page.total_count,
        offset: window.offset,
        limit: window.limit,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/catalog/workspaces/{workspaceId}/libraries",
    tag = "catalog",
    operation_id = "createCatalogLibrary",
    params(("workspaceId" = uuid::Uuid, Path, description = "Workspace identifier")),
    request_body = CreateCatalogLibraryRequest,
    responses(
        (status = 200, description = "Newly created library", body = CatalogLibraryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a workspace admin"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.create_library",
    skip_all,
    fields(workspace_id = %workspace_id)
)]
pub async fn create_library(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(workspace_id): Path<Uuid>,
    Json(payload): Json<CreateCatalogLibraryRequest>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    load_workspace_and_authorize_library_write(&auth, &state, workspace_id).await?;

    let library = state
        .application_services
        .catalog
        .create_library(
            &state,
            CreateLibraryCommand {
                workspace_id,
                slug: payload.slug,
                display_name: payload.display_name,
                description: payload.description,
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;

    record_library_audit_event(
        &state,
        auth.principal_id,
        LibraryAuditEvent {
            request_id,
            action_kind: "catalog.library.create",
            redacted_message: format!("library {} created", library.display_name),
            internal_message: format!(
                "principal {} created library {} in workspace {}",
                auth.principal_id, library.id, library.workspace_id
            ),
            workspace_id: library.workspace_id,
            library_id: library.id,
        },
    )
    .await;

    Ok(Json(map_library(library)))
}

#[utoipa::path(
    delete,
    path = "/v1/catalog/workspaces/{workspaceId}/libraries/{libraryId}",
    tag = "catalog",
    operation_id = "deleteCatalogLibrary",
    params(
        ("workspaceId" = uuid::Uuid, Path, description = "Workspace identifier"),
        ("libraryId" = uuid::Uuid, Path, description = "Library identifier"),
    ),
    responses(
        (status = 202, description = "Library deletion accepted", body = CatalogDeletionAcceptedResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a workspace admin"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.delete_library",
    skip_all,
    fields(workspace_id = %workspace_id, library_id = %library_id)
)]
pub async fn delete_library(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path((workspace_id, library_id)): Path<(Uuid, Uuid)>,
) -> Result<(StatusCode, Json<CatalogDeletionAcceptedResponse>), ApiError> {
    let request_id = request_id.map(|value| value.0.0);
    load_workspace_and_authorize_library_write(&auth, &state, workspace_id).await?;

    let admission = state
        .application_services
        .catalog
        .admit_library_deletion(&state, workspace_id, library_id, Some(auth.principal_id))
        .await?;

    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.clone(),
            action_kind: "catalog.library.delete".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("library {} deletion accepted", admission.display_name)),
            internal_message: Some(format!(
                "principal {} accepted library {} deletion in workspace {} via async operation {}",
                auth.principal_id,
                admission.library_id.unwrap_or(library_id),
                admission.workspace_id,
                admission.operation_id
            )),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "library".to_string(),
                subject_id: admission.library_id.unwrap_or(library_id),
                workspace_id: Some(admission.workspace_id),
                library_id: admission.library_id,
                document_id: None,
            }],
        },
    )
    .await;

    if admission.should_start_worker {
        spawn_library_deletion_worker(
            state.clone(),
            CatalogDeletionWorker {
                operation_id: admission.operation_id,
                workspace_id: admission.workspace_id,
                display_name: admission.display_name.clone(),
                principal_id: auth.principal_id,
                request_id,
            },
            admission.library_id.unwrap_or(library_id),
        );
    }

    Ok((StatusCode::ACCEPTED, Json(map_deletion_admission(admission))))
}

#[utoipa::path(
    get,
    path = "/v1/catalog/libraries/{libraryId}",
    tag = "catalog",
    operation_id = "getCatalogLibrary",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    responses(
        (status = 200, description = "Library detail", body = CatalogLibraryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot discover the library"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_library",
    skip_all,
    fields(library_id = %library_id)
)]
pub async fn get_library(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    let library = state.application_services.catalog.get_library(&state, library_id).await?;
    authorize_library_discovery(&auth, library.workspace_id, library.id)?;
    Ok(Json(map_library(library)))
}

#[utoipa::path(
    put,
    path = "/v1/catalog/libraries/{libraryId}",
    tag = "catalog",
    operation_id = "updateCatalogLibrary",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    request_body = UpdateCatalogLibraryRequest,
    responses(
        (status = 200, description = "Library after applying the update", body = CatalogLibraryResponse),
        (status = 400, description = "Invalid lifecycle state"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a workspace admin"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.update_library",
    skip_all,
    fields(library_id = %library_id)
)]
pub async fn update_library(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<UpdateCatalogLibraryRequest>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    let existing = state.application_services.catalog.get_library(&state, library_id).await?;
    load_workspace_and_authorize_library_write(&auth, &state, existing.workspace_id).await?;

    let lifecycle_state = payload
        .lifecycle_state
        .as_deref()
        .unwrap_or_else(|| lifecycle_state_label(&existing.lifecycle_state));
    let library = state
        .application_services
        .catalog
        .update_library(
            &state,
            UpdateLibraryCommand {
                library_id,
                slug: payload.slug,
                display_name: payload.display_name,
                description: payload.description,
                extraction_prompt: payload.extraction_prompt,
                lifecycle_state: parse_lifecycle_state_input(lifecycle_state)?,
                include_document_hint_in_mcp_answers: payload
                    .include_document_hint_in_mcp_answers
                    .unwrap_or(existing.include_document_hint_in_mcp_answers),
            },
        )
        .await?;

    record_library_audit_event(
        &state,
        auth.principal_id,
        LibraryAuditEvent {
            request_id,
            action_kind: "catalog.library.update",
            redacted_message: format!("library {} updated", library.display_name),
            internal_message: format!(
                "principal {} updated library {} in workspace {}",
                auth.principal_id, library.id, library.workspace_id
            ),
            workspace_id: library.workspace_id,
            library_id: library.id,
        },
    )
    .await;

    Ok(Json(map_library(library)))
}

#[utoipa::path(
    put,
    path = "/v1/catalog/libraries/{libraryId}/web-ingest-policy",
    tag = "catalog",
    operation_id = "updateCatalogLibraryWebIngestPolicy",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    request_body = UpdateLibraryWebIngestPolicyRequest,
    responses(
        (status = 200, description = "Library after applying the new web ingest policy", body = CatalogLibraryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller does not have library write permission"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.update_library_web_ingest_policy",
    skip_all,
    fields(library_id = %library_id)
)]
pub async fn update_library_web_ingest_policy(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<UpdateLibraryWebIngestPolicyRequest>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    let existing =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;

    let library = state
        .application_services
        .catalog
        .update_library_web_ingest_policy(
            &state,
            UpdateLibraryWebIngestPolicyCommand {
                library_id,
                web_ingest_policy: WebIngestPolicy {
                    crawl_filter: payload.crawl_filter,
                    materialization_filter: payload.materialization_filter,
                },
            },
        )
        .await?;

    record_library_audit_event(
        &state,
        auth.principal_id,
        LibraryAuditEvent {
            request_id,
            action_kind: "catalog.library.web_ingest_policy.update",
            redacted_message: format!("library {} web ingest policy updated", library.display_name),
            internal_message: format!(
                "principal {} updated web ingest policy for library {} in workspace {}",
                auth.principal_id, library.id, library.workspace_id
            ),
            workspace_id: existing.workspace_id,
            library_id: library.id,
        },
    )
    .await;

    Ok(Json(map_library(library)))
}

#[utoipa::path(
    put,
    path = "/v1/catalog/libraries/{libraryId}/recognition-policy",
    tag = "catalog",
    operation_id = "updateCatalogLibraryRecognitionPolicy",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    request_body = UpdateLibraryRecognitionPolicyRequest,
    responses(
        (status = 200, description = "Library after applying the new recognition policy", body = CatalogLibraryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller does not have library write permission"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.update_library_recognition_policy",
    skip_all,
    fields(library_id = %library_id)
)]
pub async fn update_library_recognition_policy(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<UpdateLibraryRecognitionPolicyRequest>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    let existing =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;

    let library = state
        .application_services
        .catalog
        .update_library_recognition_policy(
            &state,
            UpdateLibraryRecognitionPolicyCommand {
                library_id,
                recognition_policy: LibraryRecognitionPolicy {
                    raster_image_engine: payload.raster_image_engine,
                },
            },
        )
        .await?;

    record_library_audit_event(
        &state,
        auth.principal_id,
        LibraryAuditEvent {
            request_id,
            action_kind: "catalog.library.recognition_policy.update",
            redacted_message: format!(
                "library {} recognition policy updated",
                library.display_name
            ),
            internal_message: format!(
                "principal {} updated recognition policy for library {} in workspace {}",
                auth.principal_id, library.id, library.workspace_id
            ),
            workspace_id: existing.workspace_id,
            library_id: library.id,
        },
    )
    .await;

    Ok(Json(map_library(library)))
}

#[utoipa::path(
    get,
    path = "/v1/catalog/libraries/{libraryId}/retrieval-config",
    tag = "catalog",
    operation_id = "getCatalogLibraryRetrievalConfig",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    responses(
        (status = 200, description = "Library retrieval configuration", body = RetrievalConfig),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller does not have library write permission"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_library_retrieval_config",
    skip_all,
    fields(library_id = %library_id)
)]
pub async fn get_library_retrieval_config(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<RetrievalConfig>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;
    let config =
        RetrievalConfig::from_json(library.retrieval_config).map_err(|_| ApiError::Internal)?;
    Ok(Json(config))
}

#[utoipa::path(
    put,
    path = "/v1/catalog/libraries/{libraryId}/retrieval-config",
    tag = "catalog",
    operation_id = "updateCatalogLibraryRetrievalConfig",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    request_body = UpdateLibraryRetrievalConfigRequest,
    responses(
        (status = 200, description = "Library after applying the new retrieval configuration", body = CatalogLibraryResponse),
        (status = 400, description = "Retrieval configuration is invalid or names an unknown text search config"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller does not have library write permission"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.update_library_retrieval_config",
    skip_all,
    fields(library_id = %library_id)
)]
pub async fn update_library_retrieval_config(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<UpdateLibraryRetrievalConfigRequest>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    let existing =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;

    let library = state
        .application_services
        .catalog
        .update_library_retrieval_config(
            &state,
            UpdateLibraryRetrievalConfigCommand { library_id, retrieval_config: payload },
        )
        .await?;

    record_library_audit_event(
        &state,
        auth.principal_id,
        LibraryAuditEvent {
            request_id,
            action_kind: "catalog.library.retrieval_config.update",
            redacted_message: format!(
                "library {} retrieval configuration updated",
                library.display_name
            ),
            internal_message: format!(
                "principal {} updated retrieval configuration for library {} in workspace {}",
                auth.principal_id, library.id, library.workspace_id
            ),
            workspace_id: existing.workspace_id,
            library_id: library.id,
        },
    )
    .await;

    Ok(Json(map_library(library)))
}
