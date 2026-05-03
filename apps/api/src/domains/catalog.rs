use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::ai::AiBindingPurpose;
use crate::domains::recognition::LibraryRecognitionPolicy;
use crate::shared::web::ingest::WebIngestPolicy;

/// Chunking strategy applied when segmenting document content into chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkingTemplate {
    Naive,
    Structured,
    Tabular,
    Qa,
    Code,
    Paper,
}

impl ChunkingTemplate {
    #[must_use]
    pub fn from_db_str(s: &str) -> Self {
        match s.trim() {
            "structured" => Self::Structured,
            "tabular" => Self::Tabular,
            "qa" => Self::Qa,
            "code" => Self::Code,
            "paper" => Self::Paper,
            _ => Self::Naive,
        }
    }

    #[must_use]
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Naive => "naive",
            Self::Structured => "structured",
            Self::Tabular => "tabular",
            Self::Qa => "qa",
            Self::Code => "code",
            Self::Paper => "paper",
        }
    }

    /// Returns (max_chars, overlap_chars) for the chunking profile of this template.
    #[must_use]
    pub const fn chunking_params(self) -> (usize, usize) {
        match self {
            Self::Naive | Self::Structured => (2_800, 280),
            Self::Tabular => (4_000, 0),
            Self::Qa => (1_400, 0),
            Self::Code => (3_600, 360),
            Self::Paper => (2_200, 220),
        }
    }
}

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
    pub extraction_prompt: Option<String>,
    pub web_ingest_policy: WebIngestPolicy,
    pub recognition_policy: LibraryRecognitionPolicy,
    pub lifecycle_state: CatalogLifecycleState,
    pub chunking_template: ChunkingTemplate,
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
