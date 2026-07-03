use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        catalog::CatalogLifecycleState,
        provider_profiles::{
            ProviderAuthScheme, ProviderBaseUrlMode, ProviderBaseUrlPolicy, ProviderCapabilities,
            ProviderCapabilityState, ProviderCredentialPolicy, ProviderCredentialValidationMode,
            ProviderModelDiscovery, ProviderModelDiscoveryMode, ProviderRuntimeProfile,
            ProviderStructuredOutputMode, ProviderTokenLimitParameter,
        },
    },
    infra::repositories::iam_repository,
    interfaces::http::{
        auth::AuthContext,
        authorization::{authorize_library_discovery, authorize_workspace_discovery},
        router_support::ApiError,
    },
    services::iam::service::BootstrapStatusOutcome,
};
use ironrag_contracts::{
    auth::{
        BootstrapAiSetup, BootstrapBindingPurpose, BootstrapCredentialSource,
        BootstrapProviderBinding, BootstrapProviderBindingBundle, BootstrapStatus, UiLocale,
    },
    provider::{
        ProviderAuthScheme as ContractProviderAuthScheme,
        ProviderBaseUrlMode as ContractProviderBaseUrlMode,
        ProviderBaseUrlPolicy as ContractProviderBaseUrlPolicy,
        ProviderCapabilities as ContractProviderCapabilities,
        ProviderCapabilityState as ContractProviderCapabilityState,
        ProviderCredentialPolicy as ContractProviderCredentialPolicy,
        ProviderCredentialValidationMode as ContractProviderCredentialValidationMode,
        ProviderModelDiscovery as ContractProviderModelDiscovery,
        ProviderModelDiscoveryMode as ContractProviderModelDiscoveryMode,
        ProviderModelDiscoveryPath as ContractProviderModelDiscoveryPath,
        ProviderRuntimeProfile as ContractProviderRuntimeProfile,
        ProviderStructuredOutputMode as ContractProviderStructuredOutputMode,
        ProviderTokenLimitParameter as ContractProviderTokenLimitParameter,
    },
    shell::{LibrarySummary, ShellBootstrap, ShellRole, ShellViewer, WorkspaceSummary},
};

use crate::infra::repositories::iam_repository::SystemRole;

pub(crate) async fn build_shell_bootstrap(
    state: &AppState,
    auth: &AuthContext,
) -> Result<ShellBootstrap, ApiError> {
    let workspaces = state
        .canonical_services
        .catalog
        .list_workspaces(state, None)
        .await?
        .into_iter()
        .filter(|workspace| authorize_workspace_discovery(auth, workspace.id).is_ok())
        .collect::<Vec<_>>();

    let active_workspace_id = auth
        .workspace_id
        .filter(|workspace_id| workspaces.iter().any(|workspace| workspace.id == *workspace_id))
        .or_else(|| workspaces.first().map(|workspace| workspace.id));

    // Load libraries from ALL visible workspaces so the UI can switch freely
    let mut libraries = Vec::new();
    for workspace in &workspaces {
        let ws_libs = state
            .canonical_services
            .catalog
            .list_libraries(state, workspace.id)
            .await?
            .into_iter()
            .filter(|library| {
                authorize_library_discovery(auth, library.workspace_id, library.id).is_ok()
            })
            .collect::<Vec<_>>();
        libraries.extend(ws_libs);
    }
    let active_library_id = libraries.first().map(|library| library.id);

    let user =
        iam_repository::get_user_by_principal_id(&state.persistence.postgres, auth.principal_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or(ApiError::Unauthorized)?;

    // The shell role is the canonical system role stored on the user (it gates
    // the UI capability matrix). `is_admin` mirrors the auth context's
    // system-admin flag, which is grant-derived and remains the server-side
    // authorization boundary.
    let role = map_shell_role(user.system_role());
    let is_admin = matches!(role, ShellRole::Admin);

    Ok(ShellBootstrap {
        viewer: ShellViewer {
            principal_id: auth.principal_id,
            login: user.login,
            display_name: user.display_name,
            access_label: shell_role_access_label(role).to_string(),
            role,
            is_admin,
        },
        locale: parse_ui_locale(&state.ui_runtime.default_locale),
        workspaces: workspaces
            .into_iter()
            .map(|workspace| WorkspaceSummary {
                id: workspace.id,
                slug: workspace.slug,
                name: workspace.display_name,
                lifecycle_state: catalog_lifecycle_label(&workspace.lifecycle_state).to_string(),
            })
            .collect(),
        active_workspace_id,
        libraries: libraries
            .into_iter()
            .map(|library| {
                let ingestion_ready = library.ingestion_readiness.ready;
                let query_ready =
                    shell_library_query_ready(&library.runtime_readiness.missing_binding_purposes);
                let missing_binding_purposes = library
                    .runtime_readiness
                    .missing_binding_purposes
                    .into_iter()
                    .map(map_bootstrap_binding_purpose)
                    .collect();
                LibrarySummary {
                    id: library.id,
                    workspace_id: library.workspace_id,
                    slug: library.slug,
                    name: library.display_name,
                    description: library.description,
                    lifecycle_state: catalog_lifecycle_label(&library.lifecycle_state).to_string(),
                    ingestion_ready,
                    missing_binding_purposes,
                    query_ready: Some(query_ready),
                }
            })
            .collect(),
        active_library_id,
        workspace_memberships: Vec::new(),
        effective_grants: Vec::new(),
        capabilities: Vec::new(),
        warnings: Vec::new(),
    })
}

const fn map_shell_role(role: SystemRole) -> ShellRole {
    match role {
        SystemRole::Viewer => ShellRole::Viewer,
        SystemRole::Operator => ShellRole::Operator,
        SystemRole::Admin => ShellRole::Admin,
    }
}

const fn shell_role_access_label(role: ShellRole) -> &'static str {
    match role {
        ShellRole::Admin => "Admin",
        ShellRole::Operator => "Operator",
        ShellRole::Viewer => "Viewer",
    }
}

