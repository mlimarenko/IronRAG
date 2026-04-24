use std::collections::{HashSet, VecDeque};

use chrono::Utc;
use futures::stream::{self, StreamExt};
use tracing::error;
use uuid::Uuid;

use super::*;

pub(super) async fn enqueue_recursive_run(
    service: &WebIngestService,
    state: &AppState,
    row: WebIngestRunRow,
) -> Result<WebIngestRunRow, ApiError> {
    let job_operation = state
        .canonical_services
        .ops
        .create_async_operation(
            state,
            CreateAsyncOperationCommand {
                workspace_id: row.workspace_id,
                library_id: row.library_id,
                operation_kind: "web_discovery".to_string(),
                surface_kind: "worker".to_string(),
                requested_by_principal_id: row.requested_by_principal_id,
                status: "accepted".to_string(),
                subject_kind: "content_web_ingest_run".to_string(),
                subject_id: Some(row.id),
                parent_async_operation_id: None,
                completed_at: None,
                failure_code: None,
            },
        )
        .await?;
    let _ = state
        .canonical_services
        .ingest
        .admit_job(
            state,
            AdmitIngestJobCommand {
                workspace_id: row.workspace_id,
                library_id: row.library_id,
                mutation_id: None,
                connector_id: None,
                async_operation_id: Some(job_operation.id),
                knowledge_document_id: None,
                knowledge_revision_id: None,
                job_kind: "web_discovery".to_string(),
                priority: 40,
                dedupe_key: Some(format!("web-discovery:{}", row.id)),
                available_at: None,
            },
        )
        .await?;
    let _ = service;
    Ok(row)
}

