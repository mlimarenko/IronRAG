use super::*;

fn bootstrap_ai_setup_for_operation(
    state: &AppState,
) -> Option<crate::app::config::UiBootstrapAiSetup> {
    state.ui_bootstrap_ai_setup.clone().or_else(|| state.settings.resolved_ui_bootstrap_ai_setup())
}
pub(super) fn bootstrap_env_credential_needs_sync(
    cipher: &crate::shared::secret_encryption::CredentialCipher,
    account_id: Uuid,
    stored_api_key: Option<&str>,
    configured_api_key: &str,
) -> Result<bool, ApiError> {
    let Some(stored_api_key) = stored_api_key else {
        return Ok(true);
    };
    let stored_api_key = cipher
        .decrypt(SecretPurpose::AiAccountApiKey, account_id, stored_api_key)
        .map_err(ApiError::from_secret_encryption)?;
    let stored_digest = Sha256::digest(stored_api_key.expose_secret().as_bytes());
    let configured_digest = Sha256::digest(configured_api_key.as_bytes());
    Ok(!bool::from(stored_digest.ct_eq(&configured_digest)))
}

/// Whether an already-decrypted account credential equals the configured
/// bootstrap key. The account row is decrypted by `map_account_row`, so unlike
/// [`bootstrap_env_credential_needs_sync`] this compares plaintext. Digests and
/// constant-time equality keep the comparison independent of key length and
/// avoid an early-exit that leaks a matching prefix.
pub(super) fn bootstrap_plaintext_credential_matches(
    stored_plaintext: Option<&str>,
    configured_api_key: &str,
) -> bool {
    let Some(stored_plaintext) = stored_plaintext else {
        return false;
    };
    let stored_digest = Sha256::digest(stored_plaintext.as_bytes());
    let configured_digest = Sha256::digest(configured_api_key.as_bytes());
    bool::from(stored_digest.ct_eq(&configured_digest))
}

#[derive(Debug, Default)]
struct EnvAccountSyncCounts {
    changed: usize,
    created: usize,
    updated: usize,
    skipped: usize,
}

fn record_env_account_sync_result(
    result: Result<AiAccount, ApiError>,
    provider_kind: &str,
    account_scope: Option<&str>,
    is_creation: bool,
    counts: &mut EnvAccountSyncCounts,
) -> Result<bool, ApiError> {
    match result {
        Ok(_) => {
            counts.changed += 1;
            if is_creation {
                counts.created += 1;
            } else {
                counts.updated += 1;
            }
            Ok(false)
        }
        Err(error) if is_provider_credential_validation_error(&error) => {
            counts.skipped += 1;
            tracing::warn!(
                stage = "bootstrap",
                provider_kind,
                account_scope,
                error = %error,
                "skipped env-keyed provider account synchronization",
            );
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn log_env_account_sync_counts(counts: &EnvAccountSyncCounts) {
    log_env_account_creations(counts.created);
    log_env_account_updates(counts.updated);
    log_env_account_skips(counts.skipped);
}

fn log_env_account_creations(created: usize) {
    if created > 0 {
        tracing::info!(stage = "bootstrap", created, "ensured env-keyed provider accounts",);
    }
}

fn log_env_account_updates(updated: usize) {
    if updated > 0 {
        tracing::info!(stage = "bootstrap", updated, "synchronized env-keyed provider accounts",);
    }
}

fn log_env_account_skips(skipped: usize) {
    if skipped > 0 {
        tracing::warn!(
            stage = "bootstrap",
            skipped,
            "some env-keyed provider accounts could not be validated",
        );
    }
}
/// Inputs for ensuring one instance bootstrap account per selected provider.
struct BootstrapAccountContext<'a> {
    state: &'a AppState,
    providers: &'a [ProviderCatalogEntry],
    binding_inputs: &'a [BootstrapAiBindingInput],
    provider_credentials: &'a std::collections::HashMap<String, BootstrapAiCredentialInput>,
    existing_accounts: &'a [AiAccount],
    updated_by_principal_id: Option<Uuid>,
}

/// Inputs for validating that each ensured account can serve its selected
/// binding models through provider model discovery.
struct BootstrapModelValidationContext<'a> {
    providers: &'a [ProviderCatalogEntry],
    accounts_by_provider: &'a std::collections::HashMap<String, AiAccount>,
    binding_inputs: &'a [BootstrapAiBindingInput],
    models: &'a [ModelCatalogEntry],
}

/// Inputs for upserting one instance binding per selected purpose.
struct BootstrapBindingContext<'a> {
    state: &'a AppState,
    providers: &'a [ProviderCatalogEntry],
    models: &'a [ModelCatalogEntry],
    accounts_by_provider: &'a std::collections::HashMap<String, AiAccount>,
    binding_inputs: &'a [BootstrapAiBindingInput],
    updated_by_principal_id: Option<Uuid>,
}

