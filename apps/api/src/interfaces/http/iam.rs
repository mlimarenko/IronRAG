pub mod session;
mod source_key;
mod types;

mod access;
mod mappers;
pub(crate) mod oidc;
mod service_identities;
mod tokens;
mod users;

pub use access::*;
use mappers::*;
pub use service_identities::*;
pub use tokens::*;
pub use users::*;

use std::collections::{BTreeMap, BTreeSet};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{get, patch, post},
};
use sha2::{Digest, Sha256};
use tracing::{error, info, warn};
use uuid::Uuid;

use self::{
    session::{
        get_bootstrap_status, get_session, login_session, logout_session, resolve_session,
        setup_bootstrap_admin,
    },
    types::{
        CreateServiceIdentityRequest, CreateUserRequest, GrantResponse, IamGrantResourceKind,
        IamPermissionKind, IamPrincipalKind, ListServiceIdentitiesQuery, ListTokensQuery,
        MeResponse, MintTokenRequest, MintTokenResponse, PatchServiceIdentityRequest,
        PatchTokenRequest, PrincipalResponse, ServiceIdentityCredentialResponse,
        ServiceIdentityMutationResponse, ServiceIdentityOperationRequest,
        ServiceIdentityOwnerResponse, ServiceIdentityResponse, SetUserAccessRequest,
        SetUserRoleRequest, SystemRole, TokenGrantSummaryResponse, TokenIssuerResponse,
        TokenLibrarySummaryResponse, TokenResponse, TokenScopeKind, TokenScopeResponse,
        TokenWorkspaceSummaryResponse, UserAccessResponse, UserLibraryAccessResponse, UserResponse,
        UserWorkspaceAccessResponse, WorkspaceMembershipResponse,
    },
};
use crate::{
    app::state::AppState,
    domains::iam::{Grant, GrantResourceKind, WorkspaceMembership},
    infra::repositories::{catalog_repository, iam_repository},
    interfaces::http::{
        auth::AuthContext,
        router_support::{ApiError, RequestId, RestAuditEvent, record_rest_audit_event},
    },
    services::iam::{audit::AppendAuditEventSubjectCommand, service::CreateGrantCommand},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/iam/bootstrap", get(get_bootstrap_status).post(setup_bootstrap_admin))
        .route("/iam/sessions", post(login_session))
        .route("/iam/session", get(get_session).delete(logout_session))
        .route("/iam/context", get(resolve_session))
        .route("/iam/oidc/login", get(oidc::oidc_login))
        .route("/iam/oidc/callback", get(oidc::oidc_callback))
        .route("/iam/me", get(get_me))
        .route("/iam/users", get(list_users).post(create_user))
        .route("/iam/users/{userId}", patch(set_user_role).delete(delete_user))
        .route("/iam/users/{userId}/access", get(get_user_access).put(set_user_access))
        .route(
            "/iam/service-identities",
            get(list_service_identities).post(create_service_identity),
        )
        .route(
            "/iam/service-identities/{serviceIdentityId}",
            get(get_service_identity).patch(patch_service_identity).delete(delete_service_identity),
        )
        .route("/iam/service-identities/{serviceIdentityId}/rotate", post(rotate_service_identity))
        .route("/iam/service-identities/{serviceIdentityId}/revoke", post(revoke_service_identity))
        .route("/iam/tokens", get(list_tokens).post(mint_token))
        .route("/iam/tokens/{tokenId}", get(get_token).patch(patch_token).delete(delete_token))
}

#[utoipa::path(
    get,
    path = "/v1/iam/me",
    tag = "iam",
    operation_id = "getIamMe",
    responses(
        (status = 200, description = "Authenticated principal with effective grants and workspace memberships", body = MeResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "Principal not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_me", skip_all)]
