pub mod session;
mod types;

use std::collections::{BTreeMap, BTreeSet};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{delete, get, patch, post},
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
        CreateUserRequest, GrantResponse, IamGrantResourceKind, IamPermissionKind,
        IamPrincipalKind, ListTokensQuery, MeResponse, MintTokenRequest, MintTokenResponse,
        PatchTokenRequest, PrincipalResponse, SetUserAccessRequest, SetUserRoleRequest, SystemRole,
        TokenGrantSummaryResponse, TokenIssuerResponse, TokenLibrarySummaryResponse, TokenResponse,
        TokenScopeKind, TokenScopeResponse, TokenWorkspaceSummaryResponse, UserAccessResponse,
        UserLibraryAccessResponse, UserResponse, UserWorkspaceAccessResponse,
        WorkspaceMembershipResponse,
    },
};
use crate::{
    app::state::AppState,
    domains::iam::{Grant, GrantResourceKind, WorkspaceMembership},
    infra::repositories::{catalog_repository, iam_repository},
    interfaces::http::{
        auth::AuthContext,
        authorization::POLICY_IAM_ADMIN,
        router_support::{ApiError, RequestId},
    },
    services::iam::{
        audit::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
        service::CreateGrantCommand,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/iam/bootstrap/status", get(get_bootstrap_status))
        .route("/iam/bootstrap/setup", post(setup_bootstrap_admin))
        .route("/iam/session/login", post(login_session))
        .route("/iam/session/resolve", get(resolve_session))
        .route("/iam/session", get(get_session))
        .route("/iam/session/logout", post(logout_session))
        .route("/iam/me", get(get_me))
        .route("/iam/users", get(list_users).post(create_user))
        .route("/iam/users/{userId}", delete(delete_user))
        .route("/iam/users/{userId}/role", patch(set_user_role))
        .route("/iam/users/{userId}/access", get(get_user_access).put(set_user_access))
        .route("/iam/tokens", get(list_tokens).post(mint_token))
        .route("/iam/tokens/{tokenId}", get(get_token).patch(patch_token).delete(delete_token))
        .route("/iam/tokens/{tokenId}/revoke", post(revoke_token))
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
        state.canonical_services.iam.resolve_effective_grants(&state, auth.principal_id).await?;

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

/// Pure last-admin guard.
///
/// Returns `true` when changing `current_role` to `next_role` would remove the
/// final administrator (i.e. the caller is demoting an admin and `admin_count`
/// — the number of admins counted *before* the change — is at most one). A
/// no-op `admin → admin` update never trips the guard.
fn would_demote_last_admin(
    current_role: iam_repository::SystemRole,
    next_role: iam_repository::SystemRole,
    admin_count: i64,
) -> bool {
    current_role == iam_repository::SystemRole::Admin
        && next_role != iam_repository::SystemRole::Admin
        && admin_count <= 1
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

#[utoipa::path(
    get,
    path = "/v1/iam/users",
    tag = "iam",
    operation_id = "listIamUsers",
    responses(
        (status = 200, description = "All user principals with their system roles", body = [UserResponse]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.list_users", skip_all)]
pub async fn list_users(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<Vec<UserResponse>>, ApiError> {
    require_system_admin(&state, &auth).await?;

    let rows = iam_repository::list_users(&state.persistence.postgres).await.map_err(|error| {
        error!(auth_principal_id = %auth.principal_id, ?error, "failed to list users");
        ApiError::Internal
    })?;

    Ok(Json(rows.into_iter().map(map_user_row).collect()))
}

#[utoipa::path(
    post,
    path = "/v1/iam/users",
    tag = "iam",
    operation_id = "createIamUser",
    request_body = crate::interfaces::http::iam::types::CreateUserRequest,
    responses(
        (status = 200, description = "Newly created user", body = UserResponse),
        (status = 400, description = "Invalid request payload"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 409, description = "Login or email already exists"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.create_user", skip_all)]
pub async fn create_user(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateUserRequest>,
) -> Result<Json<UserResponse>, ApiError> {
    require_system_admin(&state, &auth).await?;
    let request_id = request_id.map(|value| value.0.0);

    let login = payload.login.trim().to_ascii_lowercase();
    if login.is_empty() {
        return Err(ApiError::BadRequest("login must not be empty".into()));
    }
    if login.bytes().any(|byte| !matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-')) {
        return Err(ApiError::BadRequest(
            "login must contain only lowercase letters, digits, '.', '_' or '-'".into(),
        ));
    }
    let email = payload.email.trim().to_string();
    if email.is_empty() {
        return Err(ApiError::BadRequest("email must not be empty".into()));
    }
    let display_name = {
        let trimmed = payload.display_name.trim();
        if trimmed.is_empty() { login.clone() } else { trimmed.to_string() }
    };
    let password = zeroize::Zeroizing::new(payload.password.trim().to_string());
    if password.len() < 8 {
        return Err(ApiError::BadRequest("password must be at least 8 characters long".into()));
    }

    if iam_repository::get_user_by_login(&state.persistence.postgres, &login)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, ?error, "failed to check login uniqueness");
            ApiError::Internal
        })?
        .is_some()
    {
        return Err(ApiError::Conflict("a user with this login already exists".to_string()));
    }
    if iam_repository::get_user_by_email(&state.persistence.postgres, &email)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, ?error, "failed to check email uniqueness");
            ApiError::Internal
        })?
        .is_some()
    {
        return Err(ApiError::Conflict("a user with this email already exists".to_string()));
    }

    let password_hash = crate::services::iam::service::hash_password(&password)?;
    let role = map_route_system_role_to_repo(payload.role);
    let row = iam_repository::create_user(
        &state.persistence.postgres,
        &login,
        &email,
        &display_name,
        &password_hash,
        role,
    )
    .await
    .map_err(|error| {
        error!(auth_principal_id = %auth.principal_id, ?error, "failed to create user");
        ApiError::Internal
    })?;

    record_iam_audit_event(
        &state,
        &auth,
        request_id,
        "iam.user.create",
        "succeeded",
        Some(format!("user {} created", row.login)),
        Some(format!(
            "principal {} created user {} with role {}",
            auth.principal_id, row.principal_id, row.role,
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "principal".to_string(),
            subject_id: row.principal_id,
            workspace_id: None,
            library_id: None,
            document_id: None,
        }],
    )
    .await;

    Ok(Json(map_user_row(row)))
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_user",
    skip_all,
    fields(principal_id = %principal_id)
)]
#[utoipa::path(
    delete,
    path = "/v1/iam/users/{userId}",
    tag = "iam",
    operation_id = "deleteIamUser",
    params(("userId" = uuid::Uuid, Path, description = "User principal id to delete")),
    responses(
        (status = 204, description = "User deleted (sessions and grants revoked, principal disabled)"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "User not found"),
        (status = 409, description = "Would delete the last remaining administrator"),
    ),
)]
pub async fn delete_user(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(principal_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_system_admin(&state, &auth).await?;
    let request_id = request_id.map(|value| value.0.0);

    let target =
        iam_repository::get_user_by_principal_id(&state.persistence.postgres, principal_id)
            .await
            .map_err(|error| {
                error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to load user for delete");
                ApiError::Internal
            })?
            .ok_or_else(|| ApiError::resource_not_found("user", principal_id))?;

    // Last-admin guard, mirroring `set_user_role` below: counting active
    // admins before the delete keeps the check accurate even though this
    // isn't a no-op-tolerant update.
    if target.system_role() == iam_repository::SystemRole::Admin {
        let admin_count = iam_repository::count_admin_users(&state.persistence.postgres)
            .await
            .map_err(|error| {
                error!(auth_principal_id = %auth.principal_id, ?error, "failed to count admins");
                ApiError::Internal
            })?;
        if admin_count <= 1 {
            return Err(ApiError::Conflict(
                "cannot delete the last remaining administrator".to_string(),
            ));
        }
    }

    let deleted = iam_repository::delete_user(&state.persistence.postgres, principal_id)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to delete user");
            ApiError::Internal
        })?;
    if deleted.is_none() {
        return Err(ApiError::resource_not_found("user", principal_id));
    }

    record_iam_audit_event(
        &state,
        &auth,
        request_id,
        "iam.user.delete",
        "succeeded",
        Some(format!("user {} deleted", target.login)),
        Some(format!("principal {} deleted user {}", auth.principal_id, principal_id)),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "principal".to_string(),
            subject_id: principal_id,
            workspace_id: None,
            library_id: None,
            document_id: None,
        }],
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    patch,
    path = "/v1/iam/users/{userId}/role",
    tag = "iam",
    operation_id = "setIamUserRole",
    params(("userId" = uuid::Uuid, Path, description = "User principal id whose role changes")),
    request_body = crate::interfaces::http::iam::types::SetUserRoleRequest,
    responses(
        (status = 200, description = "Updated user", body = UserResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "User not found"),
        (status = 409, description = "Would demote the last remaining administrator"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.set_user_role",
    skip_all,
    fields(principal_id = %principal_id)
)]
pub async fn set_user_role(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(principal_id): Path<Uuid>,
    Json(payload): Json<SetUserRoleRequest>,
) -> Result<Json<UserResponse>, ApiError> {
    require_system_admin(&state, &auth).await?;
    let request_id = request_id.map(|value| value.0.0);
    let next_role = map_route_system_role_to_repo(payload.role);

    let current = iam_repository::get_user_by_principal_id(&state.persistence.postgres, principal_id)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to load user for role change");
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::resource_not_found("user", principal_id))?;

    // Last-admin guard: blocking the demotion of the final administrator keeps
    // the instance manageable. Counting before the update keeps the check
    // idempotent (a no-op admin→admin update is always allowed).
    if current.system_role() == iam_repository::SystemRole::Admin
        && next_role != iam_repository::SystemRole::Admin
    {
        let admin_count = iam_repository::count_admin_users(&state.persistence.postgres)
            .await
            .map_err(|error| {
                error!(auth_principal_id = %auth.principal_id, ?error, "failed to count admins");
                ApiError::Internal
            })?;
        if would_demote_last_admin(current.system_role(), next_role, admin_count) {
            return Err(ApiError::Conflict(
                "cannot demote the last remaining administrator".to_string(),
            ));
        }
    }

    let row = iam_repository::set_user_role(&state.persistence.postgres, principal_id, next_role)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to set user role");
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::resource_not_found("user", principal_id))?;

    record_iam_audit_event(
        &state,
        &auth,
        request_id,
        "iam.user.set_role",
        "succeeded",
        Some(format!("user {} role set to {}", row.login, row.role)),
        Some(format!(
            "principal {} set user {} role to {}",
            auth.principal_id, principal_id, row.role
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "principal".to_string(),
            subject_id: principal_id,
            workspace_id: None,
            library_id: None,
            document_id: None,
        }],
    )
    .await;

    Ok(Json(map_user_row(row)))
}

