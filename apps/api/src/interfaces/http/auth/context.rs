use std::collections::BTreeSet;

use uuid::Uuid;

use crate::{domains::iam::PrincipalKind, infra::repositories::iam_repository::SystemRole};

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
    /// System role of the underlying user principal, when the caller is a user
    /// (session or user-backed token). Non-human principals have no global
    /// role and may act only through explicit resource-scoped grants.
    pub system_role: Option<SystemRole>,
}