pub(super) async fn discover_recursive_scope(
    service: &WebIngestService,
    state: &AppState,
    run: &WebIngestRunRow,
    seed_candidate: WebDiscoveredPageRow,
) -> Result<Vec<WebDiscoveredPageRow>, ApiError> {
    let mut frontier = VecDeque::from([seed_candidate]);
    let mut seen_urls = HashSet::from([run.normalized_seed_url.clone()]);
    let mut budgeted_urls = HashSet::from([run.normalized_seed_url.clone()]);
    let mut canonical_urls = HashSet::<String>::new();
    let mut eligible_pages = Vec::<WebDiscoveredPageRow>::new();
    let ignore_patterns = parse_run_ignore_patterns(run.ignore_patterns.clone())?;

    let fetch_concurrency = state.settings.web_ingest_crawl_concurrency.max(1);

    'outer: while !frontier.is_empty() {
        if service.run_cancel_requested(state, run.id).await? {
            break;
        }

        let mut wave: Vec<WebDiscoveredPageRow> = Vec::new();
        while wave.len() < fetch_concurrency {
            match frontier.pop_front() {
                Some(candidate) => wave.push(candidate),
                None => break,
            }
        }
        let fetched_wave: Vec<(WebDiscoveredPageRow, Result<FetchedWebResource, WebRunFailure>)> =
            stream::iter(wave.into_iter().map(|candidate| {
                let url = candidate.normalized_url.clone();
                async move {
                    let result = service.fetch_web_resource(&url).await;
                    (candidate, result)
                }
            }))
            .buffer_unordered(fetch_concurrency)
            .collect::<Vec<_>>()
            .await;

        for (candidate, fetch_result) in fetched_wave {
            if service.run_cancel_requested(state, run.id).await? {
                break 'outer;
            }
            let resource = match fetch_result {
                Ok(resource) => resource,
                Err(failure) => {
                    telemetry::web_failure_event(
                        "candidate_fetch_failed",
                        run.id,
                        Some(candidate.id),
                        &failure.failure_code,
                        failure.candidate_reason.as_deref(),
                        failure.final_url.as_deref(),
                        failure.content_type.as_deref(),
                        failure.http_status,
                    );
                    let _ = ingest_repository::update_web_discovered_page(
                        &state.persistence.postgres,
                        candidate.id,
                        &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                            final_url: failure.final_url.as_deref(),
                            canonical_url: failure.final_url.as_deref(),
                            host_classification: None,
                            candidate_state: WebCandidateState::Blocked.as_str(),
                            classification_reason: failure.candidate_reason.as_deref(),
                            classification_detail: candidate.classification_detail.as_deref(),
                            content_type: failure.content_type.as_deref(),
                            http_status: failure.http_status,
                            snapshot_storage_key: candidate.snapshot_storage_key.as_deref(),
                            updated_at: Some(Utc::now()),
                            document_id: None,
                            result_revision_id: None,
                            mutation_item_id: None,
                        },
                    )
                    .await
                    .map_err(|error| {
                        error!(
                            run_id = %run.id,
                            candidate_id = %candidate.id,
                            normalized_url = %candidate.normalized_url,
                            failure_code = %failure.failure_code,
                            db_error = %error,
                            "web ingest failed to persist blocked candidate after fetch failure"
                        );
                        ApiError::Internal
                    })?;
                    continue;
                }
            };
            if service.run_cancel_requested(state, run.id).await? {
                break;
            }

            let host_classification = crate::shared::web::url_identity::classify_host(
                &run.normalized_seed_url,
                &resource.final_url,
            )
            .unwrap_or(HostClassification::External);
            let resource_canonical_url = extract_html_canonical_url(
                &resource.payload_bytes,
                resource.content_type.as_deref(),
                &resource.final_url,
            )
            .unwrap_or_else(|| resource.final_url.clone());
            let is_same_host = host_classification == HostClassification::SameHost;
            let host_classification_label = host_classification.as_str();

            if run.boundary_policy == "same_host" && !is_same_host {
                let _ = ingest_repository::update_web_discovered_page(
                    &state.persistence.postgres,
                    candidate.id,
                    &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                        final_url: Some(resource.final_url.as_str()),
                        canonical_url: Some(resource_canonical_url.as_str()),
                        host_classification: Some(host_classification_label),
                        candidate_state: WebCandidateState::Excluded.as_str(),
                        classification_reason: Some(
                            WebClassificationReason::OutsideBoundaryPolicy.as_str(),
                        ),
                        classification_detail: None,
                        content_type: resource.content_type.as_deref(),
                        http_status: Some(resource.http_status),
                        snapshot_storage_key: None,
                        updated_at: Some(Utc::now()),
                        document_id: None,
                        result_revision_id: None,
                        mutation_item_id: None,
                    },
                )
                .await
                .map_err(|error| {
                    error!(
                        run_id = %run.id,
                        candidate_id = %candidate.id,
                        normalized_url = %candidate.normalized_url,
                        final_url = %resource.final_url,
                        db_error = %error,
                        "web ingest failed to persist boundary-excluded candidate"
                    );
                    ApiError::Internal
                })?;
                telemetry::web_candidate_event(
                    "candidate_excluded_boundary",
                    run.id,
                    candidate.id,
                    WebCandidateState::Excluded.as_str(),
                    &candidate.normalized_url,
                    candidate.depth,
                    Some(WebClassificationReason::OutsideBoundaryPolicy.as_str()),
                    Some(host_classification_label),
                );
                continue;
            }

            if let Some(ignore_match) =
                match_web_ingest_ignore_pattern(&resource.final_url, &ignore_patterns)
            {
                let _ = ingest_repository::update_web_discovered_page(
                    &state.persistence.postgres,
                    candidate.id,
                    &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                        final_url: Some(resource.final_url.as_str()),
                        canonical_url: Some(resource_canonical_url.as_str()),
                        host_classification: Some(host_classification_label),
                        candidate_state: WebCandidateState::Excluded.as_str(),
                        classification_reason: Some(
                            WebClassificationReason::IgnorePattern.as_str(),
                        ),
                        classification_detail: Some(ignore_match.detail.as_str()),
                        content_type: resource.content_type.as_deref(),
                        http_status: Some(resource.http_status),
                        snapshot_storage_key: None,
                        updated_at: Some(Utc::now()),
                        document_id: None,
                        result_revision_id: None,
                        mutation_item_id: None,
                    },
                )
                .await
                .map_err(|error| {
                    error!(
                        run_id = %run.id,
                        candidate_id = %candidate.id,
                        normalized_url = %candidate.normalized_url,
                        final_url = %resource.final_url,
                        classification_detail = %ignore_match.detail,
                        db_error = %error,
                        "web ingest failed to persist ignore-pattern-excluded candidate"
                    );
                    ApiError::Internal
                })?;
                telemetry::web_candidate_event(
                    "candidate_excluded_ignore_pattern",
                    run.id,
                    candidate.id,
                    WebCandidateState::Excluded.as_str(),
                    &candidate.normalized_url,
                    candidate.depth,
                    Some(WebClassificationReason::IgnorePattern.as_str()),
                    Some(host_classification_label),
                );
                continue;
            }

            if canonical_urls.contains(&resource_canonical_url) {
                let _ = ingest_repository::update_web_discovered_page(
                    &state.persistence.postgres,
                    candidate.id,
                    &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                        final_url: Some(resource.final_url.as_str()),
                        canonical_url: Some(resource_canonical_url.as_str()),
                        host_classification: Some(host_classification_label),
                        candidate_state: WebCandidateState::Duplicate.as_str(),
                        classification_reason: Some(
                            WebClassificationReason::DuplicateCanonicalUrl.as_str(),
                        ),
                        classification_detail: None,
                        content_type: resource.content_type.as_deref(),
                        http_status: Some(resource.http_status),
                        snapshot_storage_key: None,
                        updated_at: Some(Utc::now()),
                        document_id: None,
                        result_revision_id: None,
                        mutation_item_id: None,
                    },
                )
                .await
                .map_err(|error| {
                    error!(
                        run_id = %run.id,
                        candidate_id = %candidate.id,
                        normalized_url = %candidate.normalized_url,
                        final_url = %resource.final_url,
                        canonical_url = %resource_canonical_url,
                        db_error = %error,
                        "web ingest failed to persist duplicate canonical candidate"
                    );
                    ApiError::Internal
                })?;
                telemetry::web_candidate_event(
                    "candidate_duplicate",
                    run.id,
                    candidate.id,
                    WebCandidateState::Duplicate.as_str(),
                    &candidate.normalized_url,
                    candidate.depth,
                    Some(WebClassificationReason::DuplicateCanonicalUrl.as_str()),
                    Some(host_classification_label),
                );
                continue;
            }
            canonical_urls.insert(resource_canonical_url.clone());

            let snapshot_storage_key = service
                .persist_resource_snapshot(state, run, &resource)
                .await
                .map_err(|failure| {
                    ApiError::BadRequest(
                        failure
                            .candidate_reason
                            .clone()
                            .unwrap_or_else(|| failure.failure_code.clone()),
                    )
                })?;
            let candidate_row = ingest_repository::update_web_discovered_page(
                &state.persistence.postgres,
                candidate.id,
                &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                    final_url: Some(resource.final_url.as_str()),
                    canonical_url: Some(resource_canonical_url.as_str()),
                    host_classification: Some(host_classification_label),
                    candidate_state: WebCandidateState::Eligible.as_str(),
                    classification_reason: candidate.classification_reason.as_deref(),
                    classification_detail: candidate.classification_detail.as_deref(),
                    content_type: resource.content_type.as_deref(),
                    http_status: Some(resource.http_status),
                    snapshot_storage_key: Some(snapshot_storage_key.as_str()),
                    updated_at: Some(Utc::now()),
                    document_id: None,
                    result_revision_id: None,
                    mutation_item_id: None,
                },
            )
            .await
            .map_err(|error| {
                error!(
                    run_id = %run.id,
                    candidate_id = %candidate.id,
                    normalized_url = %candidate.normalized_url,
                    final_url = %resource.final_url,
                    content_type = ?resource.content_type,
                    snapshot_storage_key = %snapshot_storage_key,
                    db_error = %error,
                    "web ingest failed to persist discovered candidate snapshot state"
                );
                ApiError::Internal
            })?
            .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", candidate.id))?;
            telemetry::web_candidate_event(
                "candidate_eligible",
                run.id,
                candidate_row.id,
                &candidate_row.candidate_state,
                &candidate_row.normalized_url,
                candidate_row.depth,
                candidate_row.classification_reason.as_deref(),
                Some(candidate_row.host_classification.as_str()),
            );

            if candidate_row.depth < run.max_depth {
                for discovered_url in
                    service.discover_outbound_links(state, run.library_id, &resource).await?
                {
                    if service.run_cancel_requested(state, run.id).await? {
                        return Ok(eligible_pages);
                    }
                    let Ok(resolved_url) = crate::shared::web::url_identity::resolve_discovered_url(
                        &resource.final_url,
                        &discovered_url,
                    ) else {
                        continue;
                    };
                    let next_depth = candidate_row.depth.saturating_add(1);
                    let discovered_host = crate::shared::web::url_identity::classify_host(
                        &run.normalized_seed_url,
                        &resolved_url,
                    )
                    .unwrap_or(HostClassification::External);

                    if seen_urls.contains(&resolved_url) {
                        continue;
                    }
                    seen_urls.insert(resolved_url.clone());

                    let (candidate_state, classification_reason, classification_detail) =
                        if next_depth > run.max_depth {
                            (
                                WebCandidateState::Excluded,
                                Some(WebClassificationReason::ExceededMaxDepth.as_str()),
                                None,
                            )
                        } else if run.boundary_policy == "same_host"
                            && discovered_host != HostClassification::SameHost
                        {
                            (
                                WebCandidateState::Excluded,
                                Some(WebClassificationReason::OutsideBoundaryPolicy.as_str()),
                                None,
                            )
                        } else if let Some(ignore_match) =
                            match_web_ingest_ignore_pattern(&resolved_url, &ignore_patterns)
                        {
                            (
                                WebCandidateState::Excluded,
                                Some(WebClassificationReason::IgnorePattern.as_str()),
                                Some(ignore_match.detail),
                            )
                        } else if i32::try_from(budgeted_urls.len()).unwrap_or(i32::MAX)
                            >= run.max_pages
                        {
                            (
                                WebCandidateState::Excluded,
                                Some(WebClassificationReason::ExceededMaxPages.as_str()),
                                None,
                            )
                        } else {
                            budgeted_urls.insert(resolved_url.clone());
                            (
                                WebCandidateState::Eligible,
                                Some(WebClassificationReason::SeedAccepted.as_str()),
                                None,
                            )
                        };
                    if service.run_cancel_requested(state, run.id).await? {
                        return Ok(eligible_pages);
                    }

                    let discovered_row = ingest_repository::create_web_discovered_page(
                        &state.persistence.postgres,
                        &NewWebDiscoveredPage {
                            id: Uuid::now_v7(),
                            run_id: run.id,
                            discovered_url: Some(discovered_url.as_str()),
                            normalized_url: &resolved_url,
                            final_url: None,
                            canonical_url: Some(&resolved_url),
                            depth: next_depth,
                            referrer_candidate_id: Some(candidate_row.id),
                            host_classification: discovered_host.as_str(),
                            candidate_state: candidate_state.as_str(),
                            classification_reason,
                            classification_detail: classification_detail.as_deref(),
                            content_type: None,
                            http_status: None,
                            snapshot_storage_key: None,
                            discovered_at: None,
                            updated_at: None,
                            document_id: None,
                            result_revision_id: None,
                            mutation_item_id: None,
                        },
                    )
                    .await
                    .map_err(|error| {
                        error!(
                            run_id = %run.id,
                            referrer_candidate_id = %candidate_row.id,
                            normalized_url = %resolved_url,
                            depth = next_depth,
                            candidate_state = %candidate_state.as_str(),
                            classification_reason = ?classification_reason,
                            db_error = %error,
                            "web ingest failed to persist discovered outbound candidate"
                        );
                        ApiError::Internal
                    })?;
                    telemetry::web_candidate_event(
                        "candidate_discovered",
                        run.id,
                        discovered_row.id,
                        discovered_row.candidate_state.as_str(),
                        &discovered_row.normalized_url,
                        discovered_row.depth,
                        discovered_row.classification_reason.as_deref(),
                        Some(discovered_row.host_classification.as_str()),
                    );

                    if candidate_state == WebCandidateState::Eligible {
                        frontier.push_back(discovered_row);
                    }
                }
            }

            eligible_pages.push(candidate_row);
        }
    }

    Ok(eligible_pages)
}

