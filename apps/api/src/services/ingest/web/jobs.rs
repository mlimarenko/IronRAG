use super::*;

impl WebIngestService {
    pub async fn execute_recursive_discovery_job(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<(), ApiError> {
        let run = load_web_ingest_run(state, run_id).await?;
        if web_run_is_terminal(&run.run_state) {
            return Ok(());
        }
        if run.cancel_requested_at.is_some() {
            return self.cancel_and_settle_recursive_run(state, run.id).await;
        }
        let seed_candidate = load_run_seed_candidate(state, &run).await?;
        if run.mode == WebIngestMode::SinglePage.as_str() {
            let _ = self.execute_single_page_run(state, run, seed_candidate).await?;
            return Ok(());
        }
        let latest_run =
            Box::pin(self.resolve_recursive_discovery_phase(state, run_id, run, seed_candidate))
                .await?;
        let eligible_pages = self.load_eligible_pages_for_run(state, latest_run.id).await?;
        if latest_run.cancel_requested_at.is_some() {
            return self.cancel_and_settle_recursive_run(state, latest_run.id).await;
        }
        if eligible_pages.is_empty() {
            let _ = self.finalize_recursive_run_if_settled(state, latest_run.id).await?;
            return Ok(());
        }

        self.queue_recursive_processing_run(state, run_id, latest_run, &eligible_pages).await
    }

    /// Move a non-terminal refetch into discovery when needed, run scope
    /// discovery, and return the refreshed run row.
    async fn resolve_recursive_discovery_phase(
        &self,
        state: &AppState,
        run_id: Uuid,
        run: WebIngestRunRow,
        seed_candidate: WebDiscoveredPageRow,
    ) -> Result<WebIngestRunRow, ApiError> {
        if run.run_state == WebRunState::Processing.as_str() {
            return Ok(run);
        }
        let discovering_row = if run.run_state == WebRunState::Discovering.as_str() {
            run
        } else {
            self.transition_run_state(state, run, WebRunState::Discovering, "processing").await?
        };
        telemetry::web_run_event(
            "discovery_started",
            discovering_row.id,
            discovering_row.library_id,
            &discovering_row.mode,
            &discovering_row.run_state,
            &discovering_row.seed_url,
        );
        let _eligible_pages =
            self.discover_recursive_scope(state, &discovering_row, seed_candidate).await?;
        match self.get_run_row(state, run_id).await {
            Ok(run) => Ok(run),
            Err(error) => {
                log_recursive_run_refresh_failure(run_id, &error);
                Err(error)
            }
        }
    }

    /// Transition into processing and queue one worker job per eligible page.
    async fn queue_recursive_processing_run(
        &self,
        state: &AppState,
        run_id: Uuid,
        latest_run: WebIngestRunRow,
        eligible_pages: &[WebDiscoveredPageRow],
    ) -> Result<(), ApiError> {
        let processing_row = match self
            .transition_run_state(state, latest_run, WebRunState::Processing, "processing")
            .await
        {
            Ok(run) => run,
            Err(error) => {
                log_recursive_transition_failure(run_id, &error);
                return Err(error);
            }
        };
        telemetry::web_run_event(
            "processing_started",
            processing_row.id,
            processing_row.library_id,
            &processing_row.mode,
            &processing_row.run_state,
            &processing_row.seed_url,
        );
        if let Err(error) =
            self.queue_recursive_page_jobs(state, &processing_row, eligible_pages).await
        {
            log_recursive_queue_failure(processing_row.id, &error);
            return Err(error);
        }
        let _ = self.finalize_recursive_run_if_settled(state, processing_row.id).await?;
        Ok(())
    }

    async fn cancel_and_settle_recursive_run(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<(), ApiError> {
        self.mark_pending_pages_canceled(state, run_id).await?;
        let _ = self.finalize_recursive_run_if_settled(state, run_id).await?;
        Ok(())
    }

    pub async fn execute_recursive_page_job(
        &self,
        state: &AppState,
        candidate_id: Uuid,
    ) -> Result<(), ApiError> {
        let candidate = load_web_discovered_page(state, candidate_id).await?;
        let run = load_web_ingest_run(state, candidate.run_id).await?;

        if candidate.candidate_state == WebCandidateState::Materialized.as_str() {
            let mutation_item_id = materialized_mutation_item_id(&candidate)?;
            let _ =
                self.settle_materialized_page_for_mutation_item(state, mutation_item_id).await?;
            let _ = self.finalize_recursive_run_if_settled(state, run.id).await?;
            return Ok(());
        }

        if recursive_candidate_is_terminal(&candidate.candidate_state) {
            let _ = self.finalize_recursive_run_if_settled(state, run.id).await?;
            return Ok(());
        }

        if run.cancel_requested_at.is_some() || run.run_state == WebRunState::Canceled.as_str() {
            let _ = self.cancel_page_candidate(state, &candidate).await?;
            let _ = self.finalize_recursive_run_if_settled(state, run.id).await?;
            return Ok(());
        }

        let processing_page = mark_candidate_processing(state, &candidate).await?;
        let resource = match self.load_candidate_snapshot_resource(state, &processing_page).await {
            Ok(resource) => resource,
            Err(failure) => {
                let _ = self.mark_recursive_page_failed(state, &processing_page, failure).await?;
                return Ok(());
            }
        };

        self.materialize_recursive_page(state, &run, &processing_page, &resource).await?;

        let _ = self.finalize_recursive_run_if_settled(state, run.id).await?;
        Ok(())
    }

    /// Materialize a loaded snapshot resource and link the exact child mutation
    /// item. The page stays non-terminal until that item is published as
    /// applied/failed (or is already skipped by content dedup).
    async fn materialize_recursive_page(
        &self,
        state: &AppState,
        run: &WebIngestRunRow,
        processing_page: &WebDiscoveredPageRow,
        resource: &FetchedWebResource,
    ) -> Result<(), ApiError> {
        match self
            .materialize_snapshot_resource(
                state,
                run,
                resource,
                processing_page.snapshot_storage_key.as_deref().unwrap_or_default(),
            )
            .await
        {
            Ok(materialized) => {
                let classification_reason = if materialized.is_duplicate() {
                    Some(WebClassificationReason::DuplicateContent.as_str())
                } else {
                    processing_page.classification_reason.as_deref()
                };
                let materialized_page = ingest_repository::update_web_discovered_page(
                    &state.persistence.postgres,
                    processing_page.id,
                    &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                        final_url: Some(materialized.final_url()),
                        preferred_url: Some(materialized.final_url()),
                        host_classification: Some(processing_page.host_classification.as_str()),
                        candidate_state: WebCandidateState::Materialized.as_str(),
                        classification_reason,
                        classification_detail: processing_page.classification_detail.as_deref(),
                        content_type: Some(materialized.content_type()),
                        http_status: Some(resource.http_status),
                        snapshot_storage_key: processing_page.snapshot_storage_key.as_deref(),
                        updated_at: Some(Utc::now()),
                        document_id: Some(materialized.document_id()),
                        result_revision_id: materialized.revision_id(),
                        mutation_item_id: Some(materialized.mutation_item_id()),
                    },
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| {
                    ApiError::resource_not_found("web_discovered_page", processing_page.id)
                })?;
                telemetry::web_candidate_event(telemetry::WebCandidateEvent {
                    event: if materialized.is_duplicate() {
                        "candidate_materialized_duplicate"
                    } else {
                        "candidate_materialized"
                    },
                    run_id: run.id,
                    candidate_id: materialized_page.id,
                    candidate_state: WebCandidateState::Materialized.as_str(),
                    normalized_url: materialized.final_url(),
                    depth: materialized_page.depth,
                    classification_reason,
                    host_classification: Some(materialized_page.host_classification.as_str()),
                });
                let _ = self
                    .settle_materialized_page_for_mutation_item(
                        state,
                        materialized.mutation_item_id(),
                    )
                    .await?;
                Ok(())
            }
            Err(failure) => {
                let _ = self.mark_recursive_page_failed(state, processing_page, failure).await?;
                Ok(())
            }
        }
    }

    pub async fn fail_recursive_discovery_job(
        &self,
        state: &AppState,
        run_id: Uuid,
        _failure_code: &str,
    ) -> Result<(), ApiError> {
        let failure_code = WebRunFailureCode::WebDiscoveryFailed.as_str();
        let row = load_web_ingest_run(state, run_id).await?;
        if web_run_is_terminal(&row.run_state) {
            return Ok(());
        }
        let completed_at = Some(Utc::now());
        let failed_row = ingest_repository::update_web_ingest_run(
            &state.persistence.postgres,
            row.id,
            &UpdateWebIngestRun {
                run_state: WebRunState::Failed.as_str(),
                completed_at,
                failure_code: Some(failure_code),
                cancel_requested_at: row.cancel_requested_at,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", row.id))?;

        let _ = state
            .application_services
            .content
            .update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id: failed_row.mutation_id,
                    mutation_state: "failed".to_string(),
                    completed_at,
                    failure_code: Some(failure_code.to_string()),
                    conflict_code: None,
                },
            )
            .await?;

        if let Some(async_operation_id) = failed_row.async_operation_id {
            let _ = state
                .application_services
                .ops
                .update_async_operation(
                    state,
                    UpdateAsyncOperationCommand {
                        operation_id: async_operation_id,
                        status: "failed".to_string(),
                        completed_at,
                        failure_code: Some(failure_code.to_string()),
                    },
                )
                .await?;
        }

        Ok(())
    }

    pub async fn fail_recursive_page_job(
        &self,
        state: &AppState,
        candidate_id: Uuid,
        failure_code: &str,
    ) -> Result<(), ApiError> {
        let candidate = load_web_discovered_page(state, candidate_id).await?;
        let failure = WebRunFailure {
            failure_code: failure_code.to_string(),
            candidate_reason: None,
            final_url: candidate.final_url.clone().or_else(|| candidate.preferred_url.clone()),
            content_type: candidate.content_type.clone(),
            http_status: candidate.http_status,
        };
        let _ = self.mark_recursive_page_failed(state, &candidate, failure).await?;
        Ok(())
    }
}

async fn load_web_ingest_run(state: &AppState, run_id: Uuid) -> Result<WebIngestRunRow, ApiError> {
    ingest_repository::get_web_ingest_run_by_id(&state.persistence.postgres, run_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))
}