pub async fn get_me(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<MeResponse>, ApiError> {
    let principal_row =
        iam_repository::get_principal_by_id(&state.persistence.postgres, auth.principal_id)
            .await
            .map_err(|error| {
                error!(
                    auth_principal_id = %auth.principal_id,
                    ?error,
                    "failed to load authenticated principal",
                );
                ApiError::Internal
            })?
            .ok_or_else(|| ApiError::resource_not_found("principal", auth.principal_id))?;

    let user_row =
        iam_repository::get_user_by_principal_id(&state.persistence.postgres, auth.principal_id)
            .await
            .map_err(|error| {
                error!(
                    auth_principal_id = %auth.principal_id,
                    ?error,
                    "failed to load authenticated user",
                );
                ApiError::Internal
            })?;

    let resolution =
        state.application_services.iam.resolve_effective_grants(&state, auth.principal_id).await?;

    Ok(Json(MeResponse {
        principal: map_principal_row(principal_row)?,
        user: user_row.map(map_user_row),
        workspace_memberships: resolution
            .workspace_memberships
            .into_iter()
            .map(map_membership_row)
            .collect(),
        effective_grants: resolution
            .grants
            .into_iter()
            .map(map_grant_domain)
            .collect::<Result<Vec<_>, _>>()?,
    }))
}

/// Loads the caller's user row and enforces the `admin` system role.
///
/// User management is gated on the *current principal's* role (not on the
/// grant-derived token scopes), matching the owner-confirmed RBAC model. API
/// tokens and non-user principals have no `iam_user` row and are therefore
/// rejected.
async fn require_system_admin(
    state: &AppState,
    auth: &AuthContext,
) -> Result<iam_repository::IamUserRow, ApiError> {
    let user =
        iam_repository::get_user_by_principal_id(&state.persistence.postgres, auth.principal_id)
            .await
            .map_err(|error| {
                error!(
                    auth_principal_id = %auth.principal_id,
                    ?error,
                    "failed to load caller for system-admin check",
                );
                ApiError::Internal
            })?
            .ok_or(ApiError::Unauthorized)?;
    if user.system_role() != iam_repository::SystemRole::Admin {
        return Err(ApiError::Unauthorized);
    }
    Ok(user)
}

fn resolve_workspace_filter(
    auth: &AuthContext,
    requested: Option<Uuid>,
) -> Result<Option<Uuid>, ApiError> {
    if auth.is_system_admin {
        return Ok(requested);
    }

    match requested {
        Some(workspace_id) => {
            authorize_workspace_scope_for_id(auth, workspace_id)?;
            Ok(Some(workspace_id))
        }
        None if auth.visible_workspace_ids.len() == 1 => {
            let workspace_id =
                auth.visible_workspace_ids.iter().copied().next().ok_or(ApiError::Unauthorized)?;
            authorize_workspace_scope_for_id(auth, workspace_id)?;
            Ok(Some(workspace_id))
        }
        None => {
            let workspace_id = auth
                .workspace_id
                .filter(|workspace_id| auth.can_access_workspace(*workspace_id))
                .ok_or(ApiError::Unauthorized)?;
            authorize_workspace_scope_for_id(auth, workspace_id)?;
            Ok(Some(workspace_id))
        }
    }
}

fn authorize_workspace_scope_for_id(
    auth: &AuthContext,
    _workspace_id: Uuid,
) -> Result<(), ApiError> {
    if auth.is_system_admin { Ok(()) } else { Err(ApiError::Unauthorized) }
}

fn authorize_workspace_scope_for_row(
    auth: &AuthContext,
    workspace_id: Option<Uuid>,
) -> Result<(), ApiError> {
    match workspace_id {
        Some(workspace_id) => authorize_workspace_scope_for_id(auth, workspace_id),
        None if auth.is_system_admin => Ok(()),
        None => Err(ApiError::Unauthorized),
    }
}

/// Decides whether a permission may be granted on a resource kind.
///
/// Only the two *creatable* resource kinds carry a permission allow-list. Every
/// other kind of the `IamGrantResourceKind` vocabulary is denied outright,
/// because no write path can ever construct a grant for it:
///
/// * `PUT /iam/users/{userId}/access` and `POST /iam/tokens` are the only two
///   grant-creating routes, and both pass a hardcoded [`IamGrantResourceKind`]
///   literal — `Workspace` or `Library` — to this function;
/// * no HTTP request body deserializes a resource kind (`resource_kind` appears
///   only on the `Serialize`-only `GrantResponse` and `TokenGrantSummaryResponse`),
///   so a caller cannot smuggle one in;
/// * this function is module-private, so the call sites above are the complete
///   set.
///
/// The remaining kinds therefore exist only as *response* vocabulary — a
/// pre-existing row read out of the database still has to render — and the deny
/// arm below is what keeps the writable surface equal to the permissive
/// surface. `grant_write_paths_create_only_creatable_resource_kinds` in
/// `iam/tests.rs` fails if a write path ever starts passing one of them; deleting
/// the arm's guarantee then has to be a deliberate act, not an accident.
///
/// The match stays exhaustive and the deny arm names every kind explicitly, so
/// adding or removing an enum variant is a compile error rather than a silent
/// change of policy.
fn validate_permission_kind_for_resource(
    resource_kind: IamGrantResourceKind,
    permission_kind: IamPermissionKind,
) -> Result<(), ApiError> {
    let allowed = match resource_kind {
        IamGrantResourceKind::Workspace => {
            matches!(
                permission_kind,
                IamPermissionKind::WorkspaceAdmin
                    | IamPermissionKind::WorkspaceRead
                    | IamPermissionKind::LibraryRead
                    | IamPermissionKind::LibraryWrite
                    | IamPermissionKind::DocumentRead
                    | IamPermissionKind::DocumentWrite
                    | IamPermissionKind::ConnectorAdmin
                    | IamPermissionKind::CredentialAdmin
                    | IamPermissionKind::BindingAdmin
                    | IamPermissionKind::QueryRun
                    | IamPermissionKind::OpsRead
                    | IamPermissionKind::AuditRead
            )
        }
        IamGrantResourceKind::Library => {
            matches!(
                permission_kind,
                IamPermissionKind::LibraryRead
                    | IamPermissionKind::LibraryWrite
                    | IamPermissionKind::DocumentRead
                    | IamPermissionKind::DocumentWrite
                    | IamPermissionKind::ConnectorAdmin
                    | IamPermissionKind::BindingAdmin
                    | IamPermissionKind::QueryRun
            )
        }
        // Not creatable by any write path — see the doc comment. Denying them
        // here is not a restriction of a working feature; it is the statement
        // that no feature exists, kept enforceable by the source-level contract
        // test.
        IamGrantResourceKind::System
        | IamGrantResourceKind::Document
        | IamGrantResourceKind::QuerySession
        | IamGrantResourceKind::AsyncOperation
        | IamGrantResourceKind::Connector
        | IamGrantResourceKind::ProviderCredential
        | IamGrantResourceKind::LibraryBinding => false,
    };

    if allowed {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "permission_kind '{}' is not valid for resource_kind '{}'",
            permission_kind.as_str(),
            resource_kind.as_str()
        )))
    }
}

