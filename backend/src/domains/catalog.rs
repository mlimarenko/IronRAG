use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::ai::AiBindingPurpose;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CatalogLifecycleState {
    Active,
    Disabled,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogWorkspace {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_state: CatalogLifecycleState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogLibraryIngestionReadiness {
    pub ready: bool,
    pub missing_binding_purposes: Vec<AiBindingPurpose>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogLibrary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub lifecycle_state: CatalogLifecycleState,
    pub ingestion_readiness: CatalogLibraryIngestionReadiness,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogLibraryConnector {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub connector_kind: String,
    pub display_name: String,
    pub configuration_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