fn log_bootstrap_provider_selections(binding_inputs: &[BootstrapAiBindingInput]) {
    for input in binding_inputs {
        tracing::info!(
            stage = "bootstrap",
            provider_kind = %input.provider_kind,
            "AI provider selected for bootstrap"
        );
    }
}

fn log_bootstrap_bundle_applied(bindings_count: usize) {
    tracing::info!(stage = "bootstrap", bindings_count, "bootstrap bundle applied");
}

/// Run provider model-discovery validation for every ensured account whose
/// provider exposes a model-list credential policy.
async fn validate_bootstrap_account_models(
    context: &BootstrapModelValidationContext<'_>,
) -> Result<(), ApiError> {
    for (provider_kind, account) in context.accounts_by_provider {
        let provider = context
            .providers
            .iter()
            .find(|entry| entry.provider_kind == *provider_kind)
            .ok_or_else(|| {
                ApiError::resource_not_found("provider_catalog", provider_kind.clone())
            })?;
        validate_bootstrap_model_list_binding_inputs(
            provider,
            account,
            context.binding_inputs,
            context.models,
        )
        .await?;
    }
    Ok(())
}

impl AiCatalogService {
    pub async fn describe_bootstrap_ai_setup(
        &self,
        state: &AppState,
    ) -> Result<BootstrapAiSetupDescriptor, ApiError> {
        let (providers, models) = tokio::try_join!(
            self.list_provider_catalog(state),
            self.list_model_catalog(state, None),
        )?;
        // Bootstrap provider credentials are read only while this operation
        // needs them; AppState never retains another plaintext copy.
        let configured_ai = bootstrap_ai_setup_for_operation(state);
        let mut binding_bundles = Vec::new();
        for provider in &providers {
            if let Some(bundle) = resolve_bootstrap_provider_binding_bundle(
                provider,
                &providers,
                &models,
                bootstrap_credential_source(configured_ai.as_ref(), &provider.provider_kind),
            )? {
                if !bootstrap_bundle_is_self_contained(&bundle) {
                    continue;
                }
                binding_bundles.push(bundle);
            }
        }
        binding_bundles.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.provider_kind.cmp(&right.provider_kind))
        });

        Ok(BootstrapAiSetupDescriptor { binding_bundles })
    }

    pub async fn ensure_bootstrap_provider_bundle_available(
        &self,
        state: &AppState,
        provider_kind: &str,
    ) -> Result<(), ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        resolve_bootstrap_provider_bundle(&providers, &models, provider_kind)?;
        Ok(())
    }

    pub async fn apply_bootstrap_provider_binding_bundle(
        &self,
        state: &AppState,
        mut command: ApplyBootstrapProviderBindingBundleCommand,
    ) -> Result<(), ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let bundle =
            resolve_bootstrap_provider_bundle(&providers, &models, &command.provider_kind)?;
        let binding_inputs = bundle
            .bindings
            .into_iter()
            .map(|binding| BootstrapAiBindingInput {
                binding_purpose: binding.binding_purpose,
                provider_kind: binding.owner_provider_kind,
                model_catalog_id: binding.model_catalog_id,
                system_prompt: binding.system_prompt,
                temperature: binding.temperature,
                top_p: binding.top_p,
                max_output_tokens_override: binding.max_output_tokens_override,
                extra_parameters_json: binding.extra_parameters_json,
            })
            .collect();
        self.apply_bootstrap_ai_setup(
            state,
            ApplyBootstrapAiSetupCommand {
                credentials: vec![BootstrapAiCredentialInput {
                    provider_kind: bundle.provider_kind,
                    api_key: command.api_key.take(),
                    base_url: command.base_url.take(),
                }],
                binding_inputs,
                updated_by_principal_id: command.updated_by_principal_id,
            },
        )
        .await
    }

    /// Idempotently ensure every env-keyed provider has a managed
    /// `Bootstrap <DisplayName>` account.
    ///
    /// Runs independent of binding selection and creates one account
    /// per env-keyed provider. Existing env-managed rows in any scope are
    /// synchronized when the configured key changes so restored or copied
    /// bindings cannot retain a stale env-managed credential. Operator-managed
    /// state and base URL remain unchanged. Returns the number of created or
    /// updated rows.
    pub async fn ensure_env_ai_accounts(&self, state: &AppState) -> Result<usize, ApiError> {
        let Some(configured_ai) = bootstrap_ai_setup_for_operation(state) else {
            return Ok(0);
        };
        if configured_ai.provider_secrets.is_empty() {
            return Ok(0);
        }
        let providers = self.list_provider_catalog(state).await?;
        let mut counts = EnvAccountSyncCounts::default();
        for secret in &configured_ai.provider_secrets {
            let Some(provider) = providers.iter().find(|p| p.provider_kind == secret.provider_kind)
            else {
                continue;
            };
            let label = format!("Bootstrap {}", provider.display_name);
            let existing_accounts = ai_repository::list_accounts_by_provider_and_label(
                &state.persistence.postgres,
                provider.id,
                &label,
            )
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
            let has_instance_account =
                existing_accounts.iter().any(|account| account.scope_kind == "instance");
            if !has_instance_account {
                let result = self
                    .create_account(
                        state,
                        CreateAiAccountCommand {
                            scope_kind: AiScopeKind::Instance,
                            workspace_id: None,
                            library_id: None,
                            provider_catalog_id: provider.id,
                            label: label.clone(),
                            api_key: Some(secret.api_key.clone()),
                            base_url: None,
                            created_by_principal_id: None,
                        },
                    )
                    .await;
                let should_skip_provider = record_env_account_sync_result(
                    result,
                    &secret.provider_kind,
                    None,
                    true,
                    &mut counts,
                )?;
                if should_skip_provider {
                    continue;
                }
            }
            for mut account in existing_accounts {
                if !bootstrap_env_credential_needs_sync(
                    &state.credential_cipher,
                    account.id,
                    account.api_key.as_deref(),
                    &secret.api_key,
                )? {
                    continue;
                }
                let result = self
                    .update_account(
                        state,
                        UpdateAiAccountCommand {
                            account_id: account.id,
                            label: label.clone(),
                            api_key: Some(secret.api_key.clone()),
                            base_url: account.base_url.take(),
                            credential_state: std::mem::take(&mut account.credential_state),
                            expected_updated_at: account.updated_at,
                        },
                    )
                    .await;
                record_env_account_sync_result(
                    result,
                    &secret.provider_kind,
                    Some(&account.scope_kind),
                    false,
                    &mut counts,
                )?;
            }
        }
        log_env_account_sync_counts(&counts);
        Ok(counts.changed)
    }

    pub async fn apply_configured_bootstrap_ai_setup(
        &self,
        state: &AppState,
        _workspace_id: Uuid,
        _library_id: Uuid,
        updated_by_principal_id: Option<Uuid>,
    ) -> Result<bool, ApiError> {
        let Some(configured_ai) = bootstrap_ai_setup_for_operation(state) else {
            return Ok(false);
        };
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let binding_inputs =
            resolve_configured_bootstrap_binding_inputs(&configured_ai, &providers, &models)?;
        if binding_inputs.is_empty()
            || !bootstrap_binding_inputs_cover_required_purposes(&binding_inputs)
        {
            return Ok(false);
        }
        self.apply_bootstrap_ai_setup(
            state,
            ApplyBootstrapAiSetupCommand {
                credentials: configured_ai
                    .provider_secrets
                    .iter()
                    .map(|secret| BootstrapAiCredentialInput {
                        provider_kind: secret.provider_kind.clone(),
                        api_key: Some(secret.api_key.clone()),
                        base_url: None,
                    })
                    .collect(),
                binding_inputs,
                updated_by_principal_id,
            },
        )
        .await?;
        Ok(true)
    }

    pub async fn apply_bootstrap_ai_setup(
        &self,
        state: &AppState,
        command: ApplyBootstrapAiSetupCommand,
    ) -> Result<(), ApiError> {
        let providers = self.list_provider_catalog(state).await?;
        let models = self.list_model_catalog(state, None).await?;
        let binding_inputs =
            normalize_bootstrap_binding_inputs(&command.binding_inputs, &providers, &models)?;
        validate_bootstrap_binding_inputs_cover_required_purposes(&binding_inputs)?;
        log_bootstrap_provider_selections(&binding_inputs);

        let instance_scope =
            AiScopeRef { scope_kind: AiScopeKind::Instance, workspace_id: None, library_id: None };
        let existing_accounts = self.list_accounts_exact(state, instance_scope).await?;
        let configured_ai = bootstrap_ai_setup_for_operation(state);
        let provider_credentials =
            bootstrap_provider_credential_map(configured_ai.as_ref(), &command.credentials);
        let accounts_by_provider = self
            .ensure_bootstrap_accounts_by_provider(&BootstrapAccountContext {
                state,
                providers: &providers,
                binding_inputs: &binding_inputs,
                provider_credentials: &provider_credentials,
                existing_accounts: &existing_accounts,
                updated_by_principal_id: command.updated_by_principal_id,
            })
            .await?;

        validate_bootstrap_account_models(&BootstrapModelValidationContext {
            providers: &providers,
            accounts_by_provider: &accounts_by_provider,
            binding_inputs: &binding_inputs,
            models: &models,
        })
        .await?;

        let mut bindings = self.list_bindings(state, instance_scope).await?;
        self.ensure_bootstrap_bindings(
            &BootstrapBindingContext {
                state,
                providers: &providers,
                models: &models,
                accounts_by_provider: &accounts_by_provider,
                binding_inputs: &binding_inputs,
                updated_by_principal_id: command.updated_by_principal_id,
            },
            &mut bindings,
        )
        .await?;

        log_bootstrap_bundle_applied(binding_inputs.len());
        Ok(())
    }

    /// Ensure one instance `Bootstrap <DisplayName>` account per selected
    /// provider, keyed by provider kind. The loop is deterministic and mirrors
    /// the historical order: a repeated provider kind simply re-ensures the same
    /// account, which is idempotent.
    async fn ensure_bootstrap_accounts_by_provider(
        &self,
        context: &BootstrapAccountContext<'_>,
    ) -> Result<std::collections::HashMap<String, AiAccount>, ApiError> {
        let mut accounts_by_provider = std::collections::HashMap::new();
        for provider_kind in
            context.binding_inputs.iter().map(|selection| selection.provider_kind.as_str())
        {
            let provider = context
                .providers
                .iter()
                .find(|entry| entry.provider_kind == provider_kind)
                .ok_or_else(|| {
                    ApiError::resource_not_found("provider_catalog", provider_kind.to_string())
                })?;
            let account = ensure_bootstrap_provider_account(
                self,
                context.state,
                provider,
                context.provider_credentials.get(provider_kind).cloned(),
                context.existing_accounts,
                context.updated_by_principal_id,
            )
            .await?;
            // Clone required: the HashMap outlives the `providers` borrow used later.
            accounts_by_provider.insert(provider.provider_kind.clone(), account);
        }
        Ok(accounts_by_provider)
    }

    /// Upsert one instance binding per selected purpose against the account
    /// already ensured for its provider, refreshing the in-memory binding list
    /// in place so later selections see earlier ones.
    async fn ensure_bootstrap_bindings(
        &self,
        context: &BootstrapBindingContext<'_>,
        bindings: &mut Vec<AiBinding>,
    ) -> Result<(), ApiError> {
        for selection in context.binding_inputs {
            let provider = context
                .providers
                .iter()
                .find(|entry| entry.provider_kind == selection.provider_kind)
                .ok_or_else(|| {
                    ApiError::resource_not_found(
                        "provider_catalog",
                        selection.provider_kind.clone(),
                    )
                })?;
            let model = context
                .models
                .iter()
                .find(|entry| entry.id == selection.model_catalog_id)
                .ok_or_else(|| {
                    ApiError::resource_not_found("model_catalog", selection.model_catalog_id)
                })?;
            validate_model_binding_purpose(selection.binding_purpose, model)?;
            if model.provider_catalog_id != provider.id {
                return Err(ApiError::BadRequest(
                    "bootstrap model selection must belong to the selected provider".to_string(),
                ));
            }
            let account_id = context
                .accounts_by_provider
                .get(&selection.provider_kind)
                .map(|account| account.id)
                .ok_or_else(|| {
                    ApiError::BadRequest("bootstrap account was not created".to_string())
                })?;
            ensure_bootstrap_binding(
                self,
                context.state,
                selection,
                account_id,
                bindings,
                context.updated_by_principal_id,
            )
            .await?;
        }
        Ok(())
    }
}
