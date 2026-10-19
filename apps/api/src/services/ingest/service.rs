use std::collections::HashMap;

use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ops::{
        ASYNC_OP_STATUS_FAILED, ASYNC_OP_STATUS_PROCESSING, ASYNC_OP_STATUS_READY,
        OpsAsyncOperation,
    },
    domains::{
        ingest::{
            IngestAttempt, IngestJob, IngestStageEvent, is_retry_budget_exhausted,
            next_job_queue_state_after_finalize, retry_backoff_with_deferral,
        },
        webhook::{WebhookEvent, revision_ready_event_id},
    },
    infra::repositories::{
        content_document_head_repository,
        ingest_repository::{
            self, IngestStageEventRow, NewIngestAttempt, NewIngestJob, NewIngestStageEvent,
            UpdateIngestAttempt, UpdateIngestJob,
        },
        ops_repository,
    },
    interfaces::http::router_support::ApiError,
    services::ops::service::{UpdateAsyncOperationCommand, map_async_operation_row},
};

mod attempts;
mod finalization;
mod jobs;
mod progress;
mod stages;
#[cfg(test)]
mod tests;

pub use progress::{IngestStageMetadata, ingest_stage_metadata};

pub const INGEST_STAGE_EXTRACT_CONTENT: &str = "extract_content";
pub const INGEST_STAGE_PREPARE_STRUCTURE: &str = "prepare_structure";
pub const INGEST_STAGE_CHUNK_CONTENT: &str = "chunk_content";
pub const INGEST_STAGE_EMBED_CHUNK: &str = "embed_chunk";
pub const INGEST_STAGE_EXTRACT_TECHNICAL_FACTS: &str = "extract_technical_facts";
pub const INGEST_STAGE_EXTRACT_GRAPH: &str = "extract_graph";
pub const INGEST_STAGE_VERIFY_QUERY_ANSWER: &str = "verify_query_answer";
pub const INGEST_STAGE_FINALIZING: &str = "finalizing";
pub const INGEST_STAGE_WEB_DISCOVERY: &str = "web_discovery";
pub const INGEST_STAGE_WEB_MATERIALIZE_PAGE: &str = "web_materialize_page";
pub const INGEST_STAGE_WEBHOOK_DELIVERY: &str = "webhook_delivery";

pub(crate) const QUEUE_STALE_LEASE_SECONDS: i64 = 60;
const INLINE_QUEUE_LEASE_OWNER: &str = "inline";

#[derive(Debug, Clone)]
pub struct AdmitIngestJobCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
    pub connector_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub job_kind: String,
    pub priority: i32,
    pub dedupe_key: Option<String>,
    pub available_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone)]
