use super::*;

#[utoipa::path(
    get,
    path = "/v1/ai/accounts",
    tag = "ai",
    operation_id = "listAiAccounts",
    params(AiScopeQuery),
    responses(
        (status = 200, description = "Visible AI accounts for the requested scope", body = [AiAccountResponse]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the requested scope"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_accounts",
    skip_all,
    fields(workspace_id = ?query.workspace_id, library_id = ?query.library_id)
)]
pub async fn list_accounts(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AiScopeQuery>,
) -> Result<Json<Vec<AiAccountResponse>>, ApiError> {
    let entries = if let Some(scope_kind) = query.scope_kind {
        let scope = authorize_exact_ai_scope(
            &auth,
            &state,
            scope_kind,
            query.workspace_id,
            query.library_id,
        )
        .await?;
        state.application_services.ai_catalog.list_account_summaries_exact(&state, scope).await?
    } else {
        let (workspace_id, library_id) =
            authorize_visible_ai_context(&auth, &state, query.workspace_id, query.library_id)
                .await?;
        state
            .application_services
            .ai_catalog
            .list_visible_account_summaries(&state, workspace_id, library_id)
            .await?
    };
    Ok(Json(entries.into_iter().map(map_account_summary).collect()))
}

#[utoipa::path(
    get,
    path = "/v1/ai/accounts/{accountId}",
    tag = "ai",
    operation_id = "getAiAccount",
    params(("accountId" = uuid::Uuid, Path, description = "AI account identifier")),
    responses(
        (status = 200, description = "AI account (secret redacted, same as the list view)", body = AiAccountResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot view accounts in the account's scope"),
        (status = 404, description = "Account not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_account", skip_all, fields(account_id = %account_id))]
pub async fn get_account(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<AiAccountResponse>, ApiError> {
    let account = state.application_services.ai_catalog.get_account(&state, account_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        account.scope_kind,
        account.workspace_id,
        account.library_id,
    )
    .await?;
    Ok(Json(map_account(account)))
}

#[utoipa::path(
    get,
    path = "/v1/ai/accounts/{account_id}/available-models",
    tag = "ai",
    operation_id = "listAvailableProviderModels",
    params(("account_id" = Uuid, Path, description = "Account whose credentials are used")),
    responses(
        (status = 200, description = "Models the provider reports for this account", body = [AvailableProviderModelResponse]),
        (status = 400, description = "Provider does not publish a model listing"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the account scope"),
        (status = 404, description = "Account does not exist"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_available_provider_models",
    skip_all,
    fields(account_id = %account_id)
)]
pub async fn list_available_provider_models(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<Vec<AvailableProviderModelResponse>>, ApiError> {
    let account = state.application_services.ai_catalog.get_account(&state, account_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        account.scope_kind,
        account.workspace_id,
        account.library_id,
    )
    .await?;
    let entries = state
        .application_services
        .ai_catalog
        .list_available_provider_models(&state, account_id)
        .await?;
    Ok(Json(entries.into_iter().map(map_available_provider_model).collect()))
}

#[utoipa::path(
    post,
    path = "/v1/ai/accounts",
    tag = "ai",
    operation_id = "createAiAccount",
    request_body = CreateAiAccountRequest,
    responses(
        (status = 201, description = "Newly created AI account", body = AiAccountResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer accounts in the requested scope"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.create_account",
    skip_all,
    fields(workspace_id = ?payload.workspace_id, library_id = ?payload.library_id)
)]
pub async fn create_account(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(mut payload): Json<CreateAiAccountRequest>,
) -> Result<axum::response::Response, ApiError> {
    let request_id = request_id.map(|value| value.0.0);
    let scope = match authorize_exact_ai_scope(
        &auth,
        &state,
        payload.scope_kind,
        payload.workspace_id,
        payload.library_id,
    )
    .await
    {
        Ok(scope) => scope,
        Err(error @ ApiError::Unauthorized) => {
            // Governance denials are audited like the catalog and IAM
            // surfaces: the attempted credential write is itself a
            // security-relevant action.
            record_rest_audit_event(
                &state,
                auth.principal_id,
                RestAuditEvent {
                    request_id,
                    action_kind: "ai.provider_credential.create".to_string(),
                    result_kind: "rejected".to_string(),
                    redacted_message: Some("provider credential creation denied".to_string()),
                    internal_message: Some(format!(
                        "principal {} was denied provider credential creation",
                        auth.principal_id
                    )),
                    subjects: payload
                        .workspace_id
                        .map(|workspace_id| {
                            vec![AppendAuditEventSubjectCommand {
                                subject_kind: "workspace".to_string(),
                                subject_id: workspace_id,
                                workspace_id: Some(workspace_id),
                                library_id: None,
                                document_id: None,
                            }]
                        })
                        .unwrap_or_default(),
                },
            )
            .await;
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let entry = state
        .application_services
        .ai_catalog
        .create_account(
            &state,
            CreateAiAccountCommand {
                scope_kind: payload.scope_kind,
                workspace_id: scope.workspace_id,
                library_id: scope.library_id,
                provider_catalog_id: payload.provider_catalog_id,
                label: std::mem::take(&mut payload.label),
                api_key: payload.api_key.take(),
                base_url: payload.base_url.take(),
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;
    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id,
            action_kind: "ai.provider_credential.create".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("AI account {} created", entry.label)),
            internal_message: Some(format!(
                "principal {} created AI account {} in {}",
                auth.principal_id,
                entry.id,
                describe_scope(entry.scope_kind, entry.workspace_id, entry.library_id),
            )),
            subjects: vec![subject_from_scope(
                "provider_credential",
                entry.id,
                entry.workspace_id,
                entry.library_id,
            )],
        },
    )
    .await;
    let account_id = entry.id;
    Ok(created_response(&format!("/v1/ai/accounts/{account_id}"), map_account(entry)))
}

#[tracing::instrument(
    level = "info",
    name = "http.update_account",
    skip_all,
    fields(account_id = %account_id)
)]
#[utoipa::path(
    patch,
    path = "/v1/ai/accounts/{accountId}",
    tag = "ai",
    operation_id = "updateAiAccount",
    params(("accountId" = uuid::Uuid, Path, description = "AI account identifier")),
    request_body = UpdateAiAccountRequest,
    responses(
        (status = 200, description = "Updated AI account", body = AiAccountResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer accounts in the account's scope"),
        (status = 404, description = "Account not found"),
        (status = 409, description = "Account changed after the editor loaded it"),
    ),
)]
pub async fn update_account(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(account_id): Path<Uuid>,
    Json(mut payload): Json<UpdateAiAccountRequest>,
) -> Result<Json<AiAccountResponse>, ApiError> {
    let account = state.application_services.ai_catalog.get_account(&state, account_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        account.scope_kind,
        account.workspace_id,
        account.library_id,
    )
    .await?;
    let entry = state
        .application_services
        .ai_catalog
        .update_account(
            &state,
            UpdateAiAccountCommand {
                account_id,
                label: std::mem::take(&mut payload.label),
                api_key: payload.api_key.take(),
                base_url: payload.base_url.take(),
                credential_state: std::mem::take(&mut payload.credential_state),
                expected_updated_at: payload.expected_updated_at,
            },
        )
        .await?;
    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "ai.provider_credential.update".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("AI account {} updated", entry.label)),
            internal_message: Some(format!(
                "principal {} updated AI account {} in {}",
                auth.principal_id,
                entry.id,
                describe_scope(entry.scope_kind, entry.workspace_id, entry.library_id),
            )),
            subjects: vec![subject_from_scope(
                "provider_credential",
                entry.id,
                entry.workspace_id,
                entry.library_id,
            )],
        },
    )
    .await;
    Ok(Json(map_account(entry)))
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_account",
    skip_all,
    fields(account_id = %account_id)
)]
#[utoipa::path(
    delete,
    path = "/v1/ai/accounts/{accountId}",
    tag = "ai",
    operation_id = "deleteAiAccount",
    params(("accountId" = uuid::Uuid, Path, description = "AI account identifier")),
    responses(
        (status = 204, description = "Account deleted"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer accounts in the account's scope"),
        (status = 404, description = "Account not found"),
        (status = 409, description = "Account is still referenced"),
    ),
)]
pub async fn delete_account(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(account_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let account = state.application_services.ai_catalog.get_account(&state, account_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        account.scope_kind,
        account.workspace_id,
        account.library_id,
    )
    .await?;
    state.application_services.ai_catalog.delete_account(&state, account_id).await?;
    record_rest_audit_event(
        &state,
        auth.principal_id,
        RestAuditEvent {
            request_id: request_id.map(|value| value.0.0),
            action_kind: "ai.provider_credential.delete".to_string(),
            result_kind: "succeeded".to_string(),
            redacted_message: Some(format!("AI account {} deleted", account.label)),
            internal_message: Some(format!(
                "principal {} deleted AI account {} in {}",
                auth.principal_id,
                account_id,
                describe_scope(account.scope_kind, account.workspace_id, account.library_id),
            )),
            subjects: vec![subject_from_scope(
                "provider_credential",
                account_id,
                account.workspace_id,
                account.library_id,
            )],
        },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
