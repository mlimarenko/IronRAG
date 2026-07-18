use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;
use zeroize::Zeroize as _;

/// Deserializes a field as `Option<Option<T>>`, distinguishing "field
/// omitted" (outer `None`, leave the column untouched) from "field present
/// but `null`" (`Some(None)`, clear the column) from "field present with a
/// value" (`Some(Some(value))`). Pair with `#[serde(default)]` so an omitted
/// field defaults to outer `None` instead of erroring.
fn deserialize_double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IamPrincipalKind {
    User,
    ApiToken,
    Worker,
    Bootstrap,
}

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListTokensQuery {
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MintTokenRequest {
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub library_ids: Vec<Uuid>,
    #[serde(default)]
    pub permission_kinds: Vec<IamPermissionKind>,
}

/// Partial update for `PATCH /iam/tokens/{tokenId}`. Only `label` and
/// `expiresAt` are mutable post-mint — the permission scope
/// (`permissionKinds`/`libraryIds`) is immutable once a token is minted, so
/// that the audit invariant "this plaintext was created with exactly these
/// permissions" always holds; changing scope means minting a new token and
/// deleting the old one, not patching this one.
///
/// `expiresAt` distinguishes three states: omitted (leave untouched),
/// `null` (clear the expiry, making the token non-expiring), and a
/// timestamp (set the expiry).
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PatchTokenRequest {
    pub label: Option<String>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub expires_at: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrincipalResponse {
    pub id: Uuid,
    pub principal_kind: IamPrincipalKind,
    pub status: String,
    pub display_label: String,
    pub created_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SystemRole {
    Viewer,
    Operator,
    Admin,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserResponse {
    pub principal_id: Uuid,
    pub login: String,
    pub email: String,
    pub display_name: String,
    pub role: SystemRole,
    pub auth_provider_kind: String,
    pub external_subject: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateUserRequest {
    pub login: String,
    pub email: String,
    pub display_name: String,
    pub password: String,
    pub role: SystemRole,
}

impl std::fmt::Debug for CreateUserRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreateUserRequest")
            .field("login", &self.login)
            .field("email", &self.email)
            .field("display_name", &self.display_name)
            .field("password", &"<redacted>")
            .field("role", &self.role)
            .finish()
    }
}

impl Drop for CreateUserRequest {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetUserRoleRequest {
    pub role: SystemRole,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenWorkspaceSummaryResponse {
    pub id: Uuid,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenLibrarySummaryResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub display_name: String,
}

#[derive(Debug, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenScopeKind {
    System,
    Workspace,
    Library,
}

#[derive(Debug, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenScopeResponse {
    pub kind: TokenScopeKind,
    pub workspace: Option<TokenWorkspaceSummaryResponse>,
    pub libraries: Vec<TokenLibrarySummaryResponse>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenIssuerResponse {
    pub principal_id: Uuid,
    pub display_label: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenGrantSummaryResponse {
    pub resource_kind: IamGrantResourceKind,
    pub resource_id: Uuid,
    pub permission_kind: IamPermissionKind,
    pub workspace: Option<TokenWorkspaceSummaryResponse>,
    pub library: Option<TokenLibrarySummaryResponse>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMembershipResponse {
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub membership_state: String,
    pub joined_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    pub principal_id: Uuid,
    pub label: String,
    pub token_prefix: String,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub issuer: Option<TokenIssuerResponse>,
    pub scope: TokenScopeResponse,
    pub grants: Vec<TokenGrantSummaryResponse>,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MintTokenResponse {
    pub token: String,
    pub api_token: TokenResponse,
}

impl std::fmt::Debug for MintTokenResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MintTokenResponse")
            .field("token", &"<redacted>")
            .field("api_token", &self.api_token)
            .finish()
    }
}

impl Drop for MintTokenResponse {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GrantResponse {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub resource_kind: IamGrantResourceKind,
    pub resource_id: Uuid,
    pub permission_kind: IamPermissionKind,
    pub granted_by_principal_id: Option<Uuid>,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// One workspace the user has been granted access to, with the permission
/// tier that access was granted at. Returned by `GET /iam/users/{id}/access`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct UserWorkspaceAccessResponse {
    pub grant_id: Uuid,
    pub workspace_id: Uuid,
    pub display_name: String,
    pub permission_kind: IamPermissionKind,
}

/// One library the user has been granted access to, with its workspace and the
/// permission tier. Returned by `GET /iam/users/{id}/access`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct UserLibraryAccessResponse {
    pub grant_id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub display_name: String,
    pub permission_kind: IamPermissionKind,
}

/// The full per-user access picture: every workspace- and library-scoped grant
/// the admin can manage from the Users access editor.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct UserAccessResponse {
    pub principal_id: Uuid,
    pub workspaces: Vec<UserWorkspaceAccessResponse>,
    pub libraries: Vec<UserLibraryAccessResponse>,
}

/// Desired workspace access entry in a `PUT /iam/users/{id}/access` request.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAccessEntryRequest {
    pub workspace_id: Uuid,
    pub permission_kind: IamPermissionKind,
}

/// Desired library access entry in a `PUT /iam/users/{id}/access` request.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryAccessEntryRequest {
    pub library_id: Uuid,
    pub permission_kind: IamPermissionKind,
}

/// Declarative target state for a user's workspace + library access.
///
/// The handler reconciles the user's existing workspace/library grants against
/// this set: grants not present here are revoked, missing ones are created, and
/// matching ones are left untouched. Other grant kinds (system, document, etc.)
/// are not touched by this endpoint.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetUserAccessRequest {
    #[serde(default)]
    pub workspaces: Vec<WorkspaceAccessEntryRequest>,
    #[serde(default)]
    pub libraries: Vec<LibraryAccessEntryRequest>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeResponse {
    pub principal: PrincipalResponse,
    pub user: Option<UserResponse>,
    pub workspace_memberships: Vec<WorkspaceMembershipResponse>,
    pub effective_grants: Vec<GrantResponse>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoginSessionRequest {
    pub login: String,
    pub password: String,
    pub remember_me: Option<bool>,
}

impl std::fmt::Debug for LoginSessionRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoginSessionRequest")
            .field("login", &self.login)
            .field("password", &"<redacted>")
            .field("remember_me", &self.remember_me)
            .finish()
    }
}

impl Drop for LoginSessionRequest {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapSetupRequest {
    pub login: String,
    pub display_name: Option<String>,
    pub password: String,
    pub ai_setup: Option<BootstrapSetupAiRequest>,
}

impl std::fmt::Debug for BootstrapSetupRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapSetupRequest")
            .field("login", &self.login)
            .field("display_name", &self.display_name)
            .field("password", &"<redacted>")
            .field("ai_setup", &self.ai_setup)
            .finish()
    }
}

