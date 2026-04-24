use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::shared::web::ingest::WebIngestIgnorePattern;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestJob {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
    pub connector_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub job_kind: String,
    pub queue_state: String,
    pub priority: i32,
    pub dedupe_key: Option<String>,
    pub queued_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestAttempt {
    pub id: Uuid,
    pub job_id: Uuid,
    pub attempt_number: i32,
    pub worker_principal_id: Option<Uuid>,
    pub lease_token: Option<String>,
    pub knowledge_generation_id: Option<Uuid>,
    pub attempt_state: String,
    pub current_stage: Option<String>,
    pub started_at: DateTime<Utc>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure_class: Option<String>,
    pub failure_code: Option<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestStageEvent {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub stage_name: String,
    pub stage_state: String,
    pub ordinal: i32,
    pub message: Option<String>,
    pub details_json: serde_json::Value,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebIngestRunReceipt {
    pub run_id: Uuid,
    pub library_id: Uuid,
    pub mode: String,
    pub run_state: String,
    pub async_operation_id: Option<Uuid>,
    pub counts: WebRunCounts,
    pub failure_code: Option<String>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebRunCounts {
    pub discovered: i64,
    pub eligible: i64,
    pub processed: i64,
    pub queued: i64,
    pub processing: i64,
    pub duplicates: i64,
    pub excluded: i64,
    pub blocked: i64,
    pub failed: i64,
    pub canceled: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebIngestRunSummary {
    pub run_id: Uuid,
    pub library_id: Uuid,
    pub mode: String,
    pub boundary_policy: String,
    pub max_depth: i32,
    pub max_pages: i32,
    pub ignore_patterns: Vec<WebIngestIgnorePattern>,
    pub run_state: String,
    pub seed_url: String,
    pub counts: WebRunCounts,
    pub last_activity_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebIngestRun {
    pub run_id: Uuid,
    pub mutation_id: Uuid,
    pub async_operation_id: Option<Uuid>,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mode: String,
    pub seed_url: String,
    pub normalized_seed_url: String,
    pub boundary_policy: String,
    pub max_depth: i32,
    pub max_pages: i32,
    pub ignore_patterns: Vec<WebIngestIgnorePattern>,
    pub run_state: String,
    pub requested_by_principal_id: Option<Uuid>,
    pub requested_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
    pub counts: WebRunCounts,
    pub last_activity_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDiscoveredPage {
    pub candidate_id: Uuid,
    pub run_id: Uuid,
    pub discovered_url: Option<String>,
    pub normalized_url: String,
    pub final_url: Option<String>,
    pub canonical_url: Option<String>,
    pub depth: i32,
    pub referrer_candidate_id: Option<Uuid>,
    pub host_classification: String,
    pub candidate_state: String,
    pub classification_reason: Option<String>,
    pub classification_detail: Option<String>,
    pub content_type: Option<String>,
    pub http_status: Option<i32>,
    pub discovered_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub document_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
}