#[utoipa::path(
    get,
    path = "/v1/iam/users/{userId}/access",
    tag = "iam",
    operation_id = "getIamUserAccess",
    params(("userId" = uuid::Uuid, Path, description = "User principal id whose access is read")),
    responses(
        (status = 200, description = "The user's workspace and library access grants. Carries an `ETag` header (a hash of the current grant set) for optimistic concurrency on the paired PUT.", body = UserAccessResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "User not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_user_access",
    skip_all,
    fields(principal_id = %principal_id)
)]
pub async fn get_user_access(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(principal_id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    require_system_admin(&state, &auth).await?;

    // Confirm the target principal is a real user so callers get a clean 404
    // rather than an empty access list for a typo'd id.
    iam_repository::get_user_by_principal_id(&state.persistence.postgres, principal_id)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to load user for access read");
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::resource_not_found("user", principal_id))?;

    let access = load_user_access(&state, principal_id).await?;
    Ok(with_access_etag(access))
}

#[utoipa::path(
    put,
    path = "/v1/iam/users/{userId}/access",
    tag = "iam",
    operation_id = "setIamUserAccess",
    params(("userId" = uuid::Uuid, Path, description = "User principal id whose access is set")),
    request_body = crate::interfaces::http::iam::types::SetUserAccessRequest,
    responses(
        (status = 200, description = "The user's access after reconciliation. Carries a fresh `ETag` header.", body = UserAccessResponse),
        (status = 400, description = "Invalid permission kind for a resource"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "User, workspace or library not found"),
        (status = 409, description = "The optional `If-Match` header did not match the current access ETag; re-fetch with GET and retry"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.set_user_access",
    skip_all,
    fields(principal_id = %principal_id)
)]
pub async fn set_user_access(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(principal_id): Path<Uuid>,
    headers: HeaderMap,
    Json(payload): Json<SetUserAccessRequest>,
) -> Result<axum::response::Response, ApiError> {
    require_system_admin(&state, &auth).await?;
    let request_id = request_id.map(|value| value.0.0);

    iam_repository::get_user_by_principal_id(&state.persistence.postgres, principal_id)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to load user for access write");
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::resource_not_found("user", principal_id))?;

    // Optional optimistic-concurrency check: if the caller sent `If-Match`,
    // the access set must not have changed since they last read it via GET.
    // `*` always passes (the resource is known to exist by this point). This
    // is the single write model's race guard (plan §5.2) — no parallel
    // grants write path is needed to close it.
    if let Some(if_match) = headers.get(header::IF_MATCH).and_then(|value| value.to_str().ok()) {
        let if_match = if_match.trim();
        if if_match != "*" {
            let current_access = load_user_access(&state, principal_id).await?;
            let current_etag = compute_access_etag(&current_access);
            if if_match != current_etag {
                return Err(ApiError::StaleRevision(format!(
                    "user access ETag is stale for principal {principal_id}; GET the latest access and retry the PUT with the current If-Match value"
                )));
            }
        }
    }

    // Build the desired (resource_kind, resource_id, permission_kind) set,
    // validating each permission against its resource kind and that each
    // referenced workspace/library exists.
    let mut desired: BTreeSet<(&'static str, Uuid, String)> = BTreeSet::new();
    for entry in &payload.workspaces {
        validate_permission_kind_for_resource(
            IamGrantResourceKind::Workspace,
            entry.permission_kind.clone(),
        )?;
        catalog_repository::get_workspace_by_id(&state.persistence.postgres, entry.workspace_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("workspace", entry.workspace_id))?;
        desired.insert((
            "workspace",
            entry.workspace_id,
            entry.permission_kind.as_str().to_string(),
        ));
    }
    for entry in &payload.libraries {
        validate_permission_kind_for_resource(
            IamGrantResourceKind::Library,
            entry.permission_kind.clone(),
        )?;
        catalog_repository::get_library_by_id(&state.persistence.postgres, entry.library_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("library", entry.library_id))?;
        desired.insert(("library", entry.library_id, entry.permission_kind.as_str().to_string()));
    }

    // Reconcile against the user's existing workspace/library grants. Only these
    // two resource kinds are managed here; system/document/etc. grants are left
    // untouched so this endpoint cannot be used to escalate to admin.
    let existing = iam_repository::list_grants_by_principal(&state.persistence.postgres, principal_id)
        .await
        .map_err(|error| {
            error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to list grants for access reconcile");
            ApiError::Internal
        })?;

    let mut keep: BTreeSet<(&'static str, Uuid, String)> = BTreeSet::new();
    for grant in &existing {
        let kind = match grant.resource_kind.as_str() {
            "workspace" => "workspace",
            "library" => "library",
            _ => continue,
        };
        let key = (kind, grant.resource_id, grant.permission_kind.clone());
        if desired.contains(&key) {
            keep.insert(key);
        } else {
            // No longer desired: revoke it.
            state.canonical_services.iam.revoke_grant(&state, grant.id).await.map_err(|error| {
                error!(auth_principal_id = %auth.principal_id, %principal_id, grant_id = %grant.id, ?error, "failed to revoke grant during access reconcile");
                error
            })?;
        }
    }

    for (kind, resource_id, permission_kind) in &desired {
        if keep.contains(&(*kind, *resource_id, permission_kind.clone())) {
            continue;
        }
        let resource_kind = match *kind {
            "workspace" => GrantResourceKind::Workspace,
            _ => GrantResourceKind::Library,
        };
        state
            .canonical_services
            .iam
            .create_grant(
                &state,
                CreateGrantCommand {
                    principal_id,
                    resource_kind,
                    resource_id: *resource_id,
                    permission_kind: permission_kind.clone(),
                    granted_by_principal_id: Some(auth.principal_id),
                    expires_at: None,
                },
            )
            .await
            .map_err(|error| {
                error!(auth_principal_id = %auth.principal_id, %principal_id, ?error, "failed to create grant during access reconcile");
                ApiError::Internal
            })?;
    }

    record_iam_audit_event(
        &state,
        &auth,
        request_id,
        "iam.user.set_access",
        "succeeded",
        Some("user access updated".to_string()),
        Some(format!(
            "principal {} set access for user {} ({} workspace, {} library grants)",
            auth.principal_id,
            principal_id,
            payload.workspaces.len(),
            payload.libraries.len(),
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "principal".to_string(),
            subject_id: principal_id,
            workspace_id: None,
            library_id: None,
            document_id: None,
        }],
    )
    .await;

    let access = load_user_access(&state, principal_id).await?;
    Ok(with_access_etag(access))
}

/// Wraps a `UserAccessResponse` body with an `ETag` header computed from its
/// current grant set. Shared by the GET and PUT handlers so the header a
/// client reads always matches what `compute_access_etag` would recompute
/// from that exact body.
fn with_access_etag(access: UserAccessResponse) -> axum::response::Response {
    let etag = compute_access_etag(&access);
    let mut response = Json(access).into_response();
    if let Ok(value) = HeaderValue::from_str(&etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

/// Computes a stable ETag for a user's declarative access set (workspace and
/// library grants), independent of iteration order and of `grantId` (an
/// internal correlation id, not semantically part of "access"). Two calls
/// with the same effective grant set always produce the same ETag.
fn compute_access_etag(access: &UserAccessResponse) -> String {
    let mut entries: Vec<(&'static str, Uuid, &str)> = access
        .workspaces
        .iter()
        .map(|entry| ("workspace", entry.workspace_id, entry.permission_kind.as_str()))
        .chain(
            access
                .libraries
                .iter()
                .map(|entry| ("library", entry.library_id, entry.permission_kind.as_str())),
        )
        .collect();
    entries.sort();

    let mut hasher = Sha256::new();
    hasher.update(access.principal_id.as_bytes());
    for (kind, resource_id, permission_kind) in entries {
        hasher.update(kind.as_bytes());
        hasher.update(b"|");
        hasher.update(resource_id.as_bytes());
        hasher.update(b"|");
        hasher.update(permission_kind.as_bytes());
        hasher.update(b"\n");
    }
    format!("\"{}\"", hex::encode(hasher.finalize()))
}

/// Loads a user's workspace- and library-scoped grants, joined with display
/// names, for the per-user access editor.
async fn load_user_access(
    state: &AppState,
    principal_id: Uuid,
) -> Result<UserAccessResponse, ApiError> {
    let grants = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        principal_id,
    )
    .await
    .map_err(|error| {
        error!(%principal_id, ?error, "failed to resolve grants for user access");
        ApiError::Internal
    })?;

    let workspace_rows = catalog_repository::list_workspaces(&state.persistence.postgres)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let workspace_names: BTreeMap<Uuid, String> =
        workspace_rows.into_iter().map(|row| (row.id, row.display_name)).collect();
    let library_rows = catalog_repository::list_libraries(&state.persistence.postgres, None)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let library_names: BTreeMap<Uuid, (Uuid, String)> = library_rows
        .into_iter()
        .map(|row| (row.id, (row.workspace_id, row.display_name)))
        .collect();

    let mut workspaces = Vec::new();
    let mut libraries = Vec::new();
    for grant in grants {
        match grant.resource_kind.as_str() {
            "workspace" => {
                let display_name = workspace_names
                    .get(&grant.resource_id)
                    .cloned()
                    .unwrap_or_else(|| grant.resource_id.to_string());
                workspaces.push(UserWorkspaceAccessResponse {
                    grant_id: grant.id,
                    workspace_id: grant.resource_id,
                    display_name,
                    permission_kind: map_permission_kind(&grant.permission_kind)?,
                });
            }
            "library" => {
                let (workspace_id, display_name) =
                    library_names.get(&grant.resource_id).cloned().unwrap_or_else(|| {
                        (grant.workspace_id.unwrap_or_default(), grant.resource_id.to_string())
                    });
                libraries.push(UserLibraryAccessResponse {
                    grant_id: grant.id,
                    library_id: grant.resource_id,
                    workspace_id,
                    display_name,
                    permission_kind: map_permission_kind(&grant.permission_kind)?,
                });
            }
            _ => {}
        }
    }

    Ok(UserAccessResponse { principal_id, workspaces, libraries })
}

#[utoipa::path(
    get,
    path = "/v1/iam/tokens",
    tag = "iam",
    operation_id = "listIamTokens",
    params(crate::interfaces::http::iam::types::ListTokensQuery),
    responses(
        (status = 200, description = "API tokens visible to the IAM administrator", body = [TokenResponse]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not an IAM administrator"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_tokens",
    skip_all,
    fields(workspace_id = ?query.workspace_id, item_count)
)]
pub async fn list_tokens(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListTokensQuery>,
) -> Result<Json<Vec<TokenResponse>>, ApiError> {
    let span = tracing::Span::current();
    auth.require_any_scope(POLICY_IAM_ADMIN)?;
    let workspace_filter = resolve_workspace_filter(&auth, query.workspace_id)?;

    let rows = iam_repository::list_api_tokens(&state.persistence.postgres, workspace_filter)
        .await
        .map_err(|error| {
            error!(
                auth_principal_id = %auth.principal_id,
                workspace_id = ?workspace_filter,
                ?error,
                "failed to list api tokens",
            );
            ApiError::Internal
        })?;

    info!(
        auth_principal_id = %auth.principal_id,
        requested_workspace_id = ?workspace_filter,
        token_count = rows.len(),
        "listed api tokens",
    );

    let principal_ids = rows.iter().map(|row| row.principal_id).collect::<Vec<_>>();
    let grant_rows = iam_repository::list_resolved_grants_by_principal_ids(
        &state.persistence.postgres,
        &principal_ids,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            requested_workspace_id = ?workspace_filter,
            ?error,
            "failed to resolve api token grants",
        );
        ApiError::Internal
    })?;
    let lookups = load_token_response_lookups(&state, &rows, &grant_rows).await?;
    let mut grants_by_principal =
        BTreeMap::<Uuid, Vec<iam_repository::ResolvedIamGrantScopeRow>>::new();
    for grant_row in grant_rows {
        grants_by_principal.entry(grant_row.principal_id).or_default().push(grant_row);
    }
    let items = rows
        .into_iter()
        .map(|row| {
            let principal_id = row.principal_id;
            build_token_response(
                row,
                grants_by_principal.remove(&principal_id).unwrap_or_default(),
                &lookups,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    span.record("item_count", items.len());
    Ok(Json(items))
}

#[utoipa::path(
    post,
    path = "/v1/iam/tokens",
    tag = "iam",
    operation_id = "mintIamToken",
    request_body = crate::interfaces::http::iam::types::MintTokenRequest,
    responses(
        (status = 200, description = "Newly minted API token (plaintext only returned once)", body = MintTokenResponse),
        (status = 400, description = "Invalid request payload"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not an IAM administrator"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.mint_token",
    skip_all,
    fields(workspace_id = ?payload.workspace_id)
)]
pub async fn mint_token(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<MintTokenRequest>,
) -> Result<Json<MintTokenResponse>, ApiError> {
    auth.require_any_scope(POLICY_IAM_ADMIN)?;
    let mut workspace_id = resolve_mint_workspace(&auth, payload.workspace_id)?;
    let expires_at = payload.expires_at;

    if payload.label.trim().is_empty() {
        return Err(ApiError::BadRequest("label must not be empty".into()));
    }
    let library_ids = dedupe_uuids(payload.library_ids);
    let permission_kinds = dedupe_permissions(payload.permission_kinds);

    if !library_ids.is_empty() && permission_kinds.is_empty() {
        return Err(ApiError::BadRequest(
            "permissionKinds must be provided when libraryIds are present".into(),
        ));
    }

    if !library_ids.is_empty() {
        let mut library_workspace_ids = BTreeSet::new();
        for library_id in &library_ids {
            let library =
                catalog_repository::get_library_by_id(&state.persistence.postgres, *library_id)
                    .await
                    .map_err(|error| {
                        error!(
                            auth_principal_id = %auth.principal_id,
                            library_id = %library_id,
                            ?error,
                            "failed to load library for iam token mint",
                        );
                        ApiError::Internal
                    })?
                    .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;

            if let Some(requested_workspace_id) = workspace_id
                && library.workspace_id != requested_workspace_id
            {
                return Err(ApiError::BadRequest(format!(
                    "library {} does not belong to workspace {}",
                    library.id, requested_workspace_id
                )));
            }
            library_workspace_ids.insert(library.workspace_id);
        }

        if workspace_id.is_none() && library_workspace_ids.len() == 1 {
            workspace_id = library_workspace_ids.iter().copied().next();
        }
    }
    let grants = if permission_kinds.is_empty() {
        Vec::new()
    } else if !library_ids.is_empty() {
        build_mint_grants(MintGrantScope::Libraries(library_ids), &permission_kinds, expires_at)?
    } else {
        let workspace_id = workspace_id.ok_or_else(|| {
            ApiError::BadRequest(
                "workspaceId is required when permissionKinds are provided without libraryIds"
                    .to_string(),
            )
        })?;
        build_mint_grants(MintGrantScope::Workspace(workspace_id), &permission_kinds, expires_at)?
    };

    let mut outcome = state
        .canonical_services
        .iam
        .mint_api_token(
            &state,
            crate::services::iam::service::MintApiTokenCommand {
                workspace_id,
                label: payload.label,
                expires_at,
                grants,
                issued_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;

    let row = iam_repository::get_api_token_by_principal_id(
        &state.persistence.postgres,
        outcome.api_token.principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            api_token_principal_id = %outcome.api_token.principal_id,
            ?error,
            "failed to reload minted api token",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("api_token", outcome.api_token.principal_id))?;
    record_iam_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "iam.api_token.mint",
        "succeeded",
        Some(format!("api token {} minted", row.label)),
        Some(format!("principal {} minted api token {}", auth.principal_id, row.principal_id)),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "api_token".to_string(),
            subject_id: row.principal_id,
            workspace_id: row.workspace_id,
            library_id: None,
            document_id: None,
        }],
    )
    .await;

    let grant_rows = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        row.principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            api_token_principal_id = %row.principal_id,
            ?error,
            "failed to resolve minted api token grants",
        );
        ApiError::Internal
    })?;
    let lookups =
        load_token_response_lookups(&state, std::slice::from_ref(&row), &grant_rows).await?;
    let api_token = build_token_response(row, grant_rows, &lookups)?;

    Ok(Json(MintTokenResponse { token: std::mem::take(&mut outcome.token), api_token }))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_token",
    skip_all,
    fields(token_principal_id = %token_principal_id)
)]
#[utoipa::path(
    get,
    path = "/v1/iam/tokens/{tokenId}",
    tag = "iam",
    operation_id = "getIamToken",
    params(("tokenId" = uuid::Uuid, Path, description = "API token principal id")),
    responses(
        (status = 200, description = "API token detail (the plaintext secret is never returned)", body = TokenResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not an IAM administrator"),
        (status = 404, description = "Token not found"),
    ),
)]
pub async fn get_token(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(token_principal_id): Path<Uuid>,
) -> Result<Json<TokenResponse>, ApiError> {
    auth.require_any_scope(POLICY_IAM_ADMIN)?;

    let row = iam_repository::get_api_token_by_principal_id(
        &state.persistence.postgres,
        token_principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to load api token",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;

    authorize_workspace_scope_for_row(&auth, row.workspace_id)?;

    let grant_rows = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        token_principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to resolve api token grants",
        );
        ApiError::Internal
    })?;
    let lookups =
        load_token_response_lookups(&state, std::slice::from_ref(&row), &grant_rows).await?;
    Ok(Json(build_token_response(row, grant_rows, &lookups)?))
}