impl Drop for BootstrapSetupRequest {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapSetupAiRequest {
    pub provider_kind: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

impl Drop for BootstrapSetupAiRequest {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for BootstrapSetupAiRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapSetupAiRequest")
            .field("provider_kind", &self.provider_kind)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionUserResponse {
    pub principal_id: Uuid,
    pub login: String,
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    pub session_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub user: SessionUserResponse,
}

#[cfg(test)]
mod secret_debug_tests {
    use super::{
        BootstrapSetupAiRequest, BootstrapSetupRequest, CreateUserRequest, LoginSessionRequest,
        SystemRole,
    };

    #[test]
    fn credential_requests_never_expose_password_key_or_url_secrets_in_debug() {
        let create_user = CreateUserRequest {
            login: "owner".into(),
            email: "owner@example.com".into(),
            display_name: "Owner".into(),
            password: "create-user-password-regression".into(),
            role: SystemRole::Admin,
        };
        let login = LoginSessionRequest {
            login: "owner".into(),
            password: "login-password-regression".into(),
            remember_me: Some(true),
        };
        let bootstrap = BootstrapSetupRequest {
            login: "owner".into(),
            display_name: None,
            password: "bootstrap-password-regression".into(),
            ai_setup: Some(BootstrapSetupAiRequest {
                provider_kind: "synthetic".into(),
                api_key: Some("provider-key-regression".into()),
                base_url: Some("https://user:url-secret@host.example/?token=query-secret".into()),
            }),
        };

        let debug = format!("{create_user:?} {login:?} {bootstrap:?}");
        for secret in [
            "create-user-password-regression",
            "login-password-regression",
            "bootstrap-password-regression",
            "provider-key-regression",
            "url-secret",
            "query-secret",
        ] {
            assert!(!debug.contains(secret), "Debug exposed {secret}");
        }
    }
}
