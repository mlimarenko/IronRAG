use super::*;

pub(super) fn select_runtime_preset<'a>(
    presets: &'a [ModelPreset],
    canonical_name: &str,
) -> Option<&'a ModelPreset> {
    if let Some(existing) = presets.iter().find(|preset| preset.preset_name == canonical_name) {
        return Some(existing);
    }
    match presets {
        [only] => Some(only),
        _ => None,
    }
}

pub(super) fn normalize_non_empty(
    value: &str,
    field_name: &'static str,
) -> Result<String, ApiError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(ApiError::BadRequest(format!("{field_name} must not be empty")));
    }
    Ok(normalized.to_string())
}

pub(super) fn normalize_optional(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(ToString::to_string)
}

pub(super) fn map_ai_write_error(error: sqlx::Error) -> ApiError {
    match error {
        sqlx::Error::Database(database_error) if database_error.is_unique_violation() => {
            ApiError::Conflict("AI catalog resource already exists".to_string())
        }
        sqlx::Error::Database(database_error) if database_error.is_foreign_key_violation() => {
            ApiError::NotFound("referenced AI catalog resource was not found".to_string())
        }
        sqlx::Error::Database(database_error) if database_error.is_check_violation() => {
            ApiError::BadRequest("AI catalog payload violated schema constraints".to_string())
        }
        _ => ApiError::Internal,
    }
}

pub(super) fn parse_binding_purpose(value: &str) -> Result<AiBindingPurpose, ApiError> {
    match value {
        "extract_text" => Ok(AiBindingPurpose::ExtractText),
        "extract_graph" => Ok(AiBindingPurpose::ExtractGraph),
        "embed_chunk" => Ok(AiBindingPurpose::EmbedChunk),
        "query_retrieve" => Ok(AiBindingPurpose::QueryRetrieve),
        "query_answer" => Ok(AiBindingPurpose::QueryAnswer),
        "vision" => Ok(AiBindingPurpose::Vision),
        _ => Err(ApiError::Internal),
    }
}

pub(super) fn parse_scope_kind(value: &str) -> Result<AiScopeKind, ApiError> {
    match value {
        "instance" => Ok(AiScopeKind::Instance),
        "workspace" => Ok(AiScopeKind::Workspace),
        "library" => Ok(AiScopeKind::Library),
        _ => Err(ApiError::Internal),
    }
}

pub(super) fn scope_kind_key(value: AiScopeKind) -> &'static str {
    value.as_str()
}

