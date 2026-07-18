//! Authentication, authorization, and bootstrap contracts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ai::AiBindingPurpose;
use crate::provider::{
    ProviderBaseUrlPolicy, ProviderCapabilities, ProviderCredentialPolicy, ProviderModelDiscovery,
    ProviderRuntimeProfile,
};
use crate::shell::ShellBootstrap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Locales supported by the user interface.
pub enum UiLocale {
    /// English user-interface text.
    En,
    /// Russian user-interface text.
    Ru,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Availability of bootstrap credentials for a provider.
pub enum BootstrapCredentialSource {
    /// No bootstrap credential is available.
    Missing,
    /// The credential is supplied by the process environment.
    Env,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Model preset proposed for one AI purpose during initial setup.
pub struct BootstrapProviderBinding {
    /// Pipeline purpose served by this model.
    pub binding_purpose: AiBindingPurpose,
    /// Catalog entry that identifies the model.
    pub model_catalog_id: Uuid,
    /// Provider-facing model name.
    pub model_name: String,
    /// Optional system prompt for the preset.
    pub system_prompt: Option<String>,
    /// Optional sampling temperature.
    pub temperature: Option<f64>,
    /// Optional nucleus-sampling threshold.
    pub top_p: Option<f64>,
    /// Optional output-token limit that overrides the model default.
    pub max_output_tokens_override: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Provider metadata and model presets offered during initial setup.
pub struct BootstrapProviderBindingBundle {
    /// Catalog entry that identifies the provider.
    pub provider_catalog_id: Uuid,
    /// Stable provider implementation kind.
    pub provider_kind: String,
    /// Human-readable provider name.
    pub display_name: String,
    /// Source from which bootstrap credentials can be obtained.
    pub credential_source: BootstrapCredentialSource,
    /// Suggested endpoint when the provider has a default.
    pub default_base_url: Option<String>,
    /// Whether setup must supply an API key.
    pub api_key_required: bool,
    /// Whether setup must supply an endpoint URL.
    pub base_url_required: bool,
    /// Rules governing credentials for this provider.
    pub credential_policy: ProviderCredentialPolicy,
    /// Rules governing endpoint configuration.
    pub base_url_policy: ProviderBaseUrlPolicy,
    /// Strategy used to discover available models.
    pub model_discovery: ProviderModelDiscovery,
    /// Features supported by the provider.
    pub capabilities: ProviderCapabilities,
    /// Runtime behavior required by the provider adapter.
    pub runtime: ProviderRuntimeProfile,
    /// Provider-defined presentation metadata for setup forms.
    pub ui_hints: serde_json::Value,
    /// Proposed model preset for each supported AI purpose.
    pub bindings: Vec<BootstrapProviderBinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// AI configuration choices available during initial setup.
pub struct BootstrapAiSetup {
    /// Providers and their proposed purpose bindings.
    pub binding_bundles: Vec<BootstrapProviderBindingBundle>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Current requirement and options for initial system setup.
pub struct BootstrapStatus {
    /// Whether an owner account must still be created.
    pub setup_required: bool,
    /// AI setup options when configuration is available.
    pub ai_setup: Option<BootstrapAiSetup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Authentication state returned while resolving a browser session.
pub enum SessionMode {
    /// No authenticated session is present.
    Guest,
    /// Initial owner setup must be completed before sign-in.
    BootstrapRequired,
    /// A valid authenticated session is present.
    Authenticated,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Credentials and persistence preference submitted for sign-in.
pub struct LoginRequest {
    /// Account login identifier.
    pub login: String,
    /// Account password; debug output always redacts it.
    pub password: String,
    /// Whether the session should use the longer persistence policy.
    pub remember_me: bool,
}

impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoginRequest")
            .field("login", &self.login)
            .field("password", &"<redacted>")
            .field("remember_me", &self.remember_me)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Provider credentials supplied while creating the initial account.
pub struct BootstrapSetupAi {
    /// Provider implementation selected for initial bindings.
    pub provider_kind: String,
    /// Optional provider API key; debug output always redacts it.
    pub api_key: Option<String>,
    /// Optional provider endpoint; debug output always redacts it.
    pub base_url: Option<String>,
}

impl std::fmt::Debug for BootstrapSetupAi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapSetupAi")
            .field("provider_kind", &self.provider_kind)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Initial owner account and optional AI provider configuration.
pub struct BootstrapSetupRequest {
    /// Login identifier for the initial owner.
    pub login: String,
    /// Optional human-readable owner name.
    pub display_name: Option<String>,
    /// Password for the initial owner; debug output always redacts it.
    pub password: String,
    /// Optional provider configuration applied during setup.
    pub ai_setup: Option<BootstrapSetupAi>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// User identity embedded in an authenticated session.
pub struct SessionUser {
    /// Stable identity-and-access principal identifier.
    pub principal_id: Uuid,
    /// Account login identifier.
    pub login: String,
    /// Account email address.
    pub email: String,
    /// Human-readable account name.
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Active session metadata and its authenticated user.
pub struct AuthenticatedSession {
    /// Stable identifier of the session.
    pub session_id: Uuid,
    /// Time after which the session is no longer valid.
    pub expires_at: DateTime<Utc>,
    /// User authenticated by the session.
    pub user: SessionUser,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Session and localization selected after a successful sign-in.
pub struct LoginResponse {
    /// Newly authenticated session.
    pub session: AuthenticatedSession,
    /// Locale selected for the signed-in user interface.
    pub locale: UiLocale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Result of ending the current session.
pub struct LogoutResponse {
    /// Session that was revoked, when one was present.
    pub revoked_session_id: Option<Uuid>,
    /// Time at which sign-out completed.
    pub signed_out_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Complete authentication and shell state needed to start the UI.
pub struct SessionResolveResponse {
    /// Resolved authentication state.
    pub mode: SessionMode,
    /// Locale selected for the UI.
    pub locale: UiLocale,
    /// Authenticated session when the mode permits one.
    pub session: Option<AuthenticatedSession>,
    /// Effective identity-and-access view for the session principal.
    pub me: Option<IamMe>,
    /// Navigation and capability data used to initialize the shell.
    pub shell_bootstrap: Option<ShellBootstrap>,
    /// Current initial-setup requirement and options.
    pub bootstrap_status: BootstrapStatus,
    /// Optional user-facing explanation of the resolved state.
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Core identity-and-access record for a principal.
pub struct PrincipalProfile {
    /// Stable principal identifier.
    pub id: Uuid,
    /// Category of identity represented by the principal.
    pub principal_kind: String,
    /// Current lifecycle status of the principal.
    pub status: String,
    /// Human-readable principal label.
    pub display_label: String,
}

/// System role assigned to a user principal (viewer < operator < admin).
///
/// Canonical source for the UI shell's capability gating. Mirrors the
/// `public.iam_system_role` PG enum and the `ShellRole` shell-bootstrap field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SystemRole {
    /// Read-only system access.
    Viewer,
    /// Operational access without full administration.
    Operator,
    /// Full system administration access.
    Admin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// User-account attributes associated with a principal.
pub struct UserProfile {
    /// Principal represented by this user account.
    pub principal_id: Uuid,
    /// Login identifier when the account supports direct sign-in.
    pub login: Option<String>,
    /// Email address when one is recorded.
    pub email: Option<String>,
    /// Human-readable user name when one is recorded.
    pub display_name: Option<String>,
    /// System-wide role assigned to the user.
    pub role: SystemRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Principal membership and lifecycle within a workspace.
pub struct WorkspaceMembership {
    /// Workspace to which the membership applies.
    pub workspace_id: Uuid,
    /// Principal holding the membership.
    pub principal_id: Uuid,
    /// Current membership lifecycle state.
    pub membership_state: String,
    /// Time at which the membership began.
    pub joined_at: DateTime<Utc>,
    /// Time at which the membership ended, if inactive.
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Resource scopes to which a permission grant can apply.
pub enum GrantResourceKind {
    /// The entire system.
    System,
    /// One workspace.
    Workspace,
    /// One knowledge library.
    Library,
    /// One document.
    Document,
    /// One query session.
    QuerySession,
    /// One asynchronous operation.
    AsyncOperation,
    /// One external-source connector.
    Connector,
    /// One provider credential.
    ProviderCredential,
    /// One library-to-model binding.
    LibraryBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Actions that may be granted to a principal.
pub enum PermissionKind {
    /// Administer a workspace.
    WorkspaceAdmin,
    /// Read workspace metadata.
    WorkspaceRead,
    /// Read a knowledge library.
    LibraryRead,
    /// Modify a knowledge library.
    LibraryWrite,
    /// Read a document.
    DocumentRead,
    /// Create or modify a document.
    DocumentWrite,
    /// Administer connectors.
    ConnectorAdmin,
    /// Administer provider credentials.
    CredentialAdmin,
    /// Administer library-to-model bindings.
    BindingAdmin,
    /// Run knowledge queries.
    QueryRun,
    /// Read operational state.
    OpsRead,
    /// Read audit events.
    AuditRead,
    /// Administer identities and grants.
    IamAdmin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Permission assigned to a principal for a scoped resource.
pub struct TokenGrant {
    /// Stable identifier of the grant.
    pub id: Uuid,
    /// Principal receiving the permission.
    pub principal_id: Uuid,
    /// Category of the scoped resource.
    pub resource_kind: GrantResourceKind,
    /// Identifier of the scoped resource.
    pub resource_id: Uuid,
    /// Action permitted by the grant.
    pub permission_kind: PermissionKind,
    /// Time at which the grant became effective.
    pub granted_at: DateTime<Utc>,
    /// Time at which the grant expires, if temporary.
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Effective identity, memberships, and grants for the current principal.
pub struct IamMe {
    /// Core principal record.
    pub principal: PrincipalProfile,
    /// User account when the principal represents a user.
    pub user: Option<UserProfile>,
    /// Workspace memberships held by the principal.
    pub workspace_memberships: Vec<WorkspaceMembership>,
    /// Permissions effective for the principal.
    pub effective_grants: Vec<TokenGrant>,
}

#[cfg(test)]
mod secret_debug_tests {
    use super::{BootstrapSetupAi, BootstrapSetupRequest, LoginRequest};

    #[test]
    fn auth_contract_debug_redacts_password_key_and_url_credentials() {
        let login = LoginRequest {
            login: "owner".into(),
            password: "login-password-regression".into(),
            remember_me: true,
        };
        let bootstrap = BootstrapSetupRequest {
            login: "owner".into(),
            display_name: None,
            password: "bootstrap-password-regression".into(),
            ai_setup: Some(BootstrapSetupAi {
                provider_kind: "synthetic".into(),
                api_key: Some("provider-key-regression".into()),
                base_url: Some("https://user:url-secret@host.example/?token=query-secret".into()),
            }),
        };

        let debug = format!("{login:?} {bootstrap:?}");
        for secret in [
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
