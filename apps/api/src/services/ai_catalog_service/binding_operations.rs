use super::*;
use crate::domains::provider_profiles::{
    OPENAI_COMPATIBLE_RUNTIME_KIND, REASONING_MODE_FIELD, model_reasoning_capability,
};

pub(super) fn parse_request_policy(
    value: &serde_json::Value,
    field_name: &str,
) -> Result<ProviderRequestPolicy, ApiError> {
    let policy = serde_json::from_value::<ProviderRequestPolicy>(value.clone())
        .map_err(|_| ApiError::BadRequest(format!("{field_name} is invalid")))?;
    validate_request_policy(&policy, field_name)?;
    Ok(policy)
}

pub(super) fn validate_request_policy(
    policy: &ProviderRequestPolicy,
    field_name: &str,
) -> Result<(), ApiError> {
    if !policy.is_valid() {
        return Err(ApiError::BadRequest(format!(
            "{field_name}.defaultToolMaxOutputTokens must be positive"
        )));
    }
    Ok(())
}

pub(super) fn validate_binding_request_policy(
    extra_parameters_json: &serde_json::Value,
) -> Result<(), ApiError> {
    if let Some(value) = extra_parameters_json.get("_providerRequestPolicy") {
        parse_request_policy(value, "extraParametersJson._providerRequestPolicy")?;
    }
    // The typed selected mode is validated on the same write boundary: an
    // absent field is the honest "not selected" state, while a present but
    // malformed form (unknown version, unknown mode, wrong type, a conflicting
    // raw effort) is a client error, never silently read as unset (IRG-153).
    SelectedReasoningMode::from_binding_json(extra_parameters_json)
        .map_err(ApiError::BadRequest)
        .map(|_| ())?;
    Ok(())
}

/// The `ai_binding_state` enum labels. A client-supplied state is checked here
/// so an unknown value is a 400, not a failed enum cast surfacing as a 500.
const AI_BINDING_STATES: [&str; 3] = ["active", "invalid", "disabled"];

pub(super) fn validated_binding_state(value: &str) -> Result<&str, ApiError> {
    if AI_BINDING_STATES.contains(&value) {
        return Ok(value);
    }
    Err(ApiError::BadRequest(format!(
        "bindingState must be one of {}",
        AI_BINDING_STATES.join(", ")
    )))
}

pub(super) fn binding_affects_vector_index_dimension(binding_purpose: AiBindingPurpose) -> bool {
    binding_purpose == AiBindingPurpose::EmbedChunk
}

fn invalidate_vector_dimension_cache_for_binding(binding_purpose: AiBindingPurpose) {
    if binding_affects_vector_index_dimension(binding_purpose) {
        crate::services::query::vector_dimensions::invalidate_vector_index_dimension_cache();
    }
}

fn validate_binding_account_scope(scope: AiScopeRef, account: &AiAccount) -> Result<(), ApiError> {
    let account_scope = scope_ref_from_account(account)?;
    if scope_can_use_resource(scope, account_scope) {
        return Ok(());
    }
    Err(ApiError::BadRequest(
        "binding cannot use a provider account from an unrelated scope".to_string(),
    ))
}

/// Everything `validate_binding_target` (the canonical purpose-eligibility
/// gate) does not already cover for a write: the embedding-dimension check is
/// specific to the write path, since a read-path row was already validated at
/// write time.
fn validate_binding_write_components(
    binding_purpose: AiBindingPurpose,
    extra_parameters_json: &serde_json::Value,
    account: &AiAccount,
    model: &ModelCatalogEntry,
    provider: &ProviderCatalogEntry,
) -> Result<(), ApiError> {
    validate_binding_target(BindingTarget {
        purpose: binding_purpose,
        provider,
        model,
        account_credential_state: &account.credential_state,
    })?;
    resolve_effective_embedding_dimensions(
        binding_purpose,
        extra_parameters_json,
        &model.metadata_json,
    )?;
    validate_binding_reasoning_selection(extra_parameters_json, model, provider)?;
    Ok(())
}

