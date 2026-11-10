//! Authorization and mapping for the IAM diagnostics MCP tools.
//!
//! Every tool here is system-administrator only (`list_iam_tokens`,
//! `revoke_iam_token`, `get_principal_access`, `mint_iam_token`,
//! `update_iam_token`). The gate mirrors the REST `require_system_admin`
//! handler: the caller's resolved system-admin flag is the single input, so an
//! API token that merely carries broad grants — `iam_admin` included — does not
//! pass.

use crate::{
    app::state::AppState,
    domains::iam::{ApiToken, Grant, GrantResourceKind, Principal, WorkspaceMembership},
    interfaces::http::{
        auth::AuthContext,
        iam::{build_mint_token_command, parse_iam_permission_kinds},
        router_support::ApiError,
    },
    mcp_types::{
        McpGetPrincipalAccessRequest, McpIamGrant, McpIamMembership, McpIamPrincipal, McpIamToken,
        McpIamTokenList, McpListIamTokensRequest, McpMintIamTokenRequest, McpMintedIamToken,
        McpPrincipalAccess, McpRevokeIamTokenRequest, McpUpdateIamTokenRequest,
    },
    services::iam::service::{grant_resource_kind_as_str, principal_kind_as_str},
};

pub(crate) async fn list_iam_tokens(
    auth: &AuthContext,
    state: &AppState,
    request: McpListIamTokensRequest,
) -> Result<McpIamTokenList, ApiError> {
    ensure_system_admin(auth)?;
    let tokens =
        state.application_services.iam.list_api_tokens(state, request.workspace_id).await?;
    Ok(McpIamTokenList { items: tokens.iter().map(map_api_token).collect() })
}

pub(crate) async fn revoke_iam_token(
    auth: &AuthContext,
    state: &AppState,
    request: McpRevokeIamTokenRequest,
) -> Result<McpIamToken, ApiError> {
    ensure_system_admin(auth)?;
    let token = state.application_services.iam.revoke_api_token(state, request.token_id).await?;
    Ok(map_api_token(&token))
}

pub(crate) async fn mint_iam_token(
    auth: &AuthContext,
    state: &AppState,
    request: McpMintIamTokenRequest,
) -> Result<McpMintedIamToken, ApiError> {
    ensure_system_admin(auth)?;
    let permission_kinds = parse_iam_permission_kinds(request.permission_kinds)?;
    // One command builder for both surfaces: the REST route and this tool
    // validate the same rules and cannot drift.
    let command = build_mint_token_command(
        state,
        auth.principal_id,
        request.workspace_id,
        request.label,
        request.expires_at,
        request.library_ids,
        permission_kinds,
    )
    .await?;
    let outcome = state.application_services.iam.mint_api_token(state, command).await?;
    Ok(McpMintedIamToken {
        token: outcome.token.clone(),
        api_token: map_api_token(&outcome.api_token),
    })
}

/// Partial update of one API token. The label rule mirrors the REST PATCH
/// handler: an empty or whitespace-only label is rejected before the row is
/// touched, and the permission scope is not editable here.
pub(crate) async fn update_iam_token(
    auth: &AuthContext,
    state: &AppState,
    request: McpUpdateIamTokenRequest,
) -> Result<McpIamToken, ApiError> {
    ensure_system_admin(auth)?;
    let label = match request.label {
        Some(label) => {
            let trimmed = label.trim();
            if trimmed.is_empty() {
                return Err(ApiError::BadRequest("label must not be empty".into()));
            }
            Some(trimmed.to_string())
        }
        None => None,
    };
    let token = state
        .application_services
        .iam
        .update_api_token(state, request.token_id, label.as_deref(), request.expires_at)
        .await?;
    Ok(map_api_token(&token))
}

pub(crate) async fn get_principal_access(
    auth: &AuthContext,
    state: &AppState,
    request: McpGetPrincipalAccessRequest,
) -> Result<McpPrincipalAccess, ApiError> {
    ensure_system_admin(auth)?;
    let resolution = state
        .application_services
        .iam
        .resolve_effective_grants(state, request.principal_id)
        .await?;
    Ok(McpPrincipalAccess {
        principal: map_principal(&resolution.principal),
        grants: resolution.grants.iter().map(map_grant).collect(),
        workspace_memberships: resolution
            .workspace_memberships
            .iter()
            .map(map_workspace_membership)
            .collect(),
    })
}

fn ensure_system_admin(auth: &AuthContext) -> Result<(), ApiError> {
    if !auth.is_system_admin {
        return Err(ApiError::Unauthorized);
    }
    Ok(())
}

fn map_api_token(token: &ApiToken) -> McpIamToken {
    McpIamToken {
        principal_id: token.principal_id,
        workspace_id: token.workspace_id,
        label: token.label.clone(),
        token_prefix: token.token_prefix.clone(),
        status: token.status.clone(),
        expires_at: token.expires_at,
    }
}

