#![allow(
    clippy::all,
    clippy::missing_const_for_fn,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::option_if_let_else,
    clippy::result_large_err,
    clippy::too_many_lines
)]

mod recursive;
mod single_page;

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{Client, Url};
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
        content::service::{
            AcceptMutationCommand, MaterializeWebCaptureCommand, UpdateMutationCommand,
        },
        ingest::service::AdmitIngestJobCommand,
        ops::service::{CreateAsyncOperationCommand, UpdateAsyncOperationCommand},
    },
    shared::{
        extraction::html_main_content::{
            extract_html_canonical_url, payload_looks_like_html_document,
        },
        telemetry,
        web::{
            ingest::{
                WebCandidateState, WebClassificationReason, WebIngestMode, WebRunFailureCode,
                WebRunState, derive_terminal_run_state, now_if_terminal, validate_web_run_settings,
            },
            url_identity::{HostClassification, normalize_seed_url},
        },
    },
};

/// Descriptor returned by `WebIngestService::refetch_document_source` after
/// successfully fetching a fresh copy of a web document. The caller (retry
/// path) uses these fields to build a new `RevisionAdmissionMetadata` with
/// the updated blob reference so the next ingest run operates on the new
/// bytes rather than the old captured snapshot.
#[derive(Debug, Clone)]
pub struct RefetchedWebDocumentSource {
    pub storage_key: String,
    pub checksum: String,
    pub byte_size: i64,
    pub mime_type: Option<String>,
    pub final_url: String,
}

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
            user_agent: "IronRAG-WebIngest/0.1".to_string(),
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

    /// Re-fetches a web-captured document's source URL and persists the fresh
    /// bytes as a new revision snapshot. Used by the reprocess/retry path so
    /// that "retry" for a web document means "go back to the site and pull
    /// the current version" instead of "re-parse the captured HTML we already
    /// stored". Diff-aware ingest then still kicks in downstream: unchanged
    /// chunks reuse prior extractions, changed chunks get a fresh run.
    ///
    /// Returns the new storage descriptor for the revision row. On transport
    /// failure, non-2xx status, or invalid URL, returns a `BadRequest` with
    /// a message the UI can surface per-document so the user knows which URLs
    /// could not be refreshed.
    pub async fn refetch_document_source(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        source_uri: &str,
    ) -> Result<RefetchedWebDocumentSource, ApiError> {
        let trimmed = source_uri.trim();
        if trimmed.is_empty() {
            return Err(ApiError::BadRequest(
                "web document has no source_uri to re-fetch".to_string(),
            ));
        }
        let response = self.http.get(trimmed).send().await.map_err(|error| {
            ApiError::BadRequest(format!("failed to re-fetch {trimmed}: {error}"))
        })?;
        let http_status = response.status();
        if !http_status.is_success() {
            return Err(ApiError::BadRequest(format!(
                "{trimmed} returned status {http_status} on re-fetch"
            )));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase);
        let final_url = response.url().as_str().to_string();
        let payload_bytes = response
            .bytes()
            .await
            .map_err(|error| {
                ApiError::BadRequest(format!("failed to read body of {trimmed}: {error}"))
            })?
            .to_vec();
        let byte_size =
            i64::try_from(payload_bytes.len()).unwrap_or(i64::MAX);
        let checksum = format!(
            "sha256:{}",
            hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&payload_bytes))
        );
        let storage_key = state
            .content_storage
            .persist_web_snapshot(workspace_id, library_id, &final_url, &checksum, &payload_bytes)
            .await
            .map_err(|error| {
                ApiError::internal_with_log(error, "persist refetched web snapshot")
            })?;
        Ok(RefetchedWebDocumentSource {
            storage_key,
            checksum,
            byte_size,
            mime_type: content_type,
            final_url,
        })
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            summaries.push(self.build_run_summary(state, row).await?);
        }
        Ok(summaries)
    }

    pub async fn get_run(&self, state: &AppState, run_id: Uuid) -> Result<WebIngestRun, ApiError> {
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
        let rows =
            ingest_repository::list_web_discovered_pages(&state.persistence.postgres, run_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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

    pub async fn execute_recursive_discovery_job(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<(), ApiError> {
        let run = ingest_repository::get_web_ingest_run_by_id(&state.persistence.postgres, run_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", candidate_id))?;
        let run = ingest_repository::get_web_ingest_run_by_id(
            &state.persistence.postgres,
            candidate.run_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("web_discovered_page", candidate_id))?;
        let failure = WebRunFailure {
            failure_code: failure_code.to_string(),
            candidate_reason: None,
            final_url: candidate.final_url.clone().or_else(|| candidate.canonical_url.clone()),
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
        recursive::enqueue_recursive_run(self, state, row).await
    }

    async fn discover_recursive_scope(
        &self,
        state: &AppState,
        run: &WebIngestRunRow,
        seed_candidate: WebDiscoveredPageRow,
    ) -> Result<Vec<WebDiscoveredPageRow>, ApiError> {
        recursive::discover_recursive_scope(self, state, run, seed_candidate).await
    }

    async fn queue_recursive_page_jobs(
        &self,
        state: &AppState,
        run: &WebIngestRunRow,
        pages: &[WebDiscoveredPageRow],
    ) -> Result<(), ApiError> {
        recursive::queue_recursive_page_jobs(self, state, run, pages).await
    }

    async fn load_eligible_pages_for_run(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<Vec<WebDiscoveredPageRow>, ApiError> {
        recursive::load_eligible_pages_for_run(self, state, run_id).await
    }

    async fn finalize_recursive_run_if_settled(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<WebIngestRunRow, ApiError> {
        recursive::finalize_recursive_run_if_settled(self, state, run_id).await
    }

    async fn mark_recursive_page_failed(
        &self,
        state: &AppState,
        page: &WebDiscoveredPageRow,
        failure: WebRunFailure,
    ) -> Result<WebDiscoveredPageRow, ApiError> {
        recursive::mark_recursive_page_failed(self, state, page, failure).await
    }

    async fn cancel_page_candidate(
        &self,
        state: &AppState,
        page: &WebDiscoveredPageRow,
    ) -> Result<WebDiscoveredPageRow, ApiError> {
        recursive::cancel_page_candidate(self, state, page).await
    }

    async fn mark_pending_pages_canceled(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<(), ApiError> {
        recursive::mark_pending_pages_canceled(self, state, run_id).await
    }

    async fn finalize_recursive_run(
        &self,
        state: &AppState,
        row: WebIngestRunRow,
    ) -> Result<WebIngestRunRow, ApiError> {
        recursive::finalize_recursive_run(self, state, row).await
    }

    async fn transition_run_state(
        &self,
        state: &AppState,
        row: WebIngestRunRow,
        run_state: WebRunState,
        async_status: &str,
    ) -> Result<WebIngestRunRow, ApiError> {
        recursive::transition_run_state(self, state, row, run_state, async_status).await
    }

    async fn get_run_row(
        &self,
        state: &AppState,
        run_id: Uuid,
    ) -> Result<WebIngestRunRow, ApiError> {
        recursive::get_run_row(self, state, run_id).await
    }

    async fn run_cancel_requested(&self, state: &AppState, run_id: Uuid) -> Result<bool, ApiError> {
        recursive::run_cancel_requested(self, state, run_id).await
    }

    async fn discover_outbound_links(
        &self,
        state: &AppState,
        library_id: Uuid,
        resource: &FetchedWebResource,
    ) -> Result<Vec<String>, ApiError> {
        recursive::discover_outbound_links(self, state, library_id, resource).await
    }

    async fn execute_single_page_run(
        &self,
        state: &AppState,
        row: WebIngestRunRow,
        seed_candidate: WebDiscoveredPageRow,
    ) -> Result<WebIngestRunRow, ApiError> {
        single_page::execute_single_page_run(self, state, row, seed_candidate).await
    }

    async fn fetch_web_resource(
        &self,
        seed_url: &str,
    ) -> Result<FetchedWebResource, WebRunFailure> {
        single_page::fetch_web_resource(self, seed_url).await
    }

    async fn persist_resource_snapshot(
        &self,
        state: &AppState,
        run: &WebIngestRunRow,
        resource: &FetchedWebResource,
    ) -> Result<String, WebRunFailure> {
        single_page::persist_resource_snapshot(self, state, run, resource).await
    }

    async fn load_candidate_snapshot_resource(
        &self,
        state: &AppState,
        candidate: &WebDiscoveredPageRow,
    ) -> Result<FetchedWebResource, WebRunFailure> {
        single_page::load_candidate_snapshot_resource(self, state, candidate).await
    }

    async fn materialize_snapshot_resource(
        &self,
        state: &AppState,
        run: &WebIngestRunRow,
        resource: &FetchedWebResource,
        storage_key: &str,
    ) -> Result<MaterializedWebPage, WebRunFailure> {
        single_page::materialize_snapshot_resource(self, state, run, resource, storage_key).await
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
    extraction_plan: &crate::shared::extraction::file_extract::FileExtractionPlan,
) -> String {
    content_type.map_or_else(
        || match extraction_plan.extraction_kind.as_str() {
            "html_main_content" => "text/html".to_string(),
            "pdf_text" => "application/pdf".to_string(),
            "docx_text" => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    .to_string()
            }
            "tabular_text" => {
                spreadsheet_mime_type(extraction_plan.source_format_metadata.source_format.as_str())
                    .to_string()
            }
            "pptx_text" => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                    .to_string()
            }
            _ => "text/plain".to_string(),
        },
        |content_type| content_type.trim().to_string(),
    )
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
        Some("text/csv") | Some("application/csv") => "table.csv",
        Some("text/tab-separated-values") => "table.tsv",
        Some("application/vnd.ms-excel") => "workbook.xls",
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet") => {
            "workbook.xlsx"
        }
        Some("application/vnd.ms-excel.sheet.binary.macroenabled.12") => "workbook.xlsb",
        Some("application/vnd.oasis.opendocument.spreadsheet") => "workbook.ods",
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

fn spreadsheet_mime_type(source_format: &str) -> &'static str {
    match source_format {
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "xls" => "application/vnd.ms-excel",
        "xlsb" => "application/vnd.ms-excel.sheet.binary.macroenabled.12",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        _ => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    }
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
