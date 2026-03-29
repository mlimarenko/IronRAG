use std::collections::BTreeSet;

use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Query, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{self, iam_repository},
    interfaces::http::router_support::ApiError,
    shared::auth_tokens,
};

#[derive(Clone, Debug)]
pub struct AuthGrant {
    pub id: Uuid,
    pub resource_kind: String,
    pub resource_id: Uuid,
    pub permission_kind: String,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Clone, Debug)]
pub struct AuthWorkspaceMembership {
    pub workspace_id: Uuid,
    pub membership_state: String,
}

#[derive(Clone, Debug)]
pub struct AuthContext {
    pub token_id: Uuid,
    pub principal_id: Uuid,
    pub parent_principal_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub token_kind: String,
    pub scopes: Vec<String>,
    pub grants: Vec<AuthGrant>,
    pub workspace_memberships: Vec<AuthWorkspaceMembership>,
    pub visible_workspace_ids: BTreeSet<Uuid>,
    pub is_system_admin: bool,
}

impl AuthContext {
    #[must_use]
    pub fn has_scope(&self, wanted: &str) -> bool {
        self.is_system_admin || self.scopes.iter().any(|scope| scope == wanted)
    }

    #[must_use]
    pub fn has_any_scope(&self, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.scopes.iter().any(|scope| accepted.iter().any(|wanted| scope == wanted))
    }

    /// Validates that the token has at least one accepted scope.
    ///
    /// # Errors
    /// Returns [`ApiError::Unauthorized`] when the token lacks all accepted scopes.
    pub fn require_any_scope(&self, accepted: &[&str]) -> Result<(), ApiError> {
        if self.has_any_scope(accepted) {
            return Ok(());
        }

        Err(ApiError::Unauthorized)
    }

    /// Ensures the caller is allowed to access a specific workspace.
    ///
    /// Instance administrators may access any workspace. Workspace-scoped tokens must
    /// match the target workspace id exactly.
    ///
    /// # Errors
    /// Returns [`ApiError::Unauthorized`] when the caller does not belong to the target workspace.
    pub fn require_workspace_access(&self, workspace_id: Uuid) -> Result<(), ApiError> {
        if self.can_access_workspace(workspace_id) {
            return Ok(());
        }

        Err(ApiError::Unauthorized)
    }

    #[must_use]
    pub fn can_access_workspace(&self, workspace_id: Uuid) -> bool {
        self.is_system_admin || self.visible_workspace_ids.contains(&workspace_id)
    }

    #[must_use]
    pub fn is_read_only_for_library(&self, workspace_id: Uuid, write_scopes: &[&str]) -> bool {
        self.can_access_workspace(workspace_id) && !self.has_any_scope(write_scopes)
    }