pub struct LeaseAttemptCommand {
    pub job_id: Uuid,
    pub worker_principal_id: Option<Uuid>,
    pub lease_token: Option<String>,
    pub expected_queue_lease_token: Option<String>,
    pub knowledge_generation_id: Option<Uuid>,
    pub current_stage: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HeartbeatAttemptCommand {
    pub attempt_id: Uuid,
    pub knowledge_generation_id: Option<Uuid>,
    pub current_stage: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FinalizeAttemptCommand {
    pub attempt_id: Uuid,
    pub knowledge_generation_id: Option<Uuid>,
    pub attempt_state: String,
    pub current_stage: Option<String>,
    pub failure_class: Option<String>,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
    pub retryable: bool,
    /// Operational provider cooldown (a 429 `Retry-After` or rate-limit/quota
    /// cooldown). When set, the job is requeued after this delay and the
    /// attempt budget is never exhausted by the deferral.
    pub operational_defer: Option<chrono::Duration>,
}

#[derive(Debug, Clone)]
pub struct RecordStageEventCommand {
    pub attempt_id: Uuid,
    pub stage_name: String,
    pub stage_state: String,
    pub message: Option<String>,
    pub details_json: serde_json::Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub cached_tokens: Option<i32>,
    pub estimated_cost: Option<rust_decimal::Decimal>,
    pub currency_code: Option<String>,
    pub elapsed_ms: Option<i64>,
}

impl RecordStageEventCommand {
    pub(crate) fn for_stage(
        attempt_id: Uuid,
        stage_name: &str,
        stage_state: &str,
        message: Option<String>,
        details_json: serde_json::Value,
        elapsed_ms: Option<i64>,
    ) -> Self {
        Self {
            attempt_id,
            stage_name: stage_name.to_string(),
            stage_state: stage_state.to_string(),
            message,
            details_json,
            provider_kind: None,
            model_name: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            cached_tokens: None,
            estimated_cost: None,
            currency_code: None,
            elapsed_ms,
        }
    }

    pub(crate) fn for_chunk_embedding(
        attempt_id: Uuid,
        outcome: &crate::services::query::search::EmbedChunksStageOutcome,
        elapsed_ms: Option<i64>,
    ) -> Self {
        Self {
            attempt_id,
            stage_name: INGEST_STAGE_EMBED_CHUNK.to_string(),
            stage_state: "completed".to_string(),
            message: Some("chunk embeddings persisted".to_string()),
            details_json: serde_json::json!({
                "chunksEmbedded": outcome.chunks_embedded,
                "chunksReused": outcome.chunks_reused,
                "providerKind": outcome.provider_kind,
                "modelName": outcome.model_name,
            }),
            provider_kind: outcome.provider_kind.clone(),
            model_name: outcome.model_name.clone(),
            prompt_tokens: outcome.prompt_tokens,
            completion_tokens: outcome.completion_tokens,
            total_tokens: outcome.total_tokens,
            cached_tokens: None,
            estimated_cost: None,
            currency_code: None,
            elapsed_ms,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordStageUnitProgressCommand {
    pub attempt_id: Uuid,
    pub stage_name: String,
    pub completed_units: u32,
    pub total_units: u32,
    pub details_json: Value,
}

#[derive(Debug, Clone)]
pub struct IngestJobHandle {
    pub job: IngestJob,
    pub latest_attempt: Option<IngestAttempt>,
    pub async_operation: Option<OpsAsyncOperation>,
}

#[derive(Debug, Clone)]
pub struct IngestAttemptHandle {
    pub job: IngestJob,
    pub attempt: IngestAttempt,
    pub async_operation: Option<OpsAsyncOperation>,
}

#[derive(Clone, Default)]
pub struct IngestService;

impl IngestService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

fn map_job_row(row: ingest_repository::IngestJobRow) -> IngestJob {
    IngestJob {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        mutation_id: row.mutation_id,
        connector_id: row.connector_id,
        async_operation_id: row.async_operation_id,
        knowledge_document_id: row.knowledge_document_id,
        knowledge_revision_id: row.knowledge_revision_id,
        job_kind: row.job_kind,
        queue_state: row.queue_state,
        priority: row.priority,
        dedupe_key: row.dedupe_key,
        queued_at: row.queued_at,
        available_at: row.available_at,
        completed_at: row.completed_at,
    }
}

fn map_attempt_row(row: ingest_repository::IngestAttemptRow) -> IngestAttempt {
    IngestAttempt {
        id: row.id,
        job_id: row.job_id,
        attempt_number: row.attempt_number,
        worker_principal_id: row.worker_principal_id,
        lease_token: row.lease_token,
        knowledge_generation_id: row.knowledge_generation_id,
        attempt_state: row.attempt_state,
        current_stage: row.current_stage,
        started_at: row.started_at,
        heartbeat_at: row.heartbeat_at,
        finished_at: row.finished_at,
        failure_class: row.failure_class,
        failure_code: row.failure_code,
        failure_message: row.failure_message,
        progress_percent: row.progress_percent,
        retryable: row.retryable,
    }
}

fn map_stage_event_row(row: ingest_repository::IngestStageEventRow) -> IngestStageEvent {
    IngestStageEvent {
        id: row.id,
        attempt_id: row.attempt_id,
        stage_name: row.stage_name,
        stage_state: row.stage_state,
        ordinal: row.ordinal,
        message: row.message,
        details_json: row.details_json,
        recorded_at: row.recorded_at,
    }
}

async fn update_linked_async_operation(
    state: &AppState,
    operation_id: Option<Uuid>,
    status: &str,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
    failure_code: Option<String>,
) -> Result<(), ApiError> {
    if let Some(operation_id) = operation_id {
        let _ = state
            .application_services
            .ops
            .update_async_operation(
                state,
                UpdateAsyncOperationCommand {
                    operation_id,
                    status: status.to_string(),
                    completed_at,
                    failure_code,
                },
            )
            .await?;
    }
    Ok(())
}
