use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::shared::{
    structured_document::{
        StructuredBlockKind, StructuredOutlineEntry, StructuredSourceSpan,
        StructuredTableCoordinates,
    },
    technical_facts::{TechnicalFactKind, TechnicalFactQualifier, TechnicalFactValue},
};

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
#[serde(rename_all = "camelCase")]
pub struct KnowledgeLibrarySummary {
    pub library_id: Uuid,
    pub document_counts_by_readiness: BTreeMap<String, i64>,
    pub node_count: i64,
    pub edge_count: i64,
    pub graph_ready_document_count: i64,
    pub graph_sparse_document_count: i64,
    pub typed_fact_document_count: i64,
    pub updated_at: DateTime<Utc>,
    pub latest_generation: Option<KnowledgeLibraryGeneration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntity {
    pub id: Uuid,
    pub library_id: Uuid,
    pub canonical_label: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredDocumentRevision {
    pub revision_id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub preparation_state: String,
    pub normalization_profile: String,
    pub source_format: String,
    pub language_code: Option<String>,
    pub block_count: i32,
    pub chunk_count: i32,
    pub typed_fact_count: i32,
    pub outline: Vec<StructuredOutlineEntry>,
    pub prepared_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredBlock {
    pub block_id: Uuid,
    pub revision_id: Uuid,
    pub ordinal: i32,
    pub block_kind: StructuredBlockKind,
    pub text: String,
    pub normalized_text: String,
    pub heading_trail: Vec<String>,
    pub section_path: Vec<String>,
    pub page_number: Option<i32>,
    pub source_span: Option<StructuredSourceSpan>,
    pub parent_block_id: Option<Uuid>,
    pub table_coordinates: Option<StructuredTableCoordinates>,
    pub code_language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypedTechnicalFact {
    pub fact_id: Uuid,
    pub revision_id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub fact_kind: TechnicalFactKind,
    pub canonical_value: TechnicalFactValue,
    pub display_value: String,
    pub qualifiers: Vec<TechnicalFactQualifier>,
    pub support_block_ids: Vec<Uuid>,
    pub support_chunk_ids: Vec<Uuid>,
    pub confidence: Option<f64>,
    pub extraction_kind: String,
    pub conflict_group_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEvidenceLiteralSpan {
    pub start_offset: i32,
    pub end_offset: i32,
    pub literal: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEvidenceRecord {
    pub evidence_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub block_id: Option<Uuid>,
    pub fact_id: Option<Uuid>,
    pub quote_text: String,
    pub literal_spans: Vec<GraphEvidenceLiteralSpan>,
    pub confidence: Option<f64>,
    pub evidence_kind: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSegmentListItem {
    pub segment_id: Uuid,
    pub revision_id: Uuid,
    pub ordinal: i32,
    pub block_kind: StructuredBlockKind,
    pub heading_trail: Vec<String>,
    pub section_path: Vec<String>,
    pub page_number: Option<i32>,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSegmentDetail {
    pub segment: PreparedSegmentListItem,
    pub text: String,
    pub normalized_text: String,
    pub source_span: Option<StructuredSourceSpan>,
    pub parent_block_id: Option<Uuid>,
    pub table_coordinates: Option<StructuredTableCoordinates>,
    pub code_language: Option<String>,
    pub support_chunk_ids: Vec<Uuid>,
}
