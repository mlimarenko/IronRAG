use std::collections::HashMap;

use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ops::{
        ASYNC_OP_STATUS_FAILED, ASYNC_OP_STATUS_PROCESSING, ASYNC_OP_STATUS_READY,
        OpsAsyncOperation, OpsAsyncOperationStatus,
    },
    domains::{
        ingest::{
            IngestAttempt, IngestJob, IngestStageEvent, is_retry_budget_exhausted,
            next_job_queue_state_after_finalize, retry_backoff_after_attempt,
        },
        webhook::{WebhookEvent, revision_ready_event_id},
    },
    infra::repositories::{
        content_repository,
        ingest_repository::{
            self, IngestStageEventRow, NewIngestAttempt, NewIngestJob, NewIngestStageEvent,
            UpdateIngestAttempt, UpdateIngestJob,
        },
        ops_repository,
    },
    interfaces::http::router_support::ApiError,
    services::ops::service::UpdateAsyncOperationCommand,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CanonicalIngestProgressProfile {
    Balanced,
    InlineText,
    ExtractionHeavy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CanonicalIngestStageProgressWeight {
    stage_name: &'static str,
    percent_weight: i32,
}

const BALANCED_PROGRESS_WEIGHTS: [CanonicalIngestStageProgressWeight; 7] = [
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_CONTENT,
        percent_weight: 12,
    },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_PREPARE_STRUCTURE,
        percent_weight: 6,
    },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_CHUNK_CONTENT,
        percent_weight: 5,
    },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
        percent_weight: 5,
    },
    CanonicalIngestStageProgressWeight { stage_name: INGEST_STAGE_EMBED_CHUNK, percent_weight: 30 },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_GRAPH,
        percent_weight: 40,
    },
    CanonicalIngestStageProgressWeight { stage_name: INGEST_STAGE_FINALIZING, percent_weight: 2 },
];

const INLINE_TEXT_PROGRESS_WEIGHTS: [CanonicalIngestStageProgressWeight; 7] = [
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_CONTENT,
        percent_weight: 4,
    },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_PREPARE_STRUCTURE,
        percent_weight: 6,
    },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_CHUNK_CONTENT,
        percent_weight: 5,
    },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
        percent_weight: 5,
    },
    CanonicalIngestStageProgressWeight { stage_name: INGEST_STAGE_EMBED_CHUNK, percent_weight: 35 },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_GRAPH,
        percent_weight: 43,
    },
    CanonicalIngestStageProgressWeight { stage_name: INGEST_STAGE_FINALIZING, percent_weight: 2 },
];

const EXTRACTION_HEAVY_PROGRESS_WEIGHTS: [CanonicalIngestStageProgressWeight; 7] = [
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_CONTENT,
        percent_weight: 30,
    },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_PREPARE_STRUCTURE,
        percent_weight: 6,
    },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_CHUNK_CONTENT,
        percent_weight: 4,
    },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
        percent_weight: 4,
    },
    CanonicalIngestStageProgressWeight { stage_name: INGEST_STAGE_EMBED_CHUNK, percent_weight: 24 },
    CanonicalIngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_GRAPH,
        percent_weight: 30,
    },
    CanonicalIngestStageProgressWeight { stage_name: INGEST_STAGE_FINALIZING, percent_weight: 2 },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalIngestStageMetadata {
    pub stage_name: &'static str,
    pub stage_rank: i32,
    pub lifecycle_kind: &'static str,
}

const fn progress_weights_for_profile(
    profile: CanonicalIngestProgressProfile,
) -> &'static [CanonicalIngestStageProgressWeight; 7] {
    match profile {
        CanonicalIngestProgressProfile::Balanced => &BALANCED_PROGRESS_WEIGHTS,
        CanonicalIngestProgressProfile::InlineText => &INLINE_TEXT_PROGRESS_WEIGHTS,
        CanonicalIngestProgressProfile::ExtractionHeavy => &EXTRACTION_HEAVY_PROGRESS_WEIGHTS,
    }
}

fn canonical_ingest_stage_progress_percent_for_profile(
    profile: CanonicalIngestProgressProfile,
    stage_name: &str,
    stage_state: &str,
) -> Option<i32> {
    let weights = progress_weights_for_profile(profile);
    let stage_index = weights.iter().position(|candidate| candidate.stage_name == stage_name)?;
    let progress_before_stage: i32 =
        weights.iter().take(stage_index).map(|weight| weight.percent_weight).sum();
    let stage_weight = weights[stage_index].percent_weight;

    match stage_state {
        "started" | "failed" => {
            let visible_started_bump = (stage_weight / 5).clamp(1, 5);
            Some((progress_before_stage + visible_started_bump).min(99))
        }
        "completed" => Some((progress_before_stage + stage_weight).min(100)),
        _ => None,
    }
}

#[must_use]
fn canonical_ingest_stage_unit_progress_percent_for_profile(
    profile: CanonicalIngestProgressProfile,
    stage_name: &str,
    completed_units: u32,
    total_units: u32,
) -> Option<i32> {
    if total_units == 0 {
        return None;
    }

    let stage_started =
        canonical_ingest_stage_progress_percent_for_profile(profile, stage_name, "started")?;
    let stage_completed =
        canonical_ingest_stage_progress_percent_for_profile(profile, stage_name, "completed")?;
    let stage_span = (stage_completed - stage_started).max(1);
    let completed_units = completed_units.min(total_units);
    let stage_offset =
        ((i64::from(completed_units) * i64::from(stage_span)) / i64::from(total_units)) as i32;

    Some((stage_started + stage_offset).clamp(stage_started, stage_completed.min(99)))
}

fn canonical_ingest_attempt_stage_progress_percent(
    existing_events: &[IngestStageEventRow],
    stage_name: &str,
    stage_state: &str,
    current_details: &Value,
) -> Option<i32> {
    let profile = canonical_ingest_progress_profile(existing_events, current_details);
    canonical_ingest_stage_progress_percent_for_profile(profile, stage_name, stage_state)
}

fn canonical_ingest_attempt_stage_unit_progress_percent(
    existing_events: &[IngestStageEventRow],
    stage_name: &str,
    completed_units: u32,
    total_units: u32,
    current_details: &Value,
) -> Option<i32> {
    let profile = canonical_ingest_progress_profile(existing_events, current_details);
    canonical_ingest_stage_unit_progress_percent_for_profile(
        profile,
        stage_name,
        completed_units,
        total_units,
    )
}

