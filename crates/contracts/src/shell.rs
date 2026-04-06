use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::{BootstrapBindingPurpose, TokenGrant, UiLocale, WorkspaceMembership};
use crate::diagnostics::OperatorWarning;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellRole {
    Admin,
    Operator,
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellCapability {
    pub key: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub lifecycle_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibrarySummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub lifecycle_state: String,
    pub ingestion_ready: bool,
    pub missing_binding_purposes: Vec<BootstrapBindingPurpose>,
    pub query_ready: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellViewer {
    pub principal_id: Uuid,
    pub login: String,
    pub display_name: String,
    pub access_label: String,
    pub role: ShellRole,
    pub is_admin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellScopeSelection {
    pub active_workspace_id: Option<Uuid>,
    pub active_library_id: Option<Uuid>,
    pub locale: UiLocale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellBootstrap {
    pub viewer: ShellViewer,
    pub locale: UiLocale,
    pub workspaces: Vec<WorkspaceSummary>,
    pub active_workspace_id: Option<Uuid>,
    pub libraries: Vec<LibrarySummary>,
    pub active_library_id: Option<Uuid>,
    pub workspace_memberships: Vec<WorkspaceMembership>,
    pub effective_grants: Vec<TokenGrant>,
    pub capabilities: Vec<ShellCapability>,
    pub warnings: Vec<OperatorWarning>,
}
