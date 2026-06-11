use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{
        ai_repository::{self, AiBindingAssignmentRow, AiProviderCredentialRow},
        catalog_repository::{
            self, CatalogLibraryConnectorRow, CatalogLibraryRow, CatalogWorkspaceRow,
        },
        content_repository,
        ops_repository::{self, OpsAsyncOperationRow},
        query_repository::{self, QueryConversationRow},
        runtime_repository,
    },
    interfaces::http::{auth::AuthContext, router_support::ApiError},
};

pub const PERMISSION_WORKSPACE_ADMIN: &str = "workspace_admin";
pub const PERMISSION_WORKSPACE_READ: &str = "workspace_read";
pub const PERMISSION_LIBRARY_READ: &str = "library_read";
pub const PERMISSION_LIBRARY_WRITE: &str = "library_write";
pub const PERMISSION_DOCUMENT_READ: &str = "document_read";
pub const PERMISSION_DOCUMENT_WRITE: &str = "document_write";
pub const PERMISSION_CONNECTOR_ADMIN: &str = "connector_admin";
pub const PERMISSION_CREDENTIAL_ADMIN: &str = "credential_admin";
pub const PERMISSION_BINDING_ADMIN: &str = "binding_admin";
pub const PERMISSION_QUERY_RUN: &str = "query_run";
pub const PERMISSION_OPS_READ: &str = "ops_read";
pub const PERMISSION_AUDIT_READ: &str = "audit_read";
pub const PERMISSION_IAM_ADMIN: &str = "iam_admin";

pub const POLICY_WORKSPACE_ADMIN: &[&str] = &[PERMISSION_WORKSPACE_ADMIN, PERMISSION_IAM_ADMIN];
pub const POLICY_WORKSPACE_READ: &[&str] =
    &[PERMISSION_WORKSPACE_READ, PERMISSION_WORKSPACE_ADMIN, PERMISSION_IAM_ADMIN];
pub const POLICY_LIBRARY_READ: &[&str] = &[
    PERMISSION_LIBRARY_READ,
    PERMISSION_LIBRARY_WRITE,
    PERMISSION_DOCUMENT_READ,
    PERMISSION_DOCUMENT_WRITE,
    PERMISSION_QUERY_RUN,
    PERMISSION_WORKSPACE_ADMIN,
    PERMISSION_IAM_ADMIN,
];
pub const POLICY_LIBRARY_WRITE: &[&str] =
    &[PERMISSION_LIBRARY_WRITE, PERMISSION_WORKSPACE_ADMIN, PERMISSION_IAM_ADMIN];
pub const POLICY_PROJECTS_WRITE: &[&str] = POLICY_LIBRARY_WRITE;
pub const POLICY_PROVIDERS_ADMIN: &[&str] = &[
    PERMISSION_CREDENTIAL_ADMIN,
    PERMISSION_BINDING_ADMIN,
    PERMISSION_WORKSPACE_ADMIN,
    PERMISSION_IAM_ADMIN,
];
pub const POLICY_DOCUMENTS_READ: &[&str] = &[
    PERMISSION_DOCUMENT_READ,
    PERMISSION_LIBRARY_READ,
    PERMISSION_LIBRARY_WRITE,
    PERMISSION_WORKSPACE_ADMIN,
    PERMISSION_IAM_ADMIN,
];
pub const POLICY_DOCUMENTS_WRITE: &[&str] = &[
    PERMISSION_DOCUMENT_WRITE,
    PERMISSION_LIBRARY_WRITE,
    PERMISSION_WORKSPACE_ADMIN,
    PERMISSION_IAM_ADMIN,
];
pub const POLICY_KNOWLEDGE_READ: &[&str] = POLICY_GRAPH_READ;
pub const POLICY_KNOWLEDGE_WRITE: &[&str] = POLICY_DOCUMENTS_WRITE;
pub const POLICY_GRAPH_READ: &[&str] = &[
    PERMISSION_DOCUMENT_READ,
    PERMISSION_LIBRARY_READ,
    PERMISSION_QUERY_RUN,
    PERMISSION_WORKSPACE_ADMIN,
    PERMISSION_IAM_ADMIN,
];
pub const POLICY_QUERY_READ: &[&str] = &[
    PERMISSION_QUERY_RUN,
    PERMISSION_DOCUMENT_READ,
    PERMISSION_LIBRARY_READ,
    PERMISSION_WORKSPACE_ADMIN,
    PERMISSION_IAM_ADMIN,
];
pub const POLICY_QUERY_RUN: &[&str] = &[
    PERMISSION_QUERY_RUN,
    PERMISSION_LIBRARY_WRITE,
    PERMISSION_WORKSPACE_ADMIN,
    PERMISSION_IAM_ADMIN,
];
pub const POLICY_RUNTIME_READ: &[&str] = POLICY_QUERY_READ;
pub const POLICY_USAGE_READ: &[&str] =
    &[PERMISSION_OPS_READ, PERMISSION_WORKSPACE_ADMIN, PERMISSION_IAM_ADMIN];