async fn load_web_discovered_page(
    state: &AppState,
    candidate_id: Uuid,
) -> Result<WebDiscoveredPageRow, ApiError> {
    ingest_repository::get_web_discovered_page_by_id(&state.persistence.postgres, candidate_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", candidate_id))
}

async fn load_run_seed_candidate(
    state: &AppState,
    run: &WebIngestRunRow,
) -> Result<WebDiscoveredPageRow, ApiError> {
    ingest_repository::get_web_discovered_page_by_run_and_normalized_url(
        &state.persistence.postgres,
        run.id,
        &run.normalized_seed_url,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", run.id))
}

fn web_run_is_terminal(run_state: &str) -> bool {
    matches!(run_state, "completed" | "completed_partial" | "failed" | "canceled")
}

fn recursive_candidate_is_terminal(candidate_state: &str) -> bool {
    matches!(
        candidate_state,
        "processed" | "failed" | "canceled" | "blocked" | "excluded" | "duplicate"
    )
}

fn materialized_mutation_item_id(candidate: &WebDiscoveredPageRow) -> Result<Uuid, ApiError> {
    candidate.mutation_item_id.ok_or_else(|| {
        ApiError::internal_with_log(
            anyhow::anyhow!("materialized web page is missing mutation_item_id"),
            "settle materialized recursive-page retry",
        )
    })
}

async fn mark_candidate_processing(
    state: &AppState,
    candidate: &WebDiscoveredPageRow,
) -> Result<WebDiscoveredPageRow, ApiError> {
    ingest_repository::update_web_discovered_page(
        &state.persistence.postgres,
        candidate.id,
        &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
            final_url: candidate.final_url.as_deref(),
            preferred_url: candidate.preferred_url.as_deref(),
            host_classification: Some(candidate.host_classification.as_str()),
            candidate_state: WebCandidateState::Processing.as_str(),
            classification_reason: candidate.classification_reason.as_deref(),
            classification_detail: candidate.classification_detail.as_deref(),
            content_type: candidate.content_type.as_deref(),
            http_status: candidate.http_status,
            snapshot_storage_key: candidate.snapshot_storage_key.as_deref(),
            updated_at: Some(Utc::now()),
            document_id: None,
            result_revision_id: None,
            mutation_item_id: None,
        },
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", candidate.id))
}

fn log_recursive_run_refresh_failure(run_id: Uuid, error: &ApiError) {
    error!(%run_id, error = %error, "web ingest failed to refresh recursive run after discovery");
}

fn log_recursive_transition_failure(run_id: Uuid, error: &ApiError) {
    error!(%run_id, error = %error, "web ingest failed to transition recursive run into processing");
}

fn log_recursive_queue_failure(run_id: Uuid, error: &ApiError) {
    error!(run_id = %run_id, error = %error, "web ingest failed to queue recursive page jobs");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recursive_run_stops_only_after_a_terminal_outcome() {
        for (state, terminal) in [
            (WebRunState::Accepted, false),
            (WebRunState::Discovering, false),
            (WebRunState::Processing, false),
            (WebRunState::Completed, true),
            (WebRunState::CompletedPartial, true),
            (WebRunState::Failed, true),
            (WebRunState::Canceled, true),
        ] {
            assert_eq!(web_run_is_terminal(state.as_str()), terminal, "{state:?}");
        }
        assert!(!web_run_is_terminal("unknown"));
    }

    #[test]
    fn recursive_candidate_materialization_still_requires_settlement() {
        for (state, terminal) in [
            (WebCandidateState::Discovered, false),
            (WebCandidateState::Eligible, false),
            (WebCandidateState::Queued, false),
            (WebCandidateState::Processing, false),
            (WebCandidateState::Materialized, false),
            (WebCandidateState::Processed, true),
            (WebCandidateState::Failed, true),
            (WebCandidateState::Canceled, true),
            (WebCandidateState::Blocked, true),
            (WebCandidateState::Excluded, true),
            (WebCandidateState::Duplicate, true),
        ] {
            assert_eq!(recursive_candidate_is_terminal(state.as_str()), terminal, "{state:?}");
        }
        assert!(!recursive_candidate_is_terminal("unknown"));
    }
}
