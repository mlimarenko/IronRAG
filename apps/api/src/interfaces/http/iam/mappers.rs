use super::*;

pub(super) fn map_principal_row(
    row: iam_repository::IamPrincipalRow,
) -> Result<PrincipalResponse, ApiError> {
    Ok(PrincipalResponse {
        id: row.id,
        principal_kind: map_principal_kind(&row.principal_kind)?,
        status: row.status,
        display_label: row.display_label,
        created_at: row.created_at,
        disabled_at: row.disabled_at,
    })
}

pub(super) fn map_user_row(row: iam_repository::IamUserRow) -> UserResponse {
    let role = map_route_system_role_from_repo(row.system_role());
    UserResponse {
        principal_id: row.principal_id,
        login: row.login,
        email: row.email,
        display_name: row.display_name,
        role,
        auth_provider_kind: row.auth_provider_kind,
        external_subject: row.external_subject,
    }
}

pub(super) const fn map_route_system_role_from_repo(
    role: iam_repository::SystemRole,
) -> SystemRole {
    match role {
        iam_repository::SystemRole::Viewer => SystemRole::Viewer,
        iam_repository::SystemRole::Operator => SystemRole::Operator,
        iam_repository::SystemRole::Admin => SystemRole::Admin,
    }
}

pub(super) const fn map_route_system_role_to_repo(role: SystemRole) -> iam_repository::SystemRole {
    match role {
        SystemRole::Viewer => iam_repository::SystemRole::Viewer,
        SystemRole::Operator => iam_repository::SystemRole::Operator,
        SystemRole::Admin => iam_repository::SystemRole::Admin,
    }
}

pub(super) fn map_membership_row(row: WorkspaceMembership) -> WorkspaceMembershipResponse {
    WorkspaceMembershipResponse {
        workspace_id: row.workspace_id,
        principal_id: row.principal_id,
        membership_state: row.membership_state,
        joined_at: row.joined_at,
        ended_at: row.ended_at,
    }
}

pub(super) fn map_principal_row_contract(
    row: iam_repository::IamPrincipalRow,
) -> Result<ironrag_contracts::auth::PrincipalProfile, ApiError> {
    Ok(ironrag_contracts::auth::PrincipalProfile {
        id: row.id,
        principal_kind: match map_principal_kind(&row.principal_kind)? {
            IamPrincipalKind::User => "user".to_string(),
            IamPrincipalKind::ApiToken => "api_token".to_string(),
            IamPrincipalKind::Worker => "worker".to_string(),
            IamPrincipalKind::Bootstrap => "bootstrap".to_string(),
        },
        status: row.status,
        display_label: row.display_label,
    })
}

pub(super) fn map_user_row_contract(
    row: iam_repository::IamUserRow,
) -> ironrag_contracts::auth::UserProfile {
    let role = map_system_role_contract(row.system_role());
    ironrag_contracts::auth::UserProfile {
        principal_id: row.principal_id,
        login: Some(row.login),
        email: Some(row.email),
        display_name: Some(row.display_name),
        role,
    }
}

pub(crate) const fn map_system_role_contract(
    role: iam_repository::SystemRole,
) -> ironrag_contracts::auth::SystemRole {
    match role {
        iam_repository::SystemRole::Viewer => ironrag_contracts::auth::SystemRole::Viewer,
        iam_repository::SystemRole::Operator => ironrag_contracts::auth::SystemRole::Operator,
        iam_repository::SystemRole::Admin => ironrag_contracts::auth::SystemRole::Admin,
    }
}

pub(super) fn map_membership_row_contract(
    row: WorkspaceMembership,
) -> ironrag_contracts::auth::WorkspaceMembership {
    ironrag_contracts::auth::WorkspaceMembership {
        workspace_id: row.workspace_id,
        principal_id: row.principal_id,
        membership_state: row.membership_state,
        joined_at: row.joined_at,
        ended_at: row.ended_at,
    }
}

pub(super) fn map_grant_domain(row: Grant) -> Result<GrantResponse, ApiError> {
    Ok(GrantResponse {
        id: row.id,
        principal_id: row.principal_id,
        resource_kind: map_domain_grant_resource_kind(row.resource_kind)?,
        resource_id: row.resource_id,
        permission_kind: map_permission_kind(&row.permission_kind)?,
        granted_by_principal_id: None,
        granted_at: row.granted_at,
        expires_at: None,
    })
}

pub(super) fn map_grant_domain_contract(
    row: Grant,
) -> Result<ironrag_contracts::auth::TokenGrant, ApiError> {
    Ok(ironrag_contracts::auth::TokenGrant {
        id: row.id,
        principal_id: row.principal_id,
        resource_kind: map_domain_grant_resource_kind_contract(row.resource_kind),
        resource_id: row.resource_id,
        permission_kind: map_permission_kind_contract(&row.permission_kind)?,
        granted_at: row.granted_at,
        expires_at: None,
    })
}

pub(super) fn map_principal_kind(value: &str) -> Result<IamPrincipalKind, ApiError> {
    match value {
        "user" => Ok(IamPrincipalKind::User),
        "api_token" => Ok(IamPrincipalKind::ApiToken),
        "worker" => Ok(IamPrincipalKind::Worker),
        "bootstrap" => Ok(IamPrincipalKind::Bootstrap),
        other => {
            warn!(principal_kind = %other, "encountered unknown principal kind");
            Err(ApiError::Internal)
        }
    }
}

