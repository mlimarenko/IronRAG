use super::*;

pub(in super::super) async fn queue_recursive_page_jobs(
    service: &WebIngestService,
    state: &AppState,
    run: &WebIngestRunRow,
    pages: &[WebDiscoveredPageRow],
) -> Result<(), ApiError> {
    for page in pages {
        if queue_recursive_page(service, state, run, page).await? {
            return Ok(());
        }
    }

    Ok(())
}

/// Queue one eligible page for the worker. Returns `true` when crawl
/// cancellation was observed and the remaining pages were marked canceled, so
/// the caller stops queueing.
async fn queue_recursive_page(
    service: &WebIngestService,
    state: &AppState,
    run: &WebIngestRunRow,
    page: &WebDiscoveredPageRow,
) -> Result<bool, ApiError> {
    let cancel_requested = match service.run_cancel_requested(state, run.id).await {
        Ok(value) => value,
        Err(error) => {
            log_queue_cancel_check_failure(run.id, page.id, &error);
            return Err(error);
        }
    };
    if cancel_requested {
        service.mark_pending_pages_canceled(state, run.id).await?;
        return Ok(true);
    }
    let queued_page = match ingest_repository::update_web_discovered_page(
        &state.persistence.postgres,
        page.id,
        &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
            final_url: page.final_url.as_deref(),
            preferred_url: page.preferred_url.as_deref(),
            host_classification: Some(page.host_classification.as_str()),
            candidate_state: WebCandidateState::Queued.as_str(),
            classification_reason: page.classification_reason.as_deref(),
            classification_detail: page.classification_detail.as_deref(),
            content_type: page.content_type.as_deref(),
            http_status: page.http_status,
            snapshot_storage_key: page.snapshot_storage_key.as_deref(),
            updated_at: Some(Utc::now()),
            document_id: None,
            result_revision_id: None,
            mutation_item_id: None,
        },
    )
    .await
    {
        Ok(Some(page)) => page,
        Ok(None) => {
            let error = ApiError::resource_not_found("web_discovered_page", page.id);
            log_queue_missing_page(run.id, page.id, &error);
            return Err(error);
        }
        Err(_) => {
            log_queue_persist_failure(run.id, page.id);
            return Err(ApiError::Internal);
        }
    };
    telemetry::web_candidate_event(telemetry::WebCandidateEvent {
        event: "candidate_queued",
        run_id: run.id,
        candidate_id: queued_page.id,
        candidate_state: &queued_page.candidate_state,
        normalized_url: &queued_page.normalized_url,
        depth: queued_page.depth,
        classification_reason: queued_page.classification_reason.as_deref(),
        host_classification: Some(queued_page.host_classification.as_str()),
    });
    let job_operation = match state
        .application_services
        .ops
        .create_async_operation(
            state,
            CreateAsyncOperationCommand {
                workspace_id: run.workspace_id,
                library_id: Some(run.library_id),
                operation_kind: "web_materialize_page".to_string(),
                surface_kind: "worker".to_string(),
                requested_by_principal_id: run.requested_by_principal_id,
                status: "accepted".to_string(),
                subject_kind: "content_web_discovered_page".to_string(),
                subject_id: Some(queued_page.id),
                parent_async_operation_id: None,
                completed_at: None,
                failure_code: None,
            },
        )
        .await
    {
        Ok(operation) => operation,
        Err(error) => {
            log_queue_operation_failure(run.id, queued_page.id, &error);
            return Err(error);
        }
    };
    if let Err(error) = state
        .application_services
        .ingest
        .admit_job(
            state,
            AdmitIngestJobCommand {
                workspace_id: run.workspace_id,
                library_id: run.library_id,
                mutation_id: None,
                mutation_item_id: None,
                connector_id: None,
                async_operation_id: Some(job_operation.id),
                knowledge_document_id: None,
                knowledge_revision_id: None,
                job_kind: "web_materialize_page".to_string(),
                priority: 60,
                dedupe_key: Some(format!("web-materialize-page:{}", queued_page.id)),
                available_at: None,
            },
        )
        .await
    {
        log_queue_admit_failure(run.id, queued_page.id, job_operation.id, &error);
        return Err(error);
    }
    Ok(false)
}

fn log_queue_cancel_check_failure(run_id: Uuid, candidate_id: Uuid, error: &ApiError) {
    error!(run_id = %run_id, candidate_id = %candidate_id, error = %error, "web ingest failed to refresh cancel state before queueing page");
}

fn log_queue_missing_page(run_id: Uuid, candidate_id: Uuid, error: &ApiError) {
    error!(run_id = %run_id, candidate_id = %candidate_id, error = %error, "web ingest failed to mark candidate queued because page row disappeared");
}

fn log_queue_persist_failure(run_id: Uuid, candidate_id: Uuid) {
    error!(run_id = %run_id, candidate_id = %candidate_id, "web ingest failed to persist queued candidate state");
}

fn log_queue_operation_failure(run_id: Uuid, candidate_id: Uuid, error: &ApiError) {
    error!(run_id = %run_id, candidate_id = %candidate_id, error = %error, "web ingest failed to create async operation for queued candidate");
}

fn log_queue_admit_failure(
    run_id: Uuid,
    candidate_id: Uuid,
    async_operation_id: Uuid,
    error: &ApiError,
) {
    error!(run_id = %run_id, candidate_id = %candidate_id, async_operation_id = %async_operation_id, error = %error, "web ingest failed to admit web materialize page job");
}
