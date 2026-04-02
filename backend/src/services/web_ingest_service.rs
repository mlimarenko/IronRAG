use std::{
    collections::{HashSet, VecDeque},
    time::Duration,
};

use chrono::{DateTime, Utc};
use reqwest::{Client, Url, header::CONTENT_TYPE};
use sha2::Digest;
use tracing::error;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ingest::{
        WebDiscoveredPage, WebIngestRun, WebIngestRunReceipt, WebIngestRunSummary, WebRunCounts,
    },
    infra::repositories::ingest_repository::{
        self, NewWebDiscoveredPage, NewWebIngestRun, UpdateWebIngestRun, WebDiscoveredPageRow,
        WebIngestRunRow, WebRunCountsRow,
    },
    interfaces::http::router_support::ApiError,
    services::{
        content_service::{
            AcceptMutationCommand, MaterializeWebCaptureCommand, UpdateMutationCommand,
        },
        ingest_service::AdmitIngestJobCommand,
        ops_service::{CreateAsyncOperationCommand, UpdateAsyncOperationCommand},
    },
    shared::{
        extraction::html_main_content::{
            extract_html_canonical_url, payload_looks_like_html_document,
        },
        telemetry,
        url_identity::{HostClassification, normalize_absolute_url, normalize_seed_url},
        web_ingest::{
            WebCandidateState, WebClassificationReason, WebIngestMode, WebRunFailureCode,
            WebRunState, derive_terminal_run_state, now_if_terminal, validate_web_run_settings,
        },
    },
};

#[derive(Debug, Clone)]
pub struct CreateWebIngestRunCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub seed_url: String,
    pub mode: String,
    pub boundary_policy: Option<String>,
    pub max_depth: Option<i32>,
    pub max_pages: Option<i32>,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WebIngestRuntimeSettings {
    pub request_timeout_seconds: u64,
    pub max_redirects: usize,
    pub user_agent: String,
}

