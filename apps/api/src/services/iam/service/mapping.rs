use super::*;

pub(super) fn map_principal(row: iam_repository::IamPrincipalRow) -> Result<Principal, ApiError> {
    Ok(Principal {
        id: row.id,
        principal_kind: parse_principal_kind(&row.principal_kind)?,
        display_label: row.display_label,
        status: row.status,
        created_at: row.created_at,
    })
}

pub(super) fn map_api_token(row: iam_repository::IamApiTokenRow) -> ApiToken {
    ApiToken {
        principal_id: row.principal_id,
        workspace_id: row.workspace_id,
        label: row.label,
        token_prefix: row.token_prefix,
        status: row.status,
        expires_at: row.expires_at,
    }
}

pub(super) fn map_grant(row: iam_repository::IamGrantRow) -> Result<Grant, ApiError> {
    Ok(Grant {
        id: row.id,
        principal_id: row.principal_id,
        resource_kind: parse_grant_resource_kind(&row.resource_kind)?,
        resource_id: row.resource_id,
        permission_kind: row.permission_kind,
        granted_at: row.granted_at,
    })
}

pub(super) fn map_workspace_membership(
    row: iam_repository::IamWorkspaceMembershipRow,
) -> WorkspaceMembership {
    WorkspaceMembership {
        workspace_id: row.workspace_id,
        principal_id: row.principal_id,
        membership_state: row.membership_state,
        joined_at: row.joined_at,
        ended_at: row.ended_at,
    }
}

pub(super) const fn grant_resource_kind_as_str(value: &GrantResourceKind) -> &'static str {
    match value {
        GrantResourceKind::System => "system",
        GrantResourceKind::Workspace => "workspace",
        GrantResourceKind::Library => "library",
        GrantResourceKind::Document => "document",
        GrantResourceKind::QuerySession => "query_session",
        GrantResourceKind::AsyncOperation => "async_operation",
        GrantResourceKind::Connector => "connector",
        GrantResourceKind::ProviderCredential => "provider_credential",
        GrantResourceKind::LibraryBinding => "library_binding",
    }
}

fn parse_principal_kind(value: &str) -> Result<PrincipalKind, ApiError> {
    match value {
        "user" => Ok(PrincipalKind::User),
        "api_token" => Ok(PrincipalKind::ApiToken),
        "worker" => Ok(PrincipalKind::Worker),
        "bootstrap" => Ok(PrincipalKind::Bootstrap),
        _ => Err(ApiError::Internal),
    }
}

fn parse_grant_resource_kind(value: &str) -> Result<GrantResourceKind, ApiError> {
    match value {
        "system" => Ok(GrantResourceKind::System),
        "workspace" => Ok(GrantResourceKind::Workspace),
        "library" => Ok(GrantResourceKind::Library),
        "document" => Ok(GrantResourceKind::Document),
        "query_session" => Ok(GrantResourceKind::QuerySession),
        "async_operation" => Ok(GrantResourceKind::AsyncOperation),
        "connector" => Ok(GrantResourceKind::Connector),
        "provider_credential" => Ok(GrantResourceKind::ProviderCredential),
        "library_binding" => Ok(GrantResourceKind::LibraryBinding),
        _ => Err(ApiError::Internal),
    }
}
