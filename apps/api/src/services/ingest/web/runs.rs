use super::*;

impl WebIngestService {
    pub async fn create_run(
        &self,
        state: &AppState,
        command: CreateWebIngestRunCommand,
    ) -> Result<WebIngestRun, ApiError> {
        let normalized_seed_url = normalize_seed_url(&command.seed_url)
            .map_err(|error| ApiError::BadRequest(error.to_string()))?;
        let validated = validate_web_run_settings(
            &command.mode,
            command.boundary_policy.as_deref(),
            command.max_depth,
            command.max_pages,
        )
        .map_err(ApiError::BadRequest)?;
        validate_web_boundary_seed_host(&validated.boundary_policy, &normalized_seed_url)
            .map_err(ApiError::BadRequest)?;
        let crawl_filter = validate_web_ingest_url_filter(command.crawl_filter, "crawlFilter")
            .map_err(ApiError::BadRequest)?;
        let materialization_filter =
            validate_web_ingest_url_filter(command.materialization_filter, "materializationFilter")
                .map_err(ApiError::BadRequest)?;
        let crawl_allow_patterns_json =
            serde_json::to_value(&crawl_filter.allow_patterns).map_err(|_| ApiError::Internal)?;
        let crawl_block_patterns_json =
            serde_json::to_value(&crawl_filter.block_patterns).map_err(|_| ApiError::Internal)?;
        let materialization_allow_patterns_json =
            serde_json::to_value(&materialization_filter.allow_patterns)
                .map_err(|_| ApiError::Internal)?;
        let materialization_block_patterns_json =
            serde_json::to_value(&materialization_filter.block_patterns)
                .map_err(|_| ApiError::Internal)?;
        let source_identity = web_run_source_identity(
            &normalized_seed_url,
            &validated.mode,
            &validated.boundary_policy,
            validated.max_depth,
            validated.max_pages,
            WebRunFilterPatterns {
                crawl_allow: &crawl_allow_patterns_json,
                crawl_block: &crawl_block_patterns_json,
                materialization_allow: &materialization_allow_patterns_json,
                materialization_block: &materialization_block_patterns_json,
            },
        );

        let request = WebRunAdmissionRequest {
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            seed_url: command.seed_url,
            normalized_seed_url,
            mode: validated.mode,
            boundary_policy: validated.boundary_policy,
            max_depth: validated.max_depth,
            max_pages: validated.max_pages,
            crawl_allow_patterns: crawl_allow_patterns_json,
            crawl_block_patterns: crawl_block_patterns_json,
            materialization_allow_patterns: materialization_allow_patterns_json,
            materialization_block_patterns: materialization_block_patterns_json,
            requested_by_principal_id: command.requested_by_principal_id,
            request_surface: command.request_surface,
            idempotency_key: command.idempotency_key,
            source_identity: Some(source_identity),
        };
        let run_row = admission_repository::admit_web_run_with_failpoint(
            &state.persistence.postgres,
            &request,
            None,
        )
        .await
        .map_err(map_web_admission_error)?
        .into_bundle()
        .run;

        telemetry::web_run_event(
            "accepted",
            run_row.id,
            run_row.library_id,
            &run_row.mode,
            &run_row.run_state,
            &run_row.seed_url,
        );

        self.build_run(state, run_row).await
    }

