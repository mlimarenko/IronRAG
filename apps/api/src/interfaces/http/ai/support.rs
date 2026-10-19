use super::*;

pub(super) fn map_provider(entry: ProviderCatalogEntry) -> ProviderCatalogEntryResponse {
    ProviderCatalogEntryResponse {
        id: entry.id,
        provider_kind: entry.provider_kind,
        display_name: entry.display_name,
        api_style: entry.api_style,
        lifecycle_state: entry.lifecycle_state,
        default_base_url: entry.default_base_url,
        api_key_required: entry.api_key_required,
        base_url_required: entry.base_url_required,
        credential_policy: entry.credential_policy,
        base_url_policy: entry.base_url_policy,
        model_discovery: entry.model_discovery,
        capabilities: entry.capabilities,
        runtime: entry.runtime,
        ui_hints: entry.ui_hints,
        purpose_gaps: entry.purpose_gaps,
    }
}

pub(super) fn map_model(entry: ResolvedModelCatalogEntry) -> ModelCatalogEntryResponse {
    ModelCatalogEntryResponse {
        id: entry.model.id,
        provider_catalog_id: entry.model.provider_catalog_id,
        model_name: entry.model.model_name,
        lifecycle_state: entry.model.lifecycle_state,
        allowed_binding_purposes: entry.model.allowed_binding_purposes,
        context_window: entry.model.context_window,
        max_output_tokens: entry.model.max_output_tokens,
        availability_state: entry.availability_state,
        available_account_ids: entry.available_account_ids,
        metadata_json: entry.model.metadata_json,
        pricing: entry.model.pricing,
    }
}

pub(super) fn map_available_provider_model(
    entry: AvailableProviderModel,
) -> AvailableProviderModelResponse {
    let AvailableProviderModel { discovered, model_catalog_id } = entry;
    AvailableProviderModelResponse {
        model_name: discovered.model_name,
        context_window: discovered.context_window,
        max_output_tokens: discovered.max_output_tokens,
        input_modalities: discovered.input_modalities,
        pricing: discovered.pricing,
        model_catalog_id,
    }
}

