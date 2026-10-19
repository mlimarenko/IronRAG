use super::*;

pub(super) fn map_workspace(workspace: CatalogWorkspace) -> CatalogWorkspaceResponse {
    CatalogWorkspaceResponse {
        id: workspace.id,
        slug: workspace.slug,
        display_name: workspace.display_name,
        lifecycle_state: lifecycle_state_label(&workspace.lifecycle_state).to_string(),
    }
}

pub(super) fn map_library(library: CatalogLibrary) -> CatalogLibraryResponse {
    CatalogLibraryResponse {
        id: library.id,
        workspace_id: library.workspace_id,
        slug: library.slug,
        display_name: library.display_name,
        description: library.description,
        extraction_prompt: library.extraction_prompt,
        web_ingest_policy: library.web_ingest_policy,
        recognition_policy: library.recognition_policy,
        retrieval_config: library.retrieval_config,
        lifecycle_state: lifecycle_state_label(&library.lifecycle_state).to_string(),
        include_document_hint_in_mcp_answers: library.include_document_hint_in_mcp_answers,
        ingestion_readiness: CatalogLibraryIngestionReadinessResponse {
            ready: library.ingestion_readiness.ready,
            missing_binding_purposes: library.ingestion_readiness.missing_binding_purposes,
        },
        runtime_readiness: CatalogLibraryRuntimeReadinessResponse {
            missing_binding_purposes: library.runtime_readiness.missing_binding_purposes,
        },
    }
}

pub(super) fn parse_lifecycle_state_input(value: &str) -> Result<CatalogLifecycleState, ApiError> {
    match value {
        "active" => Ok(CatalogLifecycleState::Active),
        "archived" => Ok(CatalogLifecycleState::Archived),
        other => Err(ApiError::BadRequest(format!("invalid lifecycle state: {other}"))),
    }
}

pub(super) const fn lifecycle_state_label(value: &CatalogLifecycleState) -> &'static str {
    match value {
        CatalogLifecycleState::Active => "active",
        CatalogLifecycleState::Disabled => "disabled",
        CatalogLifecycleState::Archived => "archived",
    }
}
