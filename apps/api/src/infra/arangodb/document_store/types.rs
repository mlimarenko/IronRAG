use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocumentRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    #[serde(default)]
    pub file_name: Option<String>,
    pub title: Option<String>,
    pub document_state: String,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_revision_no: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRevisionRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub revision_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_number: i64,
    pub revision_state: String,
    pub revision_kind: String,
    pub storage_ref: Option<String>,
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
    pub superseded_by_revision_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeChunkRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub chunk_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_index: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_kind: Option<String>,
    pub content_text: String,
    pub normalized_text: String,
    pub span_start: Option<i32>,
    pub span_end: Option<i32>,
    pub token_count: Option<i32>,
    #[serde(default)]
    pub support_block_ids: Vec<Uuid>,
    pub section_path: Vec<String>,
    pub heading_trail: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub literal_digest: Option<String>,
    pub chunk_state: String,
    pub text_generation: Option<i64>,
    pub vector_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeStructuredRevisionRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub revision_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub preparation_state: String,
    pub normalization_profile: String,
    pub source_format: String,
    pub language_code: Option<String>,
    pub block_count: i32,
    pub chunk_count: i32,
    pub typed_fact_count: i32,
    pub outline_json: serde_json::Value,
    pub prepared_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeStructuredBlockRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub block_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub ordinal: i32,
    pub block_kind: String,
    pub text: String,
    pub normalized_text: String,
    pub heading_trail: Vec<String>,
    pub section_path: Vec<String>,
    pub page_number: Option<i32>,
    pub span_start: Option<i32>,
    pub span_end: Option<i32>,
    pub parent_block_id: Option<Uuid>,
    pub table_coordinates_json: Option<serde_json::Value>,
    pub code_language: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeTechnicalFactRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub fact_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub fact_kind: String,
    pub canonical_value_text: String,
    pub canonical_value_exact: String,
    pub canonical_value_json: serde_json::Value,
    pub display_value: String,
    pub qualifiers_json: serde_json::Value,
    pub support_block_ids: Vec<Uuid>,
    pub support_chunk_ids: Vec<Uuid>,
    pub confidence: Option<f64>,
    pub extraction_kind: String,
    pub conflict_group_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeLibraryGenerationRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub generation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub active_text_generation: i64,
    pub active_vector_generation: i64,
    pub active_graph_generation: i64,
    pub degraded_state: String,
    pub updated_at: DateTime<Utc>,
}
