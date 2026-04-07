use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::diagnostics::OperatorWarning;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeType {
    Entity,
    Person,
    Organization,
    Location,
    Event,
    Artifact,
    Natural,
    Process,
    Concept,
    Attribute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphStatus {
    Empty,
    Building,
    Rebuilding,
    Ready,
    Partial,
    Failed,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphConvergenceStatus {
    Current,
    Partial,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphGenerationSummary {
    pub generation_id: Option<Uuid>,
    pub active_graph_generation: i64,
    pub degraded_state: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphReadinessSummary {
    pub library_id: Uuid,
    pub document_counts_by_readiness: Vec<(String, i64)>,
    pub graph_ready_document_count: i64,
    pub graph_sparse_document_count: i64,
    pub typed_fact_document_count: i64,
    pub latest_generation: Option<GraphGenerationSummary>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub id: Uuid,
    pub canonical_key: String,
    pub label: String,
    pub node_type: GraphNodeType,
    pub secondary_label: Option<String>,
    pub support_count: i32,
    pub summary: Option<String>,
    pub filtered_artifact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub id: Uuid,
    pub canonical_key: String,
    pub source: Uuid,
    pub target: Uuid,
    pub relation_type: String,
    pub support_count: i32,
    pub filtered_artifact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphSurface {
    pub library_id: Uuid,
    pub status: GraphStatus,
    pub convergence_status: Option<GraphConvergenceStatus>,
    pub warning: Option<String>,
    pub node_count: i32,
    pub relation_count: i32,
    pub edge_count: i32,
    pub graph_ready_document_count: i32,
    pub graph_sparse_document_count: i32,
    pub typed_fact_document_count: i32,
    pub updated_at: Option<DateTime<Utc>>,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub readiness_summary: Option<GraphReadinessSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphDocumentReference {
    pub document_id: Uuid,
    pub document_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEvidence {
    pub id: String,
    pub document_id: Option<Uuid>,
    pub document_label: Option<String>,
    pub chunk_id: Option<Uuid>,
    pub excerpt: String,
    pub support_kind: Option<String>,
    pub extraction_method: Option<String>,
    pub confidence: Option<f64>,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphRelatedNode {
    pub id: Uuid,
    pub label: String,
    pub relation_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeDetail {
    pub id: Uuid,
    pub label: String,
    pub node_type: GraphNodeType,
    pub summary: String,
    pub properties: Vec<(String, String)>,
    pub related_nodes: Vec<GraphRelatedNode>,
    pub supporting_documents: Vec<GraphDocumentReference>,
    pub evidence: Vec<GraphEvidence>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphFilterState {
    pub search_query: Option<String>,
    pub focus_document_id: Option<Uuid>,
    pub include_filtered_artifacts: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphWorkbenchSurface {
    pub graph: GraphSurface,
    pub filters: GraphFilterState,
    pub selected_node_id: Option<Uuid>,
    pub selected_node: Option<GraphNodeDetail>,
    pub diagnostics: Vec<OperatorWarning>,
}
