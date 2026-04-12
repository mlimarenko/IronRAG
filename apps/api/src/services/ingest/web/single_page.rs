use chrono::Utc;
use sha2::Digest as _;
use uuid::Uuid;

use super::{
    ApiError, AppState, FetchedWebResource, MaterializeWebCaptureCommand, MaterializedWebPage,
    UpdateAsyncOperationCommand, UpdateMutationCommand, UpdateWebIngestRun, WebCandidateState,
    WebClassificationReason, WebDiscoveredPageRow, WebIngestRunRow, WebIngestService,
    WebRunFailure, WebRunFailureCode, WebRunState, derive_terminal_run_state, extraction_title,
    fallback_title_from_url, ingest_repository, map_web_run_counts_row, now_if_terminal,
    resolved_web_mime_type, source_file_name_from_url, telemetry,
};

pub(super) async fn execute_single_page_run(
    service: &WebIngestService,
    state: &AppState,
    row: WebIngestRunRow,
    seed_candidate: WebDiscoveredPageRow,
) -> Result<WebIngestRunRow, ApiError> {
    let processing_row = ingest_repository::update_web_ingest_run(
        &state.persistence.postgres,
        row.id,
        &UpdateWebIngestRun {
            run_state: WebRunState::Processing.as_str(),
            completed_at: None,
            failure_code: None,
            cancel_requested_at: None,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", row.id))?;
    telemetry::web_run_event(
        "single_page_started",
        processing_row.id,
        processing_row.library_id,
        &processing_row.mode,
        &processing_row.run_state,
        &processing_row.seed_url,
    );

    if let Some(async_operation_id) = processing_row.async_operation_id {
        let _ = state
            .canonical_services
            .ops
            .update_async_operation(
                state,
                UpdateAsyncOperationCommand {
                    operation_id: async_operation_id,
                    status: "processing".to_string(),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;
    }

    let _ = ingest_repository::update_web_discovered_page(
        &state.persistence.postgres,
        seed_candidate.id,
        &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
            final_url: None,
            canonical_url: Some(processing_row.normalized_seed_url.as_str()),
            host_classification: None,
            candidate_state: WebCandidateState::Processing.as_str(),
            classification_reason: Some(WebClassificationReason::SeedAccepted.as_str()),
            content_type: None,
            http_status: None,
            snapshot_storage_key: None,
            updated_at: Some(Utc::now()),
            document_id: None,
            result_revision_id: None,
            mutation_item_id: None,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    telemetry::web_candidate_event(
        "candidate_processing",
        processing_row.id,
        seed_candidate.id,
        WebCandidateState::Processing.as_str(),
        &processing_row.normalized_seed_url,
        0,
        Some(WebClassificationReason::SeedAccepted.as_str()),
        None,
    );

    let resource = match fetch_web_resource(service, &processing_row.seed_url).await {
        Ok(resource) => resource,
        Err(failure) => {
            return fail_single_page_run(
                service,
                state,
                processing_row,
                seed_candidate.id,
                failure,
            )
            .await;
        }
    };
    let snapshot_storage_key =
        match persist_resource_snapshot(service, state, &processing_row, &resource).await {
            Ok(storage_key) => storage_key,
            Err(failure) => {
                return fail_single_page_run(
                    service,
                    state,
                    processing_row,
                    seed_candidate.id,
                    failure,
                )
                .await;
            }
        };
    let materialized = match materialize_snapshot_resource(
        service,
        state,
        &processing_row,
        &resource,
        &snapshot_storage_key,
    )
    .await
    {
        Ok(page) => page,
        Err(failure) => {
            return fail_single_page_run(
                service,
                state,
                processing_row,
                seed_candidate.id,
                failure,
            )
            .await;
        }
    };

    let _ = ingest_repository::update_web_discovered_page(
        &state.persistence.postgres,
        seed_candidate.id,
        &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
            final_url: Some(materialized.final_url.as_str()),
            canonical_url: Some(materialized.final_url.as_str()),
            host_classification: None,
            candidate_state: WebCandidateState::Processed.as_str(),
            classification_reason: Some(WebClassificationReason::SeedAccepted.as_str()),
            content_type: Some(materialized.content_type.as_str()),
            http_status: Some(resource.http_status),
            snapshot_storage_key: Some(snapshot_storage_key.as_str()),
            updated_at: Some(Utc::now()),
            document_id: Some(materialized.document_id),
            result_revision_id: Some(materialized.revision_id),
            mutation_item_id: Some(materialized.mutation_item_id),
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    telemetry::web_candidate_event(
        "candidate_processed",
        processing_row.id,
        seed_candidate.id,
        WebCandidateState::Processed.as_str(),
        &materialized.final_url,
        0,
        Some(WebClassificationReason::SeedAccepted.as_str()),
        None,
    );

    let counts = map_web_run_counts_row(
        ingest_repository::get_web_run_counts(&state.persistence.postgres, processing_row.id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
    )
    .counts;
    let terminal_state = derive_terminal_run_state(&crate::shared::web::ingest::WebRunCounts {
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
    let completed_at = now_if_terminal(terminal_state.as_str());
    let completed_row = ingest_repository::update_web_ingest_run(
        &state.persistence.postgres,
        processing_row.id,
        &UpdateWebIngestRun {
            run_state: terminal_state.as_str(),
            completed_at,
            failure_code: None,
            cancel_requested_at: None,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", processing_row.id))?;

    if let Some(async_operation_id) = completed_row.async_operation_id {
        let _ = state
            .canonical_services
            .ops
            .update_async_operation(
                state,
                UpdateAsyncOperationCommand {
                    operation_id: async_operation_id,
                    status: "ready".to_string(),
                    completed_at,
                    failure_code: None,
                },
            )
            .await?;
    }

    Ok(completed_row)
}

pub(super) async fn fetch_web_resource(
    service: &WebIngestService,
    seed_url: &str,
) -> Result<FetchedWebResource, WebRunFailure> {
    let response = service.http_client().get(seed_url).send().await.map_err(|error| {
        WebRunFailure::inaccessible(format!("failed to fetch seed url: {error}"))
    })?;
    let http_status = i32::from(response.status().as_u16());
    let final_url = crate::shared::web::url_identity::normalize_absolute_url(
        response.url().as_str(),
    )
    .map_err(|error| {
        WebRunFailure::invalid_url(format!(
            "fetched resource resolved to invalid final url: {error}"
        ))
    })?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);

    if !response.status().is_success() {
        return Err(WebRunFailure::inaccessible_with_response(
            format!("remote server returned status {}", response.status()),
            Some(final_url),
            content_type,
            Some(http_status),
        ));
    }

    let payload_bytes = response.bytes().await.map_err(|error| {
        WebRunFailure::inaccessible_with_response(
            format!("failed to read fetched response body: {error}"),
            Some(final_url.clone()),
            content_type.clone(),
            Some(http_status),
        )
    })?;

    Ok(FetchedWebResource {
        final_url,
        content_type,
        http_status,
        payload_bytes: payload_bytes.to_vec(),
    })
}

pub(super) async fn persist_resource_snapshot(
    _service: &WebIngestService,
    state: &AppState,
    run: &WebIngestRunRow,
    resource: &FetchedWebResource,
) -> Result<String, WebRunFailure> {
    let checksum = format!("sha256:{}", hex::encode(sha2::Sha256::digest(&resource.payload_bytes)));
    state
        .content_storage
        .persist_web_snapshot(
            run.workspace_id,
            run.library_id,
            &resource.final_url,
            &checksum,
            &resource.payload_bytes,
        )
        .await
        .map_err(|error| {
            WebRunFailure::internal(
                WebRunFailureCode::WebSnapshotPersistFailed.as_str(),
                format!("failed to persist fetched resource snapshot: {error}"),
                Some(resource.final_url.clone()),
                resource.content_type.clone(),
                Some(resource.http_status),
            )
        })
}

pub(super) async fn load_candidate_snapshot_resource(
    _service: &WebIngestService,
    state: &AppState,
    candidate: &WebDiscoveredPageRow,
) -> Result<FetchedWebResource, WebRunFailure> {
    let storage_key = candidate
        .snapshot_storage_key
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            WebRunFailure::internal(
                WebRunFailureCode::WebSnapshotMissing.as_str(),
                "eligible page is missing stored snapshot reference".to_string(),
                candidate.final_url.clone().or_else(|| candidate.canonical_url.clone()),
                candidate.content_type.clone(),
                candidate.http_status,
            )
        })?;
    let final_url =
        candidate.final_url.as_ref().or(candidate.canonical_url.as_ref()).cloned().ok_or_else(
            || {
                WebRunFailure::internal(
                    WebRunFailureCode::WebSnapshotMissingFinalUrl.as_str(),
                    "eligible page is missing final url identity".to_string(),
                    None,
                    candidate.content_type.clone(),
                    candidate.http_status,
                )
            },
        )?;
    let payload_bytes =
        state.content_storage.read_revision_source(storage_key).await.map_err(|error| {
            WebRunFailure::internal(
                WebRunFailureCode::WebSnapshotUnavailable.as_str(),
                format!("failed to read stored web snapshot: {error}"),
                Some(final_url.clone()),
                candidate.content_type.clone(),
                candidate.http_status,
            )
        })?;

    Ok(FetchedWebResource {
        final_url,
        content_type: candidate.content_type.clone(),
        http_status: candidate.http_status.unwrap_or(200),
        payload_bytes,
    })
}

pub(super) async fn materialize_snapshot_resource(
    _service: &WebIngestService,
    state: &AppState,
    run: &WebIngestRunRow,
    resource: &FetchedWebResource,
    storage_key: &str,
) -> Result<MaterializedWebPage, WebRunFailure> {
    let file_name =
        source_file_name_from_url(&resource.final_url, resource.content_type.as_deref());
    let extraction_plan = state
        .canonical_services
        .content
        .build_runtime_extraction_plan(
            state,
            run.library_id,
            &file_name,
            resource.content_type.as_deref(),
            &resource.payload_bytes,
        )
        .await
        .map_err(|error| {
            WebRunFailure::unsupported_content(
                error.message().to_string(),
                Some(resource.final_url.clone()),
                resource.content_type.clone(),
                Some(resource.http_status),
            )
        })?;

    let checksum = format!("sha256:{}", hex::encode(sha2::Sha256::digest(&resource.payload_bytes)));
    let materialized = state
        .canonical_services
        .content
        .materialize_web_capture(
            state,
            MaterializeWebCaptureCommand {
                workspace_id: run.workspace_id,
                library_id: run.library_id,
                mutation_id: run.mutation_id,
                requested_by_principal_id: run.requested_by_principal_id,
                final_url: resource.final_url.clone(),
                checksum,
                mime_type: resolved_web_mime_type(
                    resource.content_type.as_deref(),
                    &extraction_plan,
                ),
                byte_size: i64::try_from(resource.payload_bytes.len()).unwrap_or(i64::MAX),
                title: extraction_title(&extraction_plan.source_map)
                    .or_else(|| fallback_title_from_url(&resource.final_url)),
                storage_key: storage_key.to_string(),
            },
        )
        .await
        .map_err(|_| {
            WebRunFailure::internal(
                WebRunFailureCode::WebCaptureMaterializationFailed.as_str(),
                "failed to materialize canonical web capture".to_string(),
                Some(resource.final_url.clone()),
                resource.content_type.clone(),
                Some(resource.http_status),
            )
        })?;

    Ok(MaterializedWebPage {
        final_url: resource.final_url.clone(),
        content_type: resolved_web_mime_type(resource.content_type.as_deref(), &extraction_plan),
        document_id: materialized.document.id,
        revision_id: materialized.revision.id,
        mutation_item_id: materialized.mutation_item.id,
        _job_id: materialized.job_id,
    })
}

pub(super) async fn fail_single_page_run(
    _service: &WebIngestService,
    state: &AppState,
    row: WebIngestRunRow,
    candidate_id: Uuid,
    failure: WebRunFailure,
) -> Result<WebIngestRunRow, ApiError> {
    let _ = ingest_repository::update_web_discovered_page(
        &state.persistence.postgres,
        candidate_id,
        &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
            final_url: failure.final_url.as_deref(),
            canonical_url: failure.final_url.as_deref(),
            host_classification: None,
            candidate_state: WebCandidateState::Failed.as_str(),
            classification_reason: failure.candidate_reason.as_deref(),
            content_type: failure.content_type.as_deref(),
            http_status: failure.http_status,
            snapshot_storage_key: None,
            updated_at: Some(Utc::now()),
            document_id: None,
            result_revision_id: None,
            mutation_item_id: None,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    let completed_at = Some(Utc::now());
    let failed_row = ingest_repository::update_web_ingest_run(
        &state.persistence.postgres,
        row.id,
        &UpdateWebIngestRun {
            run_state: WebRunState::Failed.as_str(),
            completed_at,
            failure_code: Some(failure.failure_code.as_str()),
            cancel_requested_at: row.cancel_requested_at,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", row.id))?;
    telemetry::web_failure_event(
        "single_page_failed",
        row.id,
        Some(candidate_id),
        &failure.failure_code,
        failure.candidate_reason.as_deref(),
        failure.final_url.as_deref(),
        failure.content_type.as_deref(),
        failure.http_status,
    );

    let _ = state
        .canonical_services
        .content
        .update_mutation(
            state,
            UpdateMutationCommand {
                mutation_id: row.mutation_id,
                mutation_state: "failed".to_string(),
                completed_at,
                failure_code: Some(failure.failure_code.clone()),
                conflict_code: None,
            },
        )
        .await?;

    if let Some(async_operation_id) = failed_row.async_operation_id {
        let _ = state
            .canonical_services
            .ops
            .update_async_operation(
                state,
                UpdateAsyncOperationCommand {
                    operation_id: async_operation_id,
                    status: "failed".to_string(),
                    completed_at,
                    failure_code: Some(failure.failure_code),
                },
            )
            .await?;
    }

    Ok(failed_row)
}
