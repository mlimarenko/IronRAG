use crate::{
    app::state::AppState,
    domains::{ai::AiBindingPurpose, catalog::CatalogLifecycleState},
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
        BootstrapProviderPreset, BootstrapProviderPresetBundle, BootstrapStatus, UiLocale,
    },
    shell::{LibrarySummary, ShellBootstrap, ShellViewer, WorkspaceSummary},
};

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

    Ok(ShellBootstrap {
        viewer: ShellViewer {
            principal_id: auth.principal_id,
            login: user.login,
            display_name: user.display_name,
            access_label: if auth.is_system_admin {
                "Admin".to_string()
            } else {
                "Operator".to_string()
            },
            role: if auth.is_system_admin {
                ironrag_contracts::shell::ShellRole::Admin
            } else {
                ironrag_contracts::shell::ShellRole::Operator
            },
            is_admin: auth.is_system_admin,
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
            .map(|library| LibrarySummary {
                id: library.id,
                workspace_id: library.workspace_id,
                slug: library.slug,
                name: library.display_name,
                description: library.description,
                lifecycle_state: catalog_lifecycle_label(&library.lifecycle_state).to_string(),
                ingestion_ready: library.ingestion_readiness.ready,
                missing_binding_purposes: library
                    .ingestion_readiness
                    .missing_binding_purposes
                    .into_iter()
                    .filter_map(map_bootstrap_binding_purpose)
                    .collect(),
                query_ready: None,
            })
            .collect(),
        active_library_id,
        workspace_memberships: Vec::new(),
        effective_grants: Vec::new(),
        capabilities: Vec::new(),
        warnings: Vec::new(),
    })
}

pub(crate) fn parse_ui_locale(locale: &str) -> UiLocale {
    match locale.trim().to_ascii_lowercase().as_str() {
        "ru" => UiLocale::Ru,
        _ => UiLocale::En,
    }
}

pub(crate) fn to_bootstrap_contract(value: &BootstrapStatusOutcome) -> BootstrapStatus {
    let ai_setup = value.ai_setup.as_ref().map(|descriptor| BootstrapAiSetup {
        preset_bundles: descriptor
            .preset_bundles
            .iter()
            .map(|bundle| BootstrapProviderPresetBundle {
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
                presets: bundle
                    .presets
                    .iter()
                    .filter_map(|preset| {
                        map_bootstrap_binding_purpose(preset.binding_purpose).map(
                            |binding_purpose| BootstrapProviderPreset {
                                binding_purpose,
                                model_catalog_id: preset.model_catalog_id,
                                model_name: preset.model_name.clone(),
                                preset_name: preset.preset_name.clone(),
                                system_prompt: preset.system_prompt.clone(),
                                temperature: preset.temperature,
                                top_p: preset.top_p,
                                max_output_tokens_override: preset.max_output_tokens_override,
                            },
                        )
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

const fn map_bootstrap_binding_purpose(value: AiBindingPurpose) -> Option<BootstrapBindingPurpose> {
    match value {
        AiBindingPurpose::ExtractGraph => Some(BootstrapBindingPurpose::ExtractGraph),
        AiBindingPurpose::EmbedChunk => Some(BootstrapBindingPurpose::EmbedChunk),
        AiBindingPurpose::QueryAnswer | AiBindingPurpose::QueryRetrieve => {
            Some(BootstrapBindingPurpose::QueryAnswer)
        }
        AiBindingPurpose::Vision => Some(BootstrapBindingPurpose::Vision),
        AiBindingPurpose::ExtractText => None,
    }
}
