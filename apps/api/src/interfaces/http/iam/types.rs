use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::ai::AiBindingPurpose;

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
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub library_ids: Vec<Uuid>,
    #[serde(default)]
    pub permission_kinds: Vec<IamPermissionKind>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGrantRequest {
    pub principal_id: Uuid,
    pub resource_kind: IamGrantResourceKind,
    pub resource_id: Uuid,
    pub permission_kind: IamPermissionKind,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrincipalResponse {
    pub id: Uuid,
    pub principal_kind: IamPrincipalKind,
    pub status: String,
    pub display_label: String,
    pub created_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserResponse {
    pub principal_id: Uuid,
    pub login: String,
    pub email: String,
    pub display_name: String,
    pub auth_provider_kind: String,
    pub external_subject: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenWorkspaceSummaryResponse {
    pub id: Uuid,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenLibrarySummaryResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub display_name: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TokenScopeKind {
    System,
    Workspace,
    Library,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenScopeResponse {
    pub kind: TokenScopeKind,
    pub workspace: Option<TokenWorkspaceSummaryResponse>,
    pub libraries: Vec<TokenLibrarySummaryResponse>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenIssuerResponse {
    pub principal_id: Uuid,
    pub display_label: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenGrantSummaryResponse {
    pub resource_kind: IamGrantResourceKind,
    pub resource_id: Uuid,
    pub permission_kind: IamPermissionKind,
    pub workspace: Option<TokenWorkspaceSummaryResponse>,
    pub library: Option<TokenLibrarySummaryResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMembershipResponse {
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub membership_state: String,
    pub joined_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
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
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
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
pub struct BootstrapStatusResponse {
    pub setup_required: bool,
    pub ai_setup: Option<BootstrapAiSetupResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginSessionRequest {
    pub login: String,
    pub password: String,
    pub remember_me: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapSetupRequest {
    pub login: String,
    pub display_name: Option<String>,
    pub password: String,
    pub ai_setup: Option<BootstrapSetupAiRequest>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapProviderPresetResponse {
    pub binding_purpose: AiBindingPurpose,
    pub model_catalog_id: Uuid,
    pub model_name: String,
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapProviderPresetBundleResponse {
    pub id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub credential_source: String,
    pub default_base_url: Option<String>,
    pub api_key_required: bool,
    pub base_url_required: bool,
    pub presets: Vec<BootstrapProviderPresetResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapAiSetupResponse {
    pub preset_bundles: Vec<BootstrapProviderPresetBundleResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapSetupAiRequest {
    pub provider_kind: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUserResponse {
    pub principal_id: Uuid,
    pub login: String,
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    pub session_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub user: SessionUserResponse,
}
