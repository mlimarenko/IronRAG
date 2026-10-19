use super::progress::normalize_optional_stage;
use super::*;

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
    // An operational deferral (provider rate limit / quota cooldown) is exempt
    // from the attempt ceiling: it requeues the job without consuming the
    // budget, so a provider outage can never permanently fail a revision.
    let operationally_deferred = command.operational_defer.is_some();
    let budget_exhausted = is_retry_budget_exhausted(
        &command.attempt_state,
        command.retryable,
        attempt.attempt_number,
        operationally_deferred,
    );
    let effective_retryable = (command.retryable && !budget_exhausted) || operationally_deferred;
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
    let head = content_document_head_repository::get_document_head(
        &state.persistence.postgres,
        document_id,
    )
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

/// The leased attempt and job rows plus the decision that finalizes them, so
/// the persistence helper carries one value instead of three positional rows.
struct FinalizeRows<'a> {
    attempt: &'a ingest_repository::IngestAttemptRow,
    job: &'a ingest_repository::IngestJobRow,
    decision: &'a FinalizeDecision,
}

async fn persist_finalized_attempt(
    state: &AppState,
    command: &FinalizeAttemptCommand,
    current_stage: Option<String>,
    rows: FinalizeRows<'_>,
    lifecycle_event: Option<&WebhookEvent>,
) -> Result<ingest_repository::IngestAttemptRow, ApiError> {
    let FinalizeRows { attempt, job, decision } = rows;
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
                Utc::now()
                    + retry_backoff_with_deferral(attempt.attempt_number, command.operational_defer)
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

pub(super) fn attempt_finalization_owns_linked_operation(job_kind: &str) -> bool {
    job_kind != "web_discovery"
}

async fn update_finalize_async_operation(
    state: &AppState,
    operation_id: Option<Uuid>,
    job_kind: &str,
    next_queue_state: &str,
    failure_code: Option<String>,
) -> Result<(), ApiError> {
    // A web-discovery job closes only the crawl/discovery phase. The run-level
    // operation remains processing until every exact materialized child item
    // settles; web-run settlement owns that terminal transition atomically
    // with the run and aggregate mutation. Marking it ready here races that
    // settlement and exposes a completed operation while content is pending.
    if !attempt_finalization_owns_linked_operation(job_kind) {
        return Ok(());
    }
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
            FinalizeRows { attempt: &attempt, job: &job, decision: &decision },
            lifecycle_event.as_ref(),
        )
        .await?;
        update_finalize_async_operation(
            state,
            job.async_operation_id,
            &job.job_kind,
            &decision.next_queue_state,
            failure_code,
        )
        .await?;
        let finished_at = row.finished_at.unwrap_or_else(Utc::now);
        crate::observability::metrics::record_ingest_attempt(
            &job.job_kind,
            crate::observability::metrics::classify_ingest_outcome(
                &command.attempt_state,
                &decision.next_queue_state,
            ),
            command.failure_code.as_deref(),
            crate::observability::metrics::elapsed_seconds(row.started_at, finished_at),
        );
        Ok(map_attempt_row(row))
    }
}
