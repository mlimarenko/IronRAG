use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::ProjectRow,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_MCP_MEMORY_READ, POLICY_MCP_MEMORY_WRITE, load_project_and_authorize,
        },
        router_support::ApiError,
    },
};

#[derive(Debug, Clone, Copy)]
pub struct RuntimeLibraryCapabilityHints {
    pub supports_search: bool,
    pub supports_read: bool,
    pub supports_write: bool,
}

#[must_use]
pub fn normalize_runtime_library_label(project: &ProjectRow) -> String {
    let trimmed = project.name.trim();
    if trimmed.is_empty() {
        return project.slug.clone();
    }
    trimmed.to_string()
}

#[must_use]
pub fn derive_runtime_library_capability_hints(
    auth: &AuthContext,
    project: &ProjectRow,
) -> RuntimeLibraryCapabilityHints {
    RuntimeLibraryCapabilityHints {
        supports_search: auth.can_access_workspace(project.workspace_id)
            && auth.has_any_scope(POLICY_MCP_MEMORY_READ),
        supports_read: auth.can_access_workspace(project.workspace_id)
            && auth.has_any_scope(POLICY_MCP_MEMORY_READ),
        supports_write: auth.can_access_workspace(project.workspace_id)
            && !auth.is_read_only_for_library(project.workspace_id, POLICY_MCP_MEMORY_WRITE),
    }
}

pub async fn load_library_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    library_id: Uuid,
    accepted_scopes: &[&str],
) -> Result<ProjectRow, ApiError> {
    load_project_and_authorize(auth, state, library_id, accepted_scopes).await
}