/// Validate the binding's typed reasoning selection against the exact model's
/// confirmed capability.
///
/// The capability is read only from the bound model's catalog metadata
/// (`metadataJson.reasoningCapability`); the provider profile cannot grant it,
/// so a provider-wide value never authorizes a model that was not itself
/// confirmed. A selection the model has not confirmed, a malformed/blank
/// confirmation, a runtime with no confirmed wire adapter, or a raw parameter
/// that disagrees with the confirmed wire control is a 4xx, never an
/// accepted-but-ignored parameter.
pub(super) fn validate_binding_reasoning_selection(
    extra_parameters_json: &serde_json::Value,
    model: &ModelCatalogEntry,
    provider: &ProviderCatalogEntry,
) -> Result<(), ApiError> {
    let Some(selection) = SelectedReasoningMode::from_binding_json(extra_parameters_json)
        .map_err(ApiError::BadRequest)?
    else {
        return Ok(());
    };
    let capability =
        model_reasoning_capability(&model.metadata_json).map_err(ApiError::BadRequest)?;
    if provider.runtime.kind != OPENAI_COMPATIBLE_RUNTIME_KIND {
        return Err(ApiError::BadRequest(format!(
            "extraParametersJson.{REASONING_MODE_FIELD} is not supported for provider `{}`: this runtime has no confirmed reasoning wire form",
            provider.provider_kind
        )));
    }
    if !capability.allows(selection.mode()) {
        return Err(ApiError::BadRequest(format!(
            "extraParametersJson.{REASONING_MODE_FIELD} mode `{}` is not confirmed for provider `{}` model `{}`; confirm the exact model's capability in its catalog metadata",
            selection.mode().as_str(),
            provider.provider_kind,
            model.model_name
        )));
    }
    let wire = capability.wire().ok_or_else(|| {
        ApiError::BadRequest(format!(
            "extraParametersJson.{REASONING_MODE_FIELD} has no confirmed wire control for provider `{}` model `{}`",
            provider.provider_kind, model.model_name
        ))
    })?;
    if let Some(conflict) = wire.raw_conflict(extra_parameters_json) {
        return Err(ApiError::BadRequest(format!(
            "extraParametersJson.{REASONING_MODE_FIELD} {conflict}"
        )));
    }
    Ok(())
}

/// The write-path target a binding points at: the account, model and request
/// policy that the canonical purpose-eligibility gate must validate within the
/// binding's own scope.
struct BindingScopeTarget<'a> {
    scope: AiScopeRef,
    binding_purpose: AiBindingPurpose,
    account_id: Uuid,
    model_catalog_id: Uuid,
    extra_parameters_json: &'a serde_json::Value,
}