pub(crate) fn parse_ui_locale(locale: &str) -> UiLocale {
    match locale.trim().to_ascii_lowercase().as_str() {
        "ru" => UiLocale::Ru,
        _ => UiLocale::En,
    }
}

fn map_contract_provider_auth_scheme(value: ProviderAuthScheme) -> ContractProviderAuthScheme {
    match value {
        ProviderAuthScheme::Bearer => ContractProviderAuthScheme::Bearer,
        ProviderAuthScheme::RawAuthorization => ContractProviderAuthScheme::RawAuthorization,
    }
}

fn map_contract_provider_token_limit_parameter(
    value: ProviderTokenLimitParameter,
) -> ContractProviderTokenLimitParameter {
    match value {
        ProviderTokenLimitParameter::MaxCompletionTokens => {
            ContractProviderTokenLimitParameter::MaxCompletionTokens
        }
        ProviderTokenLimitParameter::MaxTokens => ContractProviderTokenLimitParameter::MaxTokens,
    }
}

fn map_contract_provider_structured_output_mode(
    value: ProviderStructuredOutputMode,
) -> ContractProviderStructuredOutputMode {
    match value {
        ProviderStructuredOutputMode::JsonSchema => {
            ContractProviderStructuredOutputMode::JsonSchema
        }
        ProviderStructuredOutputMode::JsonObject => {
            ContractProviderStructuredOutputMode::JsonObject
        }
        ProviderStructuredOutputMode::PromptOnlyJsonObject => {
            ContractProviderStructuredOutputMode::PromptOnlyJsonObject
        }
        ProviderStructuredOutputMode::Unsupported => {
            ContractProviderStructuredOutputMode::Unsupported
        }
    }
}

fn map_contract_provider_runtime_profile(
    value: &ProviderRuntimeProfile,
) -> ContractProviderRuntimeProfile {
    ContractProviderRuntimeProfile {
        kind: value.kind.clone(),
        auth_scheme: map_contract_provider_auth_scheme(value.auth_scheme),
        token_limit_parameter: map_contract_provider_token_limit_parameter(
            value.token_limit_parameter,
        ),
        structured_output: map_contract_provider_structured_output_mode(value.structured_output),
        chat_path: value.chat_path.clone(),
        embeddings_path: value.embeddings_path.clone(),
        models_path: value.models_path.clone(),
    }
}

