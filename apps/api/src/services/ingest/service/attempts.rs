use super::progress::normalize_optional_stage;
use super::*;

impl IngestService {
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
    /// internally by the MCP retry/diagnostics path), this is the primary
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
                Some(state.application_services.ops.get_async_operation(state, operation_id).await?)
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
            match ingest_repository::create_ingest_attempt_for_queue_lease(
                &state.persistence.postgres,
                &new_attempt,
                &expected_queue_lease_token,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            {
                ingest_repository::QueueLeaseOutcome::Leased(attempt) => *attempt,
                ingest_repository::QueueLeaseOutcome::LeaseMoved => {
                    return Err(ApiError::Conflict(format!(
                        "ingest job {} queue lease changed before attempt creation",
                        job.id
                    )));
                }
                ingest_repository::QueueLeaseOutcome::Retired => {
                    // Loud on purpose: the job could never have completed, and
                    // until it was retired it held a per-library concurrency
                    // slot on every reaper cycle.
                    tracing::error!(
                        job_id = %job.id,
                        library_id = %job.library_id,
                        "retired ingest job whose linked async operation is already terminal",
                    );
                    return Err(ApiError::Conflict(format!(
                        "ingest job {} was retired: its async operation is already terminal",
                        job.id
                    )));
                }
            }
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
}