fn canonical_ingest_progress_profile(
    existing_events: &[IngestStageEventRow],
    current_details: &Value,
) -> CanonicalIngestProgressProfile {
    progress_profile_from_stage_details(current_details)
        .or_else(|| {
            existing_events
                .iter()
                .rev()
                .find_map(|event| progress_profile_from_stage_details(&event.details_json))
        })
        .unwrap_or(CanonicalIngestProgressProfile::Balanced)
}

fn progress_profile_from_stage_details(details: &Value) -> Option<CanonicalIngestProgressProfile> {
    let source = details.get("source").and_then(Value::as_str);
    if source == Some("knowledge_revision") {
        return Some(CanonicalIngestProgressProfile::InlineText);
    }

    let file_kind = details.get("fileKind").and_then(Value::as_str);
    match file_kind {
        Some("text_like") => Some(CanonicalIngestProgressProfile::InlineText),
        Some("pdf" | "image" | "docx" | "spreadsheet" | "pptx") => {
            Some(CanonicalIngestProgressProfile::ExtractionHeavy)
        }
        Some(_) => Some(CanonicalIngestProgressProfile::Balanced),
        None => {
            let has_pages =
                details.get("pageCount").and_then(Value::as_i64).unwrap_or_default() > 0;
            let has_extract_units =
                details.get("extractUnitCount").and_then(Value::as_i64).unwrap_or_default() > 0;
            if has_pages || has_extract_units {
                Some(CanonicalIngestProgressProfile::ExtractionHeavy)
            } else {
                None
            }
        }
    }
}

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

struct FinalizeDecision {
    effective_retryable: bool,
    failure_message: Option<String>,
    next_queue_state: String,
    completed_at: Option<chrono::DateTime<Utc>>,
}

async fn load_leased_finalize_rows(
    state: &AppState,
    command: &FinalizeAttemptCommand,
) -> Result<(ingest_repository::IngestAttemptRow, ingest_repository::IngestJobRow), ApiError> {
    let attempt = ingest_repository::get_ingest_attempt_by_id(
        &state.persistence.postgres,
        command.attempt_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", command.attempt_id))?;
    if attempt.attempt_state != "leased" {
        return Err(ApiError::Conflict(format!(
            "ingest attempt {} is no longer leased; current state is {}",
            command.attempt_id, attempt.attempt_state
        )));
    }
    let job = ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, attempt.job_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("ingest_job", attempt.job_id))?;
    if job.queue_state != "leased" {
        return Err(ApiError::Conflict(format!(
            "ingest job {} is no longer leased; current state is {}",
            job.id, job.queue_state
        )));
    }
    Ok((attempt, job))
}

fn finalize_decision(
    command: &FinalizeAttemptCommand,
    attempt: &ingest_repository::IngestAttemptRow,
) -> FinalizeDecision {
    let budget_exhausted = is_retry_budget_exhausted(
        &command.attempt_state,
        command.retryable,
        attempt.attempt_number,
    );
    let effective_retryable = command.retryable && !budget_exhausted;
    let failure_message = finalized_failure_message(command, attempt, budget_exhausted);
    let next_queue_state =
        next_job_queue_state_after_finalize(&command.attempt_state, effective_retryable)
            .to_string();
    let completed_at = matches!(next_queue_state.as_str(), "completed" | "failed").then(Utc::now);
    FinalizeDecision { effective_retryable, failure_message, next_queue_state, completed_at }
}

fn finalized_failure_message(
    command: &FinalizeAttemptCommand,
    attempt: &ingest_repository::IngestAttemptRow,
    budget_exhausted: bool,
) -> Option<String> {
    if command.attempt_state == "succeeded" {
        return None;
    }
    let message = command.failure_message.clone().or_else(|| attempt.failure_message.clone());
    if !budget_exhausted {
        return message;
    }
    Some(format!(
        "{} (exhausted retry budget after {} attempts)",
        message.unwrap_or_else(|| "ingest attempt failed".to_string()),
        attempt.attempt_number,
    ))
}

async fn build_finalize_lifecycle_event(
    state: &AppState,
    command: &FinalizeAttemptCommand,
    job: &ingest_repository::IngestJobRow,
    next_queue_state: &str,
) -> Result<Option<WebhookEvent>, ApiError> {
    if command.attempt_state != "succeeded"
        || next_queue_state != "completed"
        || job.job_kind != "content_mutation"
    {
        return Ok(None);
    }
    let document_id = job.knowledge_document_id.ok_or_else(|| {
        ApiError::InternalMessage(
            "completed content mutation is missing knowledge_document_id".to_string(),
        )
    })?;
    let revision_id = job.knowledge_revision_id.ok_or_else(|| {
        ApiError::InternalMessage(
            "completed content mutation is missing knowledge_revision_id".to_string(),
        )
    })?;
    let head = content_repository::get_document_head(&state.persistence.postgres, document_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| {
            ApiError::InternalMessage(
                "completed content mutation is missing its document head".to_string(),
            )
        })?;
    if head.readable_revision_id != Some(revision_id) {
        return Err(ApiError::Conflict(format!(
            "content mutation job {} cannot finalize before revision {} is readable",
            job.id, revision_id
        )));
    }
    Ok(Some(WebhookEvent {
        event_type: "revision.ready".to_string(),
        event_id: revision_ready_event_id(revision_id),
        occurred_at: Utc::now(),
        workspace_id: job.workspace_id,
        library_id: job.library_id,
        payload_json: serde_json::json!({
            "document_id": document_id,
            "revision_id": revision_id,
            "library_id": job.library_id,
        }),
    }))
}

