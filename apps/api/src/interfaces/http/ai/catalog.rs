use super::*;

#[utoipa::path(
    get,
    path = "/v1/ai/providers",
    tag = "ai",
    operation_id = "listAiProviders",
    responses(
        (status = 200, description = "Available AI providers", body = [ProviderCatalogEntryResponse]),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.list_providers", skip_all)]
pub async fn list_providers(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<Vec<ProviderCatalogEntryResponse>>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let entries = state.application_services.ai_catalog.list_provider_catalog(&state).await?;
    Ok(Json(entries.into_iter().map(map_provider).collect()))
}

#[utoipa::path(
    get,
    path = "/v1/ai/models",
    tag = "ai",
    operation_id = "listAiModels",
    params(ModelsQuery),
    responses(
        (status = 200, description = "Resolved AI models for the requested context", body = [ModelCatalogEntryResponse]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot view the requested workspace/library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_models",
    skip_all,
    fields(workspace_id = ?query.workspace_id, library_id = ?query.library_id)
)]
pub async fn list_models(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ModelsQuery>,
) -> Result<Json<Vec<ModelCatalogEntryResponse>>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let (workspace_id, library_id) = if query.workspace_id.is_some()
        || query.library_id.is_some()
        || query.account_id.is_some()
    {
        authorize_visible_ai_context(&auth, &state, query.workspace_id, query.library_id).await?
    } else {
        (None, None)
    };
    let entries = state
        .application_services
        .ai_catalog
        .list_resolved_model_catalog(
            &state,
            query.provider_catalog_id,
            workspace_id,
            library_id,
            query.account_id,
        )
        .await?;
    Ok(Json(entries.into_iter().map(map_model).collect()))
}

#[utoipa::path(
    post,
    path = "/v1/ai/providers",
    tag = "ai",
    operation_id = "createAiProvider",
    request_body = CreateProviderCatalogRequest,
    responses(
        (status = 201, description = "Newly created AI provider catalog entry", body = ProviderCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.create_provider", skip_all)]
pub async fn create_provider(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateProviderCatalogRequest>,
) -> Result<axum::response::Response, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry = state
        .application_services
        .ai_catalog
        .create_provider_catalog(
            &state,
            CreateProviderCatalogCommand {
                provider_kind: payload.provider_kind,
                display_name: payload.display_name,
                api_style: payload.api_style,
                lifecycle_state: payload.lifecycle_state,
                default_base_url: payload.default_base_url,
                capability_flags_json: payload.capability_flags_json,
            },
        )
        .await?;
    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "ai.provider_catalog.create".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("AI provider {} created", entry.display_name)),
            internal_message: Some(format!(
                "principal {} created AI provider catalog entry {}",
                auth.principal_id, entry.id
            )),
            subjects: vec![instance_subject("provider_catalog", entry.id)],
        },
    )
    .await;
    Ok(created_response(&format!("/v1/ai/providers/{}", entry.id), map_provider(entry)))
}

#[utoipa::path(
    get,
    path = "/v1/ai/providers/{providerId}",
    tag = "ai",
    operation_id = "getAiProvider",
    params(("providerId" = uuid::Uuid, Path, description = "AI provider catalog identifier")),
    responses(
        (status = 200, description = "AI provider catalog entry", body = ProviderCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "Provider not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_provider", skip_all, fields(provider_id = %provider_id))]
pub async fn get_provider(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(provider_id): Path<Uuid>,
) -> Result<Json<ProviderCatalogEntryResponse>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let entry =
        state.application_services.ai_catalog.get_provider_catalog(&state, provider_id).await?;
    Ok(Json(map_provider(entry)))
}

#[utoipa::path(
    patch,
    path = "/v1/ai/providers/{providerId}",
    tag = "ai",
    operation_id = "updateAiProvider",
    params(("providerId" = uuid::Uuid, Path, description = "AI provider catalog identifier")),
    request_body = UpdateProviderCatalogRequest,
    responses(
        (status = 200, description = "Updated AI provider catalog entry", body = ProviderCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "Provider not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.update_provider", skip_all, fields(provider_id = %provider_id))]
pub async fn update_provider(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(provider_id): Path<Uuid>,
    Json(payload): Json<UpdateProviderCatalogRequest>,
) -> Result<Json<ProviderCatalogEntryResponse>, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry = state
        .application_services
        .ai_catalog
        .update_provider_catalog(
            &state,
            UpdateProviderCatalogCommand {
                provider_id,
                provider_kind: payload.provider_kind,
                display_name: payload.display_name,
                api_style: payload.api_style,
                lifecycle_state: payload.lifecycle_state,
                default_base_url: payload.default_base_url,
                capability_flags_json: payload.capability_flags_json,
            },
        )
        .await?;
    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "ai.provider_catalog.update".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("AI provider {} updated", entry.display_name)),
            internal_message: Some(format!(
                "principal {} updated AI provider catalog entry {}",
                auth.principal_id, entry.id
            )),
            subjects: vec![instance_subject("provider_catalog", entry.id)],
        },
    )
    .await;
    Ok(Json(map_provider(entry)))
}

