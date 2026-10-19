use super::*;

pub(super) async fn ensure_bootstrap_provider_account(
    service: &AiCatalogService,
    state: &AppState,
    provider: &ProviderCatalogEntry,
    credential_input: Option<BootstrapAiCredentialInput>,
    existing_accounts: &[AiAccount],
    updated_by_principal_id: Option<Uuid>,
) -> Result<AiAccount, ApiError> {
    let primary_label = format!("Bootstrap {}", provider.display_name);
    let provider_accounts = bootstrap_accounts_for_provider(existing_accounts, provider.id);
    let matched_account = bootstrap_resolve_account(&primary_label, &provider_accounts);
    let api_key =
        credential_input.as_ref().and_then(|input| normalize_optional(input.api_key.as_deref()));
    let base_url =
        credential_input.as_ref().and_then(|input| normalize_optional(input.base_url.as_deref()));
    if api_key.is_some() || base_url.is_some() {
        if let Some(existing) = matched_account {
            if bootstrap_account_already_matches(
                provider,
                &existing,
                &primary_label,
                api_key.as_deref(),
                base_url.as_deref(),
            )? {
                return Ok(existing);
            }
            return match service
                .update_account(
                    state,
                    UpdateAiAccountCommand {
                        account_id: existing.id,
                        label: primary_label.clone(),
                        api_key,
                        base_url,
                        credential_state: "active".to_string(),
                        expected_updated_at: existing.updated_at,
                    },
                )
                .await
            {
                Ok(updated) => Ok(updated),
                Err(ApiError::Conflict(_)) => {
                    bootstrap_reload_account(service, state, provider, &primary_label).await
                }
                Err(error) => Err(error),
            };
        }
        return match service
            .create_account(
                state,
                CreateAiAccountCommand {
                    scope_kind: AiScopeKind::Instance,
                    workspace_id: None,
                    library_id: None,
                    provider_catalog_id: provider.id,
                    label: primary_label.clone(),
                    api_key,
                    base_url,
                    created_by_principal_id: updated_by_principal_id,
                },
            )
            .await
        {
            Ok(created) => Ok(created),
            Err(ApiError::Conflict(_)) => {
                bootstrap_reload_account(service, state, provider, &primary_label).await
            }
            Err(error) => Err(error),
        };
    }

    matched_account.ok_or_else(|| {
        let required_field = if provider.api_key_required { "apiKey" } else { "baseUrl" };
        ApiError::BadRequest(format!(
            "bootstrap ai setup requires {required_field} for provider {}",
            provider.provider_kind
        ))
    })
}

/// Whether an existing bootstrap account already holds exactly what
/// [`AiCatalogService::update_account`] would persist, so the caller can return
/// it untouched. Comparing before writing keeps the `ai_account` UPDATE trigger
/// — and the library source-truth bump it drives — from firing on every
/// startup when the configured credential and origin are unchanged.
///
/// The unchanged path intentionally skips [`AiCatalogService::update_account`]'s
/// outbound `validate_provider_access` call: re-probing the stored key against
/// the provider on every start is the churn this avoids. A genuine credential
/// change still takes the update path and is validated there.
fn bootstrap_account_already_matches(
    provider: &ProviderCatalogEntry,
    existing: &AiAccount,
    primary_label: &str,
    configured_api_key: Option<&str>,
    configured_base_url: Option<&str>,
) -> Result<bool, ApiError> {
    if existing.label != primary_label || existing.credential_state != "active" {
        return Ok(false);
    }
    let base_url_matches = provider_credential_base_url_for_update(
        provider,
        existing.base_url.as_deref(),
        configured_base_url,
    )? == existing.base_url;
    if !base_url_matches {
        return Ok(false);
    }
    let credential_matches = match configured_api_key {
        Some(configured) => bootstrap_operations::bootstrap_plaintext_credential_matches(
            existing.api_key.as_deref(),
            configured,
        ),
        None => true,
    };
    Ok(credential_matches)
}

fn bootstrap_accounts_for_provider(
    accounts: &[AiAccount],
    provider_catalog_id: Uuid,
) -> Vec<AiAccount> {
    accounts
        .iter()
        .filter(|account| account.provider_catalog_id == provider_catalog_id)
        .cloned()
        .collect()
}

fn bootstrap_resolve_account(primary_label: &str, accounts: &[AiAccount]) -> Option<AiAccount> {
    accounts
        .iter()
        .find(|account| account.label == primary_label)
        .cloned()
        .or_else(|| (accounts.len() == 1).then(|| accounts[0].clone()))
        .or_else(|| accounts.iter().find(|account| account.credential_state == "active").cloned())
}

