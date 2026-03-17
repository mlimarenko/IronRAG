use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct DocumentMutationState {
    pub kind: Option<String>,
    pub status: Option<String>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentRevisionHistoryItem {
    pub id: String,
    pub revision_no: i32,
    pub revision_kind: String,
    pub status: String,
    pub source_file_name: String,
    pub appended_text_excerpt: Option<String>,
    pub accepted_at: String,
    pub activated_at: Option<String>,
    pub superseded_at: Option<String>,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentSummaryCounters {
    pub queued: usize,
    pub processing: usize,
    pub ready: usize,
    pub ready_no_graph: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentListItem {
    pub id: String,
    pub logical_document_id: Option<String>,
    pub file_name: String,
    pub file_type: String,
    pub file_size_label: String,
    pub uploaded_at: String,
    pub library_name: String,
    pub stage: String,
    pub status: String,
    pub progress_percent: Option<u8>,
    pub activity_status: Option<String>,
    pub last_activity_at: Option<String>,
    pub stalled_reason: Option<String>,
    pub active_revision_no: Option<i32>,
    pub active_revision_kind: Option<String>,
    pub latest_attempt_no: i32,
    pub accounting_status: String,
    pub total_estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub partial_history: bool,
    pub partial_history_reason: Option<String>,
    pub mutation: DocumentMutationState,
    pub can_retry: bool,
    pub can_append: bool,
    pub can_replace: bool,
    pub can_remove: bool,
    pub detail_available: bool,
    pub chunk_count: Option<usize>,
    pub graph_node_count: Option<usize>,
    pub graph_edge_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentFilterValues {
    pub statuses: Vec<String>,
    pub file_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentHistoryItem {
    pub attempt_no: i32,
    pub status: String,
    pub stage: String,
    pub error_message: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentStageAccountingItem {
    pub pricing_status: String,
    pub usage_event_id: Option<String>,
    pub cost_ledger_id: Option<String>,
    pub pricing_catalog_entry_id: Option<String>,
    pub estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub attribution_source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentStageBenchmarkItem {
    pub stage: String,
    pub status: String,
    pub message: Option<String>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub elapsed_ms: Option<i64>,
    pub accounting: Option<DocumentStageAccountingItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentAttemptSummary {
    pub total_estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub priced_stage_count: i32,
    pub unpriced_stage_count: i32,
    pub accounting_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentAttemptGroup {
    pub attempt_no: i32,
    pub revision_no: Option<i32>,
    pub revision_id: Option<String>,
    pub attempt_kind: Option<String>,
    pub status: String,
    pub queue_elapsed_ms: Option<i64>,
    pub total_elapsed_ms: Option<i64>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub activity_status: Option<String>,
    pub last_activity_at: Option<String>,
    pub partial_history: bool,
    pub partial_history_reason: Option<String>,
    pub summary: DocumentAttemptSummary,
    pub benchmarks: Vec<DocumentStageBenchmarkItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentExtractedStats {
    pub chunk_count: Option<usize>,
    pub document_id: Option<String>,
    pub checksum: Option<String>,
    pub page_count: Option<i32>,
    pub extraction_kind: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentGraphStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub evidence_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentDetailModel {
    pub id: String,
    pub logical_document_id: Option<String>,
    pub file_name: String,
    pub file_type: String,
    pub file_size_label: String,
    pub uploaded_at: String,
    pub library_name: String,
    pub stage: String,
    pub status: String,
    pub progress_percent: Option<u8>,
    pub activity_status: Option<String>,
    pub last_activity_at: Option<String>,
    pub stalled_reason: Option<String>,
    pub active_revision_no: Option<i32>,
    pub active_revision_kind: Option<String>,
    pub active_revision_status: Option<String>,
    pub latest_attempt_no: i32,
    pub accounting_status: String,
    pub total_estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub partial_history: bool,
    pub partial_history_reason: Option<String>,
    pub mutation: DocumentMutationState,
    pub requested_by: Option<String>,
    pub error_message: Option<String>,
    pub summary: String,
    pub graph_node_id: Option<String>,
    pub can_download_text: bool,
    pub can_append: bool,
    pub can_replace: bool,
    pub can_remove: bool,
    pub extracted_stats: DocumentExtractedStats,
    pub graph_stats: DocumentGraphStats,
    pub revision_history: Vec<DocumentRevisionHistoryItem>,
    pub processing_history: Vec<DocumentHistoryItem>,
    pub attempts: Vec<DocumentAttemptGroup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentSurfaceModel {
    pub accepted_formats: Vec<String>,
    pub max_size_mb: u64,
    pub graph_status: String,
    pub graph_warning: Option<String>,
    pub rebuild_backlog_count: usize,
    pub counters: DocumentSummaryCounters,
    pub filters: DocumentFilterValues,
    pub rows: Vec<DocumentListItem>,
}