fn map_principal(principal: &Principal) -> McpIamPrincipal {
    McpIamPrincipal {
        principal_id: principal.id,
        principal_kind: principal_kind_as_str(&principal.principal_kind).to_string(),
        display_name: principal.display_label.clone(),
    }
}

fn map_grant(grant: &Grant) -> McpIamGrant {
    let (workspace_id, library_id, document_id) = match grant.resource_kind {
        GrantResourceKind::Workspace => (Some(grant.resource_id), None, None),
        GrantResourceKind::Library => (None, Some(grant.resource_id), None),
        GrantResourceKind::Document => (None, None, Some(grant.resource_id)),
        _ => (None, None, None),
    };
    McpIamGrant {
        grant_id: grant.id,
        resource_kind: grant_resource_kind_as_str(&grant.resource_kind).to_string(),
        resource_id: grant.resource_id,
        permission_kind: grant.permission_kind.clone(),
        workspace_id,
        library_id,
        document_id,
        granted_at: grant.granted_at,
    }
}

/// Compact membership view. The principal id and join/leave timestamps stay
/// in the domain row; an access check only needs the workspace and its state.
fn map_workspace_membership(membership: &WorkspaceMembership) -> McpIamMembership {
    McpIamMembership {
        workspace_id: membership.workspace_id,
        membership_state: membership.membership_state.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::{DateTime, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        ensure_system_admin, map_api_token, map_grant, map_principal, map_workspace_membership,
    };
    use crate::{
        domains::iam::{
            ApiToken, Grant, GrantResourceKind, Principal, PrincipalKind, WorkspaceMembership,
        },
        interfaces::http::{
            auth::{AuthContext, AuthGrant, AuthTokenKind},
            iam::{MintGrantScope, build_mint_grants, parse_iam_permission_kinds},
            router_support::ApiError,
        },
        mcp_types::{McpMintIamTokenRequest, McpUpdateIamTokenRequest},
    };

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::seconds(seconds)
    }

    fn auth(is_system_admin: bool) -> AuthContext {
        AuthContext {
            token_id: Uuid::from_u128(1),
            principal_id: Uuid::from_u128(2),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: AuthTokenKind::Session,
            scopes: Vec::new(),
            grants: Vec::new(),
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin,
            system_role: None,
        }
    }

    #[test]
    fn system_admin_gate_accepts_admins_only() {
        assert!(ensure_system_admin(&auth(true)).is_ok());
        assert!(matches!(ensure_system_admin(&auth(false)), Err(ApiError::Unauthorized)));
    }

    /// A token can carry `iam_admin` (and every other broad permission) and
    /// still must not reach the IAM administration tools: only the resolved
    /// system-admin flag opens that gate.
    #[test]
    fn strict_system_admin_gate_cannot_be_bypassed_by_grants() {
        let mut hostile = auth(false);
        hostile.grants = vec![
            AuthGrant {
                id: Uuid::from_u128(1),
                resource_kind: "workspace".to_string(),
                resource_id: Uuid::from_u128(101),
                permission_kind: "iam_admin".to_string(),
                workspace_id: Some(Uuid::from_u128(101)),
                library_id: None,
                document_id: None,
            },
            AuthGrant {
                id: Uuid::from_u128(2),
                resource_kind: "workspace".to_string(),
                resource_id: Uuid::from_u128(101),
                permission_kind: "connector_admin".to_string(),
                workspace_id: Some(Uuid::from_u128(101)),
                library_id: None,
                document_id: None,
            },
        ];

        assert!(
            matches!(ensure_system_admin(&hostile), Err(ApiError::Unauthorized)),
            "broad grants, including iam_admin, must not substitute for the system-admin flag"
        );
        assert!(ensure_system_admin(&auth(true)).is_ok());
    }

    #[test]
    fn mint_grants_are_exactly_the_requested_workspace_scope() {
        let workspace_id = Uuid::from_u128(42);
        let kinds =
            parse_iam_permission_kinds(vec!["library_read".to_string(), "query_run".to_string()])
                .expect("known permission kinds");

        let grants = build_mint_grants(MintGrantScope::Workspace(workspace_id), &kinds, None)
            .expect("workspace grants are allowed");

        let pairs = grants
            .iter()
            .map(|grant| (grant.permission_kind.as_str(), grant.resource_id))
            .collect::<Vec<_>>();
        assert_eq!(
            pairs,
            vec![("library_read", workspace_id), ("query_run", workspace_id)],
            "the minted token receives exactly the requested pairs and no extra kind"
        );
        assert!(
            grants.iter().all(|grant| grant.resource_kind == GrantResourceKind::Workspace),
            "workspace scope must not silently become a library grant"
        );
    }

    #[test]
    fn mint_grants_reject_a_permission_the_resource_cannot_carry() {
        let kinds = parse_iam_permission_kinds(vec!["iam_admin".to_string()])
            .expect("iam_admin is a known permission kind");
        let error =
            build_mint_grants(MintGrantScope::Libraries(vec![Uuid::from_u128(7)]), &kinds, None)
                .expect_err("a library grant cannot carry iam_admin");

        assert!(matches!(error, ApiError::BadRequest(_)));
    }

    #[test]
    fn unknown_permission_kind_is_rejected_with_its_value() {
        match parse_iam_permission_kinds(vec!["root_everything".to_string()]) {
            Err(ApiError::BadRequest(message)) => {
                assert!(message.contains("root_everything"), "message was: {message}");
            }
            other => panic!("unknown permission kind must be a client error, got {other:?}"),
        }
    }

    #[test]
    fn update_request_distinguishes_omitted_null_and_timestamp() {
        let token_id = Uuid::from_u128(5);

        let omitted: McpUpdateIamTokenRequest =
            serde_json::from_value(json!({ "tokenId": token_id })).expect("omitted expiry");
        assert_eq!(omitted.expires_at, None, "omitted expiresAt leaves the column untouched");

        let cleared: McpUpdateIamTokenRequest =
            serde_json::from_value(json!({ "tokenId": token_id, "expiresAt": null }))
                .expect("null expiry");
        assert_eq!(cleared.expires_at, Some(None), "null expiresAt clears the column");

        let set: McpUpdateIamTokenRequest =
            serde_json::from_value(json!({ "tokenId": token_id, "expiresAt": at(90) }))
                .expect("timestamp expiry");
        assert_eq!(set.expires_at, Some(Some(at(90))), "a timestamp sets the column");
    }

    #[test]
    fn mint_request_uses_camel_case_and_denies_unknown_fields() {
        let request: McpMintIamTokenRequest = serde_json::from_value(json!({
            "label": "ci",
            "workspaceId": Uuid::from_u128(8),
            "permissionKinds": ["library_read"]
        }))
        .expect("camelCase request");
        assert!(request.library_ids.is_empty(), "libraryIds defaults to empty");
        assert_eq!(request.permission_kinds, vec!["library_read"]);

        assert!(
            serde_json::from_value::<McpMintIamTokenRequest>(json!({
                "label": "ci",
                "library_ids": []
            }))
            .is_err(),
            "snake_case fields must be rejected"
        );
    }

    #[test]
    fn api_token_maps_to_camel_case_wire_shape() {
        let token = ApiToken {
            principal_id: Uuid::from_u128(3),
            workspace_id: Some(Uuid::from_u128(4)),
            label: "connector".to_string(),
            token_prefix: "irg_abc".to_string(),
            status: "active".to_string(),
            expires_at: Some(at(5_000)),
        };

        assert_eq!(
            serde_json::to_value(map_api_token(&token)).expect("serializable token"),
            json!({
                "principalId": "00000000-0000-0000-0000-000000000003",
                "workspaceId": "00000000-0000-0000-0000-000000000004",
                "label": "connector",
                "tokenPrefix": "irg_abc",
                "status": "active",
                "expiresAt": at(5_000)
            })
        );
    }

    #[test]
    fn principal_maps_display_label_to_display_name() {
        let principal = Principal {
            id: Uuid::from_u128(6),
            principal_kind: PrincipalKind::ApiToken,
            display_label: "Connector token".to_string(),
            status: "active".to_string(),
            created_at: at(10),
        };

        assert_eq!(
            serde_json::to_value(map_principal(&principal)).expect("serializable principal"),
            json!({
                "principalId": "00000000-0000-0000-0000-000000000006",
                "principalKind": "api_token",
                "displayName": "Connector token"
            })
        );
    }

    #[test]
    fn library_grant_lifts_its_scope_ids_from_the_resource() {
        let grant = Grant {
            id: Uuid::from_u128(7),
            principal_id: Uuid::from_u128(6),
            resource_kind: GrantResourceKind::Library,
            resource_id: Uuid::from_u128(8),
            permission_kind: "library_read".to_string(),
            granted_at: at(20),
        };

        assert_eq!(
            serde_json::to_value(map_grant(&grant)).expect("serializable grant"),
            json!({
                "grantId": "00000000-0000-0000-0000-000000000007",
                "resourceKind": "library",
                "resourceId": "00000000-0000-0000-0000-000000000008",
                "permissionKind": "library_read",
                "workspaceId": null,
                "libraryId": "00000000-0000-0000-0000-000000000008",
                "documentId": null,
                "grantedAt": at(20),
            })
        );
    }

    #[test]
    fn workspace_membership_maps_to_camel_case_wire_shape() {
        let membership = WorkspaceMembership {
            workspace_id: Uuid::from_u128(9),
            principal_id: Uuid::from_u128(6),
            membership_state: "active".to_string(),
            joined_at: at(30),
            ended_at: None,
        };

        assert_eq!(
            serde_json::to_value(map_workspace_membership(&membership))
                .expect("serializable membership"),
            json!({
                "workspaceId": "00000000-0000-0000-0000-000000000009",
                "membershipState": "active"
            })
        );
    }
}