pub(crate) async fn load_contract_me(
    state: &AppState,
    auth: &AuthContext,
) -> Result<ironrag_contracts::auth::IamMe, ApiError> {
    let principal_row =
        iam_repository::get_principal_by_id(&state.persistence.postgres, auth.principal_id)
            .await
            .map_err(|error| {
                error!(
                    auth_principal_id = %auth.principal_id,
                    ?error,
                    "failed to load authenticated principal",
                );
                ApiError::Internal
            })?
            .ok_or_else(|| ApiError::resource_not_found("principal", auth.principal_id))?;

    let user_row =
        iam_repository::get_user_by_principal_id(&state.persistence.postgres, auth.principal_id)
            .await
            .map_err(|error| {
                error!(
                    auth_principal_id = %auth.principal_id,
                    ?error,
                    "failed to load authenticated user",
                );
                ApiError::Internal
            })?;

    let resolution =
        state.application_services.iam.resolve_effective_grants(state, auth.principal_id).await?;

    Ok(ironrag_contracts::auth::IamMe {
        principal: map_principal_row_contract(principal_row)?,
        user: user_row.map(map_user_row_contract),
        workspace_memberships: resolution
            .workspace_memberships
            .into_iter()
            .map(map_membership_row_contract)
            .collect(),
        effective_grants: resolution
            .grants
            .into_iter()
            .map(map_grant_domain_contract)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

#[cfg(test)]
mod tests;
