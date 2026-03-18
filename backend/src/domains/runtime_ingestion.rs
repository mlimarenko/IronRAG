use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::provider_profiles::EffectiveProviderProfile;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeIngestionStatus {
    Queued,
    Processing,
    Ready,
    ReadyNoGraph,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeIngestionStage {
    Accepted,
    ExtractingContent,
    Chunking,
    EmbeddingChunks,
    ExtractingGraph,
    MergingGraph,
    ProjectingGraph,
    Finalizing,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAttemptKind {
    InitialUpload,
    Retry,
    Reprocess,
    UpdateAppend,
    UpdateReplace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDocumentActivityStatus {
    Queued,
    Active,
    Blocked,
    Retrying,
    Stalled,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStageAttributionSource {
    StageNative,
    Reconciled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAccountingTruthStatus {
    Priced,
    Unpriced,
    InFlightUnsettled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeAttemptMetadata {
    pub logical_document_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub attempt_no: i32,
    pub attempt_kind: RuntimeAttemptKind,
    pub mutation_kind: Option<String>,
    pub stale_guard_revision_no: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeBenchmarkSummary {
    pub queue_started_at: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub first_active_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub queue_elapsed_ms: Option<i64>,
    pub total_elapsed_ms: Option<i64>,
    pub last_activity_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeDocumentContributionSummary {
    pub chunk_count: Option<usize>,
    pub graph_node_count: Option<usize>,
    pub graph_edge_count: Option<usize>,
    pub filtered_graph_artifact_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeIngestionRun {
    pub id: Uuid,
    pub track_id: String,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub file_name: String,
    pub file_type: String,
    pub status: RuntimeIngestionStatus,
    pub stage: RuntimeIngestionStage,
    pub progress_percent: Option<u8>,
    pub activity_status: RuntimeDocumentActivityStatus,
    pub provider_profile: EffectiveProviderProfile,
    pub latest_error: Option<String>,
    pub attempt: RuntimeAttemptMetadata,
    pub benchmarks: RuntimeBenchmarkSummary,
    pub contribution: RuntimeDocumentContributionSummary,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStageEvent {
    pub ingestion_run_id: Uuid,
    pub attempt_no: i32,
    pub stage: RuntimeIngestionStage,
    pub status: String,
    pub message: Option<String>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub elapsed_ms: Option<i64>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedContentArtifact {
    pub ingestion_run_id: Uuid,
    pub extraction_kind: String,
    pub content_text: Option<String>,
    pub preview_text: Option<String>,
    pub preview_truncated: bool,
    pub page_count: Option<u32>,
    pub warnings: Vec<String>,
    pub warning_count: usize,
    pub normalization_status: String,
    pub ocr_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStageAccountingLine {
    pub ingestion_run_id: Uuid,
    pub stage: String,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub accounting_scope: String,
    pub capability: String,
    pub billing_unit: String,
    pub pricing_catalog_entry_id: Option<Uuid>,
    pub pricing_status: String,
    pub estimated_cost: Option<Decimal>,
    pub settled_estimated_cost: Option<Decimal>,
    pub in_flight_estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub attribution_source: RuntimeStageAttributionSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeAttemptAccountingTruth {
    pub total_estimated_cost: Option<Decimal>,
    pub settled_estimated_cost: Option<Decimal>,
    pub in_flight_estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub priced_stage_count: i32,
    pub unpriced_stage_count: i32,
    pub in_flight_stage_count: i32,
    pub missing_stage_count: i32,
    pub accounting_status: RuntimeAccountingTruthStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCollectionAccountingTruth {
    pub document_count: usize,
    pub total_estimated_cost: Option<Decimal>,
    pub settled_estimated_cost: Option<Decimal>,
    pub in_flight_estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub priced_stage_count: i32,
    pub unpriced_stage_count: i32,
    pub in_flight_stage_count: i32,
    pub missing_stage_count: i32,
    pub accounting_status: RuntimeAccountingTruthStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCollectionProgressCounters {
    pub accepted: usize,
    pub content_extracted: usize,
    pub chunked: usize,
    pub embedded: usize,
    pub extracting_graph: usize,
    pub graph_ready: usize,
    pub ready: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCollectionStageDiagnostics {
    pub stage: String,
    pub active_count: usize,
    pub completed_count: usize,
    pub failed_count: usize,
    pub avg_elapsed_ms: Option<i64>,
    pub max_elapsed_ms: Option<i64>,
    pub total_estimated_cost: Option<Decimal>,
    pub settled_estimated_cost: Option<Decimal>,
    pub in_flight_estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub accounting_status: RuntimeAccountingTruthStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeCollectionFormatDiagnostics {
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
    pub total_estimated_cost: Option<Decimal>,
    pub settled_estimated_cost: Option<Decimal>,
    pub in_flight_estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub accounting_status: RuntimeAccountingTruthStatus,
}