pub const POLICY_MCP_DISCOVERY: &[&str] = &[
    PERMISSION_WORKSPACE_READ,
    PERMISSION_LIBRARY_READ,
    PERMISSION_LIBRARY_WRITE,
    PERMISSION_DOCUMENT_READ,
    PERMISSION_DOCUMENT_WRITE,
    PERMISSION_QUERY_RUN,
    PERMISSION_CONNECTOR_ADMIN,
    PERMISSION_CREDENTIAL_ADMIN,
    PERMISSION_BINDING_ADMIN,
    PERMISSION_OPS_READ,
    PERMISSION_AUDIT_READ,
    PERMISSION_WORKSPACE_ADMIN,
    PERMISSION_IAM_ADMIN,
];
pub const POLICY_MCP_MEMORY_READ: &[&str] = POLICY_DOCUMENTS_READ;
pub const POLICY_MCP_MEMORY_WRITE: &[&str] = POLICY_DOCUMENTS_WRITE;
pub const POLICY_MCP_AUDIT_REVIEW: &[&str] =
    &[PERMISSION_AUDIT_READ, PERMISSION_WORKSPACE_ADMIN, PERMISSION_IAM_ADMIN];
pub const POLICY_IAM_ADMIN: &[&str] = &[PERMISSION_IAM_ADMIN];

/// Permission kinds that represent a write/mutation capability.
///
/// Any authorization request whose accepted-permission set is a subset of write
/// kinds (i.e. read kinds are not also accepted) is treated as a write request
/// and gated on the caller's system-role write capability. This is what keeps a
/// `viewer` user read-only even when a resource grant would otherwise match.
const WRITE_PERMISSION_KINDS: &[&str] = &[
    PERMISSION_WORKSPACE_ADMIN,
    PERMISSION_LIBRARY_WRITE,
    PERMISSION_DOCUMENT_WRITE,
    PERMISSION_CONNECTOR_ADMIN,
    PERMISSION_CREDENTIAL_ADMIN,
    PERMISSION_BINDING_ADMIN,
    PERMISSION_IAM_ADMIN,
];

/// Whether an accepted-permission set denotes a write/mutation request.
///
/// True only when *every* accepted permission is a write kind. Mixed sets that
/// also accept a read permission (e.g. a policy that lets either `library_read`
/// or `library_write` through) are treated as read requests so the role gate
/// never blocks a legitimate read for a `viewer`. Write-only policies such as
/// [`POLICY_LIBRARY_WRITE`] / [`POLICY_DOCUMENTS_WRITE`] are write requests.
#[must_use]
fn accepted_permissions_require_write(accepted_permissions: &[&str]) -> bool {
    !accepted_permissions.is_empty()
        && accepted_permissions.iter().all(|permission| WRITE_PERMISSION_KINDS.contains(permission))
}

