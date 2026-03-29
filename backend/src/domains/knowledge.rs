use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocument {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub title: Option<String>,
    pub document_state: String,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRevision {
    pub id: Uuid,
    pub document_id: Uuid,
    pub revision_number: i64,
    pub revision_state: String,
    pub source_uri: Option<String>,
    pub mime_type: String,
    pub checksum: String,
    pub title: Option<String>,
    pub byte_size: i64,
    pub normalized_text: Option<String>,
    pub text_checksum: Option<String>,
    pub text_state: String,
    pub vector_state: String,
    pub graph_state: String,
    pub text_readable_at: Option<DateTime<Utc>>,
    pub vector_ready_at: Option<DateTime<Utc>>,
    pub graph_ready_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeChunk {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub chunk_index: i32,
    pub content_text: String,
    pub token_count: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeLibraryGeneration {
    pub id: Uuid,
    pub library_id: Uuid,
    pub generation_kind: String,
    pub generation_state: String,
    pub source_revision_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntity {
    pub id: Uuid,
    pub library_id: Uuid,
    pub canonical_name: String,
    pub entity_type: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRelation {
    pub id: Uuid,
    pub library_id: Uuid,
    pub relation_type: String,
    pub canonical_label: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEvidence {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub quote_text: String,
    pub confidence_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeContextBundle {
    pub id: Uuid,
    pub library_id: Uuid,
    pub query_execution_id: Option<Uuid>,
    pub bundle_state: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBundleEdge {
    pub bundle_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
}