    pub async fn list_runs(
        &self,
        state: &AppState,
        library_id: Uuid,
        limit: i64,
    ) -> Result<Vec<WebIngestRunSummary>, ApiError> {
        let rows =
            ingest_repository::list_web_ingest_runs(&state.persistence.postgres, library_id, limit)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        // One GROUP BY aggregation over all run ids instead of one
        // aggregate per run. Before this change, a library with N runs
        // issued N+1 queries on every /content/web-runs request, which
        // on reference-sized libraries pushed the endpoint past browser
        // timeout.
        let run_ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
        let counts_rows = ingest_repository::list_web_run_counts_by_run_ids(
            &state.persistence.postgres,
            &run_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let mut counts_by_run: std::collections::HashMap<Uuid, MappedCounts> =
            std::collections::HashMap::with_capacity(counts_rows.len());
        for row in counts_rows {
            counts_by_run.insert(row.run_id, map_web_run_counts_row(row.counts));
        }

        let summaries = rows
            .into_iter()
            .map(|row| {
                let counts = counts_by_run.remove(&row.id).unwrap_or_default();
                Ok(WebIngestRunSummary {
                    run_id: row.id,
                    library_id: row.library_id,
                    mode: row.mode,
                    boundary_policy: row.boundary_policy,
                    max_depth: row.max_depth,
                    max_pages: row.max_pages,
                    crawl_filter: parse_run_url_filter(
                        row.crawl_allow_patterns,
                        row.crawl_block_patterns,
                    )?,
                    materialization_filter: parse_run_url_filter(
                        row.materialization_allow_patterns,
                        row.materialization_block_patterns,
                    )?,
                    run_state: row.run_state,
                    seed_url: row.seed_url,
                    counts: counts.counts,
                    last_activity_at: counts.last_activity_at,
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        Ok(summaries)
    }

    pub async fn get_run(&self, state: &AppState, run_id: Uuid) -> Result<WebIngestRun, ApiError> {
        self.reconcile_terminal_materialized_pages(state, run_id).await?;
        let row = ingest_repository::get_web_ingest_run_by_id(&state.persistence.postgres, run_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))?;
        self.build_run(state, row).await
    }

    pub async fn list_pages(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<Vec<WebDiscoveredPage>, ApiError> {
        self.reconcile_terminal_materialized_pages(state, run_id).await?;
        let rows =
            ingest_repository::list_web_discovered_pages(&state.persistence.postgres, run_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_web_page_row).collect())
    }

    /// Reconciles a web page after its exact child content-mutation item has
    /// been atomically published. Callers must invoke this after successful or
    /// terminally failed child publication; retryable failures remain pending.
    pub async fn settle_materialized_page_for_mutation_item(
        &self,
        state: &AppState,
        mutation_item_id: Uuid,
    ) -> Result<ingest_repository::SettleWebIngestMutationItemOutcome, ApiError> {
        ingest_repository::settle_web_ingest_mutation_item(
            &state.persistence.postgres,
            mutation_item_id,
            Utc::now(),
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "settle materialized web page"))
    }

    async fn reconcile_terminal_materialized_pages(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<(), ApiError> {
        let mutation_item_ids = ingest_repository::list_terminal_materialized_web_page_item_ids(
            &state.persistence.postgres,
            run_id,
        )
        .await
        .map_err(|error| {
            ApiError::internal_with_log(error, "load terminal materialized web pages")
        })?;
        for mutation_item_id in mutation_item_ids {
            let _ =
                self.settle_materialized_page_for_mutation_item(state, mutation_item_id).await?;
        }
        Ok(())
    }

    pub async fn cancel_run(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<WebIngestRunReceipt, ApiError> {
        let existing = self.get_run(state, run_id).await?;
        if matches!(
            existing.run_state.as_str(),
            "completed" | "completed_partial" | "failed" | "canceled"
        ) {
            return Ok(map_web_run_receipt(existing));
        }
        let row = self.get_run_row(state, run_id).await?;
        if row.cancel_requested_at.is_none() {
            let _ = ingest_repository::update_web_ingest_run(
                &state.persistence.postgres,
                run_id,
                &UpdateWebIngestRun {
                    run_state: row.run_state.as_str(),
                    completed_at: row.completed_at,
                    failure_code: row.failure_code.as_deref(),
                    cancel_requested_at: Some(Utc::now()),
                },
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))?;
        }
        self.mark_pending_pages_canceled(state, run_id).await?;
        let refreshed = self.get_run(state, run_id).await?;
        telemetry::web_cancel_event(
            "cancel_requested",
            refreshed.run_id,
            refreshed.library_id,
            &refreshed.run_state,
            refreshed.cancel_requested_at,
            &refreshed.counts,
        );
        let completed_at = refreshed.completed_at;
        let updated = ingest_repository::update_web_ingest_run(
            &state.persistence.postgres,
            run_id,
            &UpdateWebIngestRun {
                run_state: refreshed.run_state.as_str(),
                completed_at,
                failure_code: refreshed.failure_code.as_deref(),
                cancel_requested_at: refreshed.cancel_requested_at,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))?;

        Ok(map_web_run_receipt(self.build_run(state, updated).await?))
    }
}
