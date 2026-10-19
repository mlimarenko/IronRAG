use super::*;

#[tracing::instrument(level = "info", name = "http.list_workspaces", skip_all, fields(item_count))]
#[utoipa::path(
    get,
    path = "/v1/catalog/workspaces",
    tag = "catalog",
    operation_id = "listCatalogWorkspaces",
    responses(
        (status = 200, description = "Workspaces visible to the caller", body = [CatalogWorkspaceResponse]),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
pub async fn list_workspaces(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<Vec<CatalogWorkspaceResponse>>, ApiError> {
    let span = tracing::Span::current();
    auth.require_discover_any_workspace(POLICY_MCP_DISCOVERY)?;
    let workspaces = state.application_services.catalog.list_workspaces(&state, None).await?;
    let items: Vec<_> = workspaces
        .into_iter()
        .filter(|workspace| authorize_workspace_discovery(&auth, workspace.id).is_ok())
        .map(map_workspace)
        .collect();
    span.record("item_count", items.len());
    Ok(Json(items))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_workspace",
    skip_all,
    fields(workspace_id = %workspace_id)
)]
#[utoipa::path(
    get,
    path = "/v1/catalog/workspaces/{workspaceId}",
    tag = "catalog",
    operation_id = "getCatalogWorkspace",
    params(("workspaceId" = uuid::Uuid, Path, description = "Workspace identifier")),
    responses(
        (status = 200, description = "Workspace detail", body = CatalogWorkspaceResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the workspace"),
        (status = 404, description = "Workspace not found"),
    ),
)]
pub async fn get_workspace(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<CatalogWorkspaceResponse>, ApiError> {
    authorize_workspace_discovery(&auth, workspace_id)?;
    let workspace = state.application_services.catalog.get_workspace(&state, workspace_id).await?;
    Ok(Json(map_workspace(workspace)))
}

/// Partial update of a workspace's `slug`/`displayName`/`lifecycleState`.
/// Never existed in any prior version of this API — restricted to system
/// admins, same authorization boundary as create/delete.
#[utoipa::path(
    patch,
    path = "/v1/catalog/workspaces/{workspaceId}",
    tag = "catalog",
    operation_id = "updateCatalogWorkspace",
    params(("workspaceId" = uuid::Uuid, Path, description = "Workspace identifier")),
    request_body = UpdateCatalogWorkspaceRequest,
    responses(
        (status = 200, description = "Workspace after applying the update", body = CatalogWorkspaceResponse),
        (status = 400, description = "Invalid lifecycle state"),
        (status = 401, description = "Caller is not a system administrator"),
        (status = 404, description = "Workspace not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.update_workspace",
    skip_all,
    fields(workspace_id = %workspace_id)
)]
pub async fn update_workspace(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(workspace_id): Path<Uuid>,
    Json(payload): Json<UpdateCatalogWorkspaceRequest>,
) -> Result<Json<CatalogWorkspaceResponse>, ApiError> {
    let request_id = request_id.map(|value| value.0.0);
    if !auth.is_system_admin {
        record_rest_audit_event(
            &state,
            auth.principal_id,
            RestAuditEvent {
                request_id,
                action_kind: "catalog.workspace.update".to_string(),
                result_kind: "rejected".to_string(),
                redacted_message: Some("workspace update denied".to_string()),
                internal_message: Some(format!(
                    "principal {} was denied workspace update",
                    auth.principal_id
                )),
                subjects: Vec::new(),
            },
        )
        .await;
        return Err(ApiError::Unauthorized);
    }

    let existing = state.application_services.catalog.get_workspace(&state, workspace_id).await?;
    let lifecycle_state = match payload.lifecycle_state.as_deref() {
        Some(value) => parse_lifecycle_state_input(value)?,
        None => existing.lifecycle_state.clone(),
    };
    let workspace = state
        .application_services
        .catalog
        .update_workspace(
            &state,
            UpdateWorkspaceCommand {
                workspace_id,
                slug: Some(payload.slug.unwrap_or_else(|| existing.slug.clone())),
                display_name: payload.display_name.unwrap_or_else(|| existing.display_name.clone()),
                lifecycle_state,
            },
        )
        .await?;

    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id,
            action_kind: "catalog.workspace.update".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("workspace {} updated", workspace.display_name)),
            internal_message: Some(format!(
                "principal {} updated workspace {}",
                auth.principal_id, workspace.id
            )),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "workspace".to_string(),
                subject_id: workspace.id,
                workspace_id: Some(workspace.id),
                library_id: None,
                document_id: None,
            }],
        },
    )
    .await;

    Ok(Json(map_workspace(workspace)))
}

