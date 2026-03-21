use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::iam::{Grant, GrantResourceKind, WorkspaceMembership},
    infra::repositories::{
        ai_repository, catalog_repository, content_repository, iam_repository, ops_repository,
        query_repository,
    },
    interfaces::http::{
        auth::{AuthContext, build_session_cookie_value},
        authorization::POLICY_IAM_ADMIN,
        router_support::{ApiError, RequestId},
        ui_support::{build_cleared_session_cookie, build_session_cookie},
    },
    services::audit_service::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
    services::iam_service::{
        AuthenticateSessionCommand, BootstrapClaimCommand, CreateGrantCommand,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IamGrantResourceKind {
    System,
    Workspace,
    Library,
    Document,
    QuerySession,
    AsyncOperation,
    Connector,
    ProviderCredential,
    LibraryBinding,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IamPermissionKind {
    WorkspaceAdmin,
    WorkspaceRead,
    LibraryRead,
    LibraryWrite,
    DocumentRead,
    DocumentWrite,
    ConnectorAdmin,
    CredentialAdmin,
    BindingAdmin,
    QueryRun,
    OpsRead,
    AuditRead,
    IamAdmin,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTokensQuery {
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListGrantsQuery {
    pub principal_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MintTokenRequest {
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGrantRequest {
    pub principal_id: Uuid,
    pub resource_kind: IamGrantResourceKind,
    pub resource_id: Uuid,
    pub permission_kind: IamPermissionKind,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrincipalResponse {
    pub id: Uuid,
    pub principal_kind: IamPrincipalKind,
    pub status: String,
    pub display_label: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub disabled_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserResponse {
    pub principal_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub auth_provider_kind: String,
    pub external_subject: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMembershipResponse {
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub membership_state: String,
    pub joined_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    pub principal_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub token_prefix: String,
    pub status: String,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub issued_by_principal_id: Option<Uuid>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MintTokenResponse {
    pub token: String,
    pub api_token: TokenResponse,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantResponse {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub resource_kind: IamGrantResourceKind,
    pub resource_id: Uuid,
    pub permission_kind: IamPermissionKind,
    pub granted_by_principal_id: Option<Uuid>,
    pub granted_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeResponse {
    pub principal: PrincipalResponse,
    pub user: Option<UserResponse>,
    pub workspace_memberships: Vec<WorkspaceMembershipResponse>,
    pub effective_grants: Vec<GrantResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapClaimResponse {
    pub principal_id: uuid::Uuid,
    pub email: String,
    pub display_name: String,
    pub claimed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapClaimRequest {
    pub bootstrap_secret: String,
    pub email: String,
    pub display_name: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginSessionRequest {
    pub email: String,
    pub password: String,
    pub remember_me: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUserResponse {
    pub principal_id: Uuid,
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    pub session_id: Uuid,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub user: SessionUserResponse,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/iam/bootstrap/claim", post(claim_bootstrap_admin))
        .route("/iam/session/login", post(login_session))
        .route("/iam/session", get(get_session))
        .route("/iam/session/logout", post(logout_session))
        .route("/iam/me", get(get_me))
        .route("/iam/tokens", get(list_tokens).post(mint_token))
        .route("/iam/tokens/{token_principal_id}/revoke", post(revoke_token))
        .route("/iam/grants", get(list_grants).post(create_grant))
        .route("/iam/grants/{grant_id}", delete(revoke_grant))
}

async fn claim_bootstrap_admin(
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<BootstrapClaimRequest>,
) -> Result<Json<BootstrapClaimResponse>, ApiError> {
    let request_id = request_id.map_or_else(|| uuid::Uuid::now_v7().to_string(), |value| value.0.0);
    let outcome = state
        .canonical_services
        .iam
        .claim_bootstrap_admin(
            &state,
            BootstrapClaimCommand {
                bootstrap_secret: payload.bootstrap_secret,
                email: payload.email,
                display_name: payload.display_name,
                password: payload.password,
                request_id,
            },
        )
        .await?;

    Ok(Json(BootstrapClaimResponse {
        principal_id: outcome.principal_id,
        email: outcome.email,
        display_name: outcome.display_name,
        claimed_at: outcome.claimed_at,
    }))
}

async fn login_session(
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<LoginSessionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ttl_hours =
        if payload.remember_me.unwrap_or(false) { state.ui_session_cookie.ttl_hours } else { 24 };
    let outcome = state
        .canonical_services
        .iam
        .authenticate_session(
            &state,
            AuthenticateSessionCommand {
                email: payload.email,
                password: payload.password,
                ttl_hours,
            },
        )
        .await?;

    let mut headers = HeaderMap::new();
    let cookie = build_session_cookie(
        state.ui_session_cookie.name,
        &build_session_cookie_value(outcome.session_id, &outcome.session_secret),
        ttl_hours,
    );
    headers.insert(header::SET_COOKIE, cookie.parse().map_err(|_| ApiError::Internal)?);

    let _ = state
        .canonical_services
        .audit
        .append_event(
            &state,
            AppendAuditEventCommand {
                actor_principal_id: Some(outcome.principal_id),
                surface_kind: "rest".to_string(),
                action_kind: "iam.session.login".to_string(),
                request_id: request_id.map(|value| value.0.0),
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some("session login succeeded".to_string()),
                internal_message: Some(format!(
                    "principal {} created session {}",
                    outcome.principal_id, outcome.session_id
                )),
                subjects: vec![AppendAuditEventSubjectCommand {
                    subject_kind: "session".to_string(),
                    subject_id: outcome.session_id,
                    workspace_id: None,
                    library_id: None,
                    document_id: None,
                }],
            },
        )
        .await;

    Ok((
        headers,
        Json(SessionResponse {
            session_id: outcome.session_id,
            expires_at: outcome.expires_at,
            user: SessionUserResponse {
                principal_id: outcome.principal_id,
                email: outcome.email,
                display_name: outcome.display_name,
            },
        }),
    ))
}

async fn get_session(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<SessionResponse>, ApiError> {
    if auth.token_kind != "session" {
        return Err(ApiError::Unauthorized);
    }

    let session_row = iam_repository::get_session_by_id(&state.persistence.postgres, auth.token_id)
        .await
        .map_err(|error| {
            error!(
                auth_principal_id = %auth.principal_id,
                session_id = %auth.token_id,
                ?error,
                "failed to load canonical session",
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::resource_not_found("session", auth.token_id))?;
    let user_row =
        iam_repository::get_user_by_principal_id(&state.persistence.postgres, auth.principal_id)
            .await
            .map_err(|error| {
                error!(
                    auth_principal_id = %auth.principal_id,
                    ?error,
                    "failed to load canonical session user",
                );
                ApiError::Internal
            })?
            .ok_or(ApiError::Unauthorized)?;

    Ok(Json(SessionResponse {
        session_id: session_row.id,
        expires_at: session_row.expires_at,
        user: SessionUserResponse {
            principal_id: user_row.principal_id,
            email: user_row.email,
            display_name: user_row.display_name,
        },
    }))
}

async fn logout_session(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
) -> Result<impl IntoResponse, ApiError> {
    if auth.token_kind != "session" {
        return Err(ApiError::Unauthorized);
    }

    state.canonical_services.iam.revoke_session(&state, auth.token_id).await?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        build_cleared_session_cookie(state.ui_session_cookie.name)
            .parse()
            .map_err(|_| ApiError::Internal)?,
    );

    let _ = state
        .canonical_services
        .audit
        .append_event(
            &state,
            AppendAuditEventCommand {
                actor_principal_id: Some(auth.principal_id),
                surface_kind: "rest".to_string(),
                action_kind: "iam.session.logout".to_string(),
                request_id: request_id.map(|value| value.0.0),
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some("session logout succeeded".to_string()),
                internal_message: Some(format!(
                    "principal {} revoked session {}",
                    auth.principal_id, auth.token_id
                )),
                subjects: vec![AppendAuditEventSubjectCommand {
                    subject_kind: "session".to_string(),
                    subject_id: auth.token_id,
                    workspace_id: None,
                    library_id: None,
                    document_id: None,
                }],
            },
        )
        .await;

    Ok((headers, StatusCode::NO_CONTENT))
}

async fn get_me(
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

async fn list_tokens(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListTokensQuery>,
) -> Result<Json<Vec<TokenResponse>>, ApiError> {
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

    Ok(Json(rows.into_iter().map(map_token_row).collect()))
}

async fn mint_token(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<MintTokenRequest>,
) -> Result<Json<MintTokenResponse>, ApiError> {
    auth.require_any_scope(POLICY_IAM_ADMIN)?;
    let workspace_id = resolve_mint_workspace(&auth, payload.workspace_id)?;

    if payload.label.trim().is_empty() {
        return Err(ApiError::BadRequest("label must not be empty".into()));
    }

    let outcome = state
        .canonical_services
        .iam
        .mint_api_token(
            &state,
            crate::services::iam_service::MintApiTokenCommand {
                workspace_id,
                label: payload.label,
                expires_at: payload.expires_at,
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

    Ok(Json(MintTokenResponse { token: outcome.token, api_token: map_token_row(row) }))
}

async fn list_grants(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListGrantsQuery>,
) -> Result<Json<Vec<GrantResponse>>, ApiError> {
    auth.require_any_scope(POLICY_IAM_ADMIN)?;
    let principal_id = query.principal_id.unwrap_or(auth.principal_id);
    let rows = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        principal_id,
    )
    .await
    .map_err(|error| {
        error!(
            auth_principal_id = %auth.principal_id,
            principal_id = %principal_id,
            ?error,
            "failed to list grants",
        );
        ApiError::Internal
    })?;

    if !auth.is_system_admin && principal_id != auth.principal_id {
        if let Some(token_row) =
            iam_repository::get_api_token_by_principal_id(&state.persistence.postgres, principal_id)
                .await
                .map_err(|error| {
                    error!(
                        auth_principal_id = %auth.principal_id,
                        principal_id = %principal_id,
                        ?error,
                        "failed to load token scope while listing grants",
                    );
                    ApiError::Internal
                })?
        {
            authorize_workspace_scope_for_row(&auth, token_row.workspace_id)?;
        } else if rows.is_empty() {
            return Err(ApiError::Unauthorized);
        }

        let all_visible = rows.iter().all(|row| match row.resource_kind.as_str() {
            "system" => false,
            _ => {
                row.workspace_id.is_some_and(|workspace_id| auth.can_access_workspace(workspace_id))
            }
        });
        if !all_visible {
            return Err(ApiError::Unauthorized);
        }
    }

    Ok(Json(rows.into_iter().map(map_resolved_grant_row).collect::<Result<Vec<_>, _>>()?))
}

async fn revoke_token(
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

async fn create_grant(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateGrantRequest>,
) -> Result<Json<GrantResponse>, ApiError> {
    auth.require_any_scope(POLICY_IAM_ADMIN)?;

    let workspace_id =
        resolve_grant_workspace_id(&state, payload.resource_kind.clone(), payload.resource_id)
            .await?;
    authorize_workspace_scope_for_id(&auth, workspace_id)?;
    validate_permission_kind_for_resource(
        payload.resource_kind.clone(),
        payload.permission_kind.clone(),
    )?;

    state.canonical_services.iam.get_principal(&state, payload.principal_id).await?;

    let grant = state
        .canonical_services
        .iam
        .create_grant(
            &state,
            CreateGrantCommand {
                principal_id: payload.principal_id,
                resource_kind: map_route_grant_resource_kind(payload.resource_kind.clone()),
                resource_id: payload.resource_id,
                permission_kind: payload.permission_kind.as_str().to_string(),
                granted_by_principal_id: Some(auth.principal_id),
                expires_at: payload.expires_at,
            },
        )
        .await
        .map_err(|error| {
            error!(
                auth_principal_id = %auth.principal_id,
                principal_id = %payload.principal_id,
                resource_kind = %payload.resource_kind.as_str(),
                resource_id = %payload.resource_id,
                ?error,
                "failed to create grant",
            );
            ApiError::Internal
        })?;

    Ok(Json(map_grant_domain(grant)?))
}

async fn revoke_grant(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(grant_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_any_scope(POLICY_IAM_ADMIN)?;

    let row = load_grant_row(&state, grant_id).await?;
    let workspace_id = resolve_grant_workspace_id(
        &state,
        map_grant_resource_kind(&row.resource_kind)?,
        row.resource_id,
    )
    .await?;
    authorize_workspace_scope_for_id(&auth, workspace_id)?;

    iam_repository::delete_grant(&state.persistence.postgres, grant_id)
        .await
        .map_err(|error| {
            error!(
                auth_principal_id = %auth.principal_id,
                grant_id = %grant_id,
                ?error,
                "failed to revoke grant",
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::resource_not_found("grant", grant_id))?;

    Ok(StatusCode::NO_CONTENT)
}

fn resolve_workspace_filter(
    auth: &AuthContext,
    requested: Option<Uuid>,
) -> Result<Option<Uuid>, ApiError> {
    if auth.is_system_admin || auth.has_scope("iam_admin") {
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
    let _ = state
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
        .await;
}

fn resolve_mint_workspace(
    auth: &AuthContext,
    requested: Option<Uuid>,
) -> Result<Option<Uuid>, ApiError> {
    if auth.is_system_admin || auth.has_scope("iam_admin") {
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

async fn resolve_grant_workspace_id(
    state: &AppState,
    resource_kind: IamGrantResourceKind,
    resource_id: Uuid,
) -> Result<Uuid, ApiError> {
    match resource_kind {
        IamGrantResourceKind::System => Ok(Uuid::nil()),
        IamGrantResourceKind::Workspace => {
            catalog_repository::get_workspace_by_id(&state.persistence.postgres, resource_id)
                .await
                .map_err(|error| {
                    error!(resource_id = %resource_id, ?error, "failed to load workspace for grant");
                    ApiError::Internal
                })?
                .ok_or_else(|| ApiError::resource_not_found("workspace", resource_id))
                .map(|row| row.id)
        }
        IamGrantResourceKind::Library => {
            catalog_repository::get_library_by_id(&state.persistence.postgres, resource_id)
                .await
                .map_err(|error| {
                    error!(resource_id = %resource_id, ?error, "failed to load library for grant");
                    ApiError::Internal
                })?
                .ok_or_else(|| ApiError::resource_not_found("library", resource_id))
                .map(|row| row.workspace_id)
        }
        IamGrantResourceKind::Document => {
            content_repository::get_document_by_id(&state.persistence.postgres, resource_id)
                .await
                .map_err(|error| {
                    error!(resource_id = %resource_id, ?error, "failed to load document for grant");
                    ApiError::Internal
                })?
                .ok_or_else(|| ApiError::resource_not_found("document", resource_id))
                .map(|row| row.workspace_id)
        }
        IamGrantResourceKind::QuerySession => {
            query_repository::get_conversation_by_id(&state.persistence.postgres, resource_id)
                .await
                .map_err(|error| {
                    error!(resource_id = %resource_id, ?error, "failed to load query session for grant");
                    ApiError::Internal
                })?
                .ok_or_else(|| ApiError::resource_not_found("query_session", resource_id))
                .map(|row| row.workspace_id)
        }
        IamGrantResourceKind::AsyncOperation => {
            ops_repository::get_async_operation_by_id(&state.persistence.postgres, resource_id)
                .await
                .map_err(|error| {
                    error!(resource_id = %resource_id, ?error, "failed to load async operation for grant");
                    ApiError::Internal
                })?
                .ok_or_else(|| ApiError::resource_not_found("async_operation", resource_id))
                .map(|row| row.workspace_id)
        }
        IamGrantResourceKind::Connector => {
            catalog_repository::get_connector_by_id(&state.persistence.postgres, resource_id)
                .await
                .map_err(|error| {
                    error!(resource_id = %resource_id, ?error, "failed to load connector for grant");
                    ApiError::Internal
                })?
                .ok_or_else(|| ApiError::resource_not_found("connector", resource_id))
                .map(|row| row.workspace_id)
        }
        IamGrantResourceKind::ProviderCredential => {
            ai_repository::get_provider_credential_by_id(&state.persistence.postgres, resource_id)
                .await
                .map_err(|error| {
                    error!(resource_id = %resource_id, ?error, "failed to load provider credential for grant");
                    ApiError::Internal
                })?
                .ok_or_else(|| ApiError::resource_not_found("provider_credential", resource_id))
                .map(|row| row.workspace_id)
        }
        IamGrantResourceKind::LibraryBinding => {
            ai_repository::get_library_binding_by_id(&state.persistence.postgres, resource_id)
                .await
                .map_err(|error| {
                    error!(resource_id = %resource_id, ?error, "failed to load library binding for grant");
                    ApiError::Internal
                })?
                .ok_or_else(|| ApiError::resource_not_found("library_binding", resource_id))
                .map(|row| row.workspace_id)
        }
    }
}

async fn load_grant_row(
    state: &AppState,
    grant_id: Uuid,
) -> Result<iam_repository::IamGrantRow, ApiError> {
    sqlx::query_as::<_, iam_repository::IamGrantRow>(
        "select
            id,
            principal_id,
            resource_kind::text as resource_kind,
            resource_id,
            permission_kind::text as permission_kind,
            granted_at,
            granted_by_principal_id,
            expires_at
         from iam_grant
         where id = $1",
    )
    .bind(grant_id)
    .fetch_optional(&state.persistence.postgres)
    .await
    .map_err(|error| {
        error!(grant_id = %grant_id, ?error, "failed to load grant");
        ApiError::Internal
    })?
    .ok_or_else(|| ApiError::resource_not_found("grant", grant_id))
}

fn authorize_workspace_scope_for_id(
    auth: &AuthContext,
    workspace_id: Uuid,
) -> Result<(), ApiError> {
    if auth.is_system_admin || auth.has_scope("iam_admin") {
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
        None if auth.is_system_admin || auth.has_scope("iam_admin") => Ok(()),
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
    UserResponse {
        principal_id: row.principal_id,
        email: row.email,
        display_name: row.display_name,
        auth_provider_kind: row.auth_provider_kind,
        external_subject: row.external_subject,
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

fn map_token_row(row: iam_repository::IamApiTokenRow) -> TokenResponse {
    TokenResponse {
        principal_id: row.principal_id,
        workspace_id: row.workspace_id,
        label: row.label,
        token_prefix: row.token_prefix,
        status: row.status,
        expires_at: row.expires_at,
        revoked_at: row.revoked_at,
        issued_by_principal_id: row.issued_by_principal_id,
        last_used_at: row.last_used_at,
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

fn map_resolved_grant_row(
    row: iam_repository::ResolvedIamGrantScopeRow,
) -> Result<GrantResponse, ApiError> {
    Ok(GrantResponse {
        id: row.id,
        principal_id: row.principal_id,
        resource_kind: map_grant_resource_kind(&row.resource_kind)?,
        resource_id: row.resource_id,
        permission_kind: map_permission_kind(&row.permission_kind)?,
        granted_by_principal_id: row.granted_by_principal_id,
        granted_at: row.granted_at,
        expires_at: row.expires_at,
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

fn map_domain_grant_resource_kind(
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

impl IamGrantResourceKind {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Workspace => "workspace",
            Self::Library => "library",
            Self::Document => "document",
            Self::QuerySession => "query_session",
            Self::AsyncOperation => "async_operation",
            Self::Connector => "connector",
            Self::ProviderCredential => "provider_credential",
            Self::LibraryBinding => "library_binding",
        }
    }
}

fn map_route_grant_resource_kind(value: IamGrantResourceKind) -> GrantResourceKind {
    match value {
        IamGrantResourceKind::System => GrantResourceKind::System,
        IamGrantResourceKind::Workspace => GrantResourceKind::Workspace,
        IamGrantResourceKind::Library => GrantResourceKind::Library,
        IamGrantResourceKind::Document => GrantResourceKind::Document,
        IamGrantResourceKind::QuerySession => GrantResourceKind::QuerySession,
        IamGrantResourceKind::AsyncOperation => GrantResourceKind::AsyncOperation,
        IamGrantResourceKind::Connector => GrantResourceKind::Connector,
        IamGrantResourceKind::ProviderCredential => GrantResourceKind::ProviderCredential,
        IamGrantResourceKind::LibraryBinding => GrantResourceKind::LibraryBinding,
    }
}

impl IamPermissionKind {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::WorkspaceAdmin => "workspace_admin",
            Self::WorkspaceRead => "workspace_read",
            Self::LibraryRead => "library_read",
            Self::LibraryWrite => "library_write",
            Self::DocumentRead => "document_read",
            Self::DocumentWrite => "document_write",
            Self::ConnectorAdmin => "connector_admin",
            Self::CredentialAdmin => "credential_admin",
            Self::BindingAdmin => "binding_admin",
            Self::QueryRun => "query_run",
            Self::OpsRead => "ops_read",
            Self::AuditRead => "audit_read",
            Self::IamAdmin => "iam_admin",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IamPrincipalKind {
    User,
    ApiToken,
    Worker,
    Bootstrap,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
