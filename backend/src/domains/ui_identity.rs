use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct UiUser {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub role_label: String,
    pub initials: String,
    pub preferred_locale: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiSession {
    pub id: Uuid,
    pub user: UiUser,
    pub active_workspace_id: Option<Uuid>,
    pub active_library_id: Option<Uuid>,
    pub locale: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceMember {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role_label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryAccessGrant {
    pub library_id: Uuid,
    pub user_id: Uuid,
    pub access_level: String,
}
