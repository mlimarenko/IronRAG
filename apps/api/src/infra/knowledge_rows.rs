use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Slim projection for the documents-page inspector counts.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct StructuredRevisionCounts {
    #[serde(default)]
    pub block_count: i32,
    #[serde(default)]
    pub typed_fact_count: i32,
}

/// Aggregated readiness and generation signals for a library.
/// Mirrors the per-state aggregates used to derive the synthetic library
/// generation row.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct LibraryGenerationSignals {
    #[serde(default)]
    pub active_text_generation: i64,
    #[serde(default)]
    pub active_vector_generation: i64,
    #[serde(default)]
    pub active_graph_generation: i64,
    #[serde(default)]
    pub has_ready_text: bool,
    #[serde(default)]
    pub has_ready_vector: bool,
    #[serde(default)]
    pub has_ready_graph: bool,
    #[serde(default)]
    pub latest_created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeDocumentRow {
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    #[serde(default)]
    pub file_name: Option<String>,
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_hint: Option<String>,
    pub document_state: String,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_revision_no: Option<i64>,
    /// Canonical structural source parent (mirrored from
    /// `content_document.parent_document_id`). `None` for primary documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_document_id: Option<Uuid>,
    /// Typed document role mirrored from `content_document.document_role`
    /// (`primary` | `attachment` | `attached_context`).
    #[serde(default = "default_document_role")]
    pub document_role: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

fn default_document_role() -> String {
    crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeRevisionRow {
    pub revision_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_number: i64,
    pub revision_state: String,
    pub revision_kind: String,
    pub storage_ref: Option<String>,
    pub source_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_hint: Option<String>,
    pub mime_type: String,
    pub checksum: String,
    pub title: Option<String>,
    pub byte_size: i64,
    pub normalized_text: Option<String>,
    pub text_checksum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_checksum: Option<String>,
    pub text_state: String,
    pub vector_state: String,
    pub graph_state: String,
    pub text_readable_at: Option<DateTime<Utc>>,
    pub vector_ready_at: Option<DateTime<Utc>>,
    pub graph_ready_at: Option<DateTime<Utc>>,
    pub superseded_by_revision_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Slim projection used by the prepared-segments inspector to
/// map `support_block_ids` back to chunk ids without pulling the
/// full chunk text over the wire.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeChunkSupportReferenceRow {
    pub chunk_id: Uuid,
    #[serde(default)]
    pub support_block_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeChunkRow {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raptor_level: Option<i32>,
    /// Earliest record timestamp aggregated into this chunk (JSONL ingest
    /// only; None for non-temporal sources). Mirrored from
    /// `content_chunk.occurred_at` so search can hard-bound by time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Latest record timestamp aggregated into this chunk. Equals
    /// `occurred_at` for single-record chunks; None when `occurred_at`
    /// is None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_until: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeStructuredRevisionRow {
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeStructuredBlockRow {
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeTechnicalFactRow {
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeLibraryGenerationRow {
    pub generation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub active_text_generation: i64,
    pub active_vector_generation: i64,
    pub active_graph_generation: i64,
    pub degraded_state: String,
    pub updated_at: DateTime<Utc>,
}

pub const KNOWLEDGE_CHUNK_VECTOR_KIND: &str = "chunk_embedding";
pub const KNOWLEDGE_ENTITY_VECTOR_KIND: &str = "entity_embedding";

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeChunkVectorRow {
    pub vector_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub chunk_id: Uuid,
    pub revision_id: Uuid,
    pub embedding_model_key: String,
    pub vector_kind: String,
    pub dimensions: i32,
    pub vector: Vec<f32>,
    pub freshness_generation: i64,
    pub created_at: DateTime<Utc>,
    /// Mirror of `KnowledgeChunkRow.occurred_at` so the ANN post-filter
    /// can hard-bound by time without a per-candidate `DOCUMENT()`
    /// lookup. JSONL ingest only; None for non-temporal sources.
    /// (Architect-amendment-1.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<DateTime<Utc>>,
    /// Mirror of `KnowledgeChunkRow.occurred_until`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeEntityVectorRow {
    pub vector_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub entity_id: Uuid,
    pub embedding_model_key: String,
    pub vector_kind: String,
    pub dimensions: i32,
    pub vector: Vec<f32>,
    pub freshness_generation: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeChunkSearchRow {
    pub chunk_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub content_text: String,
    pub normalized_text: String,
    pub section_path: Vec<String>,
    pub heading_trail: Vec<String>,
    pub score: f64,
    #[serde(default)]
    pub quality_score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeStructuredBlockSearchRow {
    pub block_id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub ordinal: i32,
    pub block_kind: String,
    pub text: String,
    pub normalized_text: String,
    pub section_path: Vec<String>,
    pub heading_trail: Vec<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeTechnicalFactSearchRow {
    pub fact_id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub fact_kind: String,
    pub canonical_value_text: String,
    pub display_value: String,
    pub exact_match: bool,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeEntitySearchRow {
    pub entity_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub canonical_label: String,
    pub entity_type: String,
    pub summary: Option<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeRelationSearchRow {
    pub relation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub predicate: String,
    pub normalized_assertion: String,
    pub summary: Option<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeChunkVectorSearchRow {
    pub vector_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub chunk_id: Uuid,
    pub revision_id: Uuid,
    pub embedding_model_key: String,
    pub vector_kind: String,
    pub freshness_generation: i64,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeEntityVectorSearchRow {
    pub vector_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub entity_id: Uuid,
    pub embedding_model_key: String,
    pub vector_kind: String,
    pub freshness_generation: i64,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct GraphViewNodeWrite {
    pub node_id: Uuid,
    pub canonical_key: String,
    pub label: String,
    pub node_type: String,
    pub support_count: i32,
    pub summary: Option<String>,
    pub aliases: Vec<String>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct GraphViewEdgeWrite {
    pub edge_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub canonical_key: String,
    pub support_count: i32,
    pub summary: Option<String>,
    pub weight: Option<f64>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
pub struct GraphViewData {
    pub nodes: Vec<GraphViewNodeWrite>,
    pub edges: Vec<GraphViewEdgeWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphViewWriteError {
    #[error("graph write contention: {message}")]
    GraphWriteContention { message: String },
    #[error("graph persistence integrity: {message}")]
    GraphPersistenceIntegrity { message: String },
    #[error("graph write failure: {message}")]
    GraphWriteFailure { message: String },
}

impl GraphViewWriteError {
    #[must_use]
    pub const fn is_retryable_contention(&self) -> bool {
        matches!(self, Self::GraphWriteContention { .. })
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::GraphWriteContention { message }
            | Self::GraphPersistenceIntegrity { message }
            | Self::GraphWriteFailure { message } => message,
        }
    }
}

#[must_use]
pub fn sanitize_graph_view_writes(
    nodes: &[GraphViewNodeWrite],
    edges: &[GraphViewEdgeWrite],
) -> (Vec<GraphViewNodeWrite>, Vec<GraphViewEdgeWrite>, usize) {
    let mut ordered_nodes = nodes.to_vec();
    ordered_nodes.sort_by_key(|node| node.node_id);

    let available_node_ids =
        ordered_nodes.iter().map(|node| node.node_id).collect::<std::collections::BTreeSet<_>>();
    let mut ordered_edges = edges
        .iter()
        .filter(|edge| {
            available_node_ids.contains(&edge.from_node_id)
                && available_node_ids.contains(&edge.to_node_id)
        })
        .cloned()
        .collect::<Vec<_>>();
    ordered_edges.sort_by_key(|edge| (edge.from_node_id, edge.to_node_id, edge.edge_id));

    let skipped_edge_count = edges.len().saturating_sub(ordered_edges.len());
    (ordered_nodes, ordered_edges, skipped_edge_count)
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeEntityCandidateRow {
    pub candidate_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub candidate_label: String,
    pub candidate_type: String,
    #[serde(default)]
    pub candidate_sub_type: Option<String>,
    pub normalization_key: String,
    pub confidence: Option<f64>,
    pub extraction_method: String,
    pub candidate_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeRelationCandidateRow {
    pub candidate_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    #[serde(default)]
    pub subject_label: String,
    pub subject_candidate_key: String,
    pub predicate: String,
    #[serde(default)]
    pub object_label: String,
    pub object_candidate_key: String,
    pub normalized_assertion: String,
    pub confidence: Option<f64>,
    pub extraction_method: String,
    pub candidate_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeEntityRow {
    pub entity_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub canonical_label: String,
    pub aliases: Vec<String>,
    pub entity_type: String,
    #[serde(default)]
    pub entity_sub_type: Option<String>,
    pub summary: Option<String>,
    pub confidence: Option<f64>,
    pub support_count: i64,
    pub freshness_generation: i64,
    pub entity_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeRelationRow {
    pub relation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub predicate: String,
    pub normalized_assertion: String,
    pub confidence: Option<f64>,
    pub support_count: i64,
    pub contradiction_state: String,
    pub freshness_generation: i64,
    pub relation_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeEvidenceRow {
    pub evidence_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    #[serde(default)]
    pub chunk_id: Option<Uuid>,
    #[serde(default)]
    pub block_id: Option<Uuid>,
    #[serde(default)]
    pub fact_id: Option<Uuid>,
    pub span_start: Option<i32>,
    pub span_end: Option<i32>,
    #[serde(default)]
    pub quote_text: String,
    #[serde(default)]
    pub literal_spans_json: serde_json::Value,
    #[serde(default)]
    pub evidence_kind: String,
    pub extraction_method: String,
    pub confidence: Option<f64>,
    pub evidence_state: String,
    pub freshness_generation: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeGraphTraversalRow {
    pub path_length: i64,
    pub vertex_kind: String,
    pub vertex_id: Uuid,
    pub edge_kind: Option<String>,
    pub edge_key: Option<String>,
    pub edge_rank: Option<i32>,
    pub edge_score: Option<f64>,
    pub edge_inclusion_reason: Option<String>,
    pub vertex: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeRelationEvidenceLookupRow {
    pub relation: KnowledgeRelationRow,
    pub evidence: KnowledgeEvidenceRow,
    pub support_edge_rank: Option<i32>,
    pub support_edge_score: Option<f64>,
    pub support_edge_inclusion_reason: Option<String>,
    pub source_document: Option<KnowledgeDocumentRow>,
    pub source_revision: Option<KnowledgeRevisionRow>,
    pub source_chunk: Option<KnowledgeChunkRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeRelationTopologyRow {
    #[serde(flatten)]
    pub relation: KnowledgeRelationRow,
    pub subject_entity_id: Uuid,
    pub object_entity_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeDocumentGraphLinkRow {
    pub document_id: Uuid,
    pub target_node_id: Uuid,
    pub target_node_type: String,
    pub relation_type: String,
    pub support_count: i64,
}

#[derive(Debug, Clone)]
pub struct NewKnowledgeEntityCandidate {
    pub candidate_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub candidate_label: String,
    pub candidate_type: String,
    pub candidate_sub_type: Option<String>,
    pub normalization_key: String,
    pub confidence: Option<f64>,
    pub extraction_method: String,
    pub candidate_state: String,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewKnowledgeRelationCandidate {
    pub candidate_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub subject_label: String,
    pub subject_candidate_key: String,
    pub predicate: String,
    pub object_label: String,
    pub object_candidate_key: String,
    pub normalized_assertion: String,
    pub confidence: Option<f64>,
    pub extraction_method: String,
    pub candidate_state: String,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewKnowledgeEntity {
    pub entity_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub canonical_label: String,
    pub aliases: Vec<String>,
    pub entity_type: String,
    pub entity_sub_type: Option<String>,
    pub summary: Option<String>,
    pub confidence: Option<f64>,
    pub support_count: i64,
    pub freshness_generation: i64,
    pub entity_state: String,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewKnowledgeRelation {
    pub relation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub predicate: String,
    pub normalized_assertion: String,
    pub confidence: Option<f64>,
    pub support_count: i64,
    pub contradiction_state: String,
    pub freshness_generation: i64,
    pub relation_state: String,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewKnowledgeEvidence {
    pub evidence_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub block_id: Option<Uuid>,
    pub fact_id: Option<Uuid>,
    pub span_start: Option<i32>,
    pub span_end: Option<i32>,
    pub quote_text: String,
    pub literal_spans_json: serde_json::Value,
    pub evidence_kind: String,
    pub extraction_method: String,
    pub confidence: Option<f64>,
    pub evidence_state: String,
    pub freshness_generation: i64,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeContextBundleRow {
    pub bundle_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub query_execution_id: Option<Uuid>,
    pub bundle_state: String,
    pub bundle_strategy: String,
    pub requested_mode: String,
    pub resolved_mode: String,
    #[serde(default)]
    pub selected_fact_ids: Vec<Uuid>,
    pub verification_state: String,
    #[serde(default)]
    pub verification_warnings: serde_json::Value,
    pub freshness_snapshot: serde_json::Value,
    pub candidate_summary: serde_json::Value,
    pub assembly_diagnostics: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeRetrievalTraceRow {
    pub trace_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub query_execution_id: Option<Uuid>,
    pub bundle_id: Uuid,
    pub trace_state: String,
    pub retrieval_strategy: String,
    pub candidate_counts: serde_json::Value,
    pub dropped_reasons: serde_json::Value,
    pub timing_breakdown: serde_json::Value,
    pub diagnostics_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeBundleChunkEdgeRow {
    pub bundle_id: Uuid,
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeBundleEntityEdgeRow {
    pub bundle_id: Uuid,
    pub entity_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeBundleRelationEdgeRow {
    pub bundle_id: Uuid,
    pub relation_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeBundleEvidenceEdgeRow {
    pub bundle_id: Uuid,
    pub evidence_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeBundleChunkReferenceRow {
    pub bundle_id: Uuid,
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeBundleEntityReferenceRow {
    pub bundle_id: Uuid,
    pub entity_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeBundleRelationReferenceRow {
    pub bundle_id: Uuid,
    pub relation_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeBundleEvidenceReferenceRow {
    pub bundle_id: Uuid,
    pub evidence_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KnowledgeContextBundleReferenceSetRow {
    pub bundle: KnowledgeContextBundleRow,
    pub chunk_references: Vec<KnowledgeBundleChunkReferenceRow>,
    pub entity_references: Vec<KnowledgeBundleEntityReferenceRow>,
    pub relation_references: Vec<KnowledgeBundleRelationReferenceRow>,
    pub evidence_references: Vec<KnowledgeBundleEvidenceReferenceRow>,
}