#[tracing::instrument(
    level = "info",
    name = "http.patch_token",
    skip_all,
    fields(token_principal_id = %token_principal_id)
)]
#[utoipa::path(
    patch,
    path = "/v1/iam/tokens/{tokenId}",
    tag = "iam",
    operation_id = "patchIamToken",
    params(("tokenId" = uuid::Uuid, Path, description = "API token principal id")),
    request_body = crate::interfaces::http::iam::types::PatchTokenRequest,
    responses(
        (status = 200, description = "Updated API token detail", body = TokenResponse),
        (status = 400, description = "Invalid request payload"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not an IAM administrator"),
        (status = 404, description = "Token not found"),
    ),
)]
pub async fn patch_token(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(token_principal_id): Path<Uuid>,
    Json(payload): Json<PatchTokenRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    auth.require_any_scope(POLICY_IAM_ADMIN)?;
    let request_id = request_id.map(|value| value.0.0);

    let existing = iam_repository::get_api_token_by_principal_id(
        &state.persistence.postgres,
        token_principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to load api token for patch",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;
    authorize_workspace_scope_for_row(&auth, existing.workspace_id)?;

    let label = match payload.label {
        Some(label) => {
            let trimmed = label.trim();
            if trimmed.is_empty() {
                return Err(ApiError::BadRequest("label must not be empty".into()));
            }
            Some(trimmed.to_string())
        }
        None => None,
    };

    let row = iam_repository::update_api_token(
        &state.persistence.postgres,
        token_principal_id,
        label.as_deref(),
        payload.expires_at,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to update api token",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;

    record_iam_audit_event(
        &state,
        &auth,
        request_id,
        "iam.api_token.patch",
        "succeeded",
        Some(format!("api token {} updated", row.label)),
        Some(format!("principal {} updated api token {}", auth.principal_id, token_principal_id)),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "api_token".to_string(),
            subject_id: token_principal_id,
            workspace_id: row.workspace_id,
            library_id: None,
            document_id: None,
        }],
    )
    .await;

    let grant_rows = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        token_principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to resolve api token grants after patch",
        );
        ApiError::Internal
    })?;
    let lookups =
        load_token_response_lookups(&state, std::slice::from_ref(&row), &grant_rows).await?;
    Ok(Json(build_token_response(row, grant_rows, &lookups)?))
}