pub(super) async fn queue_recursive_page_jobs(
    service: &WebIngestService,
    state: &AppState,
    run: &WebIngestRunRow,
    pages: &[WebDiscoveredPageRow],
) -> Result<(), ApiError> {
    for page in pages {
        let cancel_requested = match service.run_cancel_requested(state, run.id).await {
            Ok(value) => value,
            Err(error) => {
                error!(run_id = %run.id, candidate_id = %page.id, error = %error, "web ingest failed to refresh cancel state before queueing page");
                return Err(error);
            }
        };
        if cancel_requested {
            service.mark_pending_pages_canceled(state, run.id).await?;
            return Ok(());
        }
        let queued_page = match ingest_repository::update_web_discovered_page(
            &state.persistence.postgres,
            page.id,
            &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                final_url: page.final_url.as_deref(),
                canonical_url: page.canonical_url.as_deref(),
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
                error!(run_id = %run.id, candidate_id = %page.id, error = %error, "web ingest failed to mark candidate queued because page row disappeared");
                return Err(error);
            }
            Err(_) => {
                error!(run_id = %run.id, candidate_id = %page.id, "web ingest failed to persist queued candidate state");
                return Err(ApiError::Internal);
            }
        };
        telemetry::web_candidate_event(
            "candidate_queued",
            run.id,
            queued_page.id,
            &queued_page.candidate_state,
            &queued_page.normalized_url,
            queued_page.depth,
            queued_page.classification_reason.as_deref(),
            Some(queued_page.host_classification.as_str()),
        );
        let job_operation = match state
            .canonical_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: run.workspace_id,
                    library_id: run.library_id,
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
                error!(run_id = %run.id, candidate_id = %queued_page.id, error = %error, "web ingest failed to create async operation for queued candidate");
                return Err(error);
            }
        };
        if let Err(error) = state
            .canonical_services
            .ingest
            .admit_job(
                state,
                AdmitIngestJobCommand {
                    workspace_id: run.workspace_id,
                    library_id: run.library_id,
                    mutation_id: None,
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
            error!(run_id = %run.id, candidate_id = %queued_page.id, async_operation_id = %job_operation.id, error = %error, "web ingest failed to admit web materialize page job");
            return Err(error);
        }
    }

    Ok(())
}