impl Default for WebIngestRuntimeSettings {
    fn default() -> Self {
        Self {
            request_timeout_seconds: 20,
            max_redirects: 10,
            user_agent: "RustRAG-WebIngest/0.1".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct WebIngestService {
    runtime: WebIngestRuntimeSettings,
    http: Client,
}

#[derive(Debug, Clone)]
struct MaterializedWebPage {
    final_url: String,
    content_type: String,
    document_id: Uuid,
    revision_id: Uuid,
    mutation_item_id: Uuid,
    _job_id: Uuid,
}

#[derive(Debug, Clone)]
struct FetchedWebResource {
    final_url: String,
    content_type: Option<String>,
    http_status: i32,
    payload_bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
struct WebRunFailure {
    failure_code: String,
    candidate_reason: Option<String>,
    final_url: Option<String>,
    content_type: Option<String>,
    http_status: Option<i32>,
}

impl Default for WebIngestService {
    fn default() -> Self {
        Self::new(WebIngestRuntimeSettings::default())
    }
}

impl WebIngestService {
    #[must_use]
    pub fn new(runtime: WebIngestRuntimeSettings) -> Self {
        let http = match Client::builder()
            .timeout(Duration::from_secs(runtime.request_timeout_seconds))
            .redirect(reqwest::redirect::Policy::limited(runtime.max_redirects))
            .user_agent(runtime.user_agent.clone())
            .build()
        {
            Ok(client) => client,
            Err(_) => Client::new(),
        };
        Self { runtime, http }
    }

    #[must_use]
    pub fn runtime(&self) -> &WebIngestRuntimeSettings {
        &self.runtime
    }

    #[must_use]
    pub fn http_client(&self) -> &Client {
        &self.http
    }

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

        let mutation = state
            .canonical_services
            .content
            .accept_mutation(
                state,
                AcceptMutationCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    operation_kind: "web_capture".to_string(),
                    requested_by_principal_id: command.requested_by_principal_id,
                    request_surface: command.request_surface.clone(),
                    idempotency_key: command.idempotency_key.clone(),
                    source_identity: Some(normalized_seed_url.clone()),
                },
            )
            .await?;

        if let Some(existing) = ingest_repository::get_web_ingest_run_by_mutation_id(
            &state.persistence.postgres,
            mutation.id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        {
            return self.build_run(state, existing).await;
        }

        let run_id = Uuid::now_v7();
        let async_operation = state
            .canonical_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    operation_kind: "web_capture".to_string(),
                    surface_kind: command.request_surface.clone(),
                    requested_by_principal_id: command.requested_by_principal_id,
                    status: "accepted".to_string(),
                    subject_kind: "content_web_ingest_run".to_string(),
                    subject_id: Some(run_id),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;

        let row = match ingest_repository::create_web_ingest_run(
            &state.persistence.postgres,
            &NewWebIngestRun {
                id: run_id,
                mutation_id: mutation.id,
                async_operation_id: Some(async_operation.id),
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                mode: &validated.mode,
                seed_url: &command.seed_url,
                normalized_seed_url: &normalized_seed_url,
                boundary_policy: &validated.boundary_policy,
                max_depth: validated.max_depth,
                max_pages: validated.max_pages,
                run_state: WebRunState::Accepted.as_str(),
                requested_by_principal_id: command.requested_by_principal_id,
                requested_at: None,
                completed_at: None,
                failure_code: None,
                cancel_requested_at: None,
            },
        )
        .await
        {
            Ok(row) => row,
            Err(error) if is_web_run_mutation_uniqueness_violation(&error) => {
                ingest_repository::get_web_ingest_run_by_mutation_id(
                    &state.persistence.postgres,
                    mutation.id,
                )
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or(ApiError::Internal)?
            }
            Err(_) => return Err(ApiError::Internal),
        };

        let seed_candidate = ingest_repository::create_web_discovered_page(
            &state.persistence.postgres,
            &NewWebDiscoveredPage {
                id: Uuid::now_v7(),
                run_id: row.id,
                discovered_url: Some(command.seed_url.as_str()),
                normalized_url: &normalized_seed_url,
                final_url: None,
                canonical_url: Some(&normalized_seed_url),
                depth: 0,
                referrer_candidate_id: None,
                host_classification: HostClassification::SameHost.as_str(),
                candidate_state: WebCandidateState::Eligible.as_str(),
                classification_reason: Some(WebClassificationReason::SeedAccepted.as_str()),
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
        .map_err(|_| ApiError::Internal)?;

        let run_row = if validated.mode == WebIngestMode::SinglePage.as_str() {
            self.execute_single_page_run(state, row, seed_candidate).await?
        } else {
            self.enqueue_recursive_run(state, row).await?
        };

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
    ) -> Result<Vec<WebIngestRunSummary>, ApiError> {
        let rows = ingest_repository::list_web_ingest_runs(&state.persistence.postgres, library_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            summaries.push(self.build_run_summary(state, row).await?);
        }
        Ok(summaries)
    }

    pub async fn get_run(&self, state: &AppState, run_id: Uuid) -> Result<WebIngestRun, ApiError> {
        let row = ingest_repository::get_web_ingest_run_by_id(&state.persistence.postgres, run_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))?;
        self.build_run(state, row).await
    }

    pub async fn list_pages(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<Vec<WebDiscoveredPage>, ApiError> {
        let rows =
            ingest_repository::list_web_discovered_pages(&state.persistence.postgres, run_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_web_page_row).collect())
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
            .map_err(|_| ApiError::Internal)?
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
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))?;

        Ok(map_web_run_receipt(self.build_run(state, updated).await?))
    }

    pub async fn execute_recursive_discovery_job(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<(), ApiError> {
        let run = ingest_repository::get_web_ingest_run_by_id(&state.persistence.postgres, run_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))?;
        if matches!(
            run.run_state.as_str(),
            "completed" | "completed_partial" | "failed" | "canceled"
        ) {
            return Ok(());
        }
        if run.cancel_requested_at.is_some() {
            self.mark_pending_pages_canceled(state, run.id).await?;
            let _ = self.finalize_recursive_run_if_settled(state, run.id).await?;
            return Ok(());
        }
        let seed_candidate = ingest_repository::get_web_discovered_page_by_run_and_normalized_url(
            &state.persistence.postgres,
            run.id,
            &run.normalized_seed_url,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", run.id))?;
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
        let latest_run = match self.get_run_row(state, run_id).await {
            Ok(run) => run,
            Err(error) => {
                error!(%run_id, error = %error, "web ingest failed to refresh recursive run after discovery");
                return Err(error);
            }
        };
        let eligible_pages = self.load_eligible_pages_for_run(state, latest_run.id).await?;
        if latest_run.cancel_requested_at.is_some() {
            self.mark_pending_pages_canceled(state, latest_run.id).await?;
            let _ = self.finalize_recursive_run_if_settled(state, latest_run.id).await?;
            return Ok(());
        }

        if eligible_pages.is_empty() {
            let _ = self.finalize_recursive_run(state, latest_run).await?;
            return Ok(());
        }

        let processing_row = match self
            .transition_run_state(state, latest_run, WebRunState::Processing, "processing")
            .await
        {
            Ok(run) => run,
            Err(error) => {
                error!(%run_id, error = %error, "web ingest failed to transition recursive run into processing");
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
            self.queue_recursive_page_jobs(state, &processing_row, &eligible_pages).await
        {
            error!(run_id = %processing_row.id, error = %error, "web ingest failed to queue recursive page jobs");
            return Err(error);
        }
        let _ = self.finalize_recursive_run_if_settled(state, processing_row.id).await?;
        Ok(())
    }

    pub async fn execute_recursive_page_job(
        &self,
        state: &AppState,
        candidate_id: Uuid,
    ) -> Result<(), ApiError> {
        let candidate = ingest_repository::get_web_discovered_page_by_id(
            &state.persistence.postgres,
            candidate_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", candidate_id))?;
        let run = ingest_repository::get_web_ingest_run_by_id(
            &state.persistence.postgres,
            candidate.run_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", candidate.run_id))?;

        if matches!(
            candidate.candidate_state.as_str(),
            "processed" | "failed" | "canceled" | "blocked" | "excluded" | "duplicate"
        ) {
            let _ = self.finalize_recursive_run_if_settled(state, run.id).await?;
            return Ok(());
        }

        if run.cancel_requested_at.is_some() || run.run_state == WebRunState::Canceled.as_str() {
            let _ = self.cancel_page_candidate(state, &candidate).await?;
            let _ = self.finalize_recursive_run_if_settled(state, run.id).await?;
            return Ok(());
        }

        let processing_page = ingest_repository::update_web_discovered_page(
            &state.persistence.postgres,
            candidate.id,
            &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                final_url: candidate.final_url.as_deref(),
                canonical_url: candidate.canonical_url.as_deref(),
                host_classification: Some(candidate.host_classification.as_str()),
                candidate_state: WebCandidateState::Processing.as_str(),
                classification_reason: candidate.classification_reason.as_deref(),
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
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", candidate.id))?;

        let resource = match self.load_candidate_snapshot_resource(state, &processing_page).await {
            Ok(resource) => resource,
            Err(failure) => {
                let _ = self.mark_recursive_page_failed(state, &processing_page, failure).await?;
                return Ok(());
            }
        };

        match self
            .materialize_snapshot_resource(
                state,
                &run,
                &resource,
                processing_page.snapshot_storage_key.as_deref().unwrap_or_default(),
            )
            .await
        {
            Ok(materialized) => {
                let _ = ingest_repository::update_web_discovered_page(
                    &state.persistence.postgres,
                    processing_page.id,
                    &crate::infra::repositories::ingest_repository::UpdateWebDiscoveredPage {
                        final_url: Some(materialized.final_url.as_str()),
                        canonical_url: Some(materialized.final_url.as_str()),
                        host_classification: Some(processing_page.host_classification.as_str()),
                        candidate_state: WebCandidateState::Processed.as_str(),
                        classification_reason: processing_page.classification_reason.as_deref(),
                        content_type: Some(materialized.content_type.as_str()),
                        http_status: Some(resource.http_status),
                        snapshot_storage_key: processing_page.snapshot_storage_key.as_deref(),
                        updated_at: Some(Utc::now()),
                        document_id: Some(materialized.document_id),
                        result_revision_id: Some(materialized.revision_id),
                        mutation_item_id: Some(materialized.mutation_item_id),
                    },
                )
                .await
                .map_err(|_| ApiError::Internal)?;
            }
            Err(failure) => {
                let _ = self.mark_recursive_page_failed(state, &processing_page, failure).await?;
            }
        }

        let _ = self.finalize_recursive_run_if_settled(state, run.id).await?;
        Ok(())
    }

    pub async fn fail_recursive_discovery_job(
        &self,
        state: &AppState,
        run_id: Uuid,
        _failure_code: &str,
    ) -> Result<(), ApiError> {
        let failure_code = WebRunFailureCode::WebDiscoveryFailed.as_str();
        let row = ingest_repository::get_web_ingest_run_by_id(&state.persistence.postgres, run_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))?;
        if matches!(
            row.run_state.as_str(),
            "completed" | "completed_partial" | "failed" | "canceled"
        ) {
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
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", row.id))?;

        let _ = state
            .canonical_services
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
                .canonical_services
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
        let candidate = ingest_repository::get_web_discovered_page_by_id(
            &state.persistence.postgres,
            candidate_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", candidate_id))?;
        let failure = WebRunFailure {
            failure_code: failure_code.to_string(),
            candidate_reason: None,
            final_url: candidate.final_url.clone().or(candidate.canonical_url.clone()),
            content_type: candidate.content_type.clone(),
            http_status: candidate.http_status,
        };
        let _ = self.mark_recursive_page_failed(state, &candidate, failure).await?;
        Ok(())
    }

    async fn build_run(
        &self,
        state: &AppState,
        row: WebIngestRunRow,
    ) -> Result<WebIngestRun, ApiError> {
        let counts_row = ingest_repository::get_web_run_counts(&state.persistence.postgres, row.id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let counts = map_web_run_counts_row(counts_row);
        Ok(WebIngestRun {
            run_id: row.id,
            mutation_id: row.mutation_id,
            async_operation_id: row.async_operation_id,
            workspace_id: row.workspace_id,
            library_id: row.library_id,
            mode: row.mode,
            seed_url: row.seed_url,
            normalized_seed_url: row.normalized_seed_url,
            boundary_policy: row.boundary_policy,
            max_depth: row.max_depth,
            max_pages: row.max_pages,
            run_state: row.run_state,
            requested_by_principal_id: row.requested_by_principal_id,
            requested_at: row.requested_at,
            completed_at: row.completed_at,
            failure_code: row.failure_code,
            cancel_requested_at: row.cancel_requested_at,
            counts: counts.counts,
            last_activity_at: counts.last_activity_at,
        })
    }

    async fn build_run_summary(
        &self,
        state: &AppState,
        row: WebIngestRunRow,
    ) -> Result<WebIngestRunSummary, ApiError> {
        let counts_row = ingest_repository::get_web_run_counts(&state.persistence.postgres, row.id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let counts = map_web_run_counts_row(counts_row);
        Ok(WebIngestRunSummary {
            run_id: row.id,
            library_id: row.library_id,
            mode: row.mode,
            boundary_policy: row.boundary_policy,
            max_depth: row.max_depth,
            max_pages: row.max_pages,
            run_state: row.run_state,
            seed_url: row.seed_url,
            counts: counts.counts,
            last_activity_at: counts.last_activity_at,
        })
    }

    async fn enqueue_recursive_run(
        &self,
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
        Ok(row)
    }

    async fn discover_recursive_scope(
        &self,
        state: &AppState,
        run: &WebIngestRunRow,
        seed_candidate: WebDiscoveredPageRow,
    ) -> Result<Vec<WebDiscoveredPageRow>, ApiError> {
        let mut frontier = VecDeque::from([seed_candidate]);
        let mut seen_urls = HashSet::from([run.normalized_seed_url.clone()]);
        let mut budgeted_urls = HashSet::from([run.normalized_seed_url.clone()]);
        let mut canonical_urls = HashSet::<String>::new();
        let mut eligible_pages = Vec::<WebDiscoveredPageRow>::new();

        while let Some(candidate) = frontier.pop_front() {
            if self.run_cancel_requested(state, run.id).await? {
                break;
            }
            let resource = match self.fetch_web_resource(&candidate.normalized_url).await {
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
            if self.run_cancel_requested(state, run.id).await? {
                break;
            }

            let host_classification = crate::shared::url_identity::classify_host(
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

            let snapshot_storage_key =
                self.persist_resource_snapshot(state, run, &resource).await.map_err(|failure| {
                    ApiError::BadRequest(
                        failure.candidate_reason.clone().unwrap_or(failure.failure_code.clone()),
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
                    self.discover_outbound_links(state, run.library_id, &resource).await?
                {
                    if self.run_cancel_requested(state, run.id).await? {
                        return Ok(eligible_pages);
                    }
                    let resolved_url = match crate::shared::url_identity::resolve_discovered_url(
                        &resource.final_url,
                        &discovered_url,
                    ) {
                        Ok(url) => url,
                        Err(_) => continue,
                    };
                    let next_depth = candidate_row.depth.saturating_add(1);
                    let discovered_host = crate::shared::url_identity::classify_host(
                        &run.normalized_seed_url,
                        &resolved_url,
                    )
                    .unwrap_or(HostClassification::External);

                    if seen_urls.contains(&resolved_url) {
                        continue;
                    }
                    seen_urls.insert(resolved_url.clone());

                    let (candidate_state, classification_reason) = if next_depth > run.max_depth {
                        (
                            WebCandidateState::Excluded,
                            Some(WebClassificationReason::ExceededMaxDepth.as_str()),
                        )
                    } else if run.boundary_policy == "same_host"
                        && discovered_host != HostClassification::SameHost
                    {
                        (
                            WebCandidateState::Excluded,
                            Some(WebClassificationReason::OutsideBoundaryPolicy.as_str()),
                        )
                    } else if let Some(reason) = classify_confluence_system_page(&resolved_url) {
                        (WebCandidateState::Excluded, Some(reason))
                    } else if i32::try_from(budgeted_urls.len()).unwrap_or(i32::MAX)
                        >= run.max_pages
                    {
                        (
                            WebCandidateState::Excluded,
                            Some(WebClassificationReason::ExceededMaxPages.as_str()),
                        )
                    } else {
                        budgeted_urls.insert(resolved_url.clone());
                        (
                            WebCandidateState::Eligible,
                            Some(WebClassificationReason::SeedAccepted.as_str()),
                        )
                    };
                    if self.run_cancel_requested(state, run.id).await? {
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

        Ok(eligible_pages)
    }

    async fn queue_recursive_page_jobs(
        &self,
        state: &AppState,
        run: &WebIngestRunRow,
        pages: &[WebDiscoveredPageRow],
    ) -> Result<(), ApiError> {
        for page in pages {
            let cancel_requested = match self.run_cancel_requested(state, run.id).await {
                Ok(value) => value,
                Err(error) => {
                    error!(run_id = %run.id, candidate_id = %page.id, error = %error, "web ingest failed to refresh cancel state before queueing page");
                    return Err(error);
                }
            };
            if cancel_requested {
                self.mark_pending_pages_canceled(state, run.id).await?;
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

    async fn load_eligible_pages_for_run(
        &self,
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

    async fn finalize_recursive_run_if_settled(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<WebIngestRunRow, ApiError> {
        let row = ingest_repository::get_web_ingest_run_by_id(&state.persistence.postgres, run_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))?;
        if matches!(
            row.run_state.as_str(),
            "completed" | "completed_partial" | "failed" | "canceled"
        ) {
            return Ok(row);
        }
        let counts = map_web_run_counts_row(
            ingest_repository::get_web_run_counts(&state.persistence.postgres, row.id)
                .await
                .map_err(|_| ApiError::Internal)?,
        )
        .counts;
        if counts.queued > 0 || counts.processing > 0 {
            return Ok(row);
        }
        self.finalize_recursive_run(state, row).await
    }

    async fn mark_recursive_page_failed(
        &self,
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
        .map_err(|_| ApiError::Internal)?
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
        let _ = self.finalize_recursive_run_if_settled(state, updated.run_id).await?;
        Ok(updated)
    }

    async fn cancel_page_candidate(
        &self,
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
        .map_err(|_| ApiError::Internal)?
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

    async fn mark_pending_pages_canceled(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<(), ApiError> {
        let pages =
            ingest_repository::list_web_discovered_pages(&state.persistence.postgres, run_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        for page in pages {
            if matches!(page.candidate_state.as_str(), "discovered" | "eligible" | "queued") {
                let _ = self.cancel_page_candidate(state, &page).await?;
            }
        }
        Ok(())
    }

    async fn finalize_recursive_run(
        &self,
        state: &AppState,
        row: WebIngestRunRow,
    ) -> Result<WebIngestRunRow, ApiError> {
        let counts = map_web_run_counts_row(
            ingest_repository::get_web_run_counts(&state.persistence.postgres, row.id)
                .await
                .map_err(|_| ApiError::Internal)?,
        )
        .counts;
        let terminal_state = derive_terminal_run_state(&crate::shared::web_ingest::WebRunCounts {
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
        .map_err(|_| ApiError::Internal)?
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
                        failure_code: (terminal_state == WebRunState::Failed).then_some(
                            WebRunFailureCode::RecursiveCrawlFailed.as_str().to_string(),
                        ),
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
                        failure_code: (terminal_state == WebRunState::Failed).then_some(
                            WebRunFailureCode::RecursiveCrawlFailed.as_str().to_string(),
                        ),
                        conflict_code: None,
                    },
                )
                .await?;
        }

        Ok(completed_row)
    }

    async fn transition_run_state(
        &self,
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
        .map_err(|_| ApiError::Internal)?
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

    async fn get_run_row(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<WebIngestRunRow, ApiError> {
        ingest_repository::get_web_ingest_run_by_id(&state.persistence.postgres, run_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("web_ingest_run", run_id))
    }

    async fn run_cancel_requested(&self, state: &AppState, run_id: Uuid) -> Result<bool, ApiError> {
        Ok(self.get_run_row(state, run_id).await?.cancel_requested_at.is_some())
    }

    async fn discover_outbound_links(
        &self,
        state: &AppState,
        library_id: Uuid,
        resource: &FetchedWebResource,
    ) -> Result<Vec<String>, ApiError> {
        let looks_like_html = resource
            .content_type
            .as_deref()
            .map(|value| value.starts_with("text/html") || value == "application/xhtml+xml")
            .unwrap_or_else(|| {
                payload_looks_like_html_document(&String::from_utf8_lossy(&resource.payload_bytes))
            });
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
            .map_err(|_| ApiError::Internal)?;
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

    async fn execute_single_page_run(
        &self,
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
        .map_err(|_| ApiError::Internal)?
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
        .map_err(|_| ApiError::Internal)?;
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

        let resource = match self.fetch_web_resource(&processing_row.seed_url).await {
            Ok(resource) => resource,
            Err(failure) => {
                return self
                    .fail_single_page_run(state, processing_row, seed_candidate.id, failure)
                    .await;
            }
        };
        let snapshot_storage_key =
            match self.persist_resource_snapshot(state, &processing_row, &resource).await {
                Ok(storage_key) => storage_key,
                Err(failure) => {
                    return self
                        .fail_single_page_run(state, processing_row, seed_candidate.id, failure)
                        .await;
                }
            };
        let materialized = match self
            .materialize_snapshot_resource(state, &processing_row, &resource, &snapshot_storage_key)
            .await
        {
            Ok(page) => page,
            Err(failure) => {
                return self
                    .fail_single_page_run(state, processing_row, seed_candidate.id, failure)
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
        .map_err(|_| ApiError::Internal)?;
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
                .map_err(|_| ApiError::Internal)?,
        )
        .counts;
        let terminal_state = derive_terminal_run_state(&crate::shared::web_ingest::WebRunCounts {
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
        .map_err(|_| ApiError::Internal)?
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

    async fn fetch_web_resource(
        &self,
        seed_url: &str,
    ) -> Result<FetchedWebResource, WebRunFailure> {
        let response = self.http_client().get(seed_url).send().await.map_err(|error| {
            WebRunFailure::inaccessible(format!("failed to fetch seed url: {error}"))
        })?;
        let http_status = i32::from(response.status().as_u16());
        let final_url = normalize_absolute_url(response.url().as_str()).map_err(|error| {
            WebRunFailure::invalid_url(format!(
                "fetched resource resolved to invalid final url: {error}"
            ))
        })?;
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
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

    async fn persist_resource_snapshot(
        &self,
        state: &AppState,
        run: &WebIngestRunRow,
        resource: &FetchedWebResource,
    ) -> Result<String, WebRunFailure> {
        let checksum = format!("sha256:{:x}", sha2::Sha256::digest(&resource.payload_bytes));
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

    async fn load_candidate_snapshot_resource(
        &self,
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
                    candidate.final_url.clone().or(candidate.canonical_url.clone()),
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

    async fn materialize_snapshot_resource(
        &self,
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

        let checksum = format!("sha256:{:x}", sha2::Sha256::digest(&resource.payload_bytes));
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
            content_type: resolved_web_mime_type(
                resource.content_type.as_deref(),
                &extraction_plan,
            ),
            document_id: materialized.document.id,
            revision_id: materialized.revision.id,
            mutation_item_id: materialized.mutation_item.id,
            _job_id: materialized.job_id,
        })
    }

    async fn fail_single_page_run(
        &self,
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
        .map_err(|_| ApiError::Internal)?;

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
        .map_err(|_| ApiError::Internal)?
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
}

fn classify_confluence_system_page(url: &str) -> Option<&'static str> {
    let parsed = Url::parse(url).ok()?;
    let path = parsed.path().to_ascii_lowercase();
    let query = parsed.query().unwrap_or_default().to_ascii_lowercase();

    let is_system_path = matches!(
        path.as_str(),
        "/aboutconfluencepage.action"
            | "/collector/pages.action"
            | "/dashboard/configurerssfeed.action"
            | "/exportword"
            | "/forgotuserpassword.action"
            | "/login.action"
            | "/pages/diffpages.action"
            | "/pages/diffpagesbyversion.action"
            | "/pages/listundefinedpages.action"
            | "/pages/reorderpages.action"
            | "/pages/viewinfo.action"
            | "/pages/viewpageattachments.action"
            | "/pages/viewpreviousversions.action"
            | "/plugins/viewsource/viewpagesrc.action"
            | "/spacedirectory/view.action"
            | "/spaces/flyingpdf/pdfpageexport.action"
            | "/spaces/listattachmentsforspace.action"
            | "/spaces/listrssfeeds.action"
            | "/spaces/viewspacesummary.action"
    );
    let is_profile_page = path.starts_with("/display/~");
    let is_system_query =
        query.contains("os_destination=") || query.contains("permissionviolation=");

    (is_system_path || is_profile_page || is_system_query)
        .then_some(WebClassificationReason::SystemPage.as_str())
}

#[cfg(test)]
mod tests {
    use super::classify_confluence_system_page;

    #[test]
    fn classifies_confluence_system_pages() {
        assert_eq!(
            classify_confluence_system_page(
                "https://docs.example.test/pages/diffpagesbyversion.action?pageId=1&selectedPageVersions=1&selectedPageVersions=2",
            ),
            Some("system_page")
        );
        assert_eq!(
            classify_confluence_system_page(
                "https://docs.example.test/login.action?os_destination=%2Fdisplay%2FACA%2FAcme%2BConsultant%2BApp",
            ),
            Some("system_page")
        );
        assert_eq!(
            classify_confluence_system_page(
                "https://docs.example.test/display/ACA/Acme+Consultant+App"
            ),
            None
        );
    }
}

struct MappedCounts {
    counts: WebRunCounts,
    last_activity_at: Option<DateTime<Utc>>,
}

fn map_web_run_counts_row(row: WebRunCountsRow) -> MappedCounts {
    MappedCounts {
        counts: WebRunCounts {
            discovered: row.discovered,
            eligible: row.eligible,
            processed: row.processed,
            queued: row.queued,
            processing: row.processing,
            duplicates: row.duplicates,
            excluded: row.excluded,
            blocked: row.blocked,
            failed: row.failed,
            canceled: row.canceled,
        },
        last_activity_at: row.last_activity_at,
    }
}

fn map_web_run_receipt(run: WebIngestRun) -> WebIngestRunReceipt {
    WebIngestRunReceipt {
        run_id: run.run_id,
        library_id: run.library_id,
        mode: run.mode,
        run_state: run.run_state,
        async_operation_id: run.async_operation_id,
        counts: run.counts,
        failure_code: run.failure_code,
        cancel_requested_at: run.cancel_requested_at,
    }
}

fn map_web_page_row(row: WebDiscoveredPageRow) -> WebDiscoveredPage {
    WebDiscoveredPage {
        candidate_id: row.id,
        run_id: row.run_id,
        discovered_url: row.discovered_url,
        normalized_url: row.normalized_url,
        final_url: row.final_url,
        canonical_url: row.canonical_url,
        depth: row.depth,
        referrer_candidate_id: row.referrer_candidate_id,
        host_classification: row.host_classification,
        candidate_state: row.candidate_state,
        classification_reason: row.classification_reason,
        content_type: row.content_type,
        http_status: row.http_status,
        discovered_at: row.discovered_at,
        updated_at: row.updated_at,
        document_id: row.document_id,
        result_revision_id: row.result_revision_id,
        mutation_item_id: row.mutation_item_id,
    }
}

fn is_web_run_mutation_uniqueness_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database_error) if database_error.is_unique_violation() => {
            database_error.constraint() == Some("content_web_ingest_run_mutation_id_key")
        }
        _ => false,
    }
}

impl WebRunFailure {
    fn inaccessible(_message: String) -> Self {
        Self {
            failure_code: WebRunFailureCode::Inaccessible.as_str().to_string(),
            candidate_reason: Some(WebClassificationReason::Inaccessible.as_str().to_string()),
            final_url: None,
            content_type: None,
            http_status: None,
        }
    }

    fn inaccessible_with_response(
        _message: String,
        final_url: Option<String>,
        content_type: Option<String>,
        http_status: Option<i32>,
    ) -> Self {
        Self {
            failure_code: WebRunFailureCode::Inaccessible.as_str().to_string(),
            candidate_reason: Some(WebClassificationReason::Inaccessible.as_str().to_string()),
            final_url,
            content_type,
            http_status,
        }
    }

    fn invalid_url(_message: String) -> Self {
        Self {
            failure_code: WebRunFailureCode::InvalidUrl.as_str().to_string(),
            candidate_reason: Some(WebClassificationReason::InvalidUrl.as_str().to_string()),
            final_url: None,
            content_type: None,
            http_status: None,
        }
    }

    fn unsupported_content(
        _message: String,
        final_url: Option<String>,
        content_type: Option<String>,
        http_status: Option<i32>,
    ) -> Self {
        Self {
            failure_code: WebRunFailureCode::UnsupportedContent.as_str().to_string(),
            candidate_reason: Some(
                WebClassificationReason::UnsupportedContent.as_str().to_string(),
            ),
            final_url,
            content_type,
            http_status,
        }
    }

    fn internal(
        failure_code: &str,
        _message: String,
        final_url: Option<String>,
        content_type: Option<String>,
        http_status: Option<i32>,
    ) -> Self {
        Self {
            failure_code: failure_code.to_string(),
            candidate_reason: None,
            final_url,
            content_type,
            http_status,
        }
    }
}

fn extraction_title(source_map: &serde_json::Value) -> Option<String> {
    source_map
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn resolved_web_mime_type(
    content_type: Option<&str>,
    extraction_plan: &crate::shared::file_extract::FileExtractionPlan,
) -> String {
    content_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| match extraction_plan.extraction_kind.as_str() {
            "html_main_content" => "text/html".to_string(),
            "pdf_text" => "application/pdf".to_string(),
            "docx_text" => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    .to_string()
            }
            "pptx_text" => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                    .to_string()
            }
            _ => "text/plain".to_string(),
        })
}

fn source_file_name_from_url(final_url: &str, content_type: Option<&str>) -> String {
    let fallback = match content_type {
        Some(value) if value.starts_with("text/html") || value == "application/xhtml+xml" => {
            "index.html"
        }
        Some("application/pdf") => "download.pdf",
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document") => {
            "document.docx"
        }
        Some("application/vnd.openxmlformats-officedocument.presentationml.presentation") => {
            "slides.pptx"
        }
        _ => "download.bin",
    };
    reqwest::Url::parse(final_url)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|segments| segments.filter(|segment| !segment.is_empty()).next_back())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| fallback.to_string())
}

fn fallback_title_from_url(final_url: &str) -> Option<String> {
    reqwest::Url::parse(final_url).ok().and_then(|url| {
        let path_title = url
            .path_segments()
            .and_then(|segments| segments.filter(|segment| !segment.is_empty()).next_back())
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "index.html")
            .map(ToString::to_string);
        path_title.or_else(|| url.host_str().map(ToString::to_string))
    })
}
