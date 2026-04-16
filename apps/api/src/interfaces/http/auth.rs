use std::collections::BTreeSet;

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, header, request::Parts},
};
use chrono::Utc;
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState, domains::iam::PrincipalKind, infra::repositories::iam_repository,
    interfaces::http::router_support::ApiError, shared::auth_tokens,
};

const AUTH_ACTIVITY_REFRESH_INTERVAL_SECS: i64 = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthTokenKind {
    Session,
    Principal(PrincipalKind),
}

impl AuthTokenKind {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Principal(PrincipalKind::User) => "user",
            Self::Principal(PrincipalKind::ApiToken) => "api_token",
            Self::Principal(PrincipalKind::Worker) => "worker",
            Self::Principal(PrincipalKind::Bootstrap) => "bootstrap",
        }
    }

    #[must_use]
    pub const fn is_session(&self) -> bool {
        matches!(self, Self::Session)
    }
}

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
    pub token_kind: AuthTokenKind,
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
        self.is_system_admin
            || self.visible_workspace_ids.contains(&workspace_id)
            || self.grants.iter().any(|g| g.resource_kind == "system")
    }

    #[must_use]
    pub fn can_access_any_workspace(&self) -> bool {
        self.is_system_admin
            || !self.visible_workspace_ids.is_empty()
            || self.grants.iter().any(|g| g.resource_kind == "system")
    }

    #[must_use]
    pub fn can_discover_any_workspace(&self, accepted: &[&str]) -> bool {
        self.can_access_any_workspace() || self.has_any_scope(accepted)
    }

    /// Requires that the principal can discover at least one workspace.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Unauthorized`] when the principal cannot access any
    /// workspace and does not hold any of the accepted discovery scopes.
    pub fn require_discover_any_workspace(&self, accepted: &[&str]) -> Result<(), ApiError> {
        if self.can_discover_any_workspace(accepted) {
            return Ok(());
        }

        Err(ApiError::Unauthorized)
    }

    /// If the token is scoped to exactly one workspace (either via the
    /// token's explicit `workspace_id` or because the grants point at
    /// only one), return that workspace id. MCP tools and the query API
    /// use this to auto-fill `workspace_id` when the caller omits it —
    /// no reason to force agents to pass a value they cannot choose.
    #[must_use]
    pub fn sole_workspace_id(&self) -> Option<Uuid> {
        // Explicit token workspace scope wins unconditionally.
        if let Some(ws) = self.workspace_id {
            return Some(ws);
        }
        // Fall back to the only visible workspace.
        if self.visible_workspace_ids.len() == 1 {
            return self.visible_workspace_ids.iter().next().copied();
        }
        None
    }

    /// If the token's grants reference exactly one library, return it.
    /// MCP tools that require a `library_id` parameter auto-fill it
    /// from here when the caller omits it, keeping the agent context
    /// free of boilerplate ids.
    #[must_use]
    pub fn sole_library_id(&self) -> Option<Uuid> {
        let mut seen: Option<Uuid> = None;
        for grant in &self.grants {
            if let Some(lib) = grant.library_id {
                match seen {
                    None => seen = Some(lib),
                    Some(existing) if existing == lib => {}
                    Some(_) => return None, // More than one library.
                }
            }
        }
        seen
    }

    #[must_use]
    pub const fn token_kind(&self) -> &'static str {
        self.token_kind.as_str()
    }

    /// Requires that the active authentication context came from a session token.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Unauthorized`] when the current token is not a
    /// session token.
    pub const fn require_session_token(&self) -> Result<(), ApiError> {
        if self.token_kind.is_session() {
            return Ok(());
        }
        Err(ApiError::Unauthorized)
    }

    #[must_use]
    pub fn is_read_only_for_library(&self, workspace_id: Uuid, write_scopes: &[&str]) -> bool {
        self.can_access_workspace(workspace_id) && !self.has_any_scope(write_scopes)
    }

    #[must_use]
    pub fn has_workspace_permission(&self, workspace_id: Uuid, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                (grant.resource_kind == "system"
                    || (grant.resource_kind == "workspace"
                        && grant.workspace_id == Some(workspace_id)))
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
                (grant.resource_kind == "system"
                    || (grant.resource_kind == "workspace"
                        && grant.workspace_id == Some(workspace_id))
                    || (grant.resource_kind == "library" && grant.library_id == Some(library_id)))
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
                (grant.resource_kind == "system"
                    || (grant.resource_kind == "workspace"
                        && grant.workspace_id == Some(workspace_id))
                    || (grant.resource_kind == "library" && grant.library_id == Some(library_id))
                    || (grant.resource_kind == "document"
                        && grant.document_id == Some(document_id)))
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_discover_workspace(&self, workspace_id: Uuid, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.can_access_workspace(workspace_id)
            || self.grants.iter().any(|grant| {
                (grant.resource_kind == "system" || grant.workspace_id == Some(workspace_id))
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
            || (self.token_kind.is_session() && self.can_access_workspace(workspace_id))
            || self.grants.iter().any(|grant| {
                (grant.resource_kind == "system"
                    || (grant.resource_kind == "workspace"
                        && grant.workspace_id == Some(workspace_id))
                    || (grant.resource_kind == "library" && grant.library_id == Some(library_id)))
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_admin_any_workspace(&self, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                matches!(grant.resource_kind.as_str(), "system" | "workspace")
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_read_any_library_memory(&self, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                matches!(grant.resource_kind.as_str(), "system" | "workspace" | "library")
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_read_any_document_memory(&self, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                matches!(
                    grant.resource_kind.as_str(),
                    "system" | "workspace" | "library" | "document"
                ) && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_write_any_library_memory(&self, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                matches!(grant.resource_kind.as_str(), "system" | "workspace" | "library")
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }

    #[must_use]
    pub fn can_write_any_document_memory(&self, accepted: &[&str]) -> bool {
        self.is_system_admin
            || self.grants.iter().any(|grant| {
                matches!(
                    grant.resource_kind.as_str(),
                    "system" | "workspace" | "library" | "document"
                ) && accepted.iter().any(|permission| grant.permission_kind == *permission)
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
                (grant.resource_kind == "system"
                    || (grant.resource_kind == "workspace"
                        && grant.workspace_id == Some(workspace_id))
                    || (grant.resource_kind == "library" && grant.library_id == Some(library_id))
                    || (grant.resource_kind == "document" && grant.library_id == Some(library_id)))
                    && accepted.iter().any(|permission| grant.permission_kind == *permission)
            })
    }
}

#[must_use]
pub fn hash_token(raw: &str) -> String {
    auth_tokens::hash_api_token(raw)
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

/// Resolves the optional auth context represented by the incoming request headers.
///
/// # Errors
///
/// Returns an [`ApiError`] when the headers contain an invalid or unverifiable token.
pub async fn resolve_optional_auth_context_from_headers(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<AuthContext>, ApiError> {
    let auth_header = headers.get(header::AUTHORIZATION).cloned();

    if let Some(auth_header) = auth_header {
        let header_value = auth_header.to_str().map_err(|_| ApiError::Unauthorized)?.to_owned();
        let token = header_value.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;

        let token_hash = hash_token(token);
        let token_row = iam_repository::find_active_api_token_by_secret_hash(
            &state.persistence.postgres,
            &token_hash,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or(ApiError::Unauthorized)?;

        let now = Utc::now();
        if auth_activity_refresh_due(token_row.last_used_at, now) {
            let postgres = state.persistence.postgres.clone();
            let stale_before = now - chrono::Duration::seconds(AUTH_ACTIVITY_REFRESH_INTERVAL_SECS);
            let principal_id = token_row.principal_id;
            tokio::spawn(async move {
                if let Err(error) =
                    iam_repository::touch_api_token_if_stale(&postgres, principal_id, stale_before)
                        .await
                {
                    warn!(
                        %principal_id,
                        ?error,
                        "failed to refresh iam api token last_used_at",
                    );
                }
            });
        }

        let context = build_auth_context_for_principal(
            state,
            token_row.principal_id,
            token_row.principal_id,
            AuthTokenKind::Principal(parse_principal_kind(&token_row.principal_kind)?),
            token_row.workspace_id,
            token_row.parent_principal_id,
        )
        .await?;
        return Ok(Some(context));
    }

    let Some(cookie_value) = read_cookie(headers, state.ui_session_cookie.name) else {
        return Ok(None);
    };

    let (session_id, session_secret) =
        parse_session_cookie_value(&cookie_value).ok_or(ApiError::Unauthorized)?;
    let session_row = iam_repository::get_session_by_id(&state.persistence.postgres, session_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or(ApiError::Unauthorized)?;
    if session_row.revoked_at.is_some() || session_row.expires_at < Utc::now() {
        return Err(ApiError::Unauthorized);
    }
    if session_row.session_secret_hash != hash_session_secret(&session_secret) {
        return Err(ApiError::Unauthorized);
    }

    let now = Utc::now();
    if auth_activity_refresh_due(Some(session_row.last_seen_at), now) {
        let postgres = state.persistence.postgres.clone();
        let stale_before = now - chrono::Duration::seconds(AUTH_ACTIVITY_REFRESH_INTERVAL_SECS);
        tokio::spawn(async move {
            if let Err(error) =
                iam_repository::touch_session_if_stale(&postgres, session_id, stale_before).await
            {
                warn!(
                    %session_id,
                    ?error,
                    "failed to refresh iam session last_seen_at",
                );
            }
        });
    }

    let context = build_auth_context_for_principal(
        state,
        session_row.principal_id,
        session_row.id,
        AuthTokenKind::Session,
        None,
        None,
    )
    .await?;
    Ok(Some(context))
}

fn auth_activity_refresh_due(
    last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    match last_seen_at {
        Some(last_seen_at) => {
            now.signed_duration_since(last_seen_at).num_seconds()
                >= AUTH_ACTIVITY_REFRESH_INTERVAL_SECS
        }
        None => true,
    }
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
    token_kind: AuthTokenKind,
    workspace_id: Option<Uuid>,
    parent_principal_id: Option<Uuid>,
) -> Result<AuthContext, ApiError> {
    let mut grants = iam_repository::list_resolved_grants_by_principal(
        &state.persistence.postgres,
        principal_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let mut memberships = iam_repository::list_workspace_memberships_by_principal(
        &state.persistence.postgres,
        principal_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

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
    let auth_grants = grants
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
        .collect::<Vec<_>>();
    let mut visible_workspace_ids = workspace_memberships
        .iter()
        .map(|membership| membership.workspace_id)
        .collect::<BTreeSet<_>>();
    visible_workspace_ids.extend(
        auth_grants
            .iter()
            .filter(|grant| grant.resource_kind == "workspace")
            .filter_map(|grant| grant.workspace_id),
    );

    Ok(AuthContext {
        token_id,
        principal_id,
        parent_principal_id,
        workspace_id,
        token_kind,
        scopes,
        grants: auth_grants,
        workspace_memberships,
        visible_workspace_ids,
        is_system_admin,
    })
}

fn parse_principal_kind(value: &str) -> Result<PrincipalKind, ApiError> {
    match value {
        "user" => Ok(PrincipalKind::User),
        "api_token" => Ok(PrincipalKind::ApiToken),
        "worker" => Ok(PrincipalKind::Worker),
        "bootstrap" => Ok(PrincipalKind::Bootstrap),
        _ => Err(ApiError::Unauthorized),
    }
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
    use chrono::TimeZone;

    fn api_token_auth_context() -> AuthTokenKind {
        AuthTokenKind::Principal(PrincipalKind::ApiToken)
    }

    fn library_grant_token(workspace_id: Uuid, library_id: Uuid, permission: &str) -> AuthContext {
        AuthContext {
            token_id: Uuid::now_v7(),
            workspace_id: Some(workspace_id),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            token_kind: api_token_auth_context(),
            scopes: vec![permission.to_string()],
            grants: vec![AuthGrant {
                id: Uuid::now_v7(),
                resource_kind: "library".into(),
                resource_id: library_id,
                permission_kind: permission.to_string(),
                workspace_id: Some(workspace_id),
                library_id: Some(library_id),
                document_id: None,
            }],
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: false,
        }
    }

    fn workspace_grant_token(workspace_id: Uuid, permission: &str) -> AuthContext {
        AuthContext {
            token_id: Uuid::now_v7(),
            workspace_id: Some(workspace_id),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            token_kind: api_token_auth_context(),
            scopes: vec![permission.to_string()],
            grants: vec![AuthGrant {
                id: Uuid::now_v7(),
                resource_kind: "workspace".into(),
                resource_id: workspace_id,
                permission_kind: permission.to_string(),
                workspace_id: Some(workspace_id),
                library_id: None,
                document_id: None,
            }],
            workspace_memberships: Vec::new(),
            visible_workspace_ids: std::iter::once(workspace_id).collect(),
            is_system_admin: false,
        }
    }

    fn workspace_token(workspace_id: Option<Uuid>) -> AuthContext {
        let visible_workspace_ids =
            workspace_id.into_iter().collect::<std::collections::BTreeSet<_>>();
        AuthContext {
            token_id: Uuid::now_v7(),
            workspace_id,
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            token_kind: api_token_auth_context(),
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
            token_kind: api_token_auth_context(),
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
            token_kind: api_token_auth_context(),
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
            token_kind: api_token_auth_context(),
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
            token_kind: api_token_auth_context(),
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
            token_kind: api_token_auth_context(),
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
            token_kind: api_token_auth_context(),
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
            token_kind: api_token_auth_context(),
            scopes: vec!["document_read".into()],
            grants: Vec::new(),
            workspace_memberships: vec![AuthWorkspaceMembership {
                workspace_id,
                membership_state: "active".into(),
            }],
            visible_workspace_ids: std::iter::once(workspace_id).collect(),
            is_system_admin: false,
        };
        let writable = AuthContext {
            token_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            workspace_id: Some(workspace_id),
            token_kind: api_token_auth_context(),
            scopes: vec!["document_read".into(), "document_write".into()],
            grants: Vec::new(),
            workspace_memberships: vec![AuthWorkspaceMembership {
                workspace_id,
                membership_state: "active".into(),
            }],
            visible_workspace_ids: std::iter::once(workspace_id).collect(),
            is_system_admin: false,
        };

        assert!(read_only.is_read_only_for_library(workspace_id, &["document_write"]));
        assert!(!writable.is_read_only_for_library(workspace_id, &["document_write"]));
        assert!(!read_only.is_read_only_for_library(Uuid::now_v7(), &["document_write"]));
    }

    #[test]
    fn library_grant_discovers_only_matching_library() {
        let workspace_id = Uuid::now_v7();
        let visible_library_id = Uuid::now_v7();
        let hidden_library_id = Uuid::now_v7();
        let auth = library_grant_token(workspace_id, visible_library_id, "library_read");

        assert!(auth.can_discover_workspace(workspace_id, &["library_read"]));
        assert!(auth.can_discover_library(workspace_id, visible_library_id, &["library_read"]));
        assert!(!auth.can_discover_library(workspace_id, hidden_library_id, &["library_read"]));
        assert!(!auth.can_access_workspace(workspace_id));
    }

    #[test]
    fn workspace_grant_discovers_all_libraries_in_workspace() {
        let workspace_id = Uuid::now_v7();
        let auth = workspace_grant_token(workspace_id, "workspace_admin");

        assert!(auth.can_access_workspace(workspace_id));
        assert!(auth.can_discover_library(workspace_id, Uuid::now_v7(), &["workspace_admin"]));
        assert!(auth.can_discover_library(
            workspace_id,
            Uuid::now_v7(),
            &["library_read", "workspace_admin"]
        ));
    }

    #[test]
    fn library_grant_with_workspace_membership_does_not_discover_sibling_libraries() {
        let workspace_id = Uuid::now_v7();
        let visible_library_id = Uuid::now_v7();
        let hidden_library_id = Uuid::now_v7();
        let auth = AuthContext {
            token_id: Uuid::now_v7(),
            workspace_id: Some(workspace_id),
            principal_id: Uuid::now_v7(),
            parent_principal_id: None,
            token_kind: api_token_auth_context(),
            scopes: vec!["library_read".into()],
            grants: vec![AuthGrant {
                id: Uuid::now_v7(),
                resource_kind: "library".into(),
                resource_id: visible_library_id,
                permission_kind: "library_read".into(),
                workspace_id: Some(workspace_id),
                library_id: Some(visible_library_id),
                document_id: None,
            }],
            workspace_memberships: vec![AuthWorkspaceMembership {
                workspace_id,
                membership_state: "active".into(),
            }],
            visible_workspace_ids: std::iter::once(workspace_id).collect(),
            is_system_admin: false,
        };

        assert!(auth.can_access_workspace(workspace_id));
        assert!(auth.can_discover_workspace(workspace_id, &["library_read"]));
        assert!(auth.can_discover_library(workspace_id, visible_library_id, &["library_read"]));
        assert!(!auth.can_discover_library(workspace_id, hidden_library_id, &["library_read"]));
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
    fn auth_activity_refresh_is_skipped_inside_refresh_window() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().expect("valid timestamp");
        let last_seen_at = now - chrono::Duration::seconds(29);

        assert!(!auth_activity_refresh_due(Some(last_seen_at), now));
    }

    #[test]
    fn auth_activity_refresh_is_due_at_threshold() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().expect("valid timestamp");
        let last_seen_at = now - chrono::Duration::seconds(AUTH_ACTIVITY_REFRESH_INTERVAL_SECS);

        assert!(auth_activity_refresh_due(Some(last_seen_at), now));
    }

    #[test]
    fn auth_activity_refresh_is_due_when_timestamp_missing() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().expect("valid timestamp");

        assert!(auth_activity_refresh_due(None, now));
    }
}

impl FromRequestParts<AppState> for AuthContext {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        let headers = parts.headers.clone();
        let state = state.clone();

        async move {
            resolve_optional_auth_context_from_headers(&state, &headers)
                .await?
                .ok_or(ApiError::Unauthorized)
        }
    }
}