fn map_contract_provider_base_url_mode(value: ProviderBaseUrlMode) -> ContractProviderBaseUrlMode {
    match value {
        ProviderBaseUrlMode::Fixed => ContractProviderBaseUrlMode::Fixed,
        ProviderBaseUrlMode::Required => ContractProviderBaseUrlMode::Required,
        ProviderBaseUrlMode::Optional => ContractProviderBaseUrlMode::Optional,
    }
}

fn map_contract_provider_credential_validation_mode(
    value: ProviderCredentialValidationMode,
) -> ContractProviderCredentialValidationMode {
    match value {
        ProviderCredentialValidationMode::ChatRoundTrip => {
            ContractProviderCredentialValidationMode::ChatRoundTrip
        }
        ProviderCredentialValidationMode::ModelList => {
            ContractProviderCredentialValidationMode::ModelList
        }
        ProviderCredentialValidationMode::None => ContractProviderCredentialValidationMode::None,
    }
}

fn map_contract_provider_credential_policy(
    value: &ProviderCredentialPolicy,
) -> ContractProviderCredentialPolicy {
    ContractProviderCredentialPolicy {
        api_key_required: value.api_key_required,
        base_url_required: value.base_url_required,
        base_url_mode: map_contract_provider_base_url_mode(value.base_url_mode),
        validation_mode: map_contract_provider_credential_validation_mode(value.validation_mode),
    }
}

fn map_contract_provider_base_url_policy(
    value: &ProviderBaseUrlPolicy,
) -> ContractProviderBaseUrlPolicy {
    ContractProviderBaseUrlPolicy {
        allow_override: value.allow_override,
        require_https: value.require_https,
        allow_private_network: value.allow_private_network,
        trim_suffixes: value.trim_suffixes.clone(),
    }
}

fn map_contract_provider_model_discovery_mode(
    value: ProviderModelDiscoveryMode,
) -> ContractProviderModelDiscoveryMode {
    match value {
        ProviderModelDiscoveryMode::Shared => ContractProviderModelDiscoveryMode::Shared,
        ProviderModelDiscoveryMode::Credential => ContractProviderModelDiscoveryMode::Credential,
        ProviderModelDiscoveryMode::Unsupported => ContractProviderModelDiscoveryMode::Unsupported,
    }
}

fn map_contract_provider_model_discovery(
    value: &ProviderModelDiscovery,
) -> ContractProviderModelDiscovery {
    ContractProviderModelDiscovery {
        mode: map_contract_provider_model_discovery_mode(value.mode),
        paths: value
            .paths
            .iter()
            .map(|path| ContractProviderModelDiscoveryPath {
                capability_kind: path.capability_kind.clone(),
                path: path.path.clone(),
            })
            .collect(),
    }
}

fn map_contract_provider_capability_state(
    value: ProviderCapabilityState,
) -> ContractProviderCapabilityState {
    match value {
        ProviderCapabilityState::Supported => ContractProviderCapabilityState::Supported,
        ProviderCapabilityState::Unsupported => ContractProviderCapabilityState::Unsupported,
        ProviderCapabilityState::Unknown => ContractProviderCapabilityState::Unknown,
    }
}

fn map_contract_provider_capabilities(
    value: &ProviderCapabilities,
) -> ContractProviderCapabilities {
    ContractProviderCapabilities {
        chat: map_contract_provider_capability_state(value.chat),
        embeddings: map_contract_provider_capability_state(value.embeddings),
        vision: map_contract_provider_capability_state(value.vision),
        streaming: map_contract_provider_capability_state(value.streaming),
        tools: map_contract_provider_capability_state(value.tools),
        model_discovery: map_contract_provider_capability_state(value.model_discovery),
    }
}

