use super::*;

pub(in super::super) async fn discover_recursive_scope(
    service: &WebIngestService,
    state: &AppState,
    run: &WebIngestRunRow,
    seed_candidate: WebDiscoveredPageRow,
) -> Result<Vec<WebDiscoveredPageRow>, ApiError> {
    let crawl_filter =
        parse_run_url_filter(run.crawl_allow_patterns.clone(), run.crawl_block_patterns.clone())?;
    let materialization_filter = parse_run_url_filter(
        run.materialization_allow_patterns.clone(),
        run.materialization_block_patterns.clone(),
    )?;
    let boundary_policy = parse_web_boundary_policy(&run.boundary_policy).map_err(|error| {
        error!(
            run_id = %run.id,
            boundary_policy = %run.boundary_policy,
            validation_error = %error,
            "web ingest run has invalid persisted boundary policy"
        );
        ApiError::Internal
    })?;

    if let Some(filter_exclusion) =
        classify_web_crawl_filter_exclusion(&seed_candidate.normalized_url, &crawl_filter)
    {
        let excluded_seed = ingest_repository::update_web_discovered_page(
            &state.persistence.postgres,
            seed_candidate.id,
            &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                final_url: Some(seed_candidate.normalized_url.as_str()),
                preferred_url: Some(seed_candidate.normalized_url.as_str()),
                host_classification: None,
                candidate_state: WebCandidateState::Excluded.as_str(),
                classification_reason: Some(WebClassificationReason::UrlFilter.as_str()),
                classification_detail: Some(filter_exclusion.detail.as_str()),
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", seed_candidate.id))?;
        telemetry::web_candidate_event(telemetry::WebCandidateEvent {
            event: "candidate_excluded_url_filter",
            run_id: run.id,
            candidate_id: excluded_seed.id,
            candidate_state: WebCandidateState::Excluded.as_str(),
            normalized_url: &excluded_seed.normalized_url,
            depth: excluded_seed.depth,
            classification_reason: Some(WebClassificationReason::UrlFilter.as_str()),
            host_classification: Some(filter_exclusion.detail.as_str()),
        });
        return Ok(Vec::new());
    }

    let mut frontier = VecDeque::from([seed_candidate]);
    let mut seen_urls = HashSet::from([run.normalized_seed_url.clone()]);
    let mut budgeted_urls = HashSet::from([run.normalized_seed_url.clone()]);
    let mut preferred_urls = HashSet::<String>::new();
    let mut eligible_pages = Vec::<WebDiscoveredPageRow>::new();
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

        let policy = CrawlPolicy {
            run,
            boundary_policy,
            crawl_filter: &crawl_filter,
            materialization_filter: &materialization_filter,
        };
        for (candidate, fetch_result) in fetched_wave {
            match process_fetched_candidate(
                service,
                state,
                &policy,
                candidate,
                fetch_result,
                CrawlAccumulator {
                    preferred_urls: &mut preferred_urls,
                    seen_urls: &mut seen_urls,
                    budgeted_urls: &mut budgeted_urls,
                    frontier: &mut frontier,
                },
            )
            .await?
            {
                FetchedCandidateOutcome::Stop => break 'outer,
                FetchedCandidateOutcome::Continue => {}
                FetchedCandidateOutcome::Eligible(candidate) => eligible_pages.push(*candidate),
            }
        }
    }

    Ok(eligible_pages)
}

enum FetchedCandidateOutcome {
    Stop,
    Continue,
    Eligible(Box<WebDiscoveredPageRow>),
}

/// Immutable crawl policy for a discovery pass: the run being executed and the
/// filters and boundary applied to every fetched candidate.
struct CrawlPolicy<'a> {
    run: &'a WebIngestRunRow,
    boundary_policy: crate::shared::web::ingest::WebBoundaryPolicy,
    crawl_filter: &'a WebIngestUrlFilter,
    materialization_filter: &'a WebIngestUrlFilter,
}

/// Mutable BFS accumulators shared across one discovery pass.
struct CrawlAccumulator<'a> {
    preferred_urls: &'a mut HashSet<String>,
    seen_urls: &'a mut HashSet<String>,
    budgeted_urls: &'a mut HashSet<String>,
    frontier: &'a mut VecDeque<WebDiscoveredPageRow>,
}