pub(super) fn map_account(mut entry: AiAccount) -> AiAccountResponse {
    let api_key_summary = configured_api_key_summary(
        entry.api_key.as_deref().is_some_and(|value| !value.trim().is_empty()),
    );
    AiAccountResponse {
        id: entry.id,
        scope_kind: entry.scope_kind,
        workspace_id: entry.workspace_id,
        library_id: entry.library_id,
        provider_catalog_id: entry.provider_catalog_id,
        label: std::mem::take(&mut entry.label),
        base_url: entry.base_url.take(),
        api_key_summary,
        credential_state: std::mem::take(&mut entry.credential_state),
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

pub(super) fn map_account_summary(mut entry: AiAccountSummary) -> AiAccountResponse {
    AiAccountResponse {
        id: entry.id,
        scope_kind: entry.scope_kind,
        workspace_id: entry.workspace_id,
        library_id: entry.library_id,
        provider_catalog_id: entry.provider_catalog_id,
        label: std::mem::take(&mut entry.label),
        base_url: entry.base_url.take(),
        api_key_summary: configured_api_key_summary(entry.has_api_key),
        credential_state: std::mem::take(&mut entry.credential_state),
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

pub(super) fn configured_api_key_summary(configured: bool) -> String {
    if configured { "configured" } else { "not_configured" }.to_string()
}

pub(super) fn map_binding(entry: AiBinding) -> AiBindingResponse {
    AiBindingResponse {
        id: entry.id,
        scope_kind: entry.scope_kind,
        workspace_id: entry.workspace_id,
        library_id: entry.library_id,
        binding_purpose: entry.binding_purpose,
        account_id: entry.account_id,
        provider_catalog_id: entry.provider_catalog_id,
        model_catalog_id: entry.model_catalog_id,
        system_prompt: entry.system_prompt,
        temperature: entry.temperature,
        top_p: entry.top_p,
        max_output_tokens_override: entry.max_output_tokens_override,
        extra_parameters_json: entry.extra_parameters_json,
        binding_state: entry.binding_state,
    }
}

pub(super) fn map_binding_validation(entry: BindingValidation) -> BindingValidationResponse {
    BindingValidationResponse {
        id: entry.id,
        binding_id: entry.binding_id,
        validation_state: entry.validation_state,
        checked_at: entry.checked_at,
        failure_code: entry.failure_code,
        message: entry.message,
    }
}

pub(super) fn authorize_ai_catalog_admin(auth: &AuthContext) -> Result<(), ApiError> {
    auth.require_write_capability()?;
    if auth.is_system_admin {
        return Ok(());
    }
    Err(ApiError::forbidden("system administrator required"))
}

pub(super) fn instance_subject(
    subject_kind: &str,
    subject_id: Uuid,
) -> AppendAuditEventSubjectCommand {
    AppendAuditEventSubjectCommand {
        subject_kind: subject_kind.to_string(),
        subject_id,
        workspace_id: None,
        library_id: None,
        document_id: None,
    }
}

pub(super) async fn authorize_visible_ai_context(
    auth: &AuthContext,
    state: &AppState,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<(Option<Uuid>, Option<Uuid>), ApiError> {
    if let Some(library_id) = library_id {
        let library =
            load_library_and_authorize(auth, state, library_id, POLICY_PROVIDERS_ADMIN).await?;
        if let Some(workspace_id) = workspace_id
            && workspace_id != library.workspace_id
        {
            return Err(ApiError::BadRequest(
                "libraryId does not belong to workspaceId".to_string(),
            ));
        }
        return Ok((Some(library.workspace_id), Some(library.id)));
    }

    if let Some(workspace_id) = workspace_id {
        authorize_workspace_permission(auth, workspace_id, POLICY_PROVIDERS_ADMIN)?;
        return Ok((Some(workspace_id), None));
    }

    if auth.is_system_admin {
        return Ok((None, None));
    }

    Err(ApiError::BadRequest(
        "workspaceId or libraryId is required for AI configuration".to_string(),
    ))
}

pub(super) async fn authorize_exact_ai_scope(
    auth: &AuthContext,
    state: &AppState,
    scope_kind: AiScopeKind,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<AiScopeRef, ApiError> {
    match scope_kind {
        AiScopeKind::Instance => {
            if workspace_id.is_some() || library_id.is_some() {
                return Err(ApiError::BadRequest(
                    "instance scope cannot include workspaceId or libraryId".to_string(),
                ));
            }
            if auth.is_system_admin {
                Ok(AiScopeRef { scope_kind, workspace_id: None, library_id: None })
            } else {
                Err(ApiError::Unauthorized)
            }
        }
        AiScopeKind::Workspace => {
            if library_id.is_some() {
                return Err(ApiError::BadRequest(
                    "workspace scope cannot include libraryId".to_string(),
                ));
            }
            let workspace_id = workspace_id.ok_or_else(|| {
                ApiError::BadRequest("workspaceId is required for workspace scope".to_string())
            })?;
            authorize_workspace_permission(auth, workspace_id, POLICY_PROVIDERS_ADMIN)?;
            Ok(AiScopeRef { scope_kind, workspace_id: Some(workspace_id), library_id: None })
        }
        AiScopeKind::Library => {
            let library_id = library_id.ok_or_else(|| {
                ApiError::BadRequest("libraryId is required for library scope".to_string())
            })?;
            let library =
                load_library_and_authorize(auth, state, library_id, POLICY_PROVIDERS_ADMIN).await?;
            if let Some(workspace_id) = workspace_id
                && workspace_id != library.workspace_id
            {
                return Err(ApiError::BadRequest(
                    "libraryId does not belong to workspaceId".to_string(),
                ));
            }
            Ok(AiScopeRef {
                scope_kind,
                workspace_id: Some(library.workspace_id),
                library_id: Some(library.id),
            })
        }
    }
}

pub(super) fn describe_scope(
    scope_kind: AiScopeKind,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> String {
    match scope_kind {
        AiScopeKind::Instance => "instance scope".to_string(),
        AiScopeKind::Workspace => format!(
            "workspace {}",
            workspace_id.map_or_else(|| "unknown".to_string(), |value| value.to_string())
        ),
        AiScopeKind::Library => format!(
            "library {}",
            library_id.map_or_else(|| "unknown".to_string(), |value| value.to_string())
        ),
    }
}

pub(super) fn subject_from_scope(
    subject_kind: &str,
    subject_id: Uuid,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> AppendAuditEventSubjectCommand {
    AppendAuditEventSubjectCommand {
        subject_kind: subject_kind.to_string(),
        subject_id,
        workspace_id,
        library_id,
        document_id: None,
    }
}