    #[must_use]
    pub fn has_workspace_permission(&self, workspace_id: Uuid, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                grant.resource_kind == "workspace"
                    && grant.workspace_id == Some(workspace_id)
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn has_library_permission(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        accepted: &[&str],
    ) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                ((grant.resource_kind == "workspace" && grant.workspace_id == Some(workspace_id))
                    || grant.resource_kind == "library" && grant.library_id == Some(library_id))
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn has_document_permission(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        document_id: Uuid,
        accepted: &[&str],
    ) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                ((grant.resource_kind == "workspace" && grant.workspace_id == Some(workspace_id))
                    || (grant.resource_kind == "library" && grant.library_id == Some(library_id))
                    || (grant.resource_kind == "document"
                        && grant.document_id == Some(document_id)))
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_discover_workspace(&self, workspace_id: Uuid, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                grant.workspace_id == Some(workspace_id)
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_discover_library(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        accepted: &[&str],
    ) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                (grant.workspace_id == Some(workspace_id) || grant.library_id == Some(library_id))
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_admin_any_workspace(&self, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                grant.resource_kind == "workspace"
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_read_any_library_memory(&self, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                matches!(grant.resource_kind.as_str(), "workspace" | "library")
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_read_any_document_memory(&self, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                matches!(grant.resource_kind.as_str(), "workspace" | "library" | "document")
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_write_any_library_memory(&self, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                matches!(grant.resource_kind.as_str(), "workspace" | "library")
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_write_any_document_memory(&self, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                matches!(grant.resource_kind.as_str(), "workspace" | "library" | "document")
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn has_document_or_library_read_scope_for_library(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        accepted: &[&str],
    ) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                ((grant.resource_kind == "workspace" && grant.workspace_id == Some(workspace_id))
                    || (grant.resource_kind == "library" && grant.library_id == Some(library_id))
                    || (grant.resource_kind == "document" && grant.library_id == Some(library_id)))
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }
}

#[derive(Serialize)]
pub struct TokenCreateResponse {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub token_kind: String,
    pub label: String,
    pub token: String,
    pub scopes: Vec<String>,
}

#[derive(Serialize)]
pub struct TokenSummary {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub token_kind: String,
    pub label: String,
    pub status: String,
    pub scopes: Vec<String>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize)]
pub struct CreateTokenRequest {
    pub workspace_id: Option<Uuid>,
    pub token_kind: String,
    pub label: String,
    pub scopes: Vec<String>,
}

#[derive(Deserialize)]
pub struct BootstrapTokenRequest {
    pub workspace_id: Option<Uuid>,
    pub token_kind: String,
    pub label: String,
    pub scopes: Vec<String>,
    pub bootstrap_secret: Option<String>,
}

#[derive(Deserialize)]
pub struct ListTokensQuery {
    pub workspace_id: Option<Uuid>,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/auth/tokens", axum::routing::post(create_token).get(list_tokens))
        .route("/auth/bootstrap-token", axum::routing::post(create_bootstrap_token))
        .route("/auth/tokens/{id}", axum::routing::get(get_token).delete(revoke_token))
}

#[must_use]
pub fn hash_token(raw: &str) -> String {
    auth_tokens::hash_api_token(raw)
}

#[must_use]
pub fn mint_plaintext_token() -> String {
    auth_tokens::mint_plaintext_api_token()
}

#[must_use]
pub fn preview_token(raw: &str) -> String {
    auth_tokens::preview_api_token(raw)
}

#[must_use]
pub fn hash_session_secret(raw: &str) -> String {
    auth_tokens::hash_session_secret(raw)
}

#[must_use]
pub fn mint_plaintext_session_secret() -> String {
    auth_tokens::mint_plaintext_session_secret()
}

#[must_use]
pub fn build_session_cookie_value(session_id: Uuid, secret: &str) -> String {
    auth_tokens::build_session_cookie_value(session_id, secret)
}

pub fn parse_session_cookie_value(raw: &str) -> Option<(Uuid, String)> {
    auth_tokens::parse_session_cookie_value(raw)
}

fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(header::COOKIE).and_then(|value| value.to_str().ok()).and_then(|value| {
        value.split(';').find_map(|pair| {
            let mut parts = pair.trim().splitn(2, '=');
            match (parts.next(), parts.next()) {
                (Some(cookie_name), Some(cookie_value)) if cookie_name == name => {
                    Some(cookie_value.to_string())
                }
                _ => None,
            }
        })
    })
}

async fn build_auth_context_for_principal(
    state: &AppState,
    principal_id: Uuid,
    token_id: Uuid,
    token_kind: String,
    workspace_id: Option<Uuid>,
    parent_principal_id: Option<Uuid>,
) -> Result<AuthContext, ApiError> {
    let mut grants = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        principal_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let mut memberships = iam_repository::list_workspace_memberships_by_principal(
        &state.persistence.postgres,
        principal_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    if let Some(token_workspace_id) = workspace_id {
        grants.retain(|grant| {
            grant.resource_kind == "system" || grant.workspace_id == Some(token_workspace_id)
        });
        memberships.retain(|membership| membership.workspace_id == token_workspace_id);
    }

    let is_system_admin = grants
        .iter()
        .any(|grant| grant.resource_kind == "system" && grant.permission_kind == "iam_admin");
    let scopes = collect_permission_kinds(&grants);
    let workspace_memberships = memberships
        .into_iter()
        .filter(|membership| membership.membership_state == "active")
        .map(|membership| AuthWorkspaceMembership {
            workspace_id: membership.workspace_id,
            membership_state: membership.membership_state,
        })
        .collect::<Vec<_>>();
    let mut visible_workspace_ids = workspace_memberships
        .iter()
        .map(|membership| membership.workspace_id)
        .collect::<BTreeSet<_>>();
    visible_workspace_ids.extend(grants.iter().filter_map(|grant| grant.workspace_id));

    Ok(AuthContext {
        token_id,
        principal_id,
        parent_principal_id,
        workspace_id,
        token_kind,
        scopes,
        grants: grants
            .into_iter()
            .map(|grant| AuthGrant {
                id: grant.id,
                resource_kind: grant.resource_kind,
                resource_id: grant.resource_id,
                permission_kind: grant.permission_kind,
                workspace_id: grant.workspace_id,
                library_id: grant.library_id,
                document_id: grant.document_id,
            })
            .collect(),
        workspace_memberships,
        visible_workspace_ids,
        is_system_admin,
    })
}

/// Creates a new API token and returns the plaintext token once.
///
/// # Errors
/// Returns [`ApiError::BadRequest`] for invalid payloads and [`ApiError::Internal`]
/// when token persistence or scope serialization fails.
pub async fn create_token(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateTokenRequest>,
) -> Result<Json<TokenCreateResponse>, ApiError> {
    validate_create_token_request(
        payload.workspace_id,
        &payload.token_kind,
        &payload.label,
        &payload.scopes,
    )?;
    authorize_token_creation(&auth, payload.workspace_id)?;

    mint_token_response(
        &state,
        payload.workspace_id,
        &payload.token_kind,
        &payload.label,
        payload.scopes,
    )
    .await
    .map(Json)
}

/// Creates an API token using the bootstrap secret for initial setup.
///
/// # Errors
/// Returns [`ApiError::Unauthorized`] when the provided secret is missing or invalid,
/// [`ApiError::BadRequest`] when bootstrap minting is not configured, and
/// [`ApiError::Internal`] when token persistence fails.
pub async fn create_bootstrap_token(
    State(state): State<AppState>,
    Json(payload): Json<BootstrapTokenRequest>,
) -> Result<Json<TokenCreateResponse>, ApiError> {
    if !state.settings.bootstrap_settings().legacy_bootstrap_token_endpoint_enabled {
        return Err(ApiError::forbidden(
            "legacy bootstrap token endpoint is disabled; use the canonical iam bootstrap claim flow",
        ));
    }

    validate_create_token_request(
        payload.workspace_id,
        &payload.token_kind,
        &payload.label,
        &payload.scopes,
    )?;

    let configured_secret = state.settings.resolved_bootstrap_token().ok_or_else(|| {
        ApiError::BadRequest("bootstrap token is not configured on backend".into())
    })?;
    let provided_secret = payload
        .bootstrap_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(ApiError::Unauthorized)?;

    if provided_secret != configured_secret {
        warn!(
            workspace_id = ?payload.workspace_id,
            token_kind = %payload.token_kind,
            label = %payload.label,
            "rejecting bootstrap token creation request with invalid secret",
        );
        return Err(ApiError::Unauthorized);
    }

    mint_token_response(
        &state,
        payload.workspace_id,
        &payload.token_kind,
        &payload.label,
        payload.scopes,
    )
    .await
    .map(Json)
}

/// Lists API tokens visible to a workspace administrator.
///
/// # Errors
/// Returns [`ApiError::Unauthorized`] when the caller lacks admin scope and
/// [`ApiError::Internal`] when token loading fails.
pub async fn list_tokens(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListTokensQuery>,
) -> Result<Json<Vec<TokenSummary>>, ApiError> {
    auth.require_any_scope(&["workspace_admin"])?;
    let workspace_filter = if auth.is_system_admin {
        query.workspace_id
    } else {
        match query.workspace_id {
            Some(workspace_id) => {
                auth.require_workspace_access(workspace_id)?;
                Some(workspace_id)
            }
            None => Some(auth.workspace_id.ok_or(ApiError::Unauthorized)?),
        }
    };

    let items: Vec<TokenSummary> =
        repositories::list_api_tokens(&state.persistence.postgres, workspace_filter)
            .await
            .map_err(|error| {
                error!(
                    auth_token_id = %auth.token_id,
                    workspace_id = ?workspace_filter,
                    ?error,
                    "failed to list api tokens",
                );
                ApiError::Internal
            })?
            .into_iter()
            .map(map_token_summary)
            .collect();

    info!(
        auth_token_id = %auth.token_id,
        requested_workspace_id = ?workspace_filter,
        token_count = items.len(),
        "listed api tokens",
    );

    Ok(Json(items))
}

/// Returns a single API token summary by id.
///
/// # Errors
/// Returns [`ApiError::Unauthorized`] when the caller lacks admin scope,
/// [`ApiError::NotFound`] when the token does not exist, and [`ApiError::Internal`]
/// when token loading fails.
pub async fn get_token(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TokenSummary>, ApiError> {
    auth.require_any_scope(&["workspace_admin"])?;

    let row = repositories::get_api_token_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            error!(
                auth_token_id = %auth.token_id,
                api_token_id = %id,
                ?error,
                "failed to load api token",
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("api_token {id} not found")))?;

    if let Some(workspace_id) = row.workspace_id {
        auth.require_workspace_access(workspace_id)?;
    } else if !auth.is_system_admin {
        warn!(
            auth_token_id = %auth.token_id,
            api_token_id = %id,
            "rejecting api token read outside instance admin scope",
        );
        return Err(ApiError::Unauthorized);
    }

    info!(
        auth_token_id = %auth.token_id,
        api_token_id = %row.id,
        workspace_id = ?row.workspace_id,
        token_kind = %row.token_kind,
        status = %row.status,
        "loaded api token",
    );

    Ok(Json(map_token_summary(row)))
}

/// Revokes a single API token by id.
///
/// # Errors
/// Returns [`ApiError::Unauthorized`] when the caller lacks admin scope,
/// [`ApiError::NotFound`] when the token does not exist, and [`ApiError::Internal`]
/// when token persistence fails.
pub async fn revoke_token(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_any_scope(&["workspace_admin"])?;

    let row = repositories::get_api_token_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            error!(
                auth_token_id = %auth.token_id,
                api_token_id = %id,
                ?error,
                "failed to load api token for revoke",
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("api_token {id} not found")))?;

    if let Some(workspace_id) = row.workspace_id {
        auth.require_workspace_access(workspace_id)?;
    } else if !auth.is_system_admin {
        warn!(
            auth_token_id = %auth.token_id,
            api_token_id = %id,
            "rejecting api token revoke outside instance admin scope",
        );
        return Err(ApiError::Unauthorized);
    }

    info!(
        auth_token_id = %auth.token_id,
        api_token_id = %row.id,
        workspace_id = ?row.workspace_id,
        token_kind = %row.token_kind,
        previous_status = %row.status,
        "accepted api token revoke request",
    );

    repositories::revoke_api_token(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            error!(
                auth_token_id = %auth.token_id,
                api_token_id = %id,
                workspace_id = ?row.workspace_id,
                ?error,
                "failed to revoke api token",
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("api_token {id} not found")))?;

    info!(
        auth_token_id = %auth.token_id,
        api_token_id = %row.id,
        workspace_id = ?row.workspace_id,
        token_kind = %row.token_kind,
        "revoked api token",
    );

    Ok(StatusCode::NO_CONTENT)
}

fn map_token_summary(row: repositories::ApiTokenRow) -> TokenSummary {
    let scopes = serde_json::from_value(row.scope_json).unwrap_or_default();

    TokenSummary {
        id: row.id,
        workspace_id: row.workspace_id,
        token_kind: row.token_kind,
        label: row.label,
        status: row.status,
        scopes,
        last_used_at: row.last_used_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn validate_create_token_request(
    workspace_id: Option<Uuid>,
    token_kind: &str,
    label: &str,
    scopes: &[String],
) -> Result<(), ApiError> {
    if token_kind.trim().is_empty() || label.trim().is_empty() {
        warn!(
            workspace_id = ?workspace_id,
            token_kind,
            label,
            scope_count = scopes.len(),
            "rejecting token creation request with empty identity fields",
        );
        return Err(ApiError::BadRequest("token_kind and label must not be empty".into()));
    }

    let normalized_kind = token_kind.trim();
    if normalized_kind == "instance_admin" && workspace_id.is_some() {
        return Err(ApiError::BadRequest(
            "instance_admin tokens must not target a specific workspace".into(),
        ));
    }
    if normalized_kind != "instance_admin" && workspace_id.is_none() {
        return Err(ApiError::BadRequest(
            "workspace-scoped tokens must include workspace_id".into(),
        ));
    }

    Ok(())
}

fn authorize_token_creation(
    auth: &AuthContext,
    workspace_id: Option<Uuid>,
) -> Result<(), ApiError> {
    auth.require_any_scope(&["workspace_admin"])?;

    match workspace_id {
        Some(workspace_id) => auth.require_workspace_access(workspace_id),
        None if auth.is_system_admin => Ok(()),
        None => Err(ApiError::Unauthorized),
    }
}

async fn mint_token_response(
    state: &AppState,
    workspace_id: Option<Uuid>,
    token_kind: &str,
    label: &str,
    scopes: Vec<String>,
) -> Result<TokenCreateResponse, ApiError> {
    let normalized_token_kind = token_kind.trim();
    let normalized_label = label.trim();
    let plaintext = mint_plaintext_token();
    let token_hash = hash_token(&plaintext);
    let token_preview = preview_token(&plaintext);
    let scope_json = serde_json::to_value(&scopes).map_err(|error| {
        error!(
            workspace_id = ?workspace_id,
            token_kind = normalized_token_kind,
            label = normalized_label,
            scope_count = scopes.len(),
            ?error,
            "failed to serialize token scopes",
        );
        ApiError::Internal
    })?;

    info!(
        workspace_id = ?workspace_id,
        token_kind = normalized_token_kind,
        label = normalized_label,
        scope_count = scopes.len(),
        "accepted token creation request",
    );

    let row = repositories::create_api_token(
        &state.persistence.postgres,
        workspace_id,
        normalized_token_kind,
        normalized_label,
        &token_hash,
        Some(&token_preview),
        scope_json,
        None,
    )
    .await
    .map_err(|error| {
        error!(
            workspace_id = ?workspace_id,
            token_kind = normalized_token_kind,
            label = normalized_label,
            scope_count = scopes.len(),
            ?error,
            "failed to persist api token",
        );
        ApiError::Internal
    })?;

    info!(
        workspace_id = ?row.workspace_id,
        api_token_id = %row.id,
        token_kind = %row.token_kind,
        label = %row.label,
        scope_count = scopes.len(),
        "created api token",
    );

    Ok(TokenCreateResponse {
        id: row.id,
        workspace_id: row.workspace_id,
        token_kind: row.token_kind,
        label: row.label,
        token: plaintext,
        scopes,
    })
}

fn collect_permission_kinds(grants: &[iam_repository::ResolvedIamGrantScopeRow]) -> Vec<String> {
    let mut permissions = BTreeSet::new();
    for grant in grants {
        permissions.insert(grant.permission_kind.clone());
    }
    permissions.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_token(workspace_id: Option<Uuid>) -> AuthContext {
        let visible_workspace_ids =
            workspace_id.into_iter().collect::<std::collections::BTreeSet<_>>();
        AuthContext {
            token_id: Uuid::now_v7(),
            workspace_id,
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            token_kind: "api_token".into(),
            scopes: vec!["workspace_admin".into()],
            grants: Vec::new(),
            workspace_memberships: visible_workspace_ids
                .iter()
                .map(|workspace_id| AuthWorkspaceMembership {
                    workspace_id: *workspace_id,
                    membership_state: "active".into(),
                })
                .collect(),
            visible_workspace_ids,
            is_system_admin: false,
        }
    }

    #[test]
    fn workspace_access_allows_matching_workspace() {
        let workspace_id = Uuid::now_v7();
        let auth = workspace_token(Some(workspace_id));

        assert!(auth.require_workspace_access(workspace_id).is_ok());
    }

    #[test]
    fn workspace_access_rejects_mismatched_workspace() {
        let auth = workspace_token(Some(Uuid::now_v7()));

        assert!(matches!(
            auth.require_workspace_access(Uuid::now_v7()),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn instance_admin_can_access_any_workspace() {
        let auth = AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: "api_token".into(),
            scopes: Vec::new(),
            grants: Vec::new(),
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: true,
        };

        assert!(auth.require_workspace_access(Uuid::now_v7()).is_ok());
    }

    #[test]
    fn can_access_workspace_matches_require_workspace_access_behavior() {
        let workspace_id = Uuid::now_v7();
        let matching_workspace_auth = workspace_token(Some(workspace_id));
        let mismatched_workspace_auth = workspace_token(Some(Uuid::now_v7()));
        let instance_admin = AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: "api_token".into(),
            scopes: Vec::new(),
            grants: Vec::new(),
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: true,
        };

        assert!(matching_workspace_auth.can_access_workspace(workspace_id));
        assert!(!mismatched_workspace_auth.can_access_workspace(workspace_id));
        assert!(instance_admin.can_access_workspace(workspace_id));
    }

    #[test]
    fn require_any_scope_allows_matching_scope() {
        let auth = AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: Some(Uuid::now_v7()),
            token_kind: "api_token".into(),
            scopes: vec!["document_read".into(), "query_run".into()],
            grants: Vec::new(),
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: false,
        };

        assert!(auth.require_any_scope(&["workspace_admin", "query_run"]).is_ok());
    }

    #[test]
    fn has_scope_matches_single_scope_membership() {
        let auth = AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: Some(Uuid::now_v7()),
            token_kind: "api_token".into(),
            scopes: vec!["document_read".into(), "query_run".into()],
            grants: Vec::new(),
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: false,
        };

        assert!(auth.has_scope("document_read"));
        assert!(!auth.has_scope("document_write"));
    }

    #[test]
    fn require_any_scope_rejects_when_no_scope_matches() {
        let auth = AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: Some(Uuid::now_v7()),
            token_kind: "api_token".into(),
            scopes: vec!["document_read".into()],
            grants: Vec::new(),
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: false,
        };

        assert!(matches!(
            auth.require_any_scope(&["workspace_admin", "query_run"]),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn require_any_scope_allows_instance_admin_without_explicit_scopes() {
        let auth = AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: "api_token".into(),
            scopes: Vec::new(),
            grants: Vec::new(),
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: true,
        };

        assert!(auth.require_any_scope(&["workspace_admin"]).is_ok());
    }

    #[test]
    fn is_read_only_for_library_requires_workspace_access_and_no_write_scope() {
        let workspace_id = Uuid::now_v7();
        let read_only = AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: Some(workspace_id),
            token_kind: "api_token".into(),
            scopes: vec!["document_read".into()],
            grants: Vec::new(),
            workspace_memberships: vec![AuthWorkspaceMembership {
                workspace_id,
                membership_state: "active".into(),
            }],
            visible_workspace_ids: [workspace_id].into_iter().collect(),
            is_system_admin: false,
        };
        let writable = AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: Some(workspace_id),
            token_kind: "api_token".into(),
            scopes: vec!["document_read".into(), "document_write".into()],
            grants: Vec::new(),
            workspace_memberships: vec![AuthWorkspaceMembership {
                workspace_id,
                membership_state: "active".into(),
            }],
            visible_workspace_ids: [workspace_id].into_iter().collect(),
            is_system_admin: false,
        };

        assert!(read_only.is_read_only_for_library(workspace_id, &["document_write"]));
        assert!(!writable.is_read_only_for_library(workspace_id, &["document_write"]));
        assert!(!read_only.is_read_only_for_library(Uuid::now_v7(), &["document_write"]));
    }

    #[test]
    fn hash_token_is_deterministic_and_sensitive_to_input() {
        let first = hash_token("secret-token");
        let second = hash_token("secret-token");
        let different = hash_token("secret-token-2");

        assert_eq!(first, second);
        assert_ne!(first, different);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn mint_plaintext_token_uses_expected_prefix_and_hex_payload() {
        let token = mint_plaintext_token();

        assert!(token.starts_with("rtrg_"));
        assert_eq!(token.len(), 37);
        assert!(token[5..].chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn map_token_summary_falls_back_to_empty_scopes_for_invalid_json() {
        let now = chrono::Utc::now();
        let row = repositories::ApiTokenRow {
            id: Uuid::now_v7(),
            workspace_id: Some(Uuid::now_v7()),
            token_kind: "workspace_token".into(),
            label: "ops".into(),
            token_hash: "hash".into(),
            token_preview: Some("rtrg_***abcd".into()),
            scope_json: serde_json::json!({"oops": true}),
            status: "active".into(),
            last_used_at: None,
            expires_at: None,
            created_at: now,
            updated_at: now,
        };

        let summary = map_token_summary(row);

        assert!(summary.scopes.is_empty());
        assert_eq!(summary.status, "active");
        assert_eq!(summary.label, "ops");
    }

    #[test]
    fn validate_create_token_request_rejects_workspace_less_non_admin_token() {
        let result = validate_create_token_request(None, "workspace_token", "ops", &[]);

        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn authorize_token_creation_rejects_global_token_mint_for_workspace_admin() {
        let auth = workspace_token(Some(Uuid::now_v7()));

        assert!(matches!(authorize_token_creation(&auth, None), Err(ApiError::Unauthorized)));
    }

    #[test]
    fn authorize_token_creation_allows_matching_workspace_token_mint() {
        let workspace_id = Uuid::now_v7();
        let auth = workspace_token(Some(workspace_id));

        assert!(authorize_token_creation(&auth, Some(workspace_id)).is_ok());
    }
}

impl FromRequestParts<AppState> for AuthContext {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        let auth_header = parts.headers.get(header::AUTHORIZATION).cloned();
        let state = state.clone();

        async move {
            if let Some(auth_header) = auth_header {
                let header_value =
                    auth_header.to_str().map_err(|_| ApiError::Unauthorized)?.to_owned();

                let token = header_value.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;

                let token_hash = hash_token(token);
                let token_row = iam_repository::find_active_api_token_by_secret_hash(
                    &state.persistence.postgres,
                    &token_hash,
                )
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or(ApiError::Unauthorized)?;

                iam_repository::touch_api_token(
                    &state.persistence.postgres,
                    token_row.principal_id,
                )
                .await
                .map_err(|_| ApiError::Internal)?;

                return build_auth_context_for_principal(
                    &state,
                    token_row.principal_id,
                    token_row.principal_id,
                    token_row.principal_kind,
                    token_row.workspace_id,
                    token_row.parent_principal_id,
                )
                .await;
            }

            let cookie_value = read_cookie(&parts.headers, state.ui_session_cookie.name)
                .ok_or(ApiError::Unauthorized)?;
            let (session_id, session_secret) =
                parse_session_cookie_value(&cookie_value).ok_or(ApiError::Unauthorized)?;
            let session_row =
                iam_repository::get_session_by_id(&state.persistence.postgres, session_id)
                    .await
                    .map_err(|_| ApiError::Internal)?
                    .ok_or(ApiError::Unauthorized)?;
            if session_row.revoked_at.is_some() || session_row.expires_at < Utc::now() {
                return Err(ApiError::Unauthorized);
            }
            if session_row.session_secret_hash != hash_session_secret(&session_secret) {
                return Err(ApiError::Unauthorized);
            }
            iam_repository::touch_session(&state.persistence.postgres, session_id)
                .await
                .map_err(|_| ApiError::Internal)?;

            build_auth_context_for_principal(
                &state,
                session_row.principal_id,
                session_row.id,
                "session".to_string(),
                None,
                None,
            )
            .await
        }
    }
}