pub(super) async fn normalize_scope_ref(
    state: &AppState,
    scope_kind: AiScopeKind,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<AiScopeRef, ApiError> {
    match scope_kind {
        AiScopeKind::Instance => {
            if workspace_id.is_some() || library_id.is_some() {
                return Err(ApiError::BadRequest(
                    "instance scope must not include workspaceId or libraryId".to_string(),
                ));
            }
            Ok(AiScopeRef { scope_kind, workspace_id: None, library_id: None })
        }
        AiScopeKind::Workspace => {
            let workspace_id = workspace_id.ok_or_else(|| {
                ApiError::BadRequest("workspace scope requires workspaceId".to_string())
            })?;
            if library_id.is_some() {
                return Err(ApiError::BadRequest(
                    "workspace scope must not include libraryId".to_string(),
                ));
            }
            let exists =
                catalog_repository::get_workspace_by_id(&state.persistence.postgres, workspace_id)
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                    .is_some();
            if !exists {
                return Err(ApiError::resource_not_found("workspace", workspace_id));
            }
            Ok(AiScopeRef { scope_kind, workspace_id: Some(workspace_id), library_id: None })
        }
        AiScopeKind::Library => {
            let library_id = library_id.ok_or_else(|| {
                ApiError::BadRequest("library scope requires libraryId".to_string())
            })?;
            let library =
                catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                    .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
            if let Some(expected_workspace_id) = workspace_id {
                if expected_workspace_id != library.workspace_id {
                    return Err(ApiError::BadRequest(
                        "libraryId does not belong to workspaceId".to_string(),
                    ));
                }
            }
            Ok(AiScopeRef {
                scope_kind,
                workspace_id: Some(library.workspace_id),
                library_id: Some(library.id),
            })
        }
    }
}

pub(super) fn scope_ref_from_binding_row(
    row: &ai_repository::AiBindingAssignmentRow,
) -> Result<AiScopeRef, ApiError> {
    Ok(AiScopeRef {
        scope_kind: parse_scope_kind(&row.scope_kind)?,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
    })
}

pub(super) fn scope_ref_from_provider_credential(
    credential: &ProviderCredential,
) -> Result<AiScopeRef, ApiError> {
    Ok(AiScopeRef {
        scope_kind: credential.scope_kind,
        workspace_id: credential.workspace_id,
        library_id: credential.library_id,
    })
}

pub(super) fn scope_ref_from_model_preset(preset: &ModelPreset) -> Result<AiScopeRef, ApiError> {
    Ok(AiScopeRef {
        scope_kind: preset.scope_kind,
        workspace_id: preset.workspace_id,
        library_id: preset.library_id,
    })
}

pub(super) fn scope_can_use_resource(owner_scope: AiScopeRef, resource_scope: AiScopeRef) -> bool {
    match owner_scope.scope_kind {
        AiScopeKind::Instance => resource_scope.scope_kind == AiScopeKind::Instance,
        AiScopeKind::Workspace => {
            resource_scope.scope_kind == AiScopeKind::Instance
                || (resource_scope.scope_kind == AiScopeKind::Workspace
                    && resource_scope.workspace_id == owner_scope.workspace_id)
        }
        AiScopeKind::Library => {
            resource_scope.scope_kind == AiScopeKind::Instance
                || (resource_scope.scope_kind == AiScopeKind::Workspace
                    && resource_scope.workspace_id == owner_scope.workspace_id)
                || (resource_scope.scope_kind == AiScopeKind::Library
                    && resource_scope.library_id == owner_scope.library_id)
        }
    }
}

pub(crate) fn binding_purpose_key(value: AiBindingPurpose) -> &'static str {
    match value {
        AiBindingPurpose::ExtractText => "extract_text",
        AiBindingPurpose::ExtractGraph => "extract_graph",
        AiBindingPurpose::EmbedChunk => "embed_chunk",
        AiBindingPurpose::QueryRetrieve => "query_retrieve",
        AiBindingPurpose::QueryAnswer => "query_answer",
        AiBindingPurpose::Vision => "vision",
    }
}

pub(crate) fn canonical_runtime_preset_name(
    provider_display_name: &str,
    purpose: AiBindingPurpose,
    model_name: &str,
) -> String {
    let purpose_label = match purpose {
        AiBindingPurpose::ExtractText => "Extract Text",
        AiBindingPurpose::ExtractGraph => "Extract Graph",
        AiBindingPurpose::EmbedChunk => "Embed Chunk",
        AiBindingPurpose::QueryRetrieve => "Query Retrieve",
        AiBindingPurpose::QueryAnswer => "Query Answer",
        AiBindingPurpose::Vision => "Vision",
    };
    format!("{provider_display_name} {purpose_label} · {model_name}")
}

pub(super) fn map_binding_assignment_row(
    row: ai_repository::AiBindingAssignmentRow,
) -> Result<AiBindingAssignment, ApiError> {
    Ok(AiBindingAssignment {
        id: row.id,
        scope_kind: parse_scope_kind(&row.scope_kind)?,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        binding_purpose: parse_binding_purpose(&row.binding_purpose)?,
        provider_credential_id: row.provider_credential_id,
        model_preset_id: row.model_preset_id,
        binding_state: row.binding_state,
    })
}

pub(super) fn map_binding_validation_row(
    row: ai_repository::AiBindingValidationRow,
) -> BindingValidation {
    BindingValidation {
        id: row.id,
        binding_id: row.binding_id,
        validation_state: row.validation_state,
        checked_at: row.checked_at,
        failure_code: row.failure_code,
        message: row.message,
    }
}
