use super::*;

pub(in super::super) async fn load_eligible_pages_for_run(
    _service: &WebIngestService,
    state: &AppState,
    run_id: Uuid,
) -> Result<Vec<WebDiscoveredPageRow>, ApiError> {
    Ok(ingest_repository::list_web_discovered_pages(&state.persistence.postgres, run_id)
        .await
        .map_err(|error| {
            error!(%run_id, db_error = %error, "web ingest failed to load discovered pages for run");
            ApiError::Internal
        })?
        .into_iter()
        .filter(|page| page.candidate_state == WebCandidateState::Eligible.as_str())
        .collect())
}

pub(in super::super) async fn finalize_recursive_run_if_settled(
    service: &WebIngestService,
    state: &AppState,
    run_id: Uuid,
) -> Result<WebIngestRunRow, ApiError> {
    let row = ingest_repository::get_web_ingest_run_by_id(&state.persistence.postgres, run_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))?;
    if matches!(row.run_state.as_str(), "completed" | "completed_partial" | "failed" | "canceled") {
        return Ok(row);
    }
    let counts = map_web_run_counts_row(
        ingest_repository::get_web_run_counts(&state.persistence.postgres, row.id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
    )
    .counts;
    if counts.queued > 0 || counts.processing > 0 {
        return Ok(row);
    }
    service.finalize_recursive_run(state, row).await
}

