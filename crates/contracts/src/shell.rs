//! Application shell, viewer, and scope-selection contracts.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ai::AiBindingPurpose;
use crate::auth::{TokenGrant, UiLocale, WorkspaceMembership};
use crate::diagnostics::OperatorWarning;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
/// Coarse role used to adapt the application shell.
pub enum ShellRole {
    /// May administer the deployment and its workspaces.
    Admin,
    /// May operate content and query workflows within assigned scopes.
    Operator,
    /// Has read-oriented access to assigned scopes.
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Feature capability resolved for the current viewer and scope.
pub struct ShellCapability {
    /// Stable capability identifier consumed by the client.
    pub key: String,
    /// Whether the viewer may use the capability in the active context.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Minimal workspace metadata needed by the scope selector.
pub struct WorkspaceSummary {
    /// Stable workspace identifier.
    pub id: Uuid,
    /// URL-safe workspace identifier.
    pub slug: String,
    /// Display name shown in the scope selector.
    pub name: String,
    /// Current workspace lifecycle state as exposed by the API.
    pub lifecycle_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Library metadata and readiness used by the application shell.
pub struct LibrarySummary {
    /// Stable library identifier.
    pub id: Uuid,
    /// Workspace that owns the library.
    pub workspace_id: Uuid,
    /// URL-safe library identifier within its workspace.
    pub slug: String,
    /// Display name shown to users.
    pub name: String,
    /// Optional human-readable purpose or scope of the library.
    pub description: Option<String>,
    /// Current library lifecycle state as exposed by the API.
    pub lifecycle_state: String,
    /// Whether all requirements for accepting ingestion work are satisfied.
    pub ingestion_ready: bool,
    /// Required AI purposes without an effective model binding.
    pub missing_binding_purposes: Vec<AiBindingPurpose>,
    /// Query readiness when it has been evaluated for this response.
    pub query_ready: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Identity and effective shell role of the authenticated viewer.
pub struct ShellViewer {
    /// Stable principal identifier.
    pub principal_id: Uuid,
    /// Login name used to identify the viewer.
    pub login: String,
    /// Preferred name for UI presentation.
    pub display_name: String,
    /// Human-readable summary of effective access.
    pub access_label: String,
    /// Coarse role used for shell presentation and navigation.
    pub role: ShellRole,
    /// Whether deployment-wide administration is available.
    pub is_admin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Scope and locale selected by the viewer in the application shell.
pub struct ShellScopeSelection {
    /// Selected workspace, or none when no workspace is active.
    pub active_workspace_id: Option<Uuid>,
    /// Selected library, constrained to the active workspace.
    pub active_library_id: Option<Uuid>,
    /// Locale used for client-visible shell content.
    pub locale: UiLocale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Initial authenticated state needed to render the application shell.
pub struct ShellBootstrap {
    /// Authenticated viewer and effective coarse role.
    pub viewer: ShellViewer,
    /// Locale resolved for this session.
    pub locale: UiLocale,
    /// Workspaces visible to the viewer.
    pub workspaces: Vec<WorkspaceSummary>,
    /// Workspace selected for the initial view.
    pub active_workspace_id: Option<Uuid>,
    /// Libraries visible within the active workspace.
    pub libraries: Vec<LibrarySummary>,
    /// Library selected for the initial view.
    pub active_library_id: Option<Uuid>,
    /// Membership records used to explain workspace access.
    pub workspace_memberships: Vec<WorkspaceMembership>,
    /// Token grants effective for the current session and scope.
    pub effective_grants: Vec<TokenGrant>,
    /// Feature switches resolved from role, scope, and deployment state.
    pub capabilities: Vec<ShellCapability>,
    /// Non-fatal conditions the shell should surface to the viewer.
    pub warnings: Vec<OperatorWarning>,
}