/// Enforces the caller's system-role write capability when the accepted
/// permissions denote a write request. Reads pass through untouched.
///
/// # Errors
/// Returns [`ApiError::Unauthorized`] when a `viewer` attempts a write.
fn enforce_write_capability(
    auth: &AuthContext,
    accepted_permissions: &[&str],
) -> Result<(), ApiError> {
    if accepted_permissions_require_write(accepted_permissions) {
        auth.require_write_capability()?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SanitizedMcpAuditScope {
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct AuthorizedContentDocument {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
}

pub fn authorize_workspace_scope(
    auth: &AuthContext,
    workspace_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<(), ApiError> {
    authorize_workspace_permission(auth, workspace_id, accepted_permissions)
}

pub fn authorize_workspace_permission(
    auth: &AuthContext,
    workspace_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<(), ApiError> {
    enforce_write_capability(auth, accepted_permissions)?;
    if auth.has_workspace_permission(workspace_id, accepted_permissions) {
        return Ok(());
    }
    Err(ApiError::Unauthorized)
}

pub fn authorize_workspace_discovery(
    auth: &AuthContext,
    workspace_id: Uuid,
) -> Result<(), ApiError> {
    if auth.is_system_admin
        || auth.can_access_workspace(workspace_id)
        || auth.can_discover_workspace(workspace_id, POLICY_MCP_DISCOVERY)
    {
        return Ok(());
    }
    Err(ApiError::Unauthorized)
}

/// Authorizes a library-authoring (create/edit/delete) action in a workspace.
///
/// Passes for a system administrator unconditionally, or for an `operator`+
/// user (write capability) who has workspace-level access (a `workspace`-scoped
/// grant or active membership). A `viewer` is always rejected, even with a
/// stray workspace grant. Library *content* mutations continue to flow through
/// [`authorize_library_permission`] / [`authorize_document_permission`] with the
/// write policies; this helper is the workspace-level gate that previously hard-
/// required `workspace_admin`, which excluded plain operators.
pub fn authorize_library_write(auth: &AuthContext, workspace_id: Uuid) -> Result<(), ApiError> {
    auth.require_write_capability()?;
    if auth.is_system_admin || auth.can_access_workspace(workspace_id) {
        return Ok(());
    }
    Err(ApiError::Unauthorized)
}

/// Loads a workspace and authorizes a library-authoring action against it.
///
/// Convenience wrapper mirroring [`load_workspace_and_authorize`] but using the
/// role-aware [`authorize_library_write`] gate instead of a fixed policy.
///
/// # Errors
/// Returns [`ApiError::NotFound`] when the workspace does not exist and
/// [`ApiError::Unauthorized`] when the caller may not author libraries there.
pub async fn load_workspace_and_authorize_library_write(
    auth: &AuthContext,
    state: &AppState,
    workspace_id: Uuid,
) -> Result<CatalogWorkspaceRow, ApiError> {
    let workspace =
        catalog_repository::get_workspace_by_id(&state.persistence.postgres, workspace_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("workspace", workspace_id))?;
    authorize_library_write(auth, workspace.id)?;
    Ok(workspace)
}

pub fn authorize_library_permission(
    auth: &AuthContext,
    workspace_id: Uuid,
    library_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<(), ApiError> {
    enforce_write_capability(auth, accepted_permissions)?;
    if auth.has_library_permission(workspace_id, library_id, accepted_permissions) {
        return Ok(());
    }
    Err(ApiError::Unauthorized)
}

pub fn authorize_library_discovery(
    auth: &AuthContext,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<(), ApiError> {
    if auth.is_system_admin
        || auth.can_access_workspace(workspace_id)
        || auth.can_discover_library(workspace_id, library_id, POLICY_MCP_DISCOVERY)
    {
        return Ok(());
    }
    Err(ApiError::Unauthorized)
}

pub fn authorize_document_permission(
    auth: &AuthContext,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<(), ApiError> {
    enforce_write_capability(auth, accepted_permissions)?;
    if auth.has_document_permission(workspace_id, library_id, document_id, accepted_permissions) {
        return Ok(());
    }
    Err(ApiError::Unauthorized)
}

pub fn authorize_knowledge_document_permission(
    auth: &AuthContext,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<(), ApiError> {
    authorize_document_permission(auth, workspace_id, library_id, document_id, accepted_permissions)
}

pub fn authorize_knowledge_bundle_permission(
    auth: &AuthContext,
    workspace_id: Uuid,
    library_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<(), ApiError> {
    authorize_library_permission(auth, workspace_id, library_id, accepted_permissions)
}

pub fn authorize_query_session_permission(
    auth: &AuthContext,
    workspace_id: Uuid,
    library_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<(), ApiError> {
    authorize_library_permission(auth, workspace_id, library_id, accepted_permissions)
}

pub fn authorize_connector_permission(
    auth: &AuthContext,
    workspace_id: Uuid,
    library_id: Uuid,
    connector_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<(), ApiError> {
    enforce_write_capability(auth, accepted_permissions)?;
    if auth.is_system_admin
        || auth.grants.iter().any(|grant| {
            accepted_permissions.iter().any(|permission| grant.permission_kind == *permission)
                && ((grant.resource_kind == "workspace"
                    && grant.workspace_id == Some(workspace_id))
                    || (grant.resource_kind == "library" && grant.library_id == Some(library_id))
                    || (grant.resource_kind == "connector" && grant.resource_id == connector_id))
        })
    {
        return Ok(());
    }
    Err(ApiError::Unauthorized)
}

pub fn authorize_provider_credential_permission(
    auth: &AuthContext,
    workspace_id: Uuid,
    credential_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<(), ApiError> {
    enforce_write_capability(auth, accepted_permissions)?;
    if auth.is_system_admin
        || auth.grants.iter().any(|grant| {
            accepted_permissions.iter().any(|permission| grant.permission_kind == *permission)
                && ((grant.resource_kind == "workspace"
                    && grant.workspace_id == Some(workspace_id))
                    || (grant.resource_kind == "provider_credential"
                        && grant.resource_id == credential_id))
        })
    {
        return Ok(());
    }
    Err(ApiError::Unauthorized)
}

pub fn authorize_library_binding_permission(
    auth: &AuthContext,
    workspace_id: Uuid,
    library_id: Uuid,
    binding_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<(), ApiError> {
    enforce_write_capability(auth, accepted_permissions)?;
    if auth.is_system_admin
        || auth.grants.iter().any(|grant| {
            accepted_permissions.iter().any(|permission| grant.permission_kind == *permission)
                && ((grant.resource_kind == "workspace"
                    && grant.workspace_id == Some(workspace_id))
                    || (grant.resource_kind == "library" && grant.library_id == Some(library_id))
                    || (grant.resource_kind == "library_binding"
                        && grant.resource_id == binding_id))
        })
    {
        return Ok(());
    }
    Err(ApiError::Unauthorized)
}

pub async fn load_workspace_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    workspace_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<CatalogWorkspaceRow, ApiError> {
    let workspace =
        catalog_repository::get_workspace_by_id(&state.persistence.postgres, workspace_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("workspace", workspace_id))?;
    authorize_workspace_permission(auth, workspace.id, accepted_permissions)?;
    Ok(workspace)
}

pub async fn load_library_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    library_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<CatalogLibraryRow, ApiError> {
    let library = catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
    authorize_library_permission(auth, library.workspace_id, library.id, accepted_permissions)?;
    Ok(library)
}

pub async fn load_connector_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    connector_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<CatalogLibraryConnectorRow, ApiError> {
    let connector =
        catalog_repository::get_connector_by_id(&state.persistence.postgres, connector_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("connector", connector_id))?;
    authorize_connector_permission(
        auth,
        connector.workspace_id,
        connector.library_id,
        connector.id,
        accepted_permissions,
    )?;
    Ok(connector)
}

pub async fn load_provider_credential_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    credential_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<AiProviderCredentialRow, ApiError> {
    let credential =
        ai_repository::get_provider_credential_by_id(&state.persistence.postgres, credential_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("provider_credential", credential_id))?;
    if let Some(workspace_id) = credential.workspace_id {
        authorize_provider_credential_permission(
            auth,
            workspace_id,
            credential.id,
            accepted_permissions,
        )?;
    } else if !auth.is_system_admin {
        return Err(ApiError::Unauthorized);
    }
    Ok(credential)
}

pub async fn load_library_binding_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    binding_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<AiBindingAssignmentRow, ApiError> {
    let binding =
        ai_repository::get_binding_assignment_by_id(&state.persistence.postgres, binding_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("library_binding", binding_id))?;
    match (binding.workspace_id, binding.library_id) {
        (Some(workspace_id), Some(library_id)) => authorize_library_binding_permission(
            auth,
            workspace_id,
            library_id,
            binding.id,
            accepted_permissions,
        )?,
        _ if auth.is_system_admin => {}
        _ => return Err(ApiError::Unauthorized),
    }
    Ok(binding)
}

pub async fn load_query_session_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    session_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<QueryConversationRow, ApiError> {
    let session = query_repository::get_conversation_by_id(&state.persistence.postgres, session_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("query_session", session_id))?;
    authorize_query_session_permission(
        auth,
        session.workspace_id,
        session.library_id,
        accepted_permissions,
    )?;
    Ok(session)
}

pub async fn load_query_execution_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    execution_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<query_repository::QueryExecutionRow, ApiError> {
    let execution =
        query_repository::get_execution_by_id(&state.persistence.postgres, execution_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("query_execution", execution_id))?;
    authorize_library_permission(
        auth,
        execution.workspace_id,
        execution.library_id,
        accepted_permissions,
    )?;
    Ok(execution)
}

pub async fn load_runtime_execution_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    execution_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<runtime_repository::RuntimeExecutionRow, ApiError> {
    let execution =
        runtime_repository::get_runtime_execution_by_id(&state.persistence.postgres, execution_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("runtime_execution", execution_id))?;
    let scope = state
        .canonical_services
        .iam
        .resolve_runtime_execution_access_scope(state, &execution)
        .await?;
    match scope.document_id {
        Some(document_id) => authorize_document_permission(
            auth,
            scope.workspace_id,
            scope.library_id,
            document_id,
            accepted_permissions,
        )?,
        None => authorize_library_permission(
            auth,
            scope.workspace_id,
            scope.library_id,
            accepted_permissions,
        )?,
    }
    Ok(execution)
}

pub async fn load_async_operation_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    operation_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<OpsAsyncOperationRow, ApiError> {
    let operation =
        ops_repository::get_async_operation_by_id(&state.persistence.postgres, operation_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("async_operation", operation_id))?;
    match operation.library_id {
        Some(library_id) => {
            authorize_library_permission(
                auth,
                operation.workspace_id,
                library_id,
                accepted_permissions,
            )?;
        }
        None => authorize_workspace_permission(auth, operation.workspace_id, accepted_permissions)?,
    }
    Ok(operation)
}

pub async fn load_content_document_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<AuthorizedContentDocument, ApiError> {
    let document = state
        .arango_document_store
        .get_document(document_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
    authorize_document_permission(
        auth,
        document.workspace_id,
        document.library_id,
        document.document_id,
        accepted_permissions,
    )?;
    Ok(AuthorizedContentDocument {
        id: document.document_id,
        workspace_id: document.workspace_id,
        library_id: document.library_id,
    })
}

pub async fn load_canonical_content_document_and_authorize(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<AuthorizedContentDocument, ApiError> {
    let document = content_repository::get_document_by_id(&state.persistence.postgres, document_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
    authorize_document_permission(
        auth,
        document.workspace_id,
        document.library_id,
        document.id,
        accepted_permissions,
    )?;
    Ok(AuthorizedContentDocument {
        id: document.id,
        workspace_id: document.workspace_id,
        library_id: document.library_id,
    })
}

pub fn filter_workspace_rows<T>(
    auth: &AuthContext,
    items: Vec<T>,
    workspace_id_of: impl Fn(&T) -> Option<Uuid>,
) -> Vec<T> {
    if auth.is_system_admin {
        return items;
    }
    items
        .into_iter()
        .filter(|item| {
            workspace_id_of(item)
                .is_some_and(|workspace_id| auth.can_access_workspace(workspace_id))
        })
        .collect()
}

pub fn authorize_mcp_audit_review(
    auth: &AuthContext,
    workspace_filter: Option<Uuid>,
) -> Result<Option<Uuid>, ApiError> {
    if auth.is_system_admin {
        return Ok(workspace_filter);
    }

    match workspace_filter {
        Some(workspace_id) => {
            authorize_workspace_permission(auth, workspace_id, POLICY_MCP_AUDIT_REVIEW)?;
            Ok(Some(workspace_id))
        }
        None if auth.visible_workspace_ids.len() == 1 => {
            let workspace_id =
                auth.visible_workspace_ids.iter().copied().next().unwrap_or_default();
            authorize_workspace_permission(auth, workspace_id, POLICY_MCP_AUDIT_REVIEW)?;
            Ok(Some(workspace_id))
        }
        None => Err(ApiError::Unauthorized),
    }
}

#[must_use]
pub fn sanitize_mcp_audit_scope(
    error: &ApiError,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    document_id: Option<Uuid>,
) -> SanitizedMcpAuditScope {
    if matches!(
        error,
        ApiError::Unauthorized | ApiError::InaccessibleMemoryScope(_) | ApiError::NotFound(_)
    ) {
        return SanitizedMcpAuditScope::default();
    }

    SanitizedMcpAuditScope { workspace_id, library_id, document_id }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{
        domains::iam::PrincipalKind,
        infra::repositories::iam_repository::SystemRole,
        interfaces::http::auth::{AuthGrant, AuthTokenKind},
    };

    /// Builds a session-backed user [`AuthContext`] with a system role and an
    /// optional workspace grant. `library_write` is used for the workspace
    /// grant so the matrix exercises a real write permission.
    fn user_auth(
        role: SystemRole,
        workspace_id: Uuid,
        with_workspace_grant: bool,
        grant_permission: &str,
    ) -> AuthContext {
        let mut grants = Vec::new();
        let mut visible_workspace_ids = BTreeSet::new();
        if with_workspace_grant {
            grants.push(AuthGrant {
                id: Uuid::now_v7(),
                resource_kind: "workspace".into(),
                resource_id: workspace_id,
                permission_kind: grant_permission.to_string(),
                workspace_id: Some(workspace_id),
                library_id: None,
                document_id: None,
            });
            visible_workspace_ids.insert(workspace_id);
        }
        AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: AuthTokenKind::Session,
            scopes: if with_workspace_grant {
                vec![grant_permission.to_string()]
            } else {
                Vec::new()
            },
            grants,
            workspace_memberships: Vec::new(),
            visible_workspace_ids,
            is_system_admin: role == SystemRole::Admin,
            system_role: Some(role),
        }
    }

    #[test]
    fn write_classifier_only_trips_for_write_only_policies() {
        assert!(accepted_permissions_require_write(POLICY_LIBRARY_WRITE));
        assert!(accepted_permissions_require_write(POLICY_DOCUMENTS_WRITE));
        assert!(accepted_permissions_require_write(POLICY_WORKSPACE_ADMIN));
        // Mixed read/write policies are read requests so viewers keep reading.
        assert!(!accepted_permissions_require_write(POLICY_LIBRARY_READ));
        assert!(!accepted_permissions_require_write(POLICY_DOCUMENTS_READ));
        assert!(!accepted_permissions_require_write(&[]));
    }

    #[test]
    fn admin_can_create_libraries_in_any_workspace() {
        let workspace_id = Uuid::now_v7();
        let admin = user_auth(SystemRole::Admin, workspace_id, false, PERMISSION_WORKSPACE_READ);
        // No grant at all, but admin short-circuits.
        assert!(authorize_library_write(&admin, Uuid::now_v7()).is_ok());
    }

    #[test]
    fn operator_without_grant_cannot_create_library() {
        let workspace_id = Uuid::now_v7();
        let operator =
            user_auth(SystemRole::Operator, workspace_id, false, PERMISSION_WORKSPACE_READ);
        assert!(matches!(
            authorize_library_write(&operator, workspace_id),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn operator_with_workspace_grant_can_create_library() {
        let workspace_id = Uuid::now_v7();
        let operator =
            user_auth(SystemRole::Operator, workspace_id, true, PERMISSION_WORKSPACE_READ);
        assert!(authorize_library_write(&operator, workspace_id).is_ok());
        // But not in a workspace they were never granted.
        assert!(matches!(
            authorize_library_write(&operator, Uuid::now_v7()),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn viewer_with_grant_cannot_create_library() {
        let workspace_id = Uuid::now_v7();
        // Even with a (stray) workspace grant, a viewer is never write-capable.
        let viewer = user_auth(SystemRole::Viewer, workspace_id, true, PERMISSION_WORKSPACE_ADMIN);
        assert!(matches!(
            authorize_library_write(&viewer, workspace_id),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn viewer_with_write_grant_is_blocked_at_permission_gate() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        // Viewer with a library_write grant must still be blocked from writes.
        let mut viewer =
            user_auth(SystemRole::Viewer, workspace_id, false, PERMISSION_LIBRARY_READ);
        viewer.grants.push(AuthGrant {
            id: Uuid::now_v7(),
            resource_kind: "library".into(),
            resource_id: library_id,
            permission_kind: PERMISSION_LIBRARY_WRITE.into(),
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        });
        assert!(matches!(
            authorize_library_permission(&viewer, workspace_id, library_id, POLICY_LIBRARY_WRITE),
            Err(ApiError::Unauthorized)
        ));
        // The same viewer CAN read that library via its read grant + role.
        let mut reader =
            user_auth(SystemRole::Viewer, workspace_id, false, PERMISSION_LIBRARY_READ);
        reader.grants.push(AuthGrant {
            id: Uuid::now_v7(),
            resource_kind: "library".into(),
            resource_id: library_id,
            permission_kind: PERMISSION_LIBRARY_READ.into(),
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        });
        assert!(
            authorize_library_permission(&reader, workspace_id, library_id, POLICY_LIBRARY_READ)
                .is_ok()
        );
    }

    #[test]
    fn operator_with_library_write_grant_can_write_library() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let mut operator =
            user_auth(SystemRole::Operator, workspace_id, false, PERMISSION_LIBRARY_WRITE);
        operator.grants.push(AuthGrant {
            id: Uuid::now_v7(),
            resource_kind: "library".into(),
            resource_id: library_id,
            permission_kind: PERMISSION_LIBRARY_WRITE.into(),
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        });
        assert!(
            authorize_library_permission(&operator, workspace_id, library_id, POLICY_LIBRARY_WRITE)
                .is_ok()
        );
        // No grant for a different library → denied.
        assert!(matches!(
            authorize_library_permission(
                &operator,
                workspace_id,
                Uuid::now_v7(),
                POLICY_LIBRARY_WRITE
            ),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn api_token_principals_keep_grant_only_behavior() {
        // No system_role → role gate is a no-op; the resource grant decides.
        let workspace_id = Uuid::now_v7();
        let auth = AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: Some(workspace_id),
            token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
            scopes: vec![PERMISSION_LIBRARY_WRITE.into()],
            grants: vec![AuthGrant {
                id: Uuid::now_v7(),
                resource_kind: "workspace".into(),
                resource_id: workspace_id,
                permission_kind: PERMISSION_LIBRARY_WRITE.into(),
                workspace_id: Some(workspace_id),
                library_id: None,
                document_id: None,
            }],
            workspace_memberships: Vec::new(),
            visible_workspace_ids: [workspace_id].into_iter().collect(),
            is_system_admin: false,
            system_role: None,
        };
        assert!(
            authorize_library_permission(&auth, workspace_id, Uuid::now_v7(), POLICY_LIBRARY_WRITE)
                .is_ok()
        );
    }
}
