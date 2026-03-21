use chrono::Utc;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ingest::{IngestAttempt, IngestJob, IngestStageEvent},
    domains::ops::OpsAsyncOperation,
    infra::repositories::ingest_repository::{
        self, NewIngestAttempt, NewIngestJob, NewIngestStageEvent, UpdateIngestAttempt,
        UpdateIngestJob,
    },
    interfaces::http::router_support::ApiError,
    services::ops_service::UpdateAsyncOperationCommand,
};

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
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_job_row).collect())
    }

    pub async fn list_job_handles(
        &self,
        state: &AppState,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> Result<Vec<IngestJobHandle>, ApiError> {
        let jobs = self.list_jobs(state, workspace_id, library_id).await?;
        let mut handles = Vec::with_capacity(jobs.len());
        for job in jobs {
            handles.push(self.build_job_handle(state, job).await?);
        }
        Ok(handles)
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
        .map_err(|_| ApiError::Internal)?;
        let mut handles = Vec::with_capacity(rows.len());
        for row in rows {
            handles.push(self.build_job_handle(state, map_job_row(row)).await?);
        }
        Ok(handles)
    }

    pub async fn get_job(&self, state: &AppState, job_id: Uuid) -> Result<IngestJob, ApiError> {
        let row = ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, job_id)
            .await
            .map_err(|_| ApiError::Internal)?
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
        .map_err(|_| ApiError::Internal)?;
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
        .map_err(|_| ApiError::Internal)?;
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
        .map_err(|_| ApiError::Internal)?;
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
        .map_err(|_| ApiError::Internal)?;
        let mut handles = Vec::with_capacity(rows.len());
        for row in rows {
            handles.push(self.build_job_handle(state, map_job_row(row)).await?);
        }
        Ok(handles)
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
            .map_err(|_| ApiError::Internal)?
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
        .map_err(|_| ApiError::Internal)?;
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
                .map_err(|_| ApiError::Internal)?;
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
                .map_err(|_| ApiError::Internal)?
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
        let job =
            ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, command.job_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("ingest_job", command.job_id))?;
        let latest_attempt = ingest_repository::get_latest_ingest_attempt_by_job(
            &state.persistence.postgres,
            command.job_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
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
                current_stage: command.current_stage,
                started_at: None,
                heartbeat_at: Some(Utc::now()),
                finished_at: None,
                failure_class: None,
                failure_code: None,
                retryable: false,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;

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
        .map_err(|_| ApiError::Internal)?;

        update_linked_async_operation(state, job.async_operation_id, "processing", None, None)
            .await?;

        Ok(map_attempt_row(attempt))
    }

    pub async fn heartbeat_attempt(
        &self,
        state: &AppState,
        command: HeartbeatAttemptCommand,
    ) -> Result<IngestAttempt, ApiError> {
        let existing = ingest_repository::get_ingest_attempt_by_id(
            &state.persistence.postgres,
            command.attempt_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
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
                current_stage: command.current_stage.or(existing.current_stage),
                heartbeat_at: Some(Utc::now()),
                finished_at: existing.finished_at,
                failure_class: existing.failure_class,
                failure_code: existing.failure_code,
                retryable: existing.retryable,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", command.attempt_id))?;
        Ok(map_attempt_row(row))
    }

    pub async fn finalize_attempt(
        &self,
        state: &AppState,
        command: FinalizeAttemptCommand,
    ) -> Result<IngestAttempt, ApiError> {
        let failure_code = command.failure_code.clone();
        let attempt = ingest_repository::get_ingest_attempt_by_id(
            &state.persistence.postgres,
            command.attempt_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
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
                current_stage: command.current_stage,
                heartbeat_at: Some(Utc::now()),
                finished_at: Some(Utc::now()),
                failure_class: command.failure_class,
                failure_code: failure_code.clone(),
                retryable: command.retryable,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", command.attempt_id))?;

        let job =
            ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, attempt.job_id)
                .await
                .map_err(|_| ApiError::Internal)?
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
        .map_err(|_| ApiError::Internal)?;

        let operation_status = match next_queue_state {
            "completed" => "ready",
            "failed" => "failed",
            "queued" => "accepted",
            _ => "processing",
        };
        let operation_completed_at =
            (operation_status == "ready" || operation_status == "failed").then(Utc::now);
        let operation_failure_code =
            (operation_status == "failed").then(|| failure_code.clone()).flatten();
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
            .map_err(|_| ApiError::Internal)?
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
        .map_err(|_| ApiError::Internal)?
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
        let existing_events = ingest_repository::list_ingest_stage_events_by_attempt(
            &state.persistence.postgres,
            command.attempt_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let row = ingest_repository::create_ingest_stage_event(
            &state.persistence.postgres,
            &NewIngestStageEvent {
                attempt_id: command.attempt_id,
                stage_name: command.stage_name,
                stage_state: command.stage_state,
                ordinal: i32::try_from(existing_events.len()).unwrap_or(i32::MAX) + 1,
                message: command.message,
                details_json: command.details_json,
                recorded_at: None,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
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
        .map_err(|_| ApiError::Internal)?;
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
        .map_err(|_| ApiError::Internal)?
        .map(map_attempt_row);
        let async_operation = match job.async_operation_id {
            Some(operation_id) => {
                Some(state.canonical_services.ops.get_async_operation(state, operation_id).await?)
            }
            None => None,
        };
        Ok(IngestJobHandle { job, latest_attempt, async_operation })
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