async fn persist_finalized_attempt(
    state: &AppState,
    command: &FinalizeAttemptCommand,
    current_stage: Option<String>,
    attempt: &ingest_repository::IngestAttemptRow,
    job: &ingest_repository::IngestJobRow,
    decision: &FinalizeDecision,
    lifecycle_event: Option<&WebhookEvent>,
) -> Result<ingest_repository::IngestAttemptRow, ApiError> {
    ingest_repository::finalize_leased_ingest_attempt_and_update_job(
        &state.persistence.postgres,
        command.attempt_id,
        &UpdateIngestAttempt {
            worker_principal_id: attempt.worker_principal_id,
            lease_token: attempt.lease_token.clone(),
            knowledge_generation_id: command
                .knowledge_generation_id
                .or(attempt.knowledge_generation_id),
            attempt_state: command.attempt_state.clone(),
            current_stage,
            heartbeat_at: Some(Utc::now()),
            finished_at: Some(Utc::now()),
            failure_class: command.failure_class.clone(),
            failure_code: command.failure_code.clone(),
            failure_message: decision.failure_message.clone(),
            progress_percent: if command.attempt_state == "succeeded" {
                100
            } else {
                attempt.progress_percent
            },
            retryable: decision.effective_retryable,
        },
        &UpdateIngestJob {
            mutation_id: job.mutation_id,
            connector_id: job.connector_id,
            async_operation_id: job.async_operation_id,
            knowledge_document_id: job.knowledge_document_id,
            knowledge_revision_id: job.knowledge_revision_id,
            job_kind: job.job_kind.clone(),
            queue_state: decision.next_queue_state.clone(),
            priority: job.priority,
            dedupe_key: job.dedupe_key.clone(),
            available_at: if decision.effective_retryable {
                Utc::now() + retry_backoff_after_attempt(attempt.attempt_number)
            } else {
                job.available_at
            },
            completed_at: decision.completed_at,
        },
        lifecycle_event,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .ok_or_else(|| {
        ApiError::Conflict(format!(
            "ingest attempt {} or job {} lost its lease before finalization",
            command.attempt_id, job.id
        ))
    })
}

async fn update_finalize_async_operation(
    state: &AppState,
    operation_id: Option<Uuid>,
    next_queue_state: &str,
    failure_code: Option<String>,
) -> Result<(), ApiError> {
    let status = match next_queue_state {
        "completed" => ASYNC_OP_STATUS_READY,
        "failed" => ASYNC_OP_STATUS_FAILED,
        "queued" => "accepted",
        _ => ASYNC_OP_STATUS_PROCESSING,
    };
    let completed_at =
        matches!(status, ASYNC_OP_STATUS_READY | ASYNC_OP_STATUS_FAILED).then(Utc::now);
    let failure_code = (status == ASYNC_OP_STATUS_FAILED).then_some(failure_code).flatten();
    update_linked_async_operation(state, operation_id, status, completed_at, failure_code).await
}

impl IngestService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn list_jobs(
        &self,
        state: &AppState,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> Result<Vec<IngestJob>, ApiError> {
        let rows = ingest_repository::list_ingest_jobs(
            &state.persistence.postgres,
            workspace_id,
            library_id,
            None,
            None,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_job_row).collect())
    }

    /// Keyset-paginated job history for `GET
    /// /v1/ingest/libraries/{libraryId}/jobs`. Returns the built handles for
    /// the page plus whether a further page exists. Unlike `list_jobs`
    /// (unpaginated, still used by the MCP mutation-failure lookup path),
    /// this is the canonical REST list surface.
    pub async fn list_job_handles_page(
        &self,
        state: &AppState,
        library_id: Uuid,
        cursor: Option<(chrono::DateTime<Utc>, Uuid)>,
        limit: i64,
        status_filter: &[String],
    ) -> Result<(Vec<IngestJobHandle>, bool), ApiError> {
        let page = ingest_repository::list_ingest_jobs_page(
            &state.persistence.postgres,
            library_id,
            cursor,
            limit,
            status_filter,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let jobs = page.rows.into_iter().map(map_job_row).collect();
        let handles = self.build_job_handles(state, jobs).await?;
        Ok((handles, page.has_more))
    }

    pub async fn list_job_handles_by_mutation_ids(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        mutation_ids: &[Uuid],
    ) -> Result<Vec<IngestJobHandle>, ApiError> {
        let rows = ingest_repository::list_ingest_jobs_by_mutation_ids(
            &state.persistence.postgres,
            workspace_id,
            library_id,
            mutation_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let jobs = rows.into_iter().map(map_job_row).collect();
        self.build_job_handles(state, jobs).await
    }

    pub async fn get_job(&self, state: &AppState, job_id: Uuid) -> Result<IngestJob, ApiError> {
        let row = ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, job_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("ingest_job", job_id))?;
        Ok(map_job_row(row))
    }

    pub async fn get_job_handle(
        &self,
        state: &AppState,
        job_id: Uuid,
    ) -> Result<IngestJobHandle, ApiError> {
        let job = self.get_job(state, job_id).await?;
        self.build_job_handle(state, job).await
    }

    pub async fn get_job_handle_by_mutation_id(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<Option<IngestJobHandle>, ApiError> {
        let row = ingest_repository::get_latest_ingest_job_by_mutation_id(
            &state.persistence.postgres,
            mutation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        match row {
            Some(row) => Ok(Some(self.build_job_handle(state, map_job_row(row)).await?)),
            None => Ok(None),
        }
    }

    pub async fn get_job_handle_by_async_operation_id(
        &self,
        state: &AppState,
        async_operation_id: Uuid,
    ) -> Result<Option<IngestJobHandle>, ApiError> {
        let row = ingest_repository::get_latest_ingest_job_by_async_operation_id(
            &state.persistence.postgres,
            async_operation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        match row {
            Some(row) => Ok(Some(self.build_job_handle(state, map_job_row(row)).await?)),
            None => Ok(None),
        }
    }

    pub async fn get_job_handle_by_knowledge_revision_id(
        &self,
        state: &AppState,
        knowledge_revision_id: Uuid,
    ) -> Result<Option<IngestJobHandle>, ApiError> {
        let row = ingest_repository::get_latest_ingest_job_by_knowledge_revision_id(
            &state.persistence.postgres,
            knowledge_revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        match row {
            Some(row) => Ok(Some(self.build_job_handle(state, map_job_row(row)).await?)),
            None => Ok(None),
        }
    }

    pub async fn list_job_handles_by_knowledge_document_id(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        knowledge_document_id: Uuid,
    ) -> Result<Vec<IngestJobHandle>, ApiError> {
        let rows = ingest_repository::list_ingest_jobs_by_knowledge_document_id(
            &state.persistence.postgres,
            workspace_id,
            library_id,
            knowledge_document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let jobs = rows.into_iter().map(map_job_row).collect();
        self.build_job_handles(state, jobs).await
    }

    pub async fn admit_job(
        &self,
        state: &AppState,
        command: AdmitIngestJobCommand,
    ) -> Result<IngestJob, ApiError> {
        if let Some(dedupe_key) =
            command.dedupe_key.as_deref().map(str::trim).filter(|value| !value.is_empty())
            && let Some(existing) = ingest_repository::get_ingest_job_by_dedupe_key(
                &state.persistence.postgres,
                command.library_id,
                dedupe_key,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        {
            return Ok(map_job_row(existing));
        }

        let row = ingest_repository::create_ingest_job(
            &state.persistence.postgres,
            &NewIngestJob {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                mutation_id: command.mutation_id,
                mutation_item_id: command.mutation_item_id,
                connector_id: command.connector_id,
                async_operation_id: command.async_operation_id,
                knowledge_document_id: command.knowledge_document_id,
                knowledge_revision_id: command.knowledge_revision_id,
                job_kind: command.job_kind,
                queue_state: "queued".to_string(),
                priority: command.priority,
                dedupe_key: command.dedupe_key,
                queued_at: None,
                available_at: command.available_at,
                completed_at: None,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(map_job_row(row))
    }

    pub async fn list_attempts(
        &self,
        state: &AppState,
        job_id: Uuid,
    ) -> Result<Vec<IngestAttempt>, ApiError> {
        let rows =
            ingest_repository::list_ingest_attempts_by_job(&state.persistence.postgres, job_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_attempt_row).collect())
    }

    /// Keyset-paginated attempt history for `GET
    /// /v1/ingest/jobs/{jobId}/attempts`. Returns the page plus whether a
    /// further page exists. Unlike `list_attempts` (unpaginated, used
    /// internally by the MCP retry/diagnostics path), this is the canonical
    /// REST list surface for a flaky-ingest operator drilling into one job's
    /// full retry history.
    pub async fn list_attempts_page(
        &self,
        state: &AppState,
        job_id: Uuid,
        cursor: Option<(i32, Uuid)>,
        limit: i64,
    ) -> Result<(Vec<IngestAttempt>, bool), ApiError> {
        let page = ingest_repository::list_ingest_attempts_by_job_page(
            &state.persistence.postgres,
            job_id,
            cursor,
            limit,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let attempts = page.rows.into_iter().map(map_attempt_row).collect();
        Ok((attempts, page.has_more))
    }

    pub async fn get_attempt(
        &self,
        state: &AppState,
        attempt_id: Uuid,
    ) -> Result<IngestAttempt, ApiError> {
        let row =
            ingest_repository::get_ingest_attempt_by_id(&state.persistence.postgres, attempt_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", attempt_id))?;
        Ok(map_attempt_row(row))
    }

    pub async fn get_attempt_handle(
        &self,
        state: &AppState,
        attempt_id: Uuid,
    ) -> Result<IngestAttemptHandle, ApiError> {
        let attempt = self.get_attempt(state, attempt_id).await?;
        let job = self.get_job(state, attempt.job_id).await?;
        let async_operation = match job.async_operation_id {
            Some(operation_id) => {
                Some(state.canonical_services.ops.get_async_operation(state, operation_id).await?)
            }
            None => None,
        };
        Ok(IngestAttemptHandle { job, attempt, async_operation })
    }

    pub async fn lease_attempt(
        &self,
        state: &AppState,
        command: LeaseAttemptCommand,
    ) -> Result<IngestAttempt, ApiError> {
        let current_stage = normalize_optional_stage(command.current_stage.clone())?;
        let job =
            ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, command.job_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("ingest_job", command.job_id))?;
        let new_attempt = NewIngestAttempt {
            job_id: job.id,
            attempt_number: 0,
            worker_principal_id: command.worker_principal_id,
            lease_token: command.lease_token,
            knowledge_generation_id: command.knowledge_generation_id,
            attempt_state: "leased".to_string(),
            current_stage,
            started_at: None,
            heartbeat_at: Some(Utc::now()),
            finished_at: None,
            failure_class: None,
            failure_code: None,
            failure_message: None,
            progress_percent: 0,
            retryable: false,
        };

        let attempt = if let Some(expected_queue_lease_token) = command.expected_queue_lease_token {
            ingest_repository::create_ingest_attempt_for_queue_lease(
                &state.persistence.postgres,
                &new_attempt,
                &expected_queue_lease_token,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::Conflict(format!(
                    "ingest job {} queue lease changed before attempt creation",
                    job.id
                ))
            })?
        } else {
            let inline_queue_lease_token = new_attempt.lease_token.as_deref().ok_or_else(|| {
                ApiError::BadRequest("inline ingest lease token is required".to_string())
            })?;
            ingest_repository::create_ingest_attempt_for_inline_lease(
                &state.persistence.postgres,
                &new_attempt,
                inline_queue_lease_token,
                INLINE_QUEUE_LEASE_OWNER,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::Conflict(format!(
                    "ingest job {} cannot be claimed by an inline attempt",
                    job.id
                ))
            })?
        };

        Ok(map_attempt_row(attempt))
    }

    pub async fn heartbeat_attempt(
        &self,
        state: &AppState,
        command: HeartbeatAttemptCommand,
    ) -> Result<IngestAttempt, ApiError> {
        let current_stage = normalize_optional_stage(command.current_stage.clone())?;
        let existing = ingest_repository::get_ingest_attempt_by_id(
            &state.persistence.postgres,
            command.attempt_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", command.attempt_id))?;

        let row = ingest_repository::update_ingest_attempt(
            &state.persistence.postgres,
            command.attempt_id,
            &UpdateIngestAttempt {
                worker_principal_id: existing.worker_principal_id,
                lease_token: existing.lease_token,
                knowledge_generation_id: command
                    .knowledge_generation_id
                    .or(existing.knowledge_generation_id),
                attempt_state: existing.attempt_state,
                current_stage: current_stage.or(existing.current_stage),
                heartbeat_at: Some(Utc::now()),
                finished_at: existing.finished_at,
                failure_class: existing.failure_class,
                failure_code: existing.failure_code,
                failure_message: existing.failure_message,
                progress_percent: existing.progress_percent,
                retryable: existing.retryable,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", command.attempt_id))?;
        Ok(map_attempt_row(row))
    }

    pub async fn finalize_attempt(
        &self,
        state: &AppState,
        command: FinalizeAttemptCommand,
    ) -> Result<IngestAttempt, ApiError> {
        let current_stage = normalize_optional_stage(command.current_stage.clone())?;
        let failure_code = command.failure_code.clone();
        let (attempt, job) = load_leased_finalize_rows(state, &command).await?;
        let decision = finalize_decision(&command, &attempt);
        let lifecycle_event =
            build_finalize_lifecycle_event(state, &command, &job, &decision.next_queue_state)
                .await?;
        let row = persist_finalized_attempt(
            state,
            &command,
            current_stage,
            &attempt,
            &job,
            &decision,
            lifecycle_event.as_ref(),
        )
        .await?;
        update_finalize_async_operation(
            state,
            job.async_operation_id,
            &decision.next_queue_state,
            failure_code,
        )
        .await?;
        Ok(map_attempt_row(row))
    }

    pub async fn retry_job(
        &self,
        state: &AppState,
        job_id: Uuid,
        available_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<IngestJob, ApiError> {
        let existing = ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, job_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("ingest_job", job_id))?;
        if matches!(existing.queue_state.as_str(), "completed" | "canceled" | "failed") {
            return Err(ApiError::BadRequest(
                "Completed, canceled, and failed jobs cannot be requeued from the ingest queue"
                    .to_string(),
            ));
        }
        let row = ingest_repository::retry_or_requeue_ingest_job(
            &state.persistence.postgres,
            job_id,
            chrono::Duration::seconds(QUEUE_STALE_LEASE_SECONDS),
            available_at.unwrap_or_else(Utc::now),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| {
            ApiError::BadRequest(
                "Only queued, paused, or stale leased jobs with no active attempt can be requeued"
                    .to_string(),
            )
        })?;
        update_linked_async_operation(state, row.async_operation_id, "accepted", None, None)
            .await?;
        Ok(map_job_row(row))
    }

    pub async fn pause_job(&self, state: &AppState, job_id: Uuid) -> Result<(), ApiError> {
        let existing = ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, job_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("ingest_job", job_id))?;
        let paused = ingest_repository::pause_ingest_job(&state.persistence.postgres, job_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if paused.is_none() {
            return Err(ApiError::BadRequest(
                "Only queued or running jobs can be paused".to_string(),
            ));
        }
        update_linked_async_operation(state, existing.async_operation_id, "accepted", None, None)
            .await?;
        Ok(())
    }

    pub async fn resume_job(&self, state: &AppState, job_id: Uuid) -> Result<(), ApiError> {
        let existing = ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, job_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("ingest_job", job_id))?;
        let resumed = ingest_repository::resume_ingest_job(&state.persistence.postgres, job_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if resumed.is_none() {
            return Err(ApiError::BadRequest(
                "Only paused jobs with no active worker attempt can be resumed".to_string(),
            ));
        }
        update_linked_async_operation(state, existing.async_operation_id, "accepted", None, None)
            .await?;
        Ok(())
    }

    pub async fn record_stage_event(
        &self,
        state: &AppState,
        command: RecordStageEventCommand,
    ) -> Result<IngestStageEvent, ApiError> {
        let stage_name = normalize_stage_name(&command.stage_name)?;
        let stage_state = command.stage_state.clone();
        let stage_message = command.message.clone();
        let attempt = ingest_repository::get_ingest_attempt_by_id(
            &state.persistence.postgres,
            command.attempt_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", command.attempt_id))?;
        let existing_events = ingest_repository::list_ingest_stage_events_by_attempt(
            &state.persistence.postgres,
            command.attempt_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let stage_details = command.details_json.clone();
        let row = ingest_repository::create_ingest_stage_event(
            &state.persistence.postgres,
            &NewIngestStageEvent {
                attempt_id: command.attempt_id,
                stage_name: stage_name.clone(),
                stage_state: command.stage_state,
                ordinal: i32::try_from(existing_events.len()).unwrap_or(i32::MAX) + 1,
                message: command.message,
                details_json: command.details_json,
                recorded_at: None,
                provider_kind: command.provider_kind,
                model_name: command.model_name,
                prompt_tokens: command.prompt_tokens,
                completion_tokens: command.completion_tokens,
                total_tokens: command.total_tokens,
                cached_tokens: command.cached_tokens,
                estimated_cost: command.estimated_cost,
                currency_code: command.currency_code,
                elapsed_ms: command.elapsed_ms,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let _ = ingest_repository::update_ingest_attempt(
            &state.persistence.postgres,
            command.attempt_id,
            &UpdateIngestAttempt {
                worker_principal_id: attempt.worker_principal_id,
                lease_token: attempt.lease_token,
                knowledge_generation_id: attempt.knowledge_generation_id,
                attempt_state: attempt.attempt_state,
                current_stage: Some(stage_name.clone()),
                heartbeat_at: Some(Utc::now()),
                finished_at: attempt.finished_at,
                failure_class: attempt.failure_class,
                failure_code: attempt.failure_code,
                failure_message: if stage_state == "failed" {
                    stage_message
                        .as_deref()
                        .map(str::trim)
                        .filter(|message| !message.is_empty())
                        .map(str::to_string)
                        .or(attempt.failure_message)
                } else {
                    attempt.failure_message
                },
                progress_percent: canonical_ingest_attempt_stage_progress_percent(
                    &existing_events,
                    &stage_name,
                    &stage_state,
                    &stage_details,
                )
                .map_or(attempt.progress_percent, |progress| {
                    progress.max(attempt.progress_percent)
                }),
                retryable: attempt.retryable,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(map_stage_event_row(row))
    }

    pub async fn record_stage_unit_progress(
        &self,
        state: &AppState,
        command: RecordStageUnitProgressCommand,
    ) -> Result<(), ApiError> {
        let stage_name = normalize_stage_name(&command.stage_name)?;
        if command.total_units == 0 {
            return Ok(());
        }
        let attempt = ingest_repository::get_ingest_attempt_by_id(
            &state.persistence.postgres,
            command.attempt_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", command.attempt_id))?;
        if attempt.attempt_state != "leased" {
            return Ok(());
        }
        let existing_events = ingest_repository::list_ingest_stage_events_by_attempt(
            &state.persistence.postgres,
            command.attempt_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let Some(progress_percent) = canonical_ingest_attempt_stage_unit_progress_percent(
            &existing_events,
            &stage_name,
            command.completed_units,
            command.total_units,
            &command.details_json,
        ) else {
            return Ok(());
        };
        let _ = ingest_repository::update_leased_attempt_stage_progress(
            &state.persistence.postgres,
            command.attempt_id,
            &stage_name,
            progress_percent,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(())
    }

    pub async fn list_stage_events(
        &self,
        state: &AppState,
        attempt_id: Uuid,
    ) -> Result<Vec<IngestStageEvent>, ApiError> {
        let rows = ingest_repository::list_ingest_stage_events_by_attempt(
            &state.persistence.postgres,
            attempt_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_stage_event_row).collect())
    }

    async fn build_job_handle(
        &self,
        state: &AppState,
        job: IngestJob,
    ) -> Result<IngestJobHandle, ApiError> {
        let latest_attempt = ingest_repository::get_latest_ingest_attempt_by_job(
            &state.persistence.postgres,
            job.id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .map(map_attempt_row);
        let async_operation = match job.async_operation_id {
            Some(operation_id) => {
                Some(state.canonical_services.ops.get_async_operation(state, operation_id).await?)
            }
            None => None,
        };
        Ok(IngestJobHandle { job, latest_attempt, async_operation })
    }

    async fn build_job_handles(
        &self,
        state: &AppState,
        jobs: Vec<IngestJob>,
    ) -> Result<Vec<IngestJobHandle>, ApiError> {
        if jobs.is_empty() {
            return Ok(Vec::new());
        }

        let job_ids = jobs.iter().map(|job| job.id).collect::<Vec<_>>();
        let async_operation_ids =
            jobs.iter().filter_map(|job| job.async_operation_id).collect::<Vec<_>>();

        let latest_attempts_by_job_id = ingest_repository::list_latest_ingest_attempts_by_job_ids(
            &state.persistence.postgres,
            &job_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .into_iter()
        .map(|row| (row.job_id, map_attempt_row(row)))
        .collect::<HashMap<_, _>>();

        let async_operation_rows = ops_repository::list_async_operations_by_ids(
            &state.persistence.postgres,
            &async_operation_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let mut async_operations_by_id = HashMap::with_capacity(async_operation_rows.len());
        for row in async_operation_rows {
            async_operations_by_id.insert(row.id, map_async_operation_row(row)?);
        }

        Ok(jobs
            .into_iter()
            .map(|job| IngestJobHandle {
                latest_attempt: latest_attempts_by_job_id.get(&job.id).cloned(),
                async_operation: job
                    .async_operation_id
                    .and_then(|operation_id| async_operations_by_id.get(&operation_id).cloned()),
                job,
            })
            .collect())
    }
}

#[must_use]
pub fn canonical_ingest_stage_metadata(stage_name: &str) -> Option<CanonicalIngestStageMetadata> {
    match stage_name {
        INGEST_STAGE_EXTRACT_CONTENT => Some(CanonicalIngestStageMetadata {
            stage_name: INGEST_STAGE_EXTRACT_CONTENT,
            stage_rank: 10,
            lifecycle_kind: "preparation",
        }),
        INGEST_STAGE_PREPARE_STRUCTURE => Some(CanonicalIngestStageMetadata {
            stage_name: INGEST_STAGE_PREPARE_STRUCTURE,
            stage_rank: 20,
            lifecycle_kind: "preparation",
        }),
        INGEST_STAGE_CHUNK_CONTENT => Some(CanonicalIngestStageMetadata {
            stage_name: INGEST_STAGE_CHUNK_CONTENT,
            stage_rank: 30,
            lifecycle_kind: "preparation",
        }),
        INGEST_STAGE_EMBED_CHUNK => Some(CanonicalIngestStageMetadata {
            stage_name: INGEST_STAGE_EMBED_CHUNK,
            stage_rank: 50,
            lifecycle_kind: "embedding",
        }),
        INGEST_STAGE_EXTRACT_TECHNICAL_FACTS => Some(CanonicalIngestStageMetadata {
            stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
            stage_rank: 40,
            lifecycle_kind: "grounding",
        }),
        INGEST_STAGE_EXTRACT_GRAPH => Some(CanonicalIngestStageMetadata {
            stage_name: INGEST_STAGE_EXTRACT_GRAPH,
            stage_rank: 60,
            lifecycle_kind: "graph",
        }),
        INGEST_STAGE_VERIFY_QUERY_ANSWER => Some(CanonicalIngestStageMetadata {
            stage_name: INGEST_STAGE_VERIFY_QUERY_ANSWER,
            stage_rank: 70,
            lifecycle_kind: "query",
        }),
        INGEST_STAGE_FINALIZING => Some(CanonicalIngestStageMetadata {
            stage_name: INGEST_STAGE_FINALIZING,
            stage_rank: 80,
            lifecycle_kind: "finalization",
        }),
        INGEST_STAGE_WEB_DISCOVERY => Some(CanonicalIngestStageMetadata {
            stage_name: INGEST_STAGE_WEB_DISCOVERY,
            stage_rank: 15,
            lifecycle_kind: "web_discovery",
        }),
        INGEST_STAGE_WEB_MATERIALIZE_PAGE => Some(CanonicalIngestStageMetadata {
            stage_name: INGEST_STAGE_WEB_MATERIALIZE_PAGE,
            stage_rank: 25,
            lifecycle_kind: "web_materialization",
        }),
        _ => None,
    }
}

fn normalize_optional_stage(stage_name: Option<String>) -> Result<Option<String>, ApiError> {
    stage_name.map(|value| normalize_stage_name(&value)).transpose()
}

fn normalize_stage_name(stage_name: &str) -> Result<String, ApiError> {
    let normalized = stage_name.trim().to_ascii_lowercase();
    canonical_ingest_stage_metadata(&normalized)
        .map(|metadata| metadata.stage_name.to_string())
        .ok_or_else(|| {
            ApiError::BadRequest(format!("unsupported canonical ingest stage: {stage_name}"))
        })
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

fn map_async_operation_row(
    row: ops_repository::OpsAsyncOperationRow,
) -> Result<OpsAsyncOperation, ApiError> {
    let status = OpsAsyncOperationStatus::from_db(&row.status)
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Ok(OpsAsyncOperation {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        operation_kind: row.operation_kind,
        status,
        surface_kind: Some(row.surface_kind),
        subject_kind: Some(row.subject_kind),
        subject_id: row.subject_id,
        parent_async_operation_id: row.parent_async_operation_id,
        failure_code: row.failure_code,
        created_at: row.created_at,
        completed_at: row.completed_at,
    })
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
            .canonical_services
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

#[cfg(test)]
mod tests {
    use super::{
        CanonicalIngestProgressProfile, INGEST_STAGE_CHUNK_CONTENT, INGEST_STAGE_EMBED_CHUNK,
        INGEST_STAGE_EXTRACT_CONTENT, INGEST_STAGE_EXTRACT_GRAPH,
        INGEST_STAGE_EXTRACT_TECHNICAL_FACTS, INGEST_STAGE_FINALIZING,
        INGEST_STAGE_PREPARE_STRUCTURE, INGEST_STAGE_WEB_DISCOVERY,
        INGEST_STAGE_WEB_MATERIALIZE_PAGE, canonical_ingest_attempt_stage_unit_progress_percent,
        canonical_ingest_progress_profile, canonical_ingest_stage_metadata,
        canonical_ingest_stage_progress_percent_for_profile,
        canonical_ingest_stage_unit_progress_percent_for_profile, is_retry_budget_exhausted,
        next_job_queue_state_after_finalize, normalize_stage_name,
        progress_profile_from_stage_details, retry_backoff_after_attempt,
    };
    use crate::{
        domains::ingest::MAX_INGEST_ATTEMPTS,
        infra::repositories::ingest_repository::IngestStageEventRow,
    };
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    fn stage_event(stage_name: &str, details_json: serde_json::Value) -> IngestStageEventRow {
        IngestStageEventRow {
            id: Uuid::now_v7(),
            attempt_id: Uuid::now_v7(),
            stage_name: stage_name.to_string(),
            stage_state: "completed".to_string(),
            ordinal: 1,
            message: None,
            details_json,
            recorded_at: Utc::now(),
            provider_kind: None,
            model_name: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            cached_tokens: None,
            estimated_cost: None,
            currency_code: None,
            elapsed_ms: None,
            started_at: None,
        }
    }

    #[test]
    fn normalizes_and_accepts_new_canonical_stage_names() {
        assert_eq!(
            normalize_stage_name("  Prepare_Structure ")
                .expect("prepare_structure should normalize"),
            INGEST_STAGE_PREPARE_STRUCTURE
        );
        assert_eq!(
            normalize_stage_name("extract_technical_facts")
                .expect("extract_technical_facts should be canonical"),
            INGEST_STAGE_EXTRACT_TECHNICAL_FACTS
        );
        assert_eq!(
            normalize_stage_name("WEB_DISCOVERY").expect("web_discovery should normalize"),
            INGEST_STAGE_WEB_DISCOVERY
        );
        assert_eq!(
            normalize_stage_name("web_materialize_page")
                .expect("web_materialize_page should be canonical"),
            INGEST_STAGE_WEB_MATERIALIZE_PAGE
        );
    }

    #[test]
    fn rejects_unknown_stage_names() {
        let error =
            normalize_stage_name("legacy_stage").expect_err("legacy stage must be rejected");
        assert_eq!(error.kind(), "bad_request");
    }

    #[test]
    fn exposes_ranked_stage_metadata() {
        let metadata = canonical_ingest_stage_metadata(INGEST_STAGE_EXTRACT_TECHNICAL_FACTS)
            .expect("metadata should exist");
        assert_eq!(metadata.lifecycle_kind, "grounding");
        assert_eq!(metadata.stage_rank, 40);

        let embed_metadata = canonical_ingest_stage_metadata(INGEST_STAGE_EMBED_CHUNK)
            .expect("metadata should exist");
        assert_eq!(embed_metadata.lifecycle_kind, "embedding");
        assert_eq!(embed_metadata.stage_rank, 50);

        let web_metadata = canonical_ingest_stage_metadata(INGEST_STAGE_WEB_DISCOVERY)
            .expect("metadata should exist");
        assert_eq!(web_metadata.lifecycle_kind, "web_discovery");
        assert_eq!(web_metadata.stage_rank, 15);
    }

    #[test]
    fn exposes_content_mutation_stage_progress() {
        assert_eq!(
            canonical_ingest_stage_progress_percent_for_profile(
                CanonicalIngestProgressProfile::Balanced,
                INGEST_STAGE_EXTRACT_CONTENT,
                "started"
            ),
            Some(2)
        );
        assert_eq!(
            canonical_ingest_stage_progress_percent_for_profile(
                CanonicalIngestProgressProfile::Balanced,
                INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
                "completed"
            ),
            Some(28)
        );
        assert_eq!(
            canonical_ingest_stage_progress_percent_for_profile(
                CanonicalIngestProgressProfile::Balanced,
                INGEST_STAGE_FINALIZING,
                "completed"
            ),
            Some(100)
        );
    }

    #[test]
    fn exposes_profile_aware_content_mutation_stage_progress() {
        assert_eq!(
            canonical_ingest_stage_progress_percent_for_profile(
                CanonicalIngestProgressProfile::InlineText,
                INGEST_STAGE_EXTRACT_CONTENT,
                "completed"
            ),
            Some(4)
        );
        assert_eq!(
            canonical_ingest_stage_progress_percent_for_profile(
                CanonicalIngestProgressProfile::InlineText,
                INGEST_STAGE_EMBED_CHUNK,
                "completed"
            ),
            Some(55)
        );
        assert_eq!(
            canonical_ingest_stage_progress_percent_for_profile(
                CanonicalIngestProgressProfile::InlineText,
                INGEST_STAGE_EXTRACT_GRAPH,
                "started"
            ),
            Some(60)
        );
        assert_eq!(
            canonical_ingest_stage_progress_percent_for_profile(
                CanonicalIngestProgressProfile::ExtractionHeavy,
                INGEST_STAGE_EXTRACT_CONTENT,
                "completed"
            ),
            Some(30)
        );
    }

    #[test]
    fn infers_progress_profile_from_extraction_details() {
        assert_eq!(
            progress_profile_from_stage_details(&json!({ "source": "knowledge_revision" })),
            Some(CanonicalIngestProgressProfile::InlineText)
        );
        assert_eq!(
            progress_profile_from_stage_details(&json!({ "fileKind": "text_like" })),
            Some(CanonicalIngestProgressProfile::InlineText)
        );
        assert_eq!(
            progress_profile_from_stage_details(&json!({ "fileKind": "pdf", "pageCount": 10 })),
            Some(CanonicalIngestProgressProfile::ExtractionHeavy)
        );
        assert_eq!(
            progress_profile_from_stage_details(&json!({ "pageCount": 1 })),
            Some(CanonicalIngestProgressProfile::ExtractionHeavy)
        );
        assert_eq!(progress_profile_from_stage_details(&json!({})), None);
    }

    #[test]
    fn carries_progress_profile_forward_from_prior_stage_events() {
        let existing_events = vec![
            stage_event(INGEST_STAGE_EXTRACT_CONTENT, json!({ "fileKind": "text_like" })),
            stage_event(INGEST_STAGE_CHUNK_CONTENT, json!({ "chunkCount": 4 })),
        ];

        assert_eq!(
            canonical_ingest_progress_profile(&existing_events, &json!({})),
            CanonicalIngestProgressProfile::InlineText
        );
        assert_eq!(
            canonical_ingest_attempt_stage_unit_progress_percent(
                &existing_events,
                INGEST_STAGE_EMBED_CHUNK,
                1,
                2,
                &json!({})
            ),
            Some(40)
        );
    }

    #[test]
    fn exposes_content_mutation_stage_unit_progress() {
        assert_eq!(
            canonical_ingest_stage_unit_progress_percent_for_profile(
                CanonicalIngestProgressProfile::ExtractionHeavy,
                INGEST_STAGE_EXTRACT_CONTENT,
                0,
                100
            ),
            Some(5)
        );
        assert_eq!(
            canonical_ingest_stage_unit_progress_percent_for_profile(
                CanonicalIngestProgressProfile::ExtractionHeavy,
                INGEST_STAGE_EXTRACT_CONTENT,
                50,
                100
            ),
            Some(17)
        );
        assert_eq!(
            canonical_ingest_stage_unit_progress_percent_for_profile(
                CanonicalIngestProgressProfile::ExtractionHeavy,
                INGEST_STAGE_EXTRACT_CONTENT,
                100,
                100
            ),
            Some(30)
        );
        assert_eq!(
            canonical_ingest_stage_unit_progress_percent_for_profile(
                CanonicalIngestProgressProfile::ExtractionHeavy,
                INGEST_STAGE_EXTRACT_CONTENT,
                101,
                100
            ),
            Some(30)
        );
        assert_eq!(
            canonical_ingest_stage_unit_progress_percent_for_profile(
                CanonicalIngestProgressProfile::ExtractionHeavy,
                INGEST_STAGE_EXTRACT_CONTENT,
                1,
                0
            ),
            None
        );
    }

    // BUG A (a): a transient/retryable stage failure that still has attempt
    // budget left is requeued, NOT terminally failed.
    #[test]
    fn retryable_failure_under_budget_requeues_instead_of_failing() {
        // First attempt fails retryably -> budget is not exhausted -> requeue.
        assert!(!is_retry_budget_exhausted("failed", true, 1));
        let effective_retryable = !is_retry_budget_exhausted("failed", true, 1);
        assert!(effective_retryable);
        assert_eq!(
            next_job_queue_state_after_finalize("failed", effective_retryable),
            "queued",
            "a retryable failure with budget left must go back to the queue"
        );
        // A non-retryable failure is terminal regardless of budget.
        assert!(!is_retry_budget_exhausted("failed", false, 1));
        assert_eq!(next_job_queue_state_after_finalize("failed", false), "failed");
        // A success is never subject to the retry budget.
        assert!(!is_retry_budget_exhausted("succeeded", true, 99));
        assert_eq!(next_job_queue_state_after_finalize("succeeded", true), "completed");
    }

    // BUG A (b): once the attempt budget is exhausted, a retryable failure is
    // escalated to a terminal `failed`.
    #[test]
    fn retryable_failure_at_budget_limit_becomes_terminal() {
        // Below the limit -> still retryable.
        assert!(!is_retry_budget_exhausted("failed", true, MAX_INGEST_ATTEMPTS - 1));
        // At and beyond the limit -> exhausted.
        assert!(is_retry_budget_exhausted("failed", true, MAX_INGEST_ATTEMPTS));
        assert!(is_retry_budget_exhausted("failed", true, MAX_INGEST_ATTEMPTS + 1));
        let effective_retryable = !is_retry_budget_exhausted("failed", true, MAX_INGEST_ATTEMPTS);
        assert!(!effective_retryable);
        assert_eq!(
            next_job_queue_state_after_finalize("failed", effective_retryable),
            "failed",
            "an exhausted retry budget must finalize the job terminally"
        );
    }

    // Backoff grows exponentially with the attempt number and is bounded, so the
    // requeue delay rides out a multi-minute provider outage without pushing a
    // job arbitrarily far into the future.
    #[test]
    fn retry_backoff_is_exponential_and_bounded() {
        let first = retry_backoff_after_attempt(1);
        let second = retry_backoff_after_attempt(2);
        let third = retry_backoff_after_attempt(3);
        assert!(first.num_seconds() > 0);
        assert_eq!(second.num_seconds(), first.num_seconds() * 2);
        assert_eq!(third.num_seconds(), first.num_seconds() * 4);
        // A very large attempt number saturates at the cap rather than
        // overflowing or producing an unbounded delay.
        let capped = retry_backoff_after_attempt(1000);
        assert!(capped.num_seconds() >= third.num_seconds());
        assert!(capped.num_seconds() <= 600);
    }
}