impl AiCatalogService {
    pub async fn list_bindings(
        &self,
        state: &AppState,
        scope: AiScopeRef,
    ) -> Result<Vec<AiBinding>, ApiError> {
        let rows = ai_repository::list_bindings_exact(
            &state.persistence.postgres,
            scope_kind_key(scope.scope_kind),
            scope.workspace_id,
            scope.library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        rows.into_iter().map(map_binding_row).collect()
    }

    pub async fn get_binding(
        &self,
        state: &AppState,
        binding_id: Uuid,
    ) -> Result<AiBinding, ApiError> {
        let row = ai_repository::get_binding_by_id(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("binding_assignment", binding_id))?;
        map_binding_row(row)
    }

    pub async fn create_binding(
        &self,
        state: &AppState,
        command: CreateAiBindingCommand,
    ) -> Result<AiBinding, ApiError> {
        validate_binding_request_policy(&command.extra_parameters_json)?;
        let scope = normalize_scope_ref(
            state,
            command.scope_kind,
            command.workspace_id,
            command.library_id,
        )
        .await?;
        // An omitted create-time state keeps the historical `active` default.
        // Both decision purposes are served now (query rerank since IRG-187,
        // graph entity typing since IRG-185), so no purpose fails closed here.
        let binding_state =
            validated_binding_state(command.binding_state.as_deref().unwrap_or("active"))?;
        let provider_catalog_id = self
            .validate_binding_target_for_scope(
                state,
                BindingScopeTarget {
                    scope,
                    binding_purpose: command.binding_purpose,
                    account_id: command.account_id,
                    model_catalog_id: command.model_catalog_id,
                    extra_parameters_json: &command.extra_parameters_json,
                },
            )
            .await?;
        let repository_input = ai_repository::CreateAiBindingInput {
            scope_kind: scope_kind_key(scope.scope_kind),
            workspace_id: scope.workspace_id,
            library_id: scope.library_id,
            binding_purpose: binding_purpose_key(command.binding_purpose),
            account_id: command.account_id,
            provider_catalog_id,
            model_catalog_id: command.model_catalog_id,
            system_prompt: command.system_prompt.as_deref(),
            temperature: command.temperature,
            top_p: command.top_p,
            max_output_tokens_override: command.max_output_tokens_override,
            extra_parameters_json: command.extra_parameters_json.clone(),
            binding_state,
            updated_by_principal_id: command.updated_by_principal_id,
        };
        let row = ai_repository::create_binding(&state.persistence.postgres, repository_input)
            .await
            .map_err(map_ai_write_error)?;
        invalidate_vector_dimension_cache_for_binding(command.binding_purpose);
        map_binding_row(row)
    }

    pub async fn update_binding(
        &self,
        state: &AppState,
        command: UpdateAiBindingCommand,
    ) -> Result<AiBinding, ApiError> {
        let existing =
            ai_repository::get_binding_by_id(&state.persistence.postgres, command.binding_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| {
                    ApiError::resource_not_found("binding_assignment", command.binding_id)
                })?;
        let scope = scope_ref_from_binding_row(&existing)?;
        let binding_purpose = parse_binding_purpose(&existing.binding_purpose)?;
        validated_binding_state(&command.binding_state)?;
        validate_binding_request_policy(&command.extra_parameters_json)?;
        let provider_catalog_id = self
            .validate_binding_target_for_scope(
                state,
                BindingScopeTarget {
                    scope,
                    binding_purpose,
                    account_id: command.account_id,
                    model_catalog_id: command.model_catalog_id,
                    extra_parameters_json: &command.extra_parameters_json,
                },
            )
            .await?;
        let row = ai_repository::update_binding(
            &state.persistence.postgres,
            ai_repository::UpdateAiBindingInput {
                binding_id: command.binding_id,
                account_id: command.account_id,
                provider_catalog_id,
                model_catalog_id: command.model_catalog_id,
                system_prompt: command.system_prompt.as_deref(),
                temperature: command.temperature,
                top_p: command.top_p,
                max_output_tokens_override: command.max_output_tokens_override,
                extra_parameters_json: command.extra_parameters_json.clone(),
                binding_state: &command.binding_state,
                updated_by_principal_id: command.updated_by_principal_id,
            },
        )
        .await
        .map_err(map_ai_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("binding_assignment", command.binding_id))?;
        invalidate_vector_dimension_cache_for_binding(binding_purpose);
        map_binding_row(row)
    }

    pub async fn delete_binding(&self, state: &AppState, binding_id: Uuid) -> Result<(), ApiError> {
        let existing = ai_repository::get_binding_by_id(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("binding_assignment", binding_id))?;
        let binding_purpose = parse_binding_purpose(&existing.binding_purpose)?;
        let deleted = ai_repository::delete_binding(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if !deleted {
            return Err(ApiError::resource_not_found("binding_assignment", binding_id));
        }
        invalidate_vector_dimension_cache_for_binding(binding_purpose);
        Ok(())
    }
    pub async fn validate_binding(
        &self,
        state: &AppState,
        command: CreateBindingValidationCommand,
    ) -> Result<BindingValidation, ApiError> {
        let row = ai_repository::create_binding_validation(
            &state.persistence.postgres,
            command.binding_id,
            &command.validation_state,
            normalize_optional(command.failure_code.as_deref()).as_deref(),
            normalize_optional(command.message.as_deref()).as_deref(),
        )
        .await
        .map_err(map_ai_write_error)?;
        Ok(map_binding_validation_row(row))
    }

    pub async fn get_binding_validation(
        &self,
        state: &AppState,
        validation_id: Uuid,
    ) -> Result<BindingValidation, ApiError> {
        let row =
            ai_repository::get_binding_validation_by_id(&state.persistence.postgres, validation_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("binding_validation", validation_id))?;
        Ok(map_binding_validation_row(row))
    }

    pub async fn list_binding_validations(
        &self,
        state: &AppState,
        binding_id: Uuid,
    ) -> Result<Vec<BindingValidation>, ApiError> {
        let rows = ai_repository::list_binding_validations(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_binding_validation_row).collect())
    }
    async fn validate_binding_target_for_scope(
        &self,
        state: &AppState,
        target: BindingScopeTarget<'_>,
    ) -> Result<Uuid, ApiError> {
        let BindingScopeTarget {
            scope,
            binding_purpose,
            account_id,
            model_catalog_id,
            extra_parameters_json,
        } = target;
        let account = self.get_account(state, account_id).await?;
        validate_binding_account_scope(scope, &account)?;

        let (provider, model) = tokio::try_join!(
            self.get_provider_catalog(state, account.provider_catalog_id),
            self.get_model_catalog(state, model_catalog_id),
        )?;
        validate_binding_write_components(
            binding_purpose,
            extra_parameters_json,
            &account,
            &model,
            &provider,
        )?;
        Ok(provider.id)
    }
}
