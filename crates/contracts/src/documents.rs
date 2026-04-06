use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::diagnostics::{MessageLevel, OperatorWarning};
use crate::graph::GraphSurface;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentReadiness {
    Processing,
    Readable,
    GraphSparse,
    GraphReady,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus {
    Queued,
    Processing,
    Ready,
    ReadyNoGraph,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRevisionSummary {
    pub revision_id: Option<Uuid>,
    pub revision_number: Option<i64>,
    pub mime_type: Option<String>,
    pub byte_size: Option<i64>,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub storage_key: Option<String>,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSegment {
    pub id: Uuid,
    pub ordinal: i32,
    pub block_kind: String,
    pub heading_trail: Vec<String>,
    pub excerpt: String,
    pub page_number: Option<i32>,
    pub chunk_count: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalFact {
    pub id: Uuid,
    pub fact_kind: String,
    pub display_value: String,
    pub canonical_value: String,
    pub confidence: Option<f64>,
    pub qualifiers: Vec<String>,
    pub support_chunk_count: i32,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebPageProvenance {
    pub run_id: Option<Uuid>,
    pub candidate_id: Option<Uuid>,
    pub source_uri: Option<String>,
    pub canonical_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSummary {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub file_name: String,
    pub file_type: String,
    pub file_size: i64,
    pub uploaded_at: DateTime<Utc>,
    pub status: DocumentStatus,
    pub readiness: DocumentReadiness,
    pub stage_label: Option<String>,
    pub progress_percent: Option<i32>,
    pub cost_usd: Option<f64>,
    pub failure_message: Option<String>,
    pub can_retry: bool,
    pub prepared_segment_count: Option<i32>,
    pub technical_fact_count: Option<i32>,
    pub source_format: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentsOverview {
    pub total_documents: i32,
    pub ready_documents: i32,
    pub processing_documents: i32,
    pub failed_documents: i32,
    pub graph_sparse_documents: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFilterState {
    pub search_query: Option<String>,
    pub statuses: Vec<DocumentStatus>,
    pub readiness: Vec<DocumentReadiness>,
    pub source_formats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentDetail {
    pub summary: DocumentSummary,
    pub external_key: Option<String>,
    pub document_state: Option<String>,
    pub active_revision: Option<DocumentRevisionSummary>,
    pub web_page_provenance: Option<WebPageProvenance>,
    pub prepared_segments: Vec<PreparedSegment>,
    pub technical_facts: Vec<TechnicalFact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentsSurface {
    pub overview: DocumentsOverview,
    pub filters: DocumentFilterState,
    pub documents: Vec<DocumentSummary>,
    pub selected_document_id: Option<Uuid>,
    pub selected_document: Option<DocumentDetail>,
    pub web_runs: Vec<WebIngestRunSummary>,
    pub warnings: Vec<OperatorWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebIngestRunState {
    Accepted,
    Discovering,
    Processing,
    Completed,
    CompletedPartial,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebRunCounts {
    pub discovered: i32,
    pub eligible: i32,
    pub processed: i32,
    pub queued: i32,
    pub processing: i32,
    pub duplicates: i32,
    pub excluded: i32,
    pub blocked: i32,
    pub failed: i32,
    pub canceled: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebIngestRunSummary {
    pub run_id: Uuid,
    pub library_id: Uuid,
    pub mode: String,
    pub boundary_policy: String,
    pub max_depth: i32,
    pub max_pages: i32,
    pub run_state: WebIngestRunState,
    pub seed_url: String,
    pub counts: WebRunCounts,
    pub last_activity_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebIngestRunReceipt {
    pub run_id: Uuid,
    pub library_id: Uuid,
    pub mode: String,
    pub run_state: WebIngestRunState,
    pub counts: WebRunCounts,
    pub async_operation_id: Option<Uuid>,
    pub failure_code: Option<String>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardMetric {
    pub key: String,
    pub label: String,
    pub value: String,
    pub level: MessageLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardAttentionItem {
    pub code: String,
    pub title: String,
    pub detail: String,
    pub route_path: String,
    pub level: MessageLevel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSurface {
    pub overview: DocumentsOverview,
    pub metrics: Vec<DashboardMetric>,
    pub recent_documents: Vec<DocumentSummary>,
    pub recent_web_runs: Vec<WebIngestRunSummary>,
    pub graph: GraphSurface,
    pub attention: Vec<DashboardAttentionItem>,
    pub warnings: Vec<OperatorWarning>,
}
