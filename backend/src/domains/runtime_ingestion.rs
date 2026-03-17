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
    pub page_count: Option<u32>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStageAccountingLine {
    pub ingestion_run_id: Uuid,
    pub stage: String,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub capability: String,
    pub billing_unit: String,
    pub pricing_catalog_entry_id: Option<Uuid>,
    pub pricing_status: String,
    pub estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub attribution_source: RuntimeStageAttributionSource,
}
