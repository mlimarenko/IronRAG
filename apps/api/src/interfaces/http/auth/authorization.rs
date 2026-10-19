use uuid::Uuid;

use super::context::AuthContext;
use crate::{
    infra::repositories::iam_repository::SystemRole, interfaces::http::router_support::ApiError,
};

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

    /// Whether the caller's system role permits write/mutation capability.
    ///
    /// Only human users with `operator` or `admin` roles may use generic
    /// write capability. Grant-specific resource authorization remains
    /// available to token principals at explicit resource boundaries.
    #[must_use]
    pub const fn role_permits_write(&self) -> bool {
        match self.system_role {
            Some(SystemRole::Admin | SystemRole::Operator) => true,
            Some(SystemRole::Viewer) => false,
            None => false,
        }
    }

    /// Ensures the caller's system role permits write/mutation capability.
    ///
    /// This is the capability gate that keeps `viewer` users read-only even
    /// when a grant covers the target resource. Resource scoping is enforced
    /// separately by the grant checks.
    ///
    /// # Errors
    /// Returns [`ApiError::Unauthorized`] when the caller is a `viewer`.
    pub fn require_write_capability(&self) -> Result<(), ApiError> {
        if self.role_permits_write() {
            return Ok(());
        }
        Err(ApiError::Unauthorized)
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
    pub fn can_access_any_workspace(&self) -> bool {
        self.is_system_admin || !self.visible_workspace_ids.is_empty()
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
                (grant.resource_kind == "workspace" && grant.workspace_id == Some(workspace_id))
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
            || self.can_access_workspace(workspace_id)
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
            || (self.token_kind.is_session() && self.can_access_workspace(workspace_id))
            || self.grants.iter().any(|grant| {
                ((grant.resource_kind == "workspace"
                        && grant.workspace_id == Some(workspace_id))
                    || (grant.resource_kind == "library" && grant.library_id == Some(library_id))
                    // A document grant must surface its parent library in
                    // discovery, or the principal could hold a document it can
                    // never navigate to through the catalog.
                    || (grant.resource_kind == "document"
                        && grant.library_id == Some(library_id)))
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use uuid::Uuid;

    use super::super::context::{AuthContext, AuthGrant, AuthTokenKind, AuthWorkspaceMembership};
    use crate::{
        domains::iam::PrincipalKind, infra::repositories::iam_repository::SystemRole,
        interfaces::http::router_support::ApiError,
    };

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
            system_role: None,
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
            system_role: None,
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
            system_role: None,
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
            system_role: Some(SystemRole::Admin),
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
            system_role: Some(SystemRole::Admin),
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
            system_role: None,
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
            system_role: None,
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
            system_role: None,
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
            system_role: Some(SystemRole::Admin),
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
            system_role: None,
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
            system_role: None,
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
            system_role: None,
        };

        assert!(auth.can_access_workspace(workspace_id));
        assert!(auth.can_discover_workspace(workspace_id, &["library_read"]));
        assert!(auth.can_discover_library(workspace_id, visible_library_id, &["library_read"]));
        assert!(!auth.can_discover_library(workspace_id, hidden_library_id, &["library_read"]));
    }

    #[test]
    fn role_permits_write_follows_system_role_tiers() {
        let workspace_id = Uuid::now_v7();
        let mut admin = workspace_token(Some(workspace_id));
        admin.system_role = Some(SystemRole::Admin);
        let mut operator = workspace_token(Some(workspace_id));
        operator.system_role = Some(SystemRole::Operator);
        let mut viewer = workspace_token(Some(workspace_id));
        viewer.system_role = Some(SystemRole::Viewer);
        let mut token = workspace_token(Some(workspace_id));
        token.system_role = None;

        assert!(admin.role_permits_write());
        assert!(operator.role_permits_write());
        assert!(!viewer.role_permits_write());
        // No-role contexts fail closed; resource grants are checked separately.
        assert!(!token.role_permits_write());

        assert!(viewer.require_write_capability().is_err());
        assert!(operator.require_write_capability().is_ok());
        assert!(admin.require_write_capability().is_ok());
        assert!(token.require_write_capability().is_err());
    }
}
