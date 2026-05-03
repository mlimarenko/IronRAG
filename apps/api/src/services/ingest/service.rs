use std::collections::HashMap;

use chrono::Utc;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ingest::{IngestAttempt, IngestJob, IngestStageEvent},
    domains::ops::{
        ASYNC_OP_STATUS_FAILED, ASYNC_OP_STATUS_PROCESSING, ASYNC_OP_STATUS_READY,
        OpsAsyncOperation,
    },
    infra::repositories::{
        ingest_repository::{
            self, NewIngestAttempt, NewIngestJob, NewIngestStageEvent, UpdateIngestAttempt,
            UpdateIngestJob,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalIngestStageMetadata {
    pub stage_name: &'static str,
    pub stage_rank: i32,
    pub lifecycle_kind: &'static str,
}

#[derive(Debug, Clone)]
pub struct AdmitIngestJobCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub mutation_id: Option<Uuid>,
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

    pub async fn list_job_handles(
        &self,
        state: &AppState,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> Result<Vec<IngestJobHandle>, ApiError> {
        let jobs = self.list_jobs(state, workspace_id, library_id).await?;
        self.build_job_handles(state, jobs).await
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
        {
            if let Some(existing) = ingest_repository::get_ingest_job_by_dedupe_key(
                &state.persistence.postgres,
                command.library_id,
                dedupe_key,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            {
                return Ok(map_job_row(existing));
            }
        }

        let row = ingest_repository::create_ingest_job(
            &state.persistence.postgres,
            &NewIngestJob {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                mutation_id: command.mutation_id,
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
        let latest_attempt = ingest_repository::get_latest_ingest_attempt_by_job(
            &state.persistence.postgres,
            command.job_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let next_attempt_number = latest_attempt.as_ref().map_or(1, |row| row.attempt_number + 1);

        let attempt = ingest_repository::create_ingest_attempt(
            &state.persistence.postgres,
            &NewIngestAttempt {
                job_id: job.id,
                attempt_number: next_attempt_number,
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
                retryable: false,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        let _ = ingest_repository::update_ingest_job(
            &state.persistence.postgres,
            job.id,
            &UpdateIngestJob {
                mutation_id: job.mutation_id,
                connector_id: job.connector_id,
                async_operation_id: job.async_operation_id,
                knowledge_document_id: job.knowledge_document_id,
                knowledge_revision_id: job.knowledge_revision_id,
                job_kind: job.job_kind,
                queue_state: "leased".to_string(),
                priority: job.priority,
                dedupe_key: job.dedupe_key,
                available_at: job.available_at,
                completed_at: job.completed_at,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        update_linked_async_operation(state, job.async_operation_id, "processing", None, None)
            .await?;

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
        let attempt = ingest_repository::get_ingest_attempt_by_id(
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
                worker_principal_id: attempt.worker_principal_id,
                lease_token: attempt.lease_token,
                knowledge_generation_id: command
                    .knowledge_generation_id
                    .or(attempt.knowledge_generation_id),
                attempt_state: command.attempt_state.clone(),
                current_stage,
                heartbeat_at: Some(Utc::now()),
                finished_at: Some(Utc::now()),
                failure_class: command.failure_class,
                failure_code: failure_code.clone(),
                retryable: command.retryable,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", command.attempt_id))?;

        let job =
            ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, attempt.job_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("ingest_job", attempt.job_id))?;
        let next_queue_state = match command.attempt_state.as_str() {
            "succeeded" => "completed",
            "failed" if command.retryable => "queued",
            "failed" | "abandoned" | "canceled" => "failed",
            other => other,
        };
        let completed_at = if next_queue_state == "completed" || next_queue_state == "failed" {
            Some(Utc::now())
        } else {
            None
        };
        let _ = ingest_repository::update_ingest_job(
            &state.persistence.postgres,
            job.id,
            &UpdateIngestJob {
                mutation_id: job.mutation_id,
                connector_id: job.connector_id,
                async_operation_id: job.async_operation_id,
                knowledge_document_id: job.knowledge_document_id,
                knowledge_revision_id: job.knowledge_revision_id,
                job_kind: job.job_kind,
                queue_state: next_queue_state.to_string(),
                priority: job.priority,
                dedupe_key: job.dedupe_key,
                available_at: if command.retryable { Utc::now() } else { job.available_at },
                completed_at,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        let operation_status = match next_queue_state {
            "completed" => ASYNC_OP_STATUS_READY,
            "failed" => ASYNC_OP_STATUS_FAILED,
            "queued" => "accepted",
            _ => ASYNC_OP_STATUS_PROCESSING,
        };
        let operation_completed_at = (operation_status == ASYNC_OP_STATUS_READY
            || operation_status == ASYNC_OP_STATUS_FAILED)
            .then(Utc::now);
        let operation_failure_code =
            (operation_status == ASYNC_OP_STATUS_FAILED).then(|| failure_code.clone()).flatten();
        update_linked_async_operation(
            state,
            job.async_operation_id,
            operation_status,
            operation_completed_at,
            operation_failure_code,
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
        let row = ingest_repository::update_ingest_job(
            &state.persistence.postgres,
            job_id,
            &UpdateIngestJob {
                mutation_id: existing.mutation_id,
                connector_id: existing.connector_id,
                async_operation_id: existing.async_operation_id,
                knowledge_document_id: existing.knowledge_document_id,
                knowledge_revision_id: existing.knowledge_revision_id,
                job_kind: existing.job_kind,
                queue_state: "queued".to_string(),
                priority: existing.priority,
                dedupe_key: existing.dedupe_key,
                available_at: available_at.unwrap_or_else(Utc::now),
                completed_at: None,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("ingest_job", job_id))?;
        update_linked_async_operation(state, row.async_operation_id, "accepted", None, None)
            .await?;
        Ok(map_job_row(row))
    }

    pub async fn record_stage_event(
        &self,
        state: &AppState,
        command: RecordStageEventCommand,
    ) -> Result<IngestStageEvent, ApiError> {
        let stage_name = normalize_stage_name(&command.stage_name)?;
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
                retryable: attempt.retryable,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(map_stage_event_row(row))
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

        let async_operations_by_id = ops_repository::list_async_operations_by_ids(
            &state.persistence.postgres,
            &async_operation_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .into_iter()
        .map(|row| (row.id, map_async_operation_row(row)))
        .collect::<HashMap<_, _>>();

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
            stage_rank: 40,
            lifecycle_kind: "embedding",
        }),
        INGEST_STAGE_EXTRACT_TECHNICAL_FACTS => Some(CanonicalIngestStageMetadata {
            stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
            stage_rank: 50,
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
        retryable: row.retryable,
    }
}

fn map_async_operation_row(row: ops_repository::OpsAsyncOperationRow) -> OpsAsyncOperation {
    OpsAsyncOperation {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        operation_kind: row.operation_kind,
        status: row.status,
        surface_kind: Some(row.surface_kind),
        subject_kind: Some(row.subject_kind),
        subject_id: row.subject_id,
        parent_async_operation_id: row.parent_async_operation_id,
        failure_code: row.failure_code,
        created_at: row.created_at,
        completed_at: row.completed_at,
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
        INGEST_STAGE_EXTRACT_TECHNICAL_FACTS, INGEST_STAGE_PREPARE_STRUCTURE,
        INGEST_STAGE_WEB_DISCOVERY, INGEST_STAGE_WEB_MATERIALIZE_PAGE,
        canonical_ingest_stage_metadata, normalize_stage_name,
    };

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
        assert_eq!(metadata.stage_rank, 50);

        let web_metadata = canonical_ingest_stage_metadata(INGEST_STAGE_WEB_DISCOVERY)
            .expect("metadata should exist");
        assert_eq!(web_metadata.lifecycle_kind, "web_discovery");
        assert_eq!(web_metadata.stage_rank, 15);
    }
}
