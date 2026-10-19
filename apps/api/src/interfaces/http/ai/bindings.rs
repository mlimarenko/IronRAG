use super::*;

/// One audited action on a binding assignment: the audit action key, the
/// outcome word in the summary and the verb phrase in the detail line.
#[derive(Clone, Copy)]
enum BindingAuditAction {
    Create,
    Update,
    Delete,
    Validate,
}

impl BindingAuditAction {
    const fn action_key(self) -> &'static str {
        match self {
            Self::Create => "ai.binding_assignment.create",
            Self::Update => "ai.binding_assignment.update",
            Self::Delete => "ai.binding_assignment.delete",
            Self::Validate => "ai.binding_assignment.validate",
        }
    }

    const fn outcome(self) -> &'static str {
        match self {
            Self::Create => "created",
            Self::Update => "updated",
            Self::Delete => "deleted",
            Self::Validate => "validation requested",
        }
    }

    const fn detail_verb(self) -> &'static str {
        match self {
            Self::Create => "created",
            Self::Update => "updated",
            Self::Delete => "deleted",
            Self::Validate => "requested validation for",
        }
    }
}

async fn record_binding_audit(
    state: &AppState,
    auth: &AuthContext,
    request_id: Option<axum::Extension<RequestId>>,
    binding: &AiBinding,
    action: BindingAuditAction,
) {
    record_rest_audit_event(
        state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: action.action_key().to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("binding {} {}", binding.id, action.outcome())),
            internal_message: Some(format!(
                "principal {} {} binding {} in {}",
                auth.principal_id,
                action.detail_verb(),
                binding.id,
                describe_scope(binding.scope_kind, binding.workspace_id, binding.library_id),
            )),
            subjects: vec![subject_from_scope(
                "binding_assignment",
                binding.id,
                binding.workspace_id,
                binding.library_id,
            )],
        },
    )
    .await;
}

#[utoipa::path(
    get,
    path = "/v1/ai/bindings",
    tag = "ai",
    operation_id = "listAiLibraryBindings",
    params(AiScopeQuery),
    responses(
        (status = 200, description = "Bindings for the requested scope", body = [AiBindingResponse]),
        (status = 400, description = "scopeKind query parameter is required"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the requested scope"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_bindings",
    skip_all,
    fields(workspace_id = ?query.workspace_id, library_id = ?query.library_id)
)]
pub async fn list_bindings(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AiScopeQuery>,
) -> Result<Json<Vec<AiBindingResponse>>, ApiError> {
    let scope_kind = query.scope_kind.ok_or_else(|| {
        ApiError::BadRequest("scopeKind is required for binding queries".to_string())
    })?;
    let scope =
        authorize_exact_ai_scope(&auth, &state, scope_kind, query.workspace_id, query.library_id)
            .await?;
    let entries = state.application_services.ai_catalog.list_bindings(&state, scope).await?;
    Ok(Json(entries.into_iter().map(map_binding).collect()))
}