pub(in super::super) async fn mark_recursive_page_failed(
    service: &WebIngestService,
    state: &AppState,
    page: &WebDiscoveredPageRow,
    failure: WebRunFailure,
) -> Result<WebDiscoveredPageRow, ApiError> {
    let updated = ingest_repository::update_web_discovered_page(
        &state.persistence.postgres,
        page.id,
        &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
            final_url: failure.final_url.as_deref().or(page.final_url.as_deref()),
            preferred_url: failure.final_url.as_deref().or(page.preferred_url.as_deref()),
            host_classification: Some(page.host_classification.as_str()),
            candidate_state: WebCandidateState::Failed.as_str(),
            classification_reason: failure
                .candidate_reason
                .as_deref()
                .or(page.classification_reason.as_deref()),
            classification_detail: page.classification_detail.as_deref(),
            content_type: failure.content_type.as_deref().or(page.content_type.as_deref()),
            http_status: failure.http_status.or(page.http_status),
            snapshot_storage_key: page.snapshot_storage_key.as_deref(),
            updated_at: Some(Utc::now()),
            document_id: None,
            result_revision_id: None,
            mutation_item_id: None,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", page.id))?;
    telemetry::web_failure_event(telemetry::WebFailureEvent {
        event: "candidate_failed",
        run_id: updated.run_id,
        candidate_id: Some(updated.id),
        failure_code: &failure.failure_code,
        classification_reason: updated.classification_reason.as_deref(),
        final_url: updated.final_url.as_deref(),
        content_type: updated.content_type.as_deref(),
        http_status: updated.http_status,
    });
    let _ = service.finalize_recursive_run_if_settled(state, updated.run_id).await?;
    Ok(updated)
}

pub(in super::super) async fn cancel_page_candidate(
    _service: &WebIngestService,
    state: &AppState,
    page: &WebDiscoveredPageRow,
) -> Result<WebDiscoveredPageRow, ApiError> {
    let updated = ingest_repository::update_web_discovered_page(
        &state.persistence.postgres,
        page.id,
        &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
            final_url: page.final_url.as_deref(),
            preferred_url: page.preferred_url.as_deref(),
            host_classification: Some(page.host_classification.as_str()),
            candidate_state: WebCandidateState::Canceled.as_str(),
            classification_reason: Some(WebClassificationReason::CancelRequested.as_str()),
            classification_detail: page.classification_detail.as_deref(),
            content_type: page.content_type.as_deref(),
            http_status: page.http_status,
            snapshot_storage_key: page.snapshot_storage_key.as_deref(),
            updated_at: Some(Utc::now()),
            document_id: page.document_id,
            result_revision_id: page.result_revision_id,
            mutation_item_id: page.mutation_item_id,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", page.id))?;
    telemetry::web_candidate_event(telemetry::WebCandidateEvent {
        event: "candidate_canceled",
        run_id: updated.run_id,
        candidate_id: updated.id,
        candidate_state: &updated.candidate_state,
        normalized_url: &updated.normalized_url,
        depth: updated.depth,
        classification_reason: updated.classification_reason.as_deref(),
        host_classification: Some(updated.host_classification.as_str()),
    });
    Ok(updated)
}

pub(in super::super) async fn mark_pending_pages_canceled(
    service: &WebIngestService,
    state: &AppState,
    run_id: Uuid,
) -> Result<(), ApiError> {
    let pages = ingest_repository::list_web_discovered_pages(&state.persistence.postgres, run_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    for page in pages {
        if matches!(page.candidate_state.as_str(), "discovered" | "eligible" | "queued") {
            let _ = service.cancel_page_candidate(state, &page).await?;
        }
    }
    Ok(())
}

pub(in super::super) async fn finalize_recursive_run(
    _service: &WebIngestService,
    state: &AppState,
    row: WebIngestRunRow,
) -> Result<WebIngestRunRow, ApiError> {
    let counts = map_web_run_counts_row(
        ingest_repository::get_web_run_counts(&state.persistence.postgres, row.id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
    )
    .counts;
    let mut terminal_state = derive_terminal_run_state(&crate::shared::web::ingest::WebRunCounts {
        discovered: counts.discovered,
        eligible: counts.eligible,
        processed: counts.processed,
        queued: counts.queued,
        processing: counts.processing,
        duplicates: counts.duplicates,
        excluded: counts.excluded,
        blocked: counts.blocked,
        failed: counts.failed,
        canceled: counts.canceled,
    });
    if row.cancel_requested_at.is_some() && counts.queued == 0 && counts.processing == 0 {
        terminal_state = WebRunState::Canceled;
    }
    let completed_at = now_if_terminal(terminal_state.as_str());
    let failure_code = (terminal_state == WebRunState::Failed)
        .then_some(WebRunFailureCode::RecursiveCrawlFailed.as_str());
    let completed_row = match ingest_repository::finalize_web_ingest_run(
        &state.persistence.postgres,
        row.id,
        &UpdateWebIngestRun {
            run_state: terminal_state.as_str(),
            completed_at,
            failure_code,
            cancel_requested_at: row.cancel_requested_at,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    {
        Some(updated) => updated,
        None => {
            return ingest_repository::get_web_ingest_run_by_id(
                &state.persistence.postgres,
                row.id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", row.id));
        }
    };
    telemetry::web_run_event(
        "run_finalized",
        completed_row.id,
        completed_row.library_id,
        &completed_row.mode,
        &completed_row.run_state,
        &completed_row.seed_url,
    );

    if let Some(async_operation_id) = completed_row.async_operation_id {
        let status = match terminal_state {
            WebRunState::Completed | WebRunState::CompletedPartial => "ready",
            WebRunState::Canceled => "canceled",
            WebRunState::Failed => "failed",
            _ => "processing",
        };
        let _ = state
            .application_services
            .ops
            .update_async_operation(
                state,
                UpdateAsyncOperationCommand {
                    operation_id: async_operation_id,
                    status: status.to_string(),
                    completed_at,
                    failure_code: failure_code.map(str::to_string),
                },
            )
            .await?;
    }

    let mutation_state = match terminal_state {
        WebRunState::Completed | WebRunState::CompletedPartial => "applied",
        WebRunState::Canceled => "canceled",
        WebRunState::Failed => "failed",
        _ => "processing",
    };
    if matches!(
        terminal_state,
        WebRunState::Completed
            | WebRunState::CompletedPartial
            | WebRunState::Canceled
            | WebRunState::Failed
    ) {
        let _ = state
            .application_services
            .content
            .update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id: completed_row.mutation_id,
                    mutation_state: mutation_state.to_string(),
                    completed_at,
                    failure_code: (terminal_state == WebRunState::Failed)
                        .then_some(WebRunFailureCode::RecursiveCrawlFailed.as_str().to_string()),
                    conflict_code: None,
                },
            )
            .await?;
    }

    Ok(completed_row)
}

pub(in super::super) async fn transition_run_state(
    _service: &WebIngestService,
    state: &AppState,
    row: WebIngestRunRow,
    run_state: WebRunState,
    async_status: &str,
) -> Result<WebIngestRunRow, ApiError> {
    let updated_row = ingest_repository::update_web_ingest_run(
        &state.persistence.postgres,
        row.id,
        &UpdateWebIngestRun {
            run_state: run_state.as_str(),
            completed_at: None,
            failure_code: None,
            cancel_requested_at: row.cancel_requested_at,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", row.id))?;

    if let Some(async_operation_id) = updated_row.async_operation_id {
        let _ = state
            .application_services
            .ops
            .update_async_operation(
                state,
                UpdateAsyncOperationCommand {
                    operation_id: async_operation_id,
                    status: async_status.to_string(),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;
    }

    Ok(updated_row)
}

pub(in super::super) async fn get_run_row(
    _service: &WebIngestService,
    state: &AppState,
    run_id: Uuid,
) -> Result<WebIngestRunRow, ApiError> {
    ingest_repository::get_web_ingest_run_by_id(&state.persistence.postgres, run_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))
}

pub(in super::super) async fn run_cancel_requested(
    service: &WebIngestService,
    state: &AppState,
    run_id: Uuid,
) -> Result<bool, ApiError> {
    Ok(service.get_run_row(state, run_id).await?.cancel_requested_at.is_some())
}