#[tracing::instrument(
    level = "info",
    name = "http.revoke_token",
    skip_all,
    fields(token_principal_id = %token_principal_id)
)]
#[utoipa::path(
    post,
    path = "/v1/iam/tokens/{tokenId}/revoke",
    tag = "iam",
    operation_id = "revokeIamToken",
    params(("tokenId" = uuid::Uuid, Path, description = "Principal id whose tokens are revoked")),
    responses(
        (status = 204, description = "Token revoked"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not an IAM administrator"),
        (status = 404, description = "Token principal not found"),
    ),
)]
pub async fn revoke_token(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(token_principal_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_any_scope(POLICY_IAM_ADMIN)?;
    let request_id = request_id.map(|value| value.0.0);

    let row = iam_repository::get_api_token_by_principal_id(
        &state.persistence.postgres,
        token_principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to load api token for revoke",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;

    if let Err(error) = authorize_workspace_scope_for_row(&auth, row.workspace_id) {
        record_iam_audit_event(
            &state,
            &auth,
            request_id.clone(),
            "iam.api_token.revoke",
            "rejected",
            Some("api token revoke denied".to_string()),
            Some(format!(
                "principal {} was denied api token revoke for {}",
                auth.principal_id, token_principal_id
            )),
            vec![AppendAuditEventSubjectCommand {
                subject_kind: "api_token".to_string(),
                subject_id: token_principal_id,
                workspace_id: row.workspace_id,
                library_id: None,
                document_id: None,
            }],
        )
        .await;
        return Err(error);
    }

    state.canonical_services.iam.revoke_api_token(&state, token_principal_id).await?;
    record_iam_audit_event(
        &state,
        &auth,
        request_id,
        "iam.api_token.revoke",
        "succeeded",
        Some(format!("api token {} revoked", row.label)),
        Some(format!("principal {} revoked api token {}", auth.principal_id, token_principal_id)),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "api_token".to_string(),
            subject_id: token_principal_id,
            workspace_id: row.workspace_id,
            library_id: None,
            document_id: None,
        }],
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_token",
    skip_all,
    fields(token_principal_id = %token_principal_id)
)]
#[utoipa::path(
    delete,
    path = "/v1/iam/tokens/{tokenId}",
    tag = "iam",
    operation_id = "deleteIamToken",
    params(("tokenId" = uuid::Uuid, Path, description = "Revoked API token principal id to delete")),
    responses(
        (status = 204, description = "Revoked token deleted"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not an IAM administrator"),
        (status = 404, description = "Token principal not found"),
        (status = 409, description = "Token must be revoked before deletion"),
    ),
)]
pub async fn delete_token(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(token_principal_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_any_scope(POLICY_IAM_ADMIN)?;
    let request_id = request_id.map(|value| value.0.0);

    let row = iam_repository::get_api_token_by_principal_id(
        &state.persistence.postgres,
        token_principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            token_principal_id = %token_principal_id,
            ?error,
            "failed to load api token for delete",
        );
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;

    if let Err(error) = authorize_workspace_scope_for_row(&auth, row.workspace_id) {
        record_iam_audit_event(
            &state,
            &auth,
            request_id.clone(),
            "iam.api_token.delete",
            "rejected",
            Some("api token delete denied".to_string()),
            Some(format!(
                "principal {} was denied api token delete for {}",
                auth.principal_id, token_principal_id
            )),
            vec![AppendAuditEventSubjectCommand {
                subject_kind: "api_token".to_string(),
                subject_id: token_principal_id,
                workspace_id: row.workspace_id,
                library_id: None,
                document_id: None,
            }],
        )
        .await;
        return Err(error);
    }

    if row.status != "revoked" {
        record_iam_audit_event(
            &state,
            &auth,
            request_id.clone(),
            "iam.api_token.delete",
            "rejected",
            Some("api token delete requires revoked status".to_string()),
            Some(format!(
                "principal {} tried to delete non-revoked api token {}",
                auth.principal_id, token_principal_id
            )),
            vec![AppendAuditEventSubjectCommand {
                subject_kind: "api_token".to_string(),
                subject_id: token_principal_id,
                workspace_id: row.workspace_id,
                library_id: None,
                document_id: None,
            }],
        )
        .await;
        return Err(ApiError::Conflict("api token must be revoked before deletion".to_string()));
    }

    state.canonical_services.iam.delete_revoked_api_token(&state, token_principal_id).await?;
    record_iam_audit_event(
        &state,
        &auth,
        request_id,
        "iam.api_token.delete",
        "succeeded",
        Some(format!("api token {} deleted", row.label)),
        Some(format!("principal {} deleted api token {}", auth.principal_id, token_principal_id)),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "api_token".to_string(),
            subject_id: token_principal_id,
            workspace_id: row.workspace_id,
            library_id: None,
            document_id: None,
        }],
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
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

async fn record_iam_audit_event(
    state: &AppState,
    auth: &AuthContext,
    request_id: Option<String>,
    action_kind: &str,
    result_kind: &str,
    redacted_message: Option<String>,
    internal_message: Option<String>,
    subjects: Vec<AppendAuditEventSubjectCommand>,
) {
    if let Err(error) = state
        .canonical_services
        .audit
        .append_event(
            state,
            AppendAuditEventCommand {
                actor_principal_id: Some(auth.principal_id),
                surface_kind: "rest".to_string(),
                action_kind: action_kind.to_string(),
                request_id,
                trace_id: None,
                result_kind: result_kind.to_string(),
                redacted_message,
                internal_message,
                subjects,
            },
        )
        .await
    {
        tracing::warn!(stage = "audit", error = %error, "audit append failed");
    }
}

fn resolve_mint_workspace(
    auth: &AuthContext,
    requested: Option<Uuid>,
) -> Result<Option<Uuid>, ApiError> {
    if auth.is_system_admin {
        return Ok(requested);
    }

    match requested.or(auth.workspace_id) {
        Some(workspace_id) => {
            authorize_workspace_scope_for_id(auth, workspace_id)?;
            Ok(Some(workspace_id))
        }
        None => Err(ApiError::Unauthorized),
    }
}

fn authorize_workspace_scope_for_id(
    auth: &AuthContext,
    workspace_id: Uuid,
) -> Result<(), ApiError> {
    if auth.is_system_admin {
        return Ok(());
    }
    if auth.has_workspace_permission(workspace_id, POLICY_IAM_ADMIN) {
        return Ok(());
    }
    Err(ApiError::Unauthorized)
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

fn validate_permission_kind_for_resource(
    resource_kind: IamGrantResourceKind,
    permission_kind: IamPermissionKind,
) -> Result<(), ApiError> {
    let allowed = match resource_kind {
        IamGrantResourceKind::System => {
            matches!(permission_kind, IamPermissionKind::IamAdmin)
        }
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
                    | IamPermissionKind::IamAdmin
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
        IamGrantResourceKind::Document => {
            matches!(
                permission_kind,
                IamPermissionKind::DocumentRead | IamPermissionKind::DocumentWrite
            )
        }
        IamGrantResourceKind::QuerySession => {
            matches!(permission_kind, IamPermissionKind::QueryRun)
        }
        IamGrantResourceKind::AsyncOperation => {
            matches!(permission_kind, IamPermissionKind::OpsRead | IamPermissionKind::AuditRead)
        }
        IamGrantResourceKind::Connector => {
            matches!(permission_kind, IamPermissionKind::ConnectorAdmin)
        }
        IamGrantResourceKind::ProviderCredential => {
            matches!(permission_kind, IamPermissionKind::CredentialAdmin)
        }
        IamGrantResourceKind::LibraryBinding => {
            matches!(permission_kind, IamPermissionKind::BindingAdmin)
        }
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

enum MintGrantScope {
    Workspace(Uuid),
    Libraries(Vec<Uuid>),
}

fn build_mint_grants(
    scope: MintGrantScope,
    permission_kinds: &[IamPermissionKind],
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Vec<crate::services::iam::service::MintApiTokenGrantCommand>, ApiError> {
    match scope {
        MintGrantScope::Workspace(workspace_id) => permission_kinds
            .iter()
            .map(|permission_kind| {
                validate_permission_kind_for_resource(
                    IamGrantResourceKind::Workspace,
                    permission_kind.clone(),
                )?;
                Ok(crate::services::iam::service::MintApiTokenGrantCommand {
                    resource_kind: GrantResourceKind::Workspace,
                    resource_id: workspace_id,
                    permission_kind: permission_kind.as_str().to_string(),
                    expires_at,
                })
            })
            .collect(),
        MintGrantScope::Libraries(library_ids) => {
            let mut grants = Vec::with_capacity(library_ids.len() * permission_kinds.len());
            for library_id in library_ids {
                for permission_kind in permission_kinds {
                    validate_permission_kind_for_resource(
                        IamGrantResourceKind::Library,
                        permission_kind.clone(),
                    )?;
                    grants.push(crate::services::iam::service::MintApiTokenGrantCommand {
                        resource_kind: GrantResourceKind::Library,
                        resource_id: library_id,
                        permission_kind: permission_kind.as_str().to_string(),
                        expires_at,
                    });
                }
            }
            Ok(grants)
        }
    }
}

fn dedupe_uuids(values: Vec<Uuid>) -> Vec<Uuid> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::with_capacity(values.len());
    for value in values {
        if seen.insert(value) {
            deduped.push(value);
        }
    }
    deduped
}

fn dedupe_permissions(values: Vec<IamPermissionKind>) -> Vec<IamPermissionKind> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::with_capacity(values.len());
    for value in values {
        if seen.insert(value.as_str()) {
            deduped.push(value);
        }
    }
    deduped
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
        state.canonical_services.iam.resolve_effective_grants(state, auth.principal_id).await?;

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

fn map_principal_row(row: iam_repository::IamPrincipalRow) -> Result<PrincipalResponse, ApiError> {
    Ok(PrincipalResponse {
        id: row.id,
        principal_kind: map_principal_kind(&row.principal_kind)?,
        status: row.status,
        display_label: row.display_label,
        created_at: row.created_at,
        disabled_at: row.disabled_at,
    })
}

fn map_user_row(row: iam_repository::IamUserRow) -> UserResponse {
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

const fn map_route_system_role_from_repo(role: iam_repository::SystemRole) -> SystemRole {
    match role {
        iam_repository::SystemRole::Viewer => SystemRole::Viewer,
        iam_repository::SystemRole::Operator => SystemRole::Operator,
        iam_repository::SystemRole::Admin => SystemRole::Admin,
    }
}

const fn map_route_system_role_to_repo(role: SystemRole) -> iam_repository::SystemRole {
    match role {
        SystemRole::Viewer => iam_repository::SystemRole::Viewer,
        SystemRole::Operator => iam_repository::SystemRole::Operator,
        SystemRole::Admin => iam_repository::SystemRole::Admin,
    }
}

fn map_membership_row(row: WorkspaceMembership) -> WorkspaceMembershipResponse {
    WorkspaceMembershipResponse {
        workspace_id: row.workspace_id,
        principal_id: row.principal_id,
        membership_state: row.membership_state,
        joined_at: row.joined_at,
        ended_at: row.ended_at,
    }
}

#[derive(Debug, Default)]
struct TokenResponseLookups {
    workspaces: BTreeMap<Uuid, TokenWorkspaceSummaryResponse>,
    libraries: BTreeMap<Uuid, TokenLibrarySummaryResponse>,
    issuers: BTreeMap<Uuid, TokenIssuerResponse>,
}

async fn load_token_response_lookups(
    state: &AppState,
    token_rows: &[iam_repository::IamApiTokenRow],
    grant_rows: &[iam_repository::ResolvedIamGrantScopeRow],
) -> Result<TokenResponseLookups, ApiError> {
    let workspace_ids = token_rows
        .iter()
        .filter_map(|row| row.workspace_id)
        .chain(grant_rows.iter().filter_map(|row| row.workspace_id))
        .collect::<BTreeSet<_>>();
    let library_ids = grant_rows.iter().filter_map(|row| row.library_id).collect::<BTreeSet<_>>();
    let issuer_ids =
        token_rows.iter().filter_map(|row| row.issued_by_principal_id).collect::<BTreeSet<_>>();

    let workspace_rows = catalog_repository::list_workspaces(&state.persistence.postgres)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let workspaces = workspace_rows
        .into_iter()
        .filter(|row| workspace_ids.contains(&row.id))
        .map(|row| {
            (row.id, TokenWorkspaceSummaryResponse { id: row.id, display_name: row.display_name })
        })
        .collect();

    let library_rows = catalog_repository::list_libraries(&state.persistence.postgres, None)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let libraries = library_rows
        .into_iter()
        .filter(|row| library_ids.contains(&row.id))
        .map(|row| {
            (
                row.id,
                TokenLibrarySummaryResponse {
                    id: row.id,
                    workspace_id: row.workspace_id,
                    display_name: row.display_name,
                },
            )
        })
        .collect();

    let mut issuers = BTreeMap::new();
    for principal_id in issuer_ids {
        if let Some(principal_row) =
            iam_repository::get_principal_by_id(&state.persistence.postgres, principal_id)
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        {
            issuers.insert(
                principal_id,
                TokenIssuerResponse { principal_id, display_label: principal_row.display_label },
            );
        }
    }

    Ok(TokenResponseLookups { workspaces, libraries, issuers })
}

fn workspace_summary(
    workspace_id: Uuid,
    lookups: &TokenResponseLookups,
) -> TokenWorkspaceSummaryResponse {
    lookups.workspaces.get(&workspace_id).cloned().unwrap_or_else(|| {
        TokenWorkspaceSummaryResponse { id: workspace_id, display_name: workspace_id.to_string() }
    })
}

fn library_summary(
    library_id: Uuid,
    workspace_id: Option<Uuid>,
    lookups: &TokenResponseLookups,
) -> TokenLibrarySummaryResponse {
    lookups.libraries.get(&library_id).cloned().unwrap_or_else(|| TokenLibrarySummaryResponse {
        id: library_id,
        workspace_id: workspace_id.unwrap_or(Uuid::nil()),
        display_name: library_id.to_string(),
    })
}

fn build_token_response(
    row: iam_repository::IamApiTokenRow,
    grant_rows: Vec<iam_repository::ResolvedIamGrantScopeRow>,
    lookups: &TokenResponseLookups,
) -> Result<TokenResponse, ApiError> {
    let grants = grant_rows
        .into_iter()
        .map(|grant_row| {
            let workspace =
                grant_row.workspace_id.map(|workspace_id| workspace_summary(workspace_id, lookups));
            let library = grant_row
                .library_id
                .map(|library_id| library_summary(library_id, grant_row.workspace_id, lookups));
            Ok(TokenGrantSummaryResponse {
                resource_kind: map_grant_resource_kind(&grant_row.resource_kind)?,
                resource_id: grant_row.resource_id,
                permission_kind: map_permission_kind(&grant_row.permission_kind)?,
                workspace,
                library,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    let mut libraries = grants
        .iter()
        .filter_map(|grant| grant.library.clone())
        .fold(BTreeMap::<Uuid, TokenLibrarySummaryResponse>::new(), |mut acc, library| {
            acc.entry(library.id).or_insert(library);
            acc
        })
        .into_values()
        .collect::<Vec<_>>();
    libraries.sort_by(|left, right| {
        left.display_name
            .to_ascii_lowercase()
            .cmp(&right.display_name.to_ascii_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });

    let workspace = row
        .workspace_id
        .map(|workspace_id| workspace_summary(workspace_id, lookups))
        .or_else(|| grants.iter().find_map(|grant| grant.workspace.clone()));
    let issuer = row.issued_by_principal_id.map(|principal_id| {
        lookups.issuers.get(&principal_id).cloned().unwrap_or_else(|| TokenIssuerResponse {
            principal_id,
            display_label: principal_id.to_string(),
        })
    });
    let scope_kind = if !libraries.is_empty() {
        TokenScopeKind::Library
    } else if workspace.is_some() {
        TokenScopeKind::Workspace
    } else {
        TokenScopeKind::System
    };

    Ok(TokenResponse {
        principal_id: row.principal_id,
        label: row.label,
        token_prefix: row.token_prefix,
        status: row.status,
        expires_at: row.expires_at,
        revoked_at: row.revoked_at,
        last_used_at: row.last_used_at,
        issuer,
        scope: TokenScopeResponse { kind: scope_kind, workspace, libraries },
        grants,
    })
}

fn map_principal_row_contract(
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

fn map_user_row_contract(row: iam_repository::IamUserRow) -> ironrag_contracts::auth::UserProfile {
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

fn map_membership_row_contract(
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

fn map_grant_domain(row: Grant) -> Result<GrantResponse, ApiError> {
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

fn map_grant_domain_contract(row: Grant) -> Result<ironrag_contracts::auth::TokenGrant, ApiError> {
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

fn map_principal_kind(value: &str) -> Result<IamPrincipalKind, ApiError> {
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

fn map_grant_resource_kind(value: &str) -> Result<IamGrantResourceKind, ApiError> {
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

const fn map_domain_grant_resource_kind_contract(
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

fn map_permission_kind_contract(
    value: &str,
) -> Result<ironrag_contracts::auth::PermissionKind, ApiError> {
    Ok(match value {
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
        "iam_admin" => ironrag_contracts::auth::PermissionKind::IamAdmin,
        other => {
            warn!(permission_kind = %other, "encountered unknown permission kind");
            return Err(ApiError::Internal);
        }
    })
}

const fn map_domain_grant_resource_kind(
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

fn map_permission_kind(value: &str) -> Result<IamPermissionKind, ApiError> {
    match value {
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
        "iam_admin" => Ok(IamPermissionKind::IamAdmin),
        other => {
            warn!(permission_kind = %other, "encountered unknown grant permission kind");
            Err(ApiError::Internal)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domains::iam::PrincipalKind,
        interfaces::http::auth::{AuthContext, AuthGrant, AuthTokenKind},
    };
    use chrono::Utc;

    fn workspace_iam_admin_auth(workspace_id: Uuid) -> AuthContext {
        AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: Some(workspace_id),
            token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
            scopes: vec!["iam_admin".to_string()],
            grants: vec![AuthGrant {
                id: Uuid::now_v7(),
                resource_kind: "workspace".to_string(),
                resource_id: workspace_id,
                permission_kind: "iam_admin".to_string(),
                workspace_id: Some(workspace_id),
                library_id: None,
                document_id: None,
            }],
            workspace_memberships: Vec::new(),
            visible_workspace_ids: [workspace_id].into_iter().collect(),
            is_system_admin: false,
            system_role: None,
        }
    }

    #[test]
    fn system_role_route_to_repo_round_trips() {
        for (route, repo) in [
            (SystemRole::Viewer, iam_repository::SystemRole::Viewer),
            (SystemRole::Operator, iam_repository::SystemRole::Operator),
            (SystemRole::Admin, iam_repository::SystemRole::Admin),
        ] {
            assert_eq!(map_route_system_role_to_repo(route), repo);
            assert_eq!(map_route_system_role_from_repo(repo), route);
        }
    }

    #[test]
    fn repo_system_role_string_round_trips() {
        for repo in [
            iam_repository::SystemRole::Viewer,
            iam_repository::SystemRole::Operator,
            iam_repository::SystemRole::Admin,
        ] {
            assert_eq!(iam_repository::SystemRole::parse_wire(repo.as_str()), Some(repo));
        }
        assert_eq!(iam_repository::SystemRole::parse_wire("unknown"), None);
    }

    #[test]
    fn last_admin_guard_blocks_demoting_the_final_admin() {
        // Demoting the only admin is blocked.
        assert!(would_demote_last_admin(
            iam_repository::SystemRole::Admin,
            iam_repository::SystemRole::Operator,
            1,
        ));
        // With two admins, demoting one is allowed.
        assert!(!would_demote_last_admin(
            iam_repository::SystemRole::Admin,
            iam_repository::SystemRole::Viewer,
            2,
        ));
        // A no-op admin→admin update is never a demotion.
        assert!(!would_demote_last_admin(
            iam_repository::SystemRole::Admin,
            iam_repository::SystemRole::Admin,
            1,
        ));
        // Promoting a non-admin is never a demotion.
        assert!(!would_demote_last_admin(
            iam_repository::SystemRole::Viewer,
            iam_repository::SystemRole::Admin,
            1,
        ));
    }

    #[test]
    fn permission_kind_matches_expected_resource_kinds() {
        assert!(
            validate_permission_kind_for_resource(
                IamGrantResourceKind::Workspace,
                IamPermissionKind::WorkspaceAdmin
            )
            .is_ok()
        );
        assert!(
            validate_permission_kind_for_resource(
                IamGrantResourceKind::Library,
                IamPermissionKind::DocumentWrite
            )
            .is_ok()
        );
        assert!(matches!(
            validate_permission_kind_for_resource(
                IamGrantResourceKind::Document,
                IamPermissionKind::LibraryWrite
            ),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn grant_resource_and_permission_strings_are_canonical() {
        assert_eq!(IamGrantResourceKind::ProviderCredential.as_str(), "provider_credential");
        assert_eq!(IamPermissionKind::IamAdmin.as_str(), "iam_admin");
    }

    #[test]
    fn build_mint_grants_supports_workspace_scope() {
        let workspace_id = Uuid::now_v7();
        let grants = build_mint_grants(
            MintGrantScope::Workspace(workspace_id),
            &[IamPermissionKind::WorkspaceRead, IamPermissionKind::LibraryRead],
            None,
        )
        .expect("workspace grants should build");

        assert_eq!(grants.len(), 2);
        assert!(grants.iter().all(|grant| grant.resource_kind == GrantResourceKind::Workspace));
        assert!(grants.iter().all(|grant| grant.resource_id == workspace_id));
    }

    #[test]
    fn build_mint_grants_supports_multiple_libraries() {
        let library_a = Uuid::now_v7();
        let library_b = Uuid::now_v7();
        let grants = build_mint_grants(
            MintGrantScope::Libraries(vec![library_a, library_b]),
            &[IamPermissionKind::LibraryRead, IamPermissionKind::DocumentRead],
            None,
        )
        .expect("library grants should build");

        assert_eq!(grants.len(), 4);
        assert!(grants.iter().all(|grant| grant.resource_kind == GrantResourceKind::Library));
        assert!(grants.iter().any(|grant| grant.resource_id == library_a));
        assert!(grants.iter().any(|grant| grant.resource_id == library_b));
    }

    #[test]
    fn workspace_scoped_iam_admin_cannot_filter_foreign_workspace() {
        let workspace_id = Uuid::now_v7();
        let auth = workspace_iam_admin_auth(workspace_id);

        assert_eq!(
            resolve_workspace_filter(&auth, Some(workspace_id)).expect("same workspace allowed"),
            Some(workspace_id)
        );
        assert!(matches!(
            resolve_workspace_filter(&auth, Some(Uuid::now_v7())),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn workspace_scoped_iam_admin_cannot_mint_for_foreign_workspace() {
        let workspace_id = Uuid::now_v7();
        let auth = workspace_iam_admin_auth(workspace_id);

        assert_eq!(
            resolve_mint_workspace(&auth, Some(workspace_id)).expect("same workspace allowed"),
            Some(workspace_id)
        );
        assert!(matches!(
            resolve_mint_workspace(&auth, Some(Uuid::now_v7())),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn workspace_scoped_iam_admin_cannot_manage_global_token_rows() {
        let auth = workspace_iam_admin_auth(Uuid::now_v7());

        assert!(matches!(
            authorize_workspace_scope_for_row(&auth, None),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn build_token_response_includes_resolved_scope_and_permissions() {
        let principal_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let issuer_id = Uuid::now_v7();
        let token = build_token_response(
            iam_repository::IamApiTokenRow {
                principal_id,
                workspace_id: Some(workspace_id),
                label: "Library token".to_string(),
                token_prefix: "irt_demo".to_string(),
                status: "active".to_string(),
                expires_at: None,
                revoked_at: None,
                issued_by_principal_id: Some(issuer_id),
                last_used_at: None,
            },
            vec![iam_repository::ResolvedIamGrantScopeRow {
                id: Uuid::now_v7(),
                principal_id,
                resource_kind: "library".to_string(),
                resource_id: library_id,
                permission_kind: "library_read".to_string(),
                granted_at: Utc::now(),
                granted_by_principal_id: Some(issuer_id),
                expires_at: None,
                workspace_id: Some(workspace_id),
                library_id: Some(library_id),
                document_id: None,
            }],
            &TokenResponseLookups {
                workspaces: [(
                    workspace_id,
                    TokenWorkspaceSummaryResponse {
                        id: workspace_id,
                        display_name: "Workspace One".to_string(),
                    },
                )]
                .into_iter()
                .collect(),
                libraries: [(
                    library_id,
                    TokenLibrarySummaryResponse {
                        id: library_id,
                        workspace_id,
                        display_name: "Library One".to_string(),
                    },
                )]
                .into_iter()
                .collect(),
                issuers: [(
                    issuer_id,
                    TokenIssuerResponse {
                        principal_id: issuer_id,
                        display_label: "Admin".to_string(),
                    },
                )]
                .into_iter()
                .collect(),
            },
        )
        .expect("token response should build");

        assert_eq!(token.scope.kind, TokenScopeKind::Library);
        assert_eq!(
            token.scope.workspace.as_ref().map(|workspace| workspace.display_name.as_str()),
            Some("Workspace One")
        );
        assert_eq!(token.scope.libraries[0].display_name, "Library One");
        assert_eq!(token.grants[0].permission_kind, IamPermissionKind::LibraryRead);
        assert_eq!(
            token.issuer.as_ref().map(|issuer| issuer.display_label.as_str()),
            Some("Admin")
        );
    }
}