pub(super) async fn load_eligible_pages_for_run(
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

pub(super) async fn finalize_recursive_run_if_settled(
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

pub(super) async fn mark_recursive_page_failed(
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
            canonical_url: failure.final_url.as_deref().or(page.canonical_url.as_deref()),
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
    telemetry::web_failure_event(
        "candidate_failed",
        updated.run_id,
        Some(updated.id),
        &failure.failure_code,
        updated.classification_reason.as_deref(),
        updated.final_url.as_deref(),
        updated.content_type.as_deref(),
        updated.http_status,
    );
    let _ = service.finalize_recursive_run_if_settled(state, updated.run_id).await?;
    Ok(updated)
}

pub(super) async fn cancel_page_candidate(
    _service: &WebIngestService,
    state: &AppState,
    page: &WebDiscoveredPageRow,
) -> Result<WebDiscoveredPageRow, ApiError> {
    let updated = ingest_repository::update_web_discovered_page(
        &state.persistence.postgres,
        page.id,
        &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
            final_url: page.final_url.as_deref(),
            canonical_url: page.canonical_url.as_deref(),
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
    telemetry::web_candidate_event(
        "candidate_canceled",
        updated.run_id,
        updated.id,
        &updated.candidate_state,
        &updated.normalized_url,
        updated.depth,
        updated.classification_reason.as_deref(),
        Some(updated.host_classification.as_str()),
    );
    Ok(updated)
}

pub(super) async fn mark_pending_pages_canceled(
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

pub(super) async fn finalize_recursive_run(
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
        row.id,
        &UpdateWebIngestRun {
            run_state: terminal_state.as_str(),
            completed_at,
            failure_code: (terminal_state == WebRunState::Failed)
                .then_some(WebRunFailureCode::RecursiveCrawlFailed.as_str()),
            cancel_requested_at: row.cancel_requested_at,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", row.id))?;
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
            .canonical_services
            .ops
            .update_async_operation(
                state,
                UpdateAsyncOperationCommand {
                    operation_id: async_operation_id,
                    status: status.to_string(),
                    completed_at,
                    failure_code: (terminal_state == WebRunState::Failed)
                        .then_some(WebRunFailureCode::RecursiveCrawlFailed.as_str().to_string()),
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
            .canonical_services
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

pub(super) async fn transition_run_state(
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
            .canonical_services
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

pub(super) async fn get_run_row(
    _service: &WebIngestService,
    state: &AppState,
    run_id: Uuid,
) -> Result<WebIngestRunRow, ApiError> {
    ingest_repository::get_web_ingest_run_by_id(&state.persistence.postgres, run_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))
}

pub(super) async fn run_cancel_requested(
    service: &WebIngestService,
    state: &AppState,
    run_id: Uuid,
) -> Result<bool, ApiError> {
    Ok(service.get_run_row(state, run_id).await?.cancel_requested_at.is_some())
}

pub(super) async fn discover_outbound_links(
    _service: &WebIngestService,
    state: &AppState,
    library_id: Uuid,
    resource: &FetchedWebResource,
) -> Result<Vec<String>, ApiError> {
    let looks_like_html = resource.content_type.as_deref().map_or_else(
        || payload_looks_like_html_document(&String::from_utf8_lossy(&resource.payload_bytes)),
        |value| value.starts_with("text/html") || value == "application/xhtml+xml",
    );
    if !looks_like_html {
        return Ok(Vec::new());
    }

    let file_name =
        source_file_name_from_url(&resource.final_url, resource.content_type.as_deref());
    let extraction_plan = state
        .canonical_services
        .content
        .build_runtime_extraction_plan(
            state,
            library_id,
            &file_name,
            resource.content_type.as_deref(),
            &resource.payload_bytes,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(extraction_plan
        .source_map
        .get("outboundLinks")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default())
}