/// Real hard delete; 409 when a model still references this provider. The
/// reversible pause is `PATCH {lifecycleState:"disabled"}` above — the two
/// are distinct operations (obtained by not deleting FK references but
/// blocking on them), not two paths to the same result.
#[utoipa::path(
    delete,
    path = "/v1/ai/providers/{providerId}",
    tag = "ai",
    operation_id = "deleteAiProvider",
    params(("providerId" = uuid::Uuid, Path, description = "AI provider catalog identifier")),
    responses(
        (status = 204, description = "Provider catalog entry deleted"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "Provider not found"),
        (status = 409, description = "Provider is still referenced by a model"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.delete_provider", skip_all, fields(provider_id = %provider_id))]
pub async fn delete_provider(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(provider_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry =
        state.application_services.ai_catalog.get_provider_catalog(&state, provider_id).await?;
    state.application_services.ai_catalog.delete_provider_catalog(&state, provider_id).await?;
    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "ai.provider_catalog.delete".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("AI provider {} deleted", entry.display_name)),
            internal_message: Some(format!(
                "principal {} deleted AI provider catalog entry {}",
                auth.principal_id, entry.id
            )),
            subjects: vec![instance_subject("provider_catalog", entry.id)],
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/v1/ai/models",
    tag = "ai",
    operation_id = "createAiModel",
    request_body = CreateModelCatalogRequest,
    responses(
        (status = 201, description = "Newly created AI model catalog entry", body = ModelCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.create_model", skip_all)]
pub async fn create_model(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateModelCatalogRequest>,
) -> Result<axum::response::Response, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry = state
        .application_services
        .ai_catalog
        .create_model_catalog(
            &state,
            CreateModelCatalogCommand {
                provider_catalog_id: payload.provider_catalog_id,
                model_name: payload.model_name,
                lifecycle_state: payload.lifecycle_state,
                allowed_binding_purposes: payload.allowed_binding_purposes,
                context_window: payload.context_window,
                max_output_tokens: payload.max_output_tokens,
                metadata_json: payload.metadata_json,
                pricing: payload.pricing,
            },
        )
        .await?;
    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "ai.model_catalog.create".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("AI model {} created", entry.model_name)),
            internal_message: Some(format!(
                "principal {} created AI model catalog entry {}",
                auth.principal_id, entry.id
            )),
            subjects: vec![instance_subject("model_catalog", entry.id)],
        },
    )
    .await;
    let model_id = entry.id;
    let resolved =
        state.application_services.ai_catalog.get_resolved_model_catalog(&state, model_id).await?;
    Ok(created_response(&format!("/v1/ai/models/{model_id}"), map_model(resolved)))
}

#[utoipa::path(
    get,
    path = "/v1/ai/models/{modelId}",
    tag = "ai",
    operation_id = "getAiModel",
    params(("modelId" = uuid::Uuid, Path, description = "AI model catalog identifier")),
    responses(
        (status = 200, description = "AI model catalog entry with its resolved availability", body = ModelCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "Model not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_model", skip_all, fields(model_id = %model_id))]
pub async fn get_model(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(model_id): Path<Uuid>,
) -> Result<Json<ModelCatalogEntryResponse>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let resolved =
        state.application_services.ai_catalog.get_resolved_model_catalog(&state, model_id).await?;
    Ok(Json(map_model(resolved)))
}

#[utoipa::path(
    patch,
    path = "/v1/ai/models/{modelId}",
    tag = "ai",
    operation_id = "updateAiModel",
    params(("modelId" = uuid::Uuid, Path, description = "AI model catalog identifier")),
    request_body = UpdateModelCatalogRequest,
    responses(
        (status = 200, description = "Updated AI model catalog entry", body = ModelCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "Model not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.update_model", skip_all, fields(model_id = %model_id))]
pub async fn update_model(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(model_id): Path<Uuid>,
    Json(payload): Json<UpdateModelCatalogRequest>,
) -> Result<Json<ModelCatalogEntryResponse>, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry = state
        .application_services
        .ai_catalog
        .update_model_catalog(
            &state,
            UpdateModelCatalogCommand {
                model_id,
                lifecycle_state: payload.lifecycle_state,
                allowed_binding_purposes: payload.allowed_binding_purposes,
                context_window: payload.context_window,
                max_output_tokens: payload.max_output_tokens,
                metadata_json: payload.metadata_json,
                pricing: payload.pricing,
            },
        )
        .await?;
    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "ai.model_catalog.update".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("AI model {} updated", entry.model_name)),
            internal_message: Some(format!(
                "principal {} updated AI model catalog entry {}",
                auth.principal_id, entry.id
            )),
            subjects: vec![instance_subject("model_catalog", entry.id)],
        },
    )
    .await;
    let resolved =
        state.application_services.ai_catalog.get_resolved_model_catalog(&state, model_id).await?;
    Ok(Json(map_model(resolved)))
}

/// Real hard delete; 409 when a binding still references
/// this model. The reversible pause is `PATCH {lifecycleState:"disabled"}`
/// above — the two are distinct operations, not two paths to the same
/// result.
#[utoipa::path(
    delete,
    path = "/v1/ai/models/{modelId}",
    tag = "ai",
    operation_id = "deleteAiModel",
    params(("modelId" = uuid::Uuid, Path, description = "AI model catalog identifier")),
    responses(
        (status = 204, description = "Model catalog entry deleted"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "Model not found"),
        (status = 409, description = "Model is still referenced by a binding"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.delete_model", skip_all, fields(model_id = %model_id))]
pub async fn delete_model(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(model_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry = state.application_services.ai_catalog.get_model_catalog(&state, model_id).await?;
    state.application_services.ai_catalog.delete_model_catalog(&state, model_id).await?;
    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "ai.model_catalog.delete".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("AI model {} deleted", entry.model_name)),
            internal_message: Some(format!(
                "principal {} deleted AI model catalog entry {}",
                auth.principal_id, entry.id
            )),
            subjects: vec![instance_subject("model_catalog", entry.id)],
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