pub(super) fn map_grant_resource_kind(value: &str) -> Result<IamGrantResourceKind, ApiError> {
    match value {
        "system" => Ok(IamGrantResourceKind::System),
        "workspace" => Ok(IamGrantResourceKind::Workspace),
        "library" => Ok(IamGrantResourceKind::Library),
        "document" => Ok(IamGrantResourceKind::Document),
        "query_session" => Ok(IamGrantResourceKind::QuerySession),
        "async_operation" => Ok(IamGrantResourceKind::AsyncOperation),
        "connector" => Ok(IamGrantResourceKind::Connector),
        "provider_credential" => Ok(IamGrantResourceKind::ProviderCredential),
        "library_binding" => Ok(IamGrantResourceKind::LibraryBinding),
        other => {
            warn!(resource_kind = %other, "encountered unknown grant resource kind");
            Err(ApiError::Internal)
        }
    }
}

pub(super) const fn map_domain_grant_resource_kind_contract(
    value: GrantResourceKind,
) -> ironrag_contracts::auth::GrantResourceKind {
    match value {
        GrantResourceKind::System => ironrag_contracts::auth::GrantResourceKind::System,
        GrantResourceKind::Workspace => ironrag_contracts::auth::GrantResourceKind::Workspace,
        GrantResourceKind::Library => ironrag_contracts::auth::GrantResourceKind::Library,
        GrantResourceKind::Document => ironrag_contracts::auth::GrantResourceKind::Document,
        GrantResourceKind::QuerySession => ironrag_contracts::auth::GrantResourceKind::QuerySession,
        GrantResourceKind::AsyncOperation => {
            ironrag_contracts::auth::GrantResourceKind::AsyncOperation
        }
        GrantResourceKind::Connector => ironrag_contracts::auth::GrantResourceKind::Connector,
        GrantResourceKind::ProviderCredential => {
            ironrag_contracts::auth::GrantResourceKind::ProviderCredential
        }
        GrantResourceKind::LibraryBinding => {
            ironrag_contracts::auth::GrantResourceKind::LibraryBinding
        }
    }
}

pub(super) fn map_permission_kind_contract(
    value: &str,
) -> Result<ironrag_contracts::auth::PermissionKind, ApiError> {
    Ok(match value {
        "iam_admin" => ironrag_contracts::auth::PermissionKind::IamAdmin,
        "workspace_admin" => ironrag_contracts::auth::PermissionKind::WorkspaceAdmin,
        "workspace_read" => ironrag_contracts::auth::PermissionKind::WorkspaceRead,
        "library_read" => ironrag_contracts::auth::PermissionKind::LibraryRead,
        "library_write" => ironrag_contracts::auth::PermissionKind::LibraryWrite,
        "document_read" => ironrag_contracts::auth::PermissionKind::DocumentRead,
        "document_write" => ironrag_contracts::auth::PermissionKind::DocumentWrite,
        "connector_admin" => ironrag_contracts::auth::PermissionKind::ConnectorAdmin,
        "credential_admin" => ironrag_contracts::auth::PermissionKind::CredentialAdmin,
        "binding_admin" => ironrag_contracts::auth::PermissionKind::BindingAdmin,
        "query_run" => ironrag_contracts::auth::PermissionKind::QueryRun,
        "ops_read" => ironrag_contracts::auth::PermissionKind::OpsRead,
        "audit_read" => ironrag_contracts::auth::PermissionKind::AuditRead,
        other => {
            warn!(permission_kind = %other, "encountered unknown permission kind");
            return Err(ApiError::Internal);
        }
    })
}

pub(super) const fn map_domain_grant_resource_kind(
    value: GrantResourceKind,
) -> Result<IamGrantResourceKind, ApiError> {
    match value {
        GrantResourceKind::System => Ok(IamGrantResourceKind::System),
        GrantResourceKind::Workspace => Ok(IamGrantResourceKind::Workspace),
        GrantResourceKind::Library => Ok(IamGrantResourceKind::Library),
        GrantResourceKind::Document => Ok(IamGrantResourceKind::Document),
        GrantResourceKind::QuerySession => Ok(IamGrantResourceKind::QuerySession),
        GrantResourceKind::AsyncOperation => Ok(IamGrantResourceKind::AsyncOperation),
        GrantResourceKind::Connector => Ok(IamGrantResourceKind::Connector),
        GrantResourceKind::ProviderCredential => Ok(IamGrantResourceKind::ProviderCredential),
        GrantResourceKind::LibraryBinding => Ok(IamGrantResourceKind::LibraryBinding),
    }
}

pub(super) fn map_permission_kind(value: &str) -> Result<IamPermissionKind, ApiError> {
    match value {
        "iam_admin" => Ok(IamPermissionKind::IamAdmin),
        "workspace_admin" => Ok(IamPermissionKind::WorkspaceAdmin),
        "workspace_read" => Ok(IamPermissionKind::WorkspaceRead),
        "library_read" => Ok(IamPermissionKind::LibraryRead),
        "library_write" => Ok(IamPermissionKind::LibraryWrite),
        "document_read" => Ok(IamPermissionKind::DocumentRead),
        "document_write" => Ok(IamPermissionKind::DocumentWrite),
        "connector_admin" => Ok(IamPermissionKind::ConnectorAdmin),
        "credential_admin" => Ok(IamPermissionKind::CredentialAdmin),
        "binding_admin" => Ok(IamPermissionKind::BindingAdmin),
        "query_run" => Ok(IamPermissionKind::QueryRun),
        "ops_read" => Ok(IamPermissionKind::OpsRead),
        "audit_read" => Ok(IamPermissionKind::AuditRead),
        other => {
            warn!(permission_kind = %other, "encountered unknown grant permission kind");
            Err(ApiError::Internal)
        }
    }
}