pub(crate) fn to_bootstrap_contract(value: &BootstrapStatusOutcome) -> BootstrapStatus {
    let ai_setup = value.ai_setup.as_ref().map(|descriptor| BootstrapAiSetup {
        binding_bundles: descriptor
            .binding_bundles
            .iter()
            .map(|bundle| BootstrapProviderBindingBundle {
                provider_catalog_id: bundle.provider_catalog_id,
                provider_kind: bundle.provider_kind.clone(),
                display_name: bundle.display_name.clone(),
                credential_source: match bundle.credential_source {
                    crate::services::ai_catalog_service::BootstrapAiCredentialSource::Missing => {
                        BootstrapCredentialSource::Missing
                    }
                    crate::services::ai_catalog_service::BootstrapAiCredentialSource::Env => {
                        BootstrapCredentialSource::Env
                    }
                },
                default_base_url: bundle.default_base_url.clone(),
                api_key_required: bundle.api_key_required,
                base_url_required: bundle.base_url_required,
                credential_policy: map_contract_provider_credential_policy(
                    &bundle.credential_policy,
                ),
                base_url_policy: map_contract_provider_base_url_policy(&bundle.base_url_policy),
                model_discovery: map_contract_provider_model_discovery(&bundle.model_discovery),
                capabilities: map_contract_provider_capabilities(&bundle.capabilities),
                runtime: map_contract_provider_runtime_profile(&bundle.runtime),
                ui_hints: bundle.ui_hints.clone(),
                bindings: bundle
                    .bindings
                    .iter()
                    .map(|binding| BootstrapProviderBinding {
                        binding_purpose: map_bootstrap_binding_purpose(binding.binding_purpose),
                        model_catalog_id: binding.model_catalog_id,
                        model_name: binding.model_name.clone(),
                        system_prompt: binding.system_prompt.clone(),
                        temperature: binding.temperature,
                        top_p: binding.top_p,
                        max_output_tokens_override: binding.max_output_tokens_override,
                    })
                    .collect(),
            })
            .collect(),
    });
    BootstrapStatus { setup_required: value.setup_required, ai_setup }
}

const fn catalog_lifecycle_label(value: &CatalogLifecycleState) -> &'static str {
    match value {
        CatalogLifecycleState::Active => "active",
        CatalogLifecycleState::Disabled => "disabled",
        CatalogLifecycleState::Archived => "archived",
    }
}

fn shell_library_query_ready(missing: &[AiBindingPurpose]) -> bool {
    !missing.iter().any(|purpose| {
        matches!(
            purpose,
            AiBindingPurpose::QueryRetrieve
                | AiBindingPurpose::QueryCompile
                | AiBindingPurpose::QueryAnswer
        )
    })
}

fn map_bootstrap_binding_purpose(value: AiBindingPurpose) -> BootstrapBindingPurpose {
    match value {
        AiBindingPurpose::ExtractText => BootstrapBindingPurpose::ExtractText,
        AiBindingPurpose::ExtractGraph => BootstrapBindingPurpose::ExtractGraph,
        AiBindingPurpose::EmbedChunk => BootstrapBindingPurpose::EmbedChunk,
        AiBindingPurpose::QueryCompile => BootstrapBindingPurpose::QueryCompile,
        AiBindingPurpose::QueryRetrieve => BootstrapBindingPurpose::QueryRetrieve,
        AiBindingPurpose::QueryAnswer => BootstrapBindingPurpose::QueryAnswer,
        AiBindingPurpose::Vision => BootstrapBindingPurpose::Vision,
        AiBindingPurpose::Agent => BootstrapBindingPurpose::Agent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_binding_purpose_mapping_preserves_provider_router_vocabulary() {
        assert_eq!(
            map_bootstrap_binding_purpose(AiBindingPurpose::ExtractText),
            BootstrapBindingPurpose::ExtractText,
        );
        assert_eq!(
            map_bootstrap_binding_purpose(AiBindingPurpose::QueryRetrieve),
            BootstrapBindingPurpose::QueryRetrieve,
        );
        assert_eq!(
            map_bootstrap_binding_purpose(AiBindingPurpose::QueryAnswer),
            BootstrapBindingPurpose::QueryAnswer,
        );
    }

    #[test]
    fn shell_query_readiness_only_blocks_on_query_purposes() {
        assert!(shell_library_query_ready(&[AiBindingPurpose::ExtractText]));
        assert!(!shell_library_query_ready(&[AiBindingPurpose::QueryCompile]));
        assert!(!shell_library_query_ready(&[AiBindingPurpose::QueryAnswer]));
        assert!(!shell_library_query_ready(&[AiBindingPurpose::QueryRetrieve]));
        assert!(shell_library_query_ready(&[]));
    }
}
