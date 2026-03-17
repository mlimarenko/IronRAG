use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{self, DocumentRow, IngestionJobRow, ProjectRow},
    interfaces::http::{auth::AuthContext, router_support::ApiError},
};

pub const SCOPE_WORKSPACE_ADMIN: &str = "workspace:admin";
pub const SCOPE_PROJECTS_WRITE: &str = "projects:write";
pub const SCOPE_PROVIDERS_ADMIN: &str = "providers:admin";
pub const SCOPE_DOCUMENTS_READ: &str = "documents:read";
pub const SCOPE_DOCUMENTS_WRITE: &str = "documents:write";
pub const SCOPE_GRAPH_READ: &str = "graph:read";
pub const SCOPE_QUERY_READ: &str = "query:read";
pub const SCOPE_QUERY_WRITE: &str = "query:write";
pub const SCOPE_QUERY_RUN: &str = "query:run";
pub const SCOPE_USAGE_READ: &str = "usage:read";

pub const POLICY_WORKSPACE_ADMIN: &[&str] = &[SCOPE_WORKSPACE_ADMIN];
pub const POLICY_PROJECTS_WRITE: &[&str] = &[SCOPE_PROJECTS_WRITE, SCOPE_WORKSPACE_ADMIN];
pub const POLICY_PROVIDERS_ADMIN: &[&str] = &[SCOPE_PROVIDERS_ADMIN, SCOPE_WORKSPACE_ADMIN];
pub const POLICY_DOCUMENTS_READ: &[&str] = &[SCOPE_DOCUMENTS_READ, SCOPE_WORKSPACE_ADMIN];
pub const POLICY_DOCUMENTS_WRITE: &[&str] = &[SCOPE_DOCUMENTS_WRITE, SCOPE_WORKSPACE_ADMIN];
pub const POLICY_GRAPH_READ: &[&str] =
    &[SCOPE_GRAPH_READ, SCOPE_DOCUMENTS_READ, SCOPE_WORKSPACE_ADMIN];
pub const POLICY_QUERY_READ: &[&str] = &[
    SCOPE_QUERY_READ,
    SCOPE_QUERY_WRITE,
    SCOPE_QUERY_RUN,
    SCOPE_GRAPH_READ,
    SCOPE_DOCUMENTS_READ,
    SCOPE_WORKSPACE_ADMIN,
];
pub const POLICY_QUERY_RUN: &[&str] = &[SCOPE_QUERY_WRITE, SCOPE_QUERY_RUN, SCOPE_WORKSPACE_ADMIN];
pub const POLICY_USAGE_READ: &[&str] = &[SCOPE_USAGE_READ, SCOPE_WORKSPACE_ADMIN];

pub async fn authorize_workspace_scope(
    auth: &AuthContext,
    workspace_id: Uuid,
    accepted_scopes: &[&str],
) -> Result<(), ApiError> {
    auth.require_any_scope(accepted_scopes)?;
    auth.require_workspace_access(workspace_id)
}

pub async fn load_project_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    project_id: Uuid,
    accepted_scopes: &[&str],
) -> Result<ProjectRow, ApiError> {
    auth.require_any_scope(accepted_scopes)?;

    let project = repositories::get_project_by_id(&state.persistence.postgres, project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {project_id} not found")))?;

    auth.require_workspace_access(project.workspace_id)?;
    Ok(project)
}

pub async fn load_document_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
    accepted_scopes: &[&str],
) -> Result<(DocumentRow, ProjectRow), ApiError> {
    auth.require_any_scope(accepted_scopes)?;

    let document = repositories::get_document_by_id(&state.persistence.postgres, document_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("document {document_id} not found")))?;

    let project = repositories::get_project_by_id(&state.persistence.postgres, document.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {} not found", document.project_id)))?;

    auth.require_workspace_access(project.workspace_id)?;
    Ok((document, project))
}

pub async fn load_ingestion_job_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    ingestion_job_id: Uuid,
    accepted_scopes: &[&str],
) -> Result<(IngestionJobRow, ProjectRow), ApiError> {
    auth.require_any_scope(accepted_scopes)?;

    let row = repositories::get_ingestion_job_by_id(&state.persistence.postgres, ingestion_job_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("ingestion_job {ingestion_job_id} not found")))?;

    let project = repositories::get_project_by_id(&state.persistence.postgres, row.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("project {} not found", row.project_id)))?;

    auth.require_workspace_access(project.workspace_id)?;
    Ok((row, project))
}

pub fn filter_workspace_rows<T>(
    auth: &AuthContext,
    items: Vec<T>,
    workspace_id_of: impl Fn(&T) -> Option<Uuid>,
) -> Vec<T> {
    if auth.token_kind == "instance_admin" {
        return items;
    }

    let Some(workspace_id) = auth.workspace_id else {
        return Vec::new();
    };

    items.into_iter().filter(|item| workspace_id_of(item) == Some(workspace_id)).collect()
}
