mod api_tokens;
mod bootstrap;
mod grants;
mod memberships;
mod principals;
mod service_identities;
mod sessions;
mod users;

pub use self::{
    api_tokens::*, bootstrap::*, grants::*, memberships::*, principals::*, service_identities::*,
    sessions::*, users::*,
};

use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgConnection, PgPool, Postgres};
use uuid::Uuid;

/// System role assigned to a user principal.
///
/// Maps 1:1 to the `public.iam_system_role` PG enum and to the
/// `ShellRole` contract the UI shell gates its capability matrix on. The
/// ordering reflects increasing privilege (`viewer` < `operator` < `admin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemRole {
    Viewer,
    Operator,
    Admin,
}

impl SystemRole {
    /// Stable wire string, matching the PG enum labels.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Operator => "operator",
            Self::Admin => "admin",
        }
    }

    /// Parses the stable wire string into a [`SystemRole`].
    #[must_use]
    pub fn parse_wire(value: &str) -> Option<Self> {
        match value {
            "viewer" => Some(Self::Viewer),
            "operator" => Some(Self::Operator),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct IamPrincipalRow {
    pub id: Uuid,
    pub principal_kind: String,
    pub status: String,
    pub display_label: String,
    pub created_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct IamPrincipalProfileRow {
    pub id: Uuid,
    pub principal_kind: String,
    pub status: String,
    pub display_label: String,
    pub login: Option<String>,
    pub display_name: Option<String>,
    pub role: Option<String>,
}

#[derive(Clone, FromRow)]
pub struct IamUserRow {
    pub principal_id: Uuid,
    pub login: String,
    pub email: String,
    pub display_name: String,
    pub password_hash: String,
    pub auth_provider_kind: String,
    pub external_subject: Option<String>,
    /// Stable system-role wire string (`viewer` | `operator` | `admin`).
    pub role: String,
}

impl std::fmt::Debug for IamUserRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IamUserRow")
            .field("principal_id", &self.principal_id)
            .field("login", &self.login)
            .field("email", &self.email)
            .field("display_name", &self.display_name)
            .field("password_hash", &"<redacted>")
            .field("auth_provider_kind", &self.auth_provider_kind)
            .field("external_subject", &self.external_subject)
            .field("role", &self.role)
            .finish()
    }
}

impl IamUserRow {
    /// Parses the stored role string into a [`SystemRole`], defaulting to
    /// the least-privileged `viewer` if the column ever holds an unknown value
    /// (fail-closed).
    #[must_use]
    pub fn system_role(&self) -> SystemRole {
        SystemRole::parse_wire(&self.role).unwrap_or(SystemRole::Viewer)
    }
}

#[derive(Clone, FromRow)]
pub struct IamSessionRow {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub session_secret_hash: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_seen_at: DateTime<Utc>,
}

impl std::fmt::Debug for IamSessionRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IamSessionRow")
            .field("id", &self.id)
            .field("principal_id", &self.principal_id)
            .field("session_secret_hash", &"<redacted>")
            .field("issued_at", &self.issued_at)
            .field("expires_at", &self.expires_at)
            .field("revoked_at", &self.revoked_at)
            .field("last_seen_at", &self.last_seen_at)
            .finish()
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct IamApiTokenRow {
    pub principal_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub token_prefix: String,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub issued_by_principal_id: Option<Uuid>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct IamServiceIdentityRow {
    pub principal_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub token_prefix: String,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub issued_by_principal_id: Option<Uuid>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub owner_auth_provider_kind: String,
    pub owner_external_subject: String,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub parent_principal_id: Option<Uuid>,
    pub provisioning_authority: Option<String>,
}

impl IamServiceIdentityRow {
    #[must_use]
    pub fn api_token(&self) -> IamApiTokenRow {
        IamApiTokenRow {
            principal_id: self.principal_id,
            workspace_id: self.workspace_id,
            label: self.label.clone(),
            token_prefix: self.token_prefix.clone(),
            status: self.status.clone(),
            expires_at: self.expires_at,
            revoked_at: self.revoked_at,
            issued_by_principal_id: self.issued_by_principal_id,
            last_used_at: self.last_used_at,
        }
    }
}

#[derive(Clone, FromRow)]
pub struct IamServiceIdentityOperationRow {
    pub id: Uuid,
    pub service_identity_principal_id: Uuid,
    pub owner_auth_provider_kind: String,
    pub owner_external_subject: String,
    pub actor_principal_id: Option<Uuid>,
    pub operation_kind: String,
    pub idempotency_key: String,
    pub request_fingerprint: String,
    pub credential_secret_version: Option<i32>,
    pub encrypted_credential: Option<String>,
    pub created_at: DateTime<Utc>,
    pub superseded_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for IamServiceIdentityOperationRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IamServiceIdentityOperationRow")
            .field("id", &self.id)
            .field("service_identity_principal_id", &self.service_identity_principal_id)
            .field("owner_auth_provider_kind", &self.owner_auth_provider_kind)
            .field("owner_external_subject", &self.owner_external_subject)
            .field("actor_principal_id", &self.actor_principal_id)
            .field("operation_kind", &self.operation_kind)
            .field("idempotency_key", &self.idempotency_key)
            .field("request_fingerprint", &self.request_fingerprint)
            .field("credential_secret_version", &self.credential_secret_version)
            .field(
                "encrypted_credential",
                &self.encrypted_credential.as_ref().map(|_| "<redacted>"),
            )
            .field("created_at", &self.created_at)
            .field("superseded_at", &self.superseded_at)
            .finish()
    }
}

#[derive(Clone, FromRow)]
pub struct IamApiTokenSecretRow {
    pub token_principal_id: Uuid,
    pub secret_version: i32,
    pub secret_hash: String,
    pub issued_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for IamApiTokenSecretRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IamApiTokenSecretRow")
            .field("token_principal_id", &self.token_principal_id)
            .field("secret_version", &self.secret_version)
            .field("secret_hash", &"<redacted>")
            .field("issued_at", &self.issued_at)
            .field("revoked_at", &self.revoked_at)
            .finish()
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct IamWorkspaceMembershipRow {
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub membership_state: String,
    pub joined_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct IamGrantRow {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub resource_kind: String,
    pub resource_id: Uuid,
    pub permission_kind: String,
    pub granted_at: DateTime<Utc>,
    pub granted_by_principal_id: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct AuthenticatedApiTokenRow {
    pub principal_id: Uuid,
    pub principal_kind: String,
    pub principal_status: String,
    pub parent_principal_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub token_prefix: String,
    pub token_status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ResolvedIamGrantScopeRow {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub resource_kind: String,
    pub resource_id: Uuid,
    pub permission_kind: String,
    pub granted_at: DateTime<Utc>,
    pub granted_by_principal_id: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BootstrapClaimRow {
    pub principal_id: Uuid,
    pub login: String,
    pub email: String,
    pub display_name: String,
    pub claimed_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests;