#[utoipa::path(
    get,
    path = "/v1/ai/bindings/{bindingId}",
    tag = "ai",
    operation_id = "getAiLibraryBinding",
    params(("bindingId" = uuid::Uuid, Path, description = "Binding identifier")),
    responses(
        (status = 200, description = "Binding", body = AiBindingResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot view bindings in the binding's scope"),
        (status = 404, description = "Binding not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_binding", skip_all, fields(binding_id = %binding_id))]
pub async fn get_binding(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(binding_id): Path<Uuid>,
) -> Result<Json<AiBindingResponse>, ApiError> {
    let binding = state.application_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    Ok(Json(map_binding(binding)))
}

#[tracing::instrument(
    level = "info",
    name = "http.create_binding",
    skip_all,
    fields(workspace_id = ?payload.workspace_id, library_id = ?payload.library_id)
)]
#[utoipa::path(
    post,
    path = "/v1/ai/bindings",
    tag = "ai",
    operation_id = "createAiLibraryBinding",
    request_body = CreateAiBindingRequest,
    responses(
        (status = 201, description = "Newly created binding", body = AiBindingResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer bindings in the requested scope"),
    ),
)]
pub async fn create_binding(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateAiBindingRequest>,
) -> Result<axum::response::Response, ApiError> {
    let scope = authorize_exact_ai_scope(
        &auth,
        &state,
        payload.scope_kind,
        payload.workspace_id,
        payload.library_id,
    )
    .await?;
    let entry = state
        .application_services
        .ai_catalog
        .create_binding(
            &state,
            CreateAiBindingCommand {
                scope_kind: payload.scope_kind,
                workspace_id: scope.workspace_id,
                library_id: scope.library_id,
                binding_purpose: payload.binding_purpose,
                account_id: payload.account_id,
                model_catalog_id: payload.model_catalog_id,
                system_prompt: payload.system_prompt,
                temperature: payload.temperature,
                top_p: payload.top_p,
                max_output_tokens_override: payload.max_output_tokens_override,
                extra_parameters_json: payload.extra_parameters_json,
                binding_state: payload.binding_state,
                updated_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;
    record_binding_audit(&state, &auth, request_id, &entry, BindingAuditAction::Create).await;
    let binding_id = entry.id;
    Ok(created_response(&format!("/v1/ai/bindings/{binding_id}"), map_binding(entry)))
}

#[tracing::instrument(
    level = "info",
    name = "http.update_binding",
    skip_all,
    fields(binding_id = %binding_id)
)]
#[utoipa::path(
    patch,
    path = "/v1/ai/bindings/{bindingId}",
    tag = "ai",
    operation_id = "updateAiLibraryBinding",
    params(("bindingId" = uuid::Uuid, Path, description = "Binding identifier")),
    request_body = UpdateAiBindingRequest,
    responses(
        (status = 200, description = "Updated binding", body = AiBindingResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer bindings in the binding's scope"),
        (status = 404, description = "Binding not found"),
    ),
)]
pub async fn update_binding(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(binding_id): Path<Uuid>,
    Json(payload): Json<UpdateAiBindingRequest>,
) -> Result<Json<AiBindingResponse>, ApiError> {
    let binding = state.application_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    let entry = state
        .application_services
        .ai_catalog
        .update_binding(
            &state,
            UpdateAiBindingCommand {
                binding_id,
                account_id: payload.account_id,
                model_catalog_id: payload.model_catalog_id,
                system_prompt: payload.system_prompt,
                temperature: payload.temperature,
                top_p: payload.top_p,
                max_output_tokens_override: payload.max_output_tokens_override,
                extra_parameters_json: payload.extra_parameters_json,
                binding_state: payload.binding_state,
                updated_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;
    record_binding_audit(&state, &auth, request_id, &entry, BindingAuditAction::Update).await;
    Ok(Json(map_binding(entry)))
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_binding",
    skip_all,
    fields(binding_id = %binding_id)
)]
#[utoipa::path(
    delete,
    path = "/v1/ai/bindings/{bindingId}",
    tag = "ai",
    operation_id = "deleteAiLibraryBinding",
    params(("bindingId" = uuid::Uuid, Path, description = "Binding identifier")),
    responses(
        (status = 204, description = "Binding deleted"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer bindings in the binding's scope"),
        (status = 404, description = "Binding not found"),
    ),
)]
pub async fn delete_binding(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(binding_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let binding = state.application_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    state.application_services.ai_catalog.delete_binding(&state, binding_id).await?;
    record_binding_audit(&state, &auth, request_id, &binding, BindingAuditAction::Delete).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Creates a binding-validation attempt. Resource-shaped replacement for the
/// former RPC-verb `POST .../validate`: the handler already created a real
/// record with a `pending` → `succeeded`/`failed` lifecycle (`ai_binding_validation`,
/// unchanged by this redesign); the defect was the unpollable verb-shaped
/// form, not the underlying action. `Location` points at the created
/// validation's own item-read endpoint below.
#[tracing::instrument(
    level = "info",
    name = "http.create_binding_validation",
    skip_all,
    fields(binding_id = %binding_id)
)]
#[utoipa::path(
    post,
    path = "/v1/ai/bindings/{bindingId}/validations",
    tag = "ai",
    operation_id = "createAiBindingValidation",
    params(("bindingId" = uuid::Uuid, Path, description = "Binding identifier")),
    responses(
        (status = 201, description = "Binding validation attempt created", body = BindingValidationResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot validate bindings in the binding's scope"),
        (status = 404, description = "Binding not found"),
    ),
)]
pub async fn create_binding_validation(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(binding_id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    let binding = state.application_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    let validation = state
        .application_services
        .ai_catalog
        .validate_binding(
            &state,
            CreateBindingValidationCommand {
                binding_id,
                validation_state: "pending".to_string(),
                failure_code: None,
                message: Some("validation requested".to_string()),
            },
        )
        .await?;
    record_binding_audit(&state, &auth, request_id, &binding, BindingAuditAction::Validate).await;
    let validation_id = validation.id;
    Ok(created_response(
        &format!("/v1/ai/bindings/{binding_id}/validations/{validation_id}"),
        map_binding_validation(validation),
    ))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_binding_validation",
    skip_all,
    fields(binding_id = %binding_id, validation_id = %validation_id)
)]
#[utoipa::path(
    get,
    path = "/v1/ai/bindings/{bindingId}/validations/{validationId}",
    tag = "ai",
    operation_id = "getAiBindingValidation",
    params(
        ("bindingId" = uuid::Uuid, Path, description = "Binding identifier"),
        ("validationId" = uuid::Uuid, Path, description = "Binding validation attempt identifier"),
    ),
    responses(
        (status = 200, description = "Binding validation outcome", body = BindingValidationResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot view validations in the binding's scope"),
        (status = 404, description = "Binding or validation not found"),
    ),
)]
pub async fn get_binding_validation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((binding_id, validation_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<BindingValidationResponse>, ApiError> {
    let binding = state.application_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    let validation =
        state.application_services.ai_catalog.get_binding_validation(&state, validation_id).await?;
    if validation.binding_id != binding_id {
        return Err(ApiError::resource_not_found("binding_validation", validation_id));
    }
    Ok(Json(map_binding_validation(validation)))
}

#[tracing::instrument(
    level = "info",
    name = "http.list_binding_validations",
    skip_all,
    fields(binding_id = %binding_id, item_count)
)]
#[utoipa::path(
    get,
    path = "/v1/ai/bindings/{bindingId}/validations",
    tag = "ai",
    operation_id = "listAiBindingValidations",
    params(("bindingId" = uuid::Uuid, Path, description = "Binding identifier")),
    responses(
        (status = 200, description = "Validation attempt history for the binding, newest first", body = [BindingValidationResponse]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot view validations in the binding's scope"),
        (status = 404, description = "Binding not found"),
    ),
)]
pub async fn list_binding_validations(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(binding_id): Path<Uuid>,
) -> Result<Json<Vec<BindingValidationResponse>>, ApiError> {
    let span = tracing::Span::current();
    let binding = state.application_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    let validations =
        state.application_services.ai_catalog.list_binding_validations(&state, binding_id).await?;
    span.record("item_count", validations.len());
    Ok(Json(validations.into_iter().map(map_binding_validation).collect()))
}