#[utoipa::path(
    post,
    path = "/v1/catalog/workspaces",
    tag = "catalog",
    operation_id = "createCatalogWorkspace",
    request_body = CreateCatalogWorkspaceRequest,
    responses(
        (status = 200, description = "Newly created workspace", body = CatalogWorkspaceResponse),
        (status = 401, description = "Caller is not a system administrator"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.create_workspace", skip_all)]
pub async fn create_workspace(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateCatalogWorkspaceRequest>,
) -> Result<Json<CatalogWorkspaceResponse>, ApiError> {
    if !auth.is_system_admin {
        record_rest_audit_event(
            &state,
            auth.principal_id,
            RestAuditEvent {
                request_id: request_id.map(|value| value.0.0),
                action_kind: "catalog.workspace.create".to_string(),
                result_kind: "rejected".to_string(),
                redacted_message: Some("workspace create denied".to_string()),
                internal_message: Some(format!(
                    "principal {} was denied workspace creation",
                    auth.principal_id
                )),
                subjects: Vec::new(),
            },
        )
        .await;
        return Err(ApiError::Unauthorized);
    }

    let workspace = state
        .application_services
        .catalog
        .create_workspace(
            &state,
            CreateWorkspaceCommand {
                slug: payload.slug,
                display_name: payload.display_name,
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;

    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "catalog.workspace.create".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("workspace {} created", workspace.display_name)),
            internal_message: Some(format!(
                "principal {} created workspace {}",
                auth.principal_id, workspace.id
            )),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "workspace".to_string(),
                subject_id: workspace.id,
                workspace_id: Some(workspace.id),
                library_id: None,
                document_id: None,
            }],
        },
    )
    .await;

    Ok(Json(map_workspace(workspace)))
}

#[utoipa::path(
    delete,
    path = "/v1/catalog/workspaces/{workspaceId}",
    tag = "catalog",
    operation_id = "deleteCatalogWorkspace",
    params(("workspaceId" = uuid::Uuid, Path, description = "Workspace identifier")),
    responses(
        (status = 202, description = "Workspace deletion accepted", body = CatalogDeletionAcceptedResponse),
        (status = 401, description = "Caller is not a system administrator"),
        (status = 404, description = "Workspace not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.delete_workspace",
    skip_all,
    fields(workspace_id = %workspace_id)
)]
pub async fn delete_workspace(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(workspace_id): Path<Uuid>,
) -> Result<(StatusCode, Json<CatalogDeletionAcceptedResponse>), ApiError> {
    let request_id = request_id.map(|value| value.0.0);
    if !auth.is_system_admin {
        record_rest_audit_event(
            &state,
            auth.principal_id,
            RestAuditEvent {
                request_id,
                action_kind: "catalog.workspace.delete".to_string(),
                result_kind: "rejected".to_string(),
                redacted_message: Some("workspace delete denied".to_string()),
                internal_message: Some(format!(
                    "principal {} was denied workspace deletion",
                    auth.principal_id
                )),
                subjects: Vec::new(),
            },
        )
        .await;
        return Err(ApiError::Unauthorized);
    }

    let admission = state
        .application_services
        .catalog
        .admit_workspace_deletion(&state, workspace_id, Some(auth.principal_id))
        .await?;

    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.clone(),
            action_kind: "catalog.workspace.delete".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!(
                "workspace {} deletion accepted",
                admission.display_name
            )),
            internal_message: Some(format!(
                "principal {} accepted workspace {} deletion via async operation {}",
                auth.principal_id, admission.workspace_id, admission.operation_id
            )),
            subjects: vec![AppendAuditEventSubjectCommand {
                subject_kind: "workspace".to_string(),
                subject_id: admission.workspace_id,
                workspace_id: Some(admission.workspace_id),
                library_id: None,
                document_id: None,
            }],
        },
    )
    .await;

    if admission.should_start_worker {
        spawn_workspace_deletion_worker(
            state.clone(),
            CatalogDeletionWorker {
                operation_id: admission.operation_id,
                workspace_id: admission.workspace_id,
                display_name: admission.display_name.clone(),
                principal_id: auth.principal_id,
                request_id,
            },
        );
    }

    Ok((StatusCode::ACCEPTED, Json(map_deletion_admission(admission))))
}
