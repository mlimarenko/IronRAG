use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PrincipalKind {
    User,
    ApiToken,
    Worker,
    Bootstrap,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GrantResourceKind {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMembership {
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub membership_state: String,
    pub joined_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal {
    pub id: Uuid,
    pub principal_kind: PrincipalKind,
    pub display_label: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiToken {
    pub principal_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub token_prefix: String,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub resource_kind: GrantResourceKind,
    pub resource_id: Uuid,
    pub permission_kind: String,
    pub granted_at: DateTime<Utc>,
}