/// Immutable context for classifying and persisting outbound candidates.
struct OutboundContext<'a> {
    run: &'a WebIngestRunRow,
    boundary_policy: crate::shared::web::ingest::WebBoundaryPolicy,
    referrer: &'a WebDiscoveredPageRow,
    resource: &'a FetchedWebResource,
    crawl_filter: &'a WebIngestUrlFilter,
    materialization_filter: &'a WebIngestUrlFilter,
}

/// Mutable stores consulted while enqueueing outbound candidates.
struct OutboundAccumulator<'a> {
    seen_urls: &'a mut HashSet<String>,
    budgeted_urls: &'a mut HashSet<String>,
    frontier: &'a mut VecDeque<WebDiscoveredPageRow>,
}

/// The run, boundary and filters an outbound candidate is classified against.
struct OutboundScope<'a> {
    run: &'a WebIngestRunRow,
    boundary_policy: crate::shared::web::ingest::WebBoundaryPolicy,
    crawl_filter: &'a WebIngestUrlFilter,
    materialization_filter: &'a WebIngestUrlFilter,
}

async fn process_fetched_candidate(
    service: &WebIngestService,
    state: &AppState,
    policy: &CrawlPolicy<'_>,
    candidate: WebDiscoveredPageRow,
    fetch_result: Result<FetchedWebResource, WebRunFailure>,
    accumulator: CrawlAccumulator<'_>,
) -> Result<FetchedCandidateOutcome, ApiError> {
    let CrawlPolicy { run, boundary_policy, crawl_filter, materialization_filter } = *policy;
    let CrawlAccumulator { preferred_urls, seen_urls, budgeted_urls, frontier } = accumulator;
    if service.run_cancel_requested(state, run.id).await? {
        return Ok(FetchedCandidateOutcome::Stop);
    }
    let resource = match fetch_result {
        Ok(resource) => resource,
        Err(failure) => {
            telemetry::web_failure_event(telemetry::WebFailureEvent {
                event: "candidate_fetch_failed",
                run_id: run.id,
                candidate_id: Some(candidate.id),
                failure_code: &failure.failure_code,
                classification_reason: failure.candidate_reason.as_deref(),
                final_url: failure.final_url.as_deref(),
                content_type: failure.content_type.as_deref(),
                http_status: failure.http_status,
            });
            let _ = ingest_repository::update_web_discovered_page(
                &state.persistence.postgres,
                candidate.id,
                &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                    final_url: failure.final_url.as_deref(),
                    preferred_url: failure.final_url.as_deref(),
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
            return Ok(FetchedCandidateOutcome::Continue);
        }
    };
    if service.run_cancel_requested(state, run.id).await? {
        return Ok(FetchedCandidateOutcome::Stop);
    }

    let host_classification = crate::shared::web::url_identity::classify_host(
        &run.normalized_seed_url,
        &resource.final_url,
    )
    .unwrap_or(HostClassification::External);
    let resource_preferred_url = extract_html_preferred_url(
        &resource.payload_bytes,
        resource.content_type.as_deref(),
        &resource.final_url,
    )
    .unwrap_or_else(|| resource.final_url.clone());
    let host_classification_label = host_classification.as_str();

    if !boundary_policy.allows_host_classification(&host_classification) {
        let _ = ingest_repository::update_web_discovered_page(
            &state.persistence.postgres,
            candidate.id,
            &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                final_url: Some(resource.final_url.as_str()),
                preferred_url: Some(resource_preferred_url.as_str()),
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
        telemetry::web_candidate_event(telemetry::WebCandidateEvent {
            event: "candidate_excluded_boundary",
            run_id: run.id,
            candidate_id: candidate.id,
            candidate_state: WebCandidateState::Excluded.as_str(),
            normalized_url: &candidate.normalized_url,
            depth: candidate.depth,
            classification_reason: Some(WebClassificationReason::OutsideBoundaryPolicy.as_str()),
            host_classification: Some(host_classification_label),
        });
        return Ok(FetchedCandidateOutcome::Continue);
    }

    if let Some(filter_exclusion) =
        classify_web_crawl_filter_exclusion(&resource.final_url, crawl_filter)
    {
        let _ = ingest_repository::update_web_discovered_page(
            &state.persistence.postgres,
            candidate.id,
            &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                final_url: Some(resource.final_url.as_str()),
                preferred_url: Some(resource_preferred_url.as_str()),
                host_classification: Some(host_classification_label),
                candidate_state: WebCandidateState::Excluded.as_str(),
                classification_reason: Some(WebClassificationReason::UrlFilter.as_str()),
                classification_detail: Some(filter_exclusion.detail.as_str()),
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
                classification_detail = %filter_exclusion.detail,
                db_error = %error,
                "web ingest failed to persist url-filter-excluded candidate"
            );
            ApiError::Internal
        })?;
        telemetry::web_candidate_event(telemetry::WebCandidateEvent {
            event: "candidate_excluded_url_filter",
            run_id: run.id,
            candidate_id: candidate.id,
            candidate_state: WebCandidateState::Excluded.as_str(),
            normalized_url: &candidate.normalized_url,
            depth: candidate.depth,
            classification_reason: Some(WebClassificationReason::UrlFilter.as_str()),
            host_classification: Some(filter_exclusion.detail.as_str()),
        });
        return Ok(FetchedCandidateOutcome::Continue);
    }

    if preferred_urls.contains(&resource_preferred_url) {
        let _ = ingest_repository::update_web_discovered_page(
            &state.persistence.postgres,
            candidate.id,
            &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                final_url: Some(resource.final_url.as_str()),
                preferred_url: Some(resource_preferred_url.as_str()),
                host_classification: Some(host_classification_label),
                candidate_state: WebCandidateState::Duplicate.as_str(),
                classification_reason: Some(
                    WebClassificationReason::DuplicatePreferredUrl.as_str(),
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
                preferred_url = %resource_preferred_url,
                db_error = %error,
                "web ingest failed to persist duplicate preferred-URL candidate"
            );
            ApiError::Internal
        })?;
        telemetry::web_candidate_event(telemetry::WebCandidateEvent {
            event: "candidate_duplicate",
            run_id: run.id,
            candidate_id: candidate.id,
            candidate_state: WebCandidateState::Duplicate.as_str(),
            normalized_url: &candidate.normalized_url,
            depth: candidate.depth,
            classification_reason: Some(WebClassificationReason::DuplicatePreferredUrl.as_str()),
            host_classification: Some(host_classification_label),
        });
        return Ok(FetchedCandidateOutcome::Continue);
    }
    preferred_urls.insert(resource_preferred_url.clone());

    if let Some(filter_exclusion) =
        classify_web_materialization_filter_exclusion(&resource.final_url, materialization_filter)
    {
        let candidate_row = ingest_repository::update_web_discovered_page(
            &state.persistence.postgres,
            candidate.id,
            &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                final_url: Some(resource.final_url.as_str()),
                preferred_url: Some(resource_preferred_url.as_str()),
                host_classification: Some(host_classification_label),
                candidate_state: WebCandidateState::Excluded.as_str(),
                classification_reason: Some(WebClassificationReason::UrlFilter.as_str()),
                classification_detail: Some(filter_exclusion.detail.as_str()),
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
                classification_detail = %filter_exclusion.detail,
                db_error = %error,
                "web ingest failed to persist materialization-filter-excluded candidate"
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", candidate.id))?;
        telemetry::web_candidate_event(telemetry::WebCandidateEvent {
            event: "candidate_excluded_url_filter",
            run_id: run.id,
            candidate_id: candidate_row.id,
            candidate_state: WebCandidateState::Excluded.as_str(),
            normalized_url: &candidate_row.normalized_url,
            depth: candidate_row.depth,
            classification_reason: Some(WebClassificationReason::UrlFilter.as_str()),
            host_classification: Some(filter_exclusion.detail.as_str()),
        });
        discover_outbound_candidates(
            service,
            state,
            &OutboundContext {
                run,
                boundary_policy,
                referrer: &candidate_row,
                resource: &resource,
                crawl_filter,
                materialization_filter,
            },
            OutboundAccumulator { seen_urls, budgeted_urls, frontier },
        )
        .await?;
        return Ok(FetchedCandidateOutcome::Continue);
    }

    if is_direct_image_web_resource(&resource.final_url, resource.content_type.as_deref()) {
        let candidate_row = ingest_repository::update_web_discovered_page(
            &state.persistence.postgres,
            candidate.id,
            &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                final_url: Some(resource.final_url.as_str()),
                preferred_url: Some(resource_preferred_url.as_str()),
                host_classification: Some(host_classification_label),
                candidate_state: WebCandidateState::Excluded.as_str(),
                classification_reason: Some(WebClassificationReason::UnsupportedContent.as_str()),
                classification_detail: Some("embedded_web_resource:image"),
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
                content_type = ?resource.content_type,
                db_error = %error,
                "web ingest failed to persist direct image resource exclusion"
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", candidate.id))?;
        telemetry::web_candidate_event(telemetry::WebCandidateEvent {
            event: "candidate_excluded_embedded_resource",
            run_id: run.id,
            candidate_id: candidate_row.id,
            candidate_state: WebCandidateState::Excluded.as_str(),
            normalized_url: &candidate_row.normalized_url,
            depth: candidate_row.depth,
            classification_reason: Some(WebClassificationReason::UnsupportedContent.as_str()),
            host_classification: Some("embedded_web_resource:image"),
        });
        return Ok(FetchedCandidateOutcome::Continue);
    }

    let snapshot_storage_key =
        service.persist_resource_snapshot(state, run, &resource).await.map_err(|failure| {
            ApiError::BadRequest(
                failure.candidate_reason.clone().unwrap_or_else(|| failure.failure_code.clone()),
            )
        })?;
    let candidate_row = ingest_repository::update_web_discovered_page(
        &state.persistence.postgres,
        candidate.id,
        &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
            final_url: Some(resource.final_url.as_str()),
            preferred_url: Some(resource_preferred_url.as_str()),
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
    telemetry::web_candidate_event(telemetry::WebCandidateEvent {
        event: "candidate_eligible",
        run_id: run.id,
        candidate_id: candidate_row.id,
        candidate_state: &candidate_row.candidate_state,
        normalized_url: &candidate_row.normalized_url,
        depth: candidate_row.depth,
        classification_reason: candidate_row.classification_reason.as_deref(),
        host_classification: Some(candidate_row.host_classification.as_str()),
    });

    discover_outbound_candidates(
        service,
        state,
        &OutboundContext {
            run,
            boundary_policy,
            referrer: &candidate_row,
            resource: &resource,
            crawl_filter,
            materialization_filter,
        },
        OutboundAccumulator { seen_urls, budgeted_urls, frontier },
    )
    .await?;
    Ok(FetchedCandidateOutcome::Eligible(Box::new(candidate_row)))
}

pub(in super::super) async fn discover_outbound_links(
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
        .application_services
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
    let mut discovered = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for key in ["outboundLinks", "outboundResources"] {
        if let Some(values) =
            extraction_plan.source_map.get(key).and_then(serde_json::Value::as_array)
        {
            for value in values.iter().filter_map(serde_json::Value::as_str).map(str::trim) {
                if !value.is_empty() && seen.insert(value.to_string()) {
                    discovered.push(value.to_string());
                }
            }
        }
    }
    Ok(discovered)
}

struct OutboundCandidateClassification {
    state: WebCandidateState,
    reason: Option<&'static str>,
    detail: Option<String>,
    enqueue_for_fetch: bool,
}

impl OutboundCandidateClassification {
    fn persisted_fields(&self) -> (WebCandidateState, Option<&'static str>, Option<String>) {
        (self.state, self.reason, self.detail.clone())
    }
}

fn classify_outbound_candidate(
    scope: &OutboundScope<'_>,
    host: &HostClassification,
    url: &str,
    depth: i32,
    budgeted_urls: &mut HashSet<String>,
) -> OutboundCandidateClassification {
    let OutboundScope { run, boundary_policy, crawl_filter, materialization_filter } = *scope;
    if depth > run.max_depth {
        return excluded_outbound(WebClassificationReason::ExceededMaxDepth, None);
    }
    if !boundary_policy.allows_host_classification(host) {
        return excluded_outbound(WebClassificationReason::OutsideBoundaryPolicy, None);
    }
    if let Some(exclusion) = classify_web_crawl_filter_exclusion(url, crawl_filter) {
        return excluded_outbound(WebClassificationReason::UrlFilter, Some(exclusion.detail));
    }
    if i32::try_from(budgeted_urls.len()).unwrap_or(i32::MAX) >= run.max_pages {
        return excluded_outbound(WebClassificationReason::ExceededMaxPages, None);
    }
    budgeted_urls.insert(url.to_string());
    classify_materialization_outbound(url, materialization_filter)
}

fn classify_materialization_outbound(
    url: &str,
    materialization_filter: &WebIngestUrlFilter,
) -> OutboundCandidateClassification {
    classify_web_materialization_filter_exclusion(url, materialization_filter).map_or(
        OutboundCandidateClassification {
            state: WebCandidateState::Eligible,
            reason: Some(WebClassificationReason::SeedAccepted.as_str()),
            detail: None,
            enqueue_for_fetch: true,
        },
        |exclusion| OutboundCandidateClassification {
            state: WebCandidateState::Discovered,
            reason: Some(WebClassificationReason::UrlFilter.as_str()),
            detail: Some(exclusion.detail),
            enqueue_for_fetch: true,
        },
    )
}

fn excluded_outbound(
    reason: WebClassificationReason,
    detail: Option<String>,
) -> OutboundCandidateClassification {
    OutboundCandidateClassification {
        state: WebCandidateState::Excluded,
        reason: Some(reason.as_str()),
        detail,
        enqueue_for_fetch: false,
    }
}

async fn discover_outbound_candidates(
    service: &WebIngestService,
    state: &AppState,
    context: &OutboundContext<'_>,
    accumulator: OutboundAccumulator<'_>,
) -> Result<(), ApiError> {
    let OutboundContext {
        run,
        boundary_policy,
        referrer,
        resource,
        crawl_filter,
        materialization_filter,
    } = *context;
    let OutboundAccumulator { seen_urls, budgeted_urls, frontier } = accumulator;
    if referrer.depth >= run.max_depth {
        return Ok(());
    }

    for discovered_url in service.discover_outbound_links(state, run.library_id, resource).await? {
        if service.run_cancel_requested(state, run.id).await? {
            return Ok(());
        }
        let Ok(resolved_url) = crate::shared::web::url_identity::resolve_discovered_url(
            &resource.final_url,
            &discovered_url,
        ) else {
            continue;
        };
        let next_depth = referrer.depth.saturating_add(1);
        let discovered_host = crate::shared::web::url_identity::classify_host(
            &run.normalized_seed_url,
            &resolved_url,
        )
        .unwrap_or(HostClassification::External);

        if seen_urls.contains(&resolved_url) {
            continue;
        }
        seen_urls.insert(resolved_url.clone());

        let classification = classify_outbound_candidate(
            &OutboundScope { run, boundary_policy, crawl_filter, materialization_filter },
            &discovered_host,
            &resolved_url,
            next_depth,
            budgeted_urls,
        );
        let (candidate_state, classification_reason, classification_detail) =
            classification.persisted_fields();
        if service.run_cancel_requested(state, run.id).await? {
            return Ok(());
        }

        let discovered_row = ingest_repository::create_web_discovered_page(
            &state.persistence.postgres,
            &NewWebDiscoveredPage {
                id: Uuid::now_v7(),
                run_id: run.id,
                discovered_url: Some(discovered_url.as_str()),
                normalized_url: &resolved_url,
                final_url: None,
                preferred_url: Some(&resolved_url),
                depth: next_depth,
                referrer_candidate_id: Some(referrer.id),
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
                referrer_candidate_id = %referrer.id,
                normalized_url = %resolved_url,
                depth = next_depth,
                candidate_state = %candidate_state.as_str(),
                classification_reason = ?classification_reason,
                db_error = %error,
                "web ingest failed to persist discovered outbound candidate"
            );
            ApiError::Internal
        })?;
        telemetry::web_candidate_event(telemetry::WebCandidateEvent {
            event: "candidate_discovered",
            run_id: run.id,
            candidate_id: discovered_row.id,
            candidate_state: discovered_row.candidate_state.as_str(),
            normalized_url: &discovered_row.normalized_url,
            depth: discovered_row.depth,
            classification_reason: discovered_row.classification_reason.as_deref(),
            host_classification: Some(discovered_row.host_classification.as_str()),
        });

        if classification.enqueue_for_fetch {
            frontier.push_back(discovered_row);
        }
    }

    Ok(())
}
