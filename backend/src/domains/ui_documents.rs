use serde::Serialize;

use crate::domains::graph_quality::{
    CanonicalGraphSummary, ExtractionRecoverySummary, MutationImpactScopeSummary,
};

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
    pub settled_estimated_cost: Option<f64>,
    pub in_flight_estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub in_flight_stage_count: i32,
    pub missing_stage_count: i32,
    pub partial_history: bool,
    pub partial_history_reason: Option<String>,
    pub graph_throughput: Option<DocumentGraphThroughputSummary>,
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
pub struct DocumentCollectionAccountingSummary {
    pub total_estimated_cost: Option<f64>,
    pub settled_estimated_cost: Option<f64>,
    pub in_flight_estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub priced_stage_count: i32,
    pub unpriced_stage_count: i32,
    pub in_flight_stage_count: i32,
    pub missing_stage_count: i32,
    pub accounting_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentQueueIsolationSummary {
    pub waiting_reason: String,
    pub queued_count: usize,
    pub processing_count: usize,
    pub isolated_capacity_count: usize,
    pub available_capacity_count: usize,
    pub last_claimed_at: Option<String>,
    pub last_progress_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentCollectionSettlementSummary {
    pub progress_state: String,
    pub live_total_estimated_cost: Option<f64>,
    pub settled_total_estimated_cost: Option<f64>,
    pub missing_total_estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub is_fully_settled: bool,
    pub settled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentCollectionWarning {
    pub warning_kind: String,
    pub warning_scope: String,
    pub warning_message: String,
    pub is_degraded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentTerminalOutcomeSummary {
    pub terminal_state: String,
    pub residual_reason: Option<String>,
    pub queued_count: usize,
    pub processing_count: usize,
    pub pending_graph_count: usize,
    pub failed_document_count: usize,
    pub settled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentGraphHealthSummary {
    pub projection_health: String,
    pub active_projection_count: usize,
    pub retrying_projection_count: usize,
    pub failed_projection_count: usize,
    pub pending_node_write_count: usize,
    pub pending_edge_write_count: usize,
    pub last_failure_kind: Option<String>,
    pub last_failure_at: Option<String>,
    pub is_runtime_readable: bool,
    pub snapshot_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentProviderFailureSummary {
    pub failure_class: String,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<usize>,
    pub upstream_status: Option<String>,
    pub elapsed_ms: Option<i64>,
    pub retry_decision: Option<String>,
    pub usage_visible: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentCollectionProgressCounters {
    pub accepted: usize,
    pub content_extracted: usize,
    pub chunked: usize,
    pub embedded: usize,
    pub extracting_graph: usize,
    pub graph_ready: usize,
    pub ready: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentCollectionStageDiagnostics {
    pub stage: String,
    pub active_count: usize,
    pub completed_count: usize,
    pub failed_count: usize,
    pub avg_elapsed_ms: Option<i64>,
    pub max_elapsed_ms: Option<i64>,
    pub total_estimated_cost: Option<f64>,
    pub settled_estimated_cost: Option<f64>,
    pub in_flight_estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub accounting_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentCollectionFormatDiagnostics {
    pub file_type: String,
    pub document_count: usize,
    pub queued_count: usize,
    pub processing_count: usize,
    pub ready_count: usize,
    pub ready_no_graph_count: usize,
    pub failed_count: usize,
    pub content_extracted_count: usize,
    pub chunked_count: usize,
    pub embedded_count: usize,
    pub extracting_graph_count: usize,
    pub graph_ready_count: usize,
    pub avg_queue_elapsed_ms: Option<i64>,
    pub max_queue_elapsed_ms: Option<i64>,
    pub avg_total_elapsed_ms: Option<i64>,
    pub max_total_elapsed_ms: Option<i64>,
    pub bottleneck_stage: Option<String>,
    pub bottleneck_avg_elapsed_ms: Option<i64>,
    pub bottleneck_max_elapsed_ms: Option<i64>,
    pub total_estimated_cost: Option<f64>,
    pub settled_estimated_cost: Option<f64>,
    pub in_flight_estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub accounting_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentCollectionDiagnostics {
    pub progress: DocumentCollectionProgressCounters,
    pub queue_backlog_count: usize,
    pub processing_backlog_count: usize,
    pub active_backlog_count: usize,
    pub queue_isolation: Option<DocumentQueueIsolationSummary>,
    pub graph_throughput: Option<DocumentCollectionGraphThroughputSummary>,
    pub settlement: Option<DocumentCollectionSettlementSummary>,
    pub terminal_outcome: Option<DocumentTerminalOutcomeSummary>,
    pub graph_health: Option<DocumentGraphHealthSummary>,
    pub warnings: Vec<DocumentCollectionWarning>,
    pub per_stage: Vec<DocumentCollectionStageDiagnostics>,
    pub per_format: Vec<DocumentCollectionFormatDiagnostics>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentsWorkspacePrimarySummary {
    pub progress_label: String,
    pub spend_label: String,
    pub backlog_label: String,
    pub terminal_state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentsWorkspaceDiagnosticChip {
    pub kind: String,
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentsWorkspaceNotice {
    pub kind: String,
    pub title: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentsWorkspaceModel {
    pub primary_summary: DocumentsWorkspacePrimarySummary,
    pub secondary_diagnostics: Vec<DocumentsWorkspaceDiagnosticChip>,
    pub degraded_notices: Vec<DocumentsWorkspaceNotice>,
    pub informational_notices: Vec<DocumentsWorkspaceNotice>,
    pub table_document_count: usize,
    pub active_filter_count: usize,
    pub highlighted_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentGraphThroughputSummary {
    pub processed_chunks: usize,
    pub total_chunks: usize,
    pub progress_percent: Option<i32>,
    pub provider_call_count: usize,
    pub resumed_chunk_count: usize,
    pub resume_hit_count: usize,
    pub replayed_chunk_count: usize,
    pub duplicate_work_ratio: Option<f64>,
    pub max_downgrade_level: usize,
    pub avg_call_elapsed_ms: Option<i64>,
    pub avg_chunk_elapsed_ms: Option<i64>,
    pub avg_chars_per_second: Option<f64>,
    pub avg_tokens_per_second: Option<f64>,
    pub last_provider_call_at: Option<String>,
    pub last_checkpoint_at: String,
    pub last_checkpoint_elapsed_ms: i64,
    pub next_checkpoint_eta_ms: Option<i64>,
    pub pressure_kind: Option<String>,
    pub cadence: String,
    pub recommended_poll_interval_ms: i64,
    pub bottleneck_rank: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentCollectionGraphThroughputSummary {
    pub tracked_document_count: usize,
    pub active_document_count: usize,
    pub processed_chunks: usize,
    pub total_chunks: usize,
    pub progress_percent: Option<i32>,
    pub provider_call_count: usize,
    pub resumed_chunk_count: usize,
    pub resume_hit_count: usize,
    pub replayed_chunk_count: usize,
    pub duplicate_work_ratio: Option<f64>,
    pub max_downgrade_level: usize,
    pub avg_call_elapsed_ms: Option<i64>,
    pub avg_chunk_elapsed_ms: Option<i64>,
    pub avg_chars_per_second: Option<f64>,
    pub avg_tokens_per_second: Option<f64>,
    pub last_provider_call_at: Option<String>,
    pub last_checkpoint_at: String,
    pub last_checkpoint_elapsed_ms: i64,
    pub next_checkpoint_eta_ms: Option<i64>,
    pub pressure_kind: Option<String>,
    pub cadence: String,
    pub recommended_poll_interval_ms: i64,
    pub bottleneck_rank: Option<usize>,
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
    pub accounting_scope: String,
    pub pricing_status: String,
    pub usage_event_id: Option<String>,
    pub cost_ledger_id: Option<String>,
    pub pricing_catalog_entry_id: Option<String>,
    pub estimated_cost: Option<f64>,
    pub settled_estimated_cost: Option<f64>,
    pub in_flight_estimated_cost: Option<f64>,
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
    pub settled_estimated_cost: Option<f64>,
    pub in_flight_estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub priced_stage_count: i32,
    pub unpriced_stage_count: i32,
    pub in_flight_stage_count: i32,
    pub missing_stage_count: i32,
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
    pub preview_text: Option<String>,
    pub preview_truncated: bool,
    pub warning_count: usize,
    pub normalization_status: String,
    pub ocr_source: Option<String>,
    pub recovery: Option<ExtractionRecoverySummary>,
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
    pub settled_estimated_cost: Option<f64>,
    pub in_flight_estimated_cost: Option<f64>,
    pub currency: Option<String>,
    pub in_flight_stage_count: i32,
    pub missing_stage_count: i32,
    pub partial_history: bool,
    pub partial_history_reason: Option<String>,
    pub mutation: DocumentMutationState,
    pub requested_by: Option<String>,
    pub error_message: Option<String>,
    pub failure_class: Option<String>,
    pub operator_action: Option<String>,
    pub summary: String,
    pub graph_node_id: Option<String>,
    pub canonical_summary_preview: Option<CanonicalGraphSummary>,
    pub can_download_text: bool,
    pub can_append: bool,
    pub can_replace: bool,
    pub can_remove: bool,
    pub reconciliation_scope: Option<MutationImpactScopeSummary>,
    pub provider_failure: Option<DocumentProviderFailureSummary>,
    pub graph_throughput: Option<DocumentGraphThroughputSummary>,
    pub extracted_stats: DocumentExtractedStats,
    pub graph_stats: DocumentGraphStats,
    pub collection_diagnostics: Option<DocumentCollectionDiagnostics>,
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
    pub accounting: DocumentCollectionAccountingSummary,
    pub diagnostics: DocumentCollectionDiagnostics,
    pub workspace: Option<DocumentsWorkspaceModel>,
    pub rows: Vec<DocumentListItem>,
}