pub(super) async fn bootstrap_reload_account(
    service: &AiCatalogService,
    state: &AppState,
    provider: &ProviderCatalogEntry,
    primary_label: &str,
) -> Result<AiAccount, ApiError> {
    let reloaded = service
        .list_accounts_exact(
            state,
            AiScopeRef { scope_kind: AiScopeKind::Instance, workspace_id: None, library_id: None },
        )
        .await?;
    bootstrap_resolve_account(
        primary_label,
        &bootstrap_accounts_for_provider(&reloaded, provider.id),
    )
    .ok_or_else(|| ApiError::Conflict("AI catalog resource already exists".to_string()))
}

fn bootstrap_find_binding(bindings: &[AiBinding], purpose: AiBindingPurpose) -> Option<AiBinding> {
    bindings.iter().find(|binding| binding.binding_purpose == purpose).cloned()
}

/// Build the only bootstrap update that may be applied to an existing binding.
///
/// Replaying the same account/model selection must preserve every operator-owned
/// tuning field. Provider presets are defaults for a new target, not an
/// authority that silently rewrites prompts, sampling, output limits, or
/// embedding parameters on each startup. A deliberate account/model switch
/// adopts the new target's complete preset instead.
pub(super) fn bootstrap_binding_update_command(
    existing: &AiBinding,
    binding_input: &BootstrapAiBindingInput,
    account_id: Uuid,
    updated_by_principal_id: Option<Uuid>,
) -> Option<UpdateAiBindingCommand> {
    let target_changed = existing.account_id != account_id
        || existing.model_catalog_id != binding_input.model_catalog_id;
    if !target_changed && existing.binding_state == "active" {
        return None;
    }

    let (system_prompt, temperature, top_p, max_output_tokens_override, extra_parameters_json) =
        if target_changed {
            (
                binding_input.system_prompt.clone(),
                binding_input.temperature,
                binding_input.top_p,
                binding_input.max_output_tokens_override,
                binding_input.extra_parameters_json.clone(),
            )
        } else {
            (
                existing.system_prompt.clone(),
                existing.temperature,
                existing.top_p,
                existing.max_output_tokens_override,
                existing.extra_parameters_json.clone(),
            )
        };

    Some(UpdateAiBindingCommand {
        binding_id: existing.id,
        account_id,
        model_catalog_id: binding_input.model_catalog_id,
        system_prompt,
        temperature,
        top_p,
        max_output_tokens_override,
        extra_parameters_json,
        binding_state: "active".to_string(),
        updated_by_principal_id,
    })
}

pub(super) async fn ensure_bootstrap_binding(
    service: &AiCatalogService,
    state: &AppState,
    binding_input: &BootstrapAiBindingInput,
    account_id: Uuid,
    bindings: &mut Vec<AiBinding>,
    updated_by_principal_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let existing = bootstrap_find_binding(bindings, binding_input.binding_purpose);
    let operation = if let Some(existing) = existing {
        let Some(command) = bootstrap_binding_update_command(
            &existing,
            binding_input,
            account_id,
            updated_by_principal_id,
        ) else {
            return Ok(());
        };
        service.update_binding(state, command).await
    } else {
        service
            .create_binding(
                state,
                CreateAiBindingCommand {
                    scope_kind: AiScopeKind::Instance,
                    workspace_id: None,
                    library_id: None,
                    binding_purpose: binding_input.binding_purpose,
                    account_id,
                    model_catalog_id: binding_input.model_catalog_id,
                    system_prompt: binding_input.system_prompt.clone(),
                    temperature: binding_input.temperature,
                    top_p: binding_input.top_p,
                    max_output_tokens_override: binding_input.max_output_tokens_override,
                    extra_parameters_json: binding_input.extra_parameters_json.clone(),
                    binding_state: Some("active".to_string()),
                    updated_by_principal_id,
                },
            )
            .await
    };

    match operation {
        Ok(binding) => {
            if let Some(index) = bindings
                .iter()
                .position(|entry| entry.binding_purpose == binding_input.binding_purpose)
            {
                bindings[index] = binding;
            } else {
                bindings.push(binding);
            }
            Ok(())
        }
        Err(ApiError::Conflict(_)) => {
            *bindings = service
                .list_bindings(
                    state,
                    AiScopeRef {
                        scope_kind: AiScopeKind::Instance,
                        workspace_id: None,
                        library_id: None,
                    },
                )
                .await?;
            let existing = bootstrap_find_binding(bindings, binding_input.binding_purpose)
                .ok_or_else(|| {
                    ApiError::Conflict("AI catalog resource already exists".to_string())
                })?;
            let Some(command) = bootstrap_binding_update_command(
                &existing,
                binding_input,
                account_id,
                updated_by_principal_id,
            ) else {
                return Ok(());
            };
            let updated = service.update_binding(state, command).await?;
            if let Some(index) = bindings
                .iter()
                .position(|entry| entry.binding_purpose == binding_input.binding_purpose)
            {
                bindings[index] = updated;
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}
