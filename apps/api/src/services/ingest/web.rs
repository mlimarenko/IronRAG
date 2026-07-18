mod recursive;
mod single_page;

use std::time::Duration;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use tracing::error;
use uuid::Uuid;

#[cfg(feature = "test-support")]
use crate::shared::outbound_http::get_loopback_test_http_following_redirects;

use crate::{
    app::state::AppState,
    domains::ingest::{
        WebDiscoveredPage, WebIngestRun, WebIngestRunReceipt, WebIngestRunSummary, WebRunCounts,
    },
    infra::repositories::{
        admission_repository::{self, AdmissionError, WebRunAdmissionRequest},
        ingest_repository::{
            self, UpdateWebIngestRun, WebDiscoveredPageRow, WebIngestRunRow, WebRunCountsRow,
        },
    },
    interfaces::http::router_support::ApiError,
    services::{
        content::service::{MaterializeWebCaptureCommand, UpdateMutationCommand},
        ops::service::UpdateAsyncOperationCommand,
    },
    shared::{
        extraction::html_main_content::{
            extract_html_canonical_url, payload_looks_like_html_document,
        },
        outbound_http::{
            PublicHttpUrlError, get_public_http_following_redirects, read_response_bytes_with_limit,
        },
        telemetry,
        web::{
            ingest::{
                WebCandidateState, WebClassificationReason, WebIngestMode, WebIngestPattern,
                WebIngestUrlFilter, WebRunFailureCode, WebRunState, derive_terminal_run_state,
                now_if_terminal, validate_web_boundary_seed_host, validate_web_ingest_url_filter,
                validate_web_run_settings,
            },
            url_identity::{HostClassification, normalize_seed_url},
        },
    },
};

const MAX_WEB_FETCH_BODY_BYTES: u64 = 50 * 1024 * 1024;

fn map_web_admission_error(error: AdmissionError) -> ApiError {
    match error {
        AdmissionError::IdempotencyConflict { .. } => ApiError::Conflict(
            "idempotency key was already used for a different web ingest request".to_string(),
        ),
        other => ApiError::internal_with_log(other, "web ingest admission failed"),
    }
}

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
    pub crawl_filter: WebIngestUrlFilter,
    pub materialization_filter: WebIngestUrlFilter,
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
    fetch_policy: WebFetchPolicy,
}

#[derive(Debug, Clone, Copy, Default)]
enum WebFetchPolicy {
    #[default]
    PublicOnly,
    #[cfg(feature = "test-support")]
    LoopbackTestOnly,
}

/// Result of `materialize_snapshot_resource`. Mirrors
/// `MaterializedWebCapture` — a fetched page can either be `Ingested`
/// (fresh content, normal pipeline) or collapsed under
/// `DuplicateContent` (body already present in the library under some
/// other URL variant, no new `content_document` created). Both variants are
/// linked through their exact mutation item before terminal page settlement.
#[derive(Debug, Clone)]
enum MaterializedWebPage {
    Ingested {
        final_url: String,
        content_type: String,
        document_id: Uuid,
        revision_id: Uuid,
        mutation_item_id: Uuid,
        _job_id: Uuid,
    },
    DuplicateContent {
        final_url: String,
        content_type: String,
        existing_document_id: Uuid,
        mutation_item_id: Uuid,
    },
}

impl MaterializedWebPage {
    fn final_url(&self) -> &str {
        match self {
            Self::Ingested { final_url, .. } | Self::DuplicateContent { final_url, .. } => {
                final_url
            }
        }
    }

    fn content_type(&self) -> &str {
        match self {
            Self::Ingested { content_type, .. } | Self::DuplicateContent { content_type, .. } => {
                content_type
            }
        }
    }

    const fn document_id(&self) -> Uuid {
        match self {
            Self::Ingested { document_id, .. } => *document_id,
            Self::DuplicateContent { existing_document_id, .. } => *existing_document_id,
        }
    }

    const fn revision_id(&self) -> Option<Uuid> {
        match self {
            Self::Ingested { revision_id, .. } => Some(*revision_id),
            Self::DuplicateContent { .. } => None,
        }
    }

    const fn mutation_item_id(&self) -> Uuid {
        match self {
            Self::Ingested { mutation_item_id, .. }
            | Self::DuplicateContent { mutation_item_id, .. } => *mutation_item_id,
        }
    }

    const fn is_duplicate(&self) -> bool {
        matches!(self, Self::DuplicateContent { .. })
    }
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
    pub const fn new(runtime: WebIngestRuntimeSettings) -> Self {
        Self { runtime, fetch_policy: WebFetchPolicy::PublicOnly }
    }

    /// Explicit integration-test seam. Normal constructors and every
    /// production entry point remain public-network-only, even in an
    /// `--all-features` build.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    #[must_use]
    pub fn with_loopback_test_transport(mut self) -> Self {
        self.fetch_policy = WebFetchPolicy::LoopbackTestOnly;
        self
    }

    #[must_use]
    pub const fn runtime(&self) -> &WebIngestRuntimeSettings {
        &self.runtime
    }

    async fn fetch_http_response(
        &self,
        initial_url: &str,
    ) -> Result<reqwest::Response, PublicHttpUrlError> {
        let timeout = Duration::from_secs(self.runtime.request_timeout_seconds);
        let connect_timeout = Duration::from_secs(self.runtime.request_timeout_seconds.min(10));
        match self.fetch_policy {
            WebFetchPolicy::PublicOnly => {
                get_public_http_following_redirects(
                    initial_url,
                    true,
                    self.runtime.max_redirects,
                    timeout,
                    connect_timeout,
                    Some(&self.runtime.user_agent),
                )
                .await
            }
            #[cfg(feature = "test-support")]
            WebFetchPolicy::LoopbackTestOnly => {
                get_loopback_test_http_following_redirects(
                    initial_url,
                    self.runtime.max_redirects,
                    timeout,
                    connect_timeout,
                    Some(&self.runtime.user_agent),
                )
                .await
            }
        }
    }

    /// Re-fetches a web-captured document's source URL and persists the fresh
    /// bytes as a new revision snapshot. Used by the reprocess/retry path so
    /// that "retry" for a web document means "go back to the site and pull
    /// the current version" instead of "re-parse the captured HTML we already
    /// stored". Diff-aware ingest then still kicks in downstream: unchanged
    /// chunks reuse prior extractions, changed chunks get a fresh run.
    ///
    /// Returns the new storage descriptor for the revision row. Invalid or
    /// policy-rejected sources and permanent response errors are reported as
    /// `BadRequest`; transient transport and upstream failures are reported as
    /// typed service-unavailable errors so callers can retry safely.
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
        let response = self.fetch_http_response(trimmed).await.map_err(|error| {
            if error.is_terminal_policy_rejection() {
                ApiError::BadRequest(format!("web source URL is not allowed: {error}"))
            } else {
                ApiError::service_unavailable(
                    "web source is temporarily unavailable",
                    "web_source_unavailable",
                )
            }
        })?;
        let http_status = response.status();
        if !http_status.is_success() {
            if http_status.is_server_error() {
                return Err(ApiError::service_unavailable(
                    "web source is temporarily unavailable",
                    "web_source_unavailable",
                ));
            }
            return Err(ApiError::BadRequest(format!(
                "web source returned status {http_status} on re-fetch"
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
        let payload_bytes = read_response_bytes_with_limit(response, MAX_WEB_FETCH_BODY_BYTES)
            .await
            .map_err(|error| match error {
                PublicHttpUrlError::ContentLengthExceeded { .. }
                | PublicHttpUrlError::BodySizeExceeded { .. } => {
                    ApiError::BadRequest(error.to_string())
                }
                _ => ApiError::service_unavailable(
                    "web source response could not be read",
                    "web_source_unavailable",
                ),
            })?;
        let byte_size = i64::try_from(payload_bytes.len()).unwrap_or(i64::MAX);
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
            &crawl_allow_patterns_json,
            &crawl_block_patterns_json,
            &materialization_allow_patterns_json,
            &materialization_block_patterns_json,
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
        if run.mode == WebIngestMode::SinglePage.as_str() {
            let _ = self.execute_single_page_run(state, run, seed_candidate).await?;
            return Ok(());
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
            let _ = self.finalize_recursive_run_if_settled(state, latest_run.id).await?;
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

        if candidate.candidate_state == WebCandidateState::Materialized.as_str() {
            let mutation_item_id = candidate.mutation_item_id.ok_or_else(|| {
                ApiError::internal_with_log(
                    anyhow::anyhow!("materialized web page is missing mutation_item_id"),
                    "settle materialized recursive-page retry",
                )
            })?;
            let _ =
                self.settle_materialized_page_for_mutation_item(state, mutation_item_id).await?;
            let _ = self.finalize_recursive_run_if_settled(state, run.id).await?;
            return Ok(());
        }

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
                // Materialization only links the exact child mutation item.
                // The page remains non-terminal until that item is published
                // as applied/failed (or is already skipped by content dedup).
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
                        canonical_url: Some(materialized.final_url()),
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
                telemetry::web_candidate_event(
                    if materialized.is_duplicate() {
                        "candidate_materialized_duplicate"
                    } else {
                        "candidate_materialized"
                    },
                    run.id,
                    materialized_page.id,
                    WebCandidateState::Materialized.as_str(),
                    materialized.final_url(),
                    materialized_page.depth,
                    classification_reason,
                    Some(materialized_page.host_classification.as_str()),
                );
                let _ = self
                    .settle_materialized_page_for_mutation_item(
                        state,
                        materialized.mutation_item_id(),
                    )
                    .await?;
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
            crawl_filter: parse_run_url_filter(row.crawl_allow_patterns, row.crawl_block_patterns)?,
            materialization_filter: parse_run_url_filter(
                row.materialization_allow_patterns,
                row.materialization_block_patterns,
            )?,
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

#[derive(Default)]
struct MappedCounts {
    counts: WebRunCounts,
    last_activity_at: Option<DateTime<Utc>>,
}

const fn map_web_run_counts_row(row: WebRunCountsRow) -> MappedCounts {
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

fn web_run_source_identity(
    normalized_seed_url: &str,
    mode: &str,
    boundary_policy: &str,
    max_depth: i32,
    max_pages: i32,
    crawl_allow_patterns_json: &serde_json::Value,
    crawl_block_patterns_json: &serde_json::Value,
    materialization_allow_patterns_json: &serde_json::Value,
    materialization_block_patterns_json: &serde_json::Value,
) -> String {
    let payload = serde_json::json!({
        "normalizedSeedUrl": normalized_seed_url,
        "mode": mode,
        "boundaryPolicy": boundary_policy,
        "maxDepth": max_depth,
        "maxPages": max_pages,
        "crawlFilter": {
            "allowPatterns": crawl_allow_patterns_json,
            "blockPatterns": crawl_block_patterns_json,
        },
        "materializationFilter": {
            "allowPatterns": materialization_allow_patterns_json,
            "blockPatterns": materialization_block_patterns_json,
        },
    });
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    format!("web_capture:sha256:{}", hex::encode(Sha256::digest(bytes)))
}

pub(super) fn parse_run_filter_patterns(
    value: serde_json::Value,
) -> Result<Vec<WebIngestPattern>, ApiError> {
    serde_json::from_value(value).map_err(|_| ApiError::Internal)
}

pub(super) fn parse_run_url_filter(
    allow_patterns: serde_json::Value,
    block_patterns: serde_json::Value,
) -> Result<WebIngestUrlFilter, ApiError> {
    Ok(WebIngestUrlFilter {
        allow_patterns: parse_run_filter_patterns(allow_patterns)?,
        block_patterns: parse_run_filter_patterns(block_patterns)?,
    })
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
        classification_detail: row.classification_detail,
        content_type: row.content_type,
        http_status: row.http_status,
        discovered_at: row.discovered_at,
        updated_at: row.updated_at,
        document_id: row.document_id,
        result_revision_id: row.result_revision_id,
        mutation_item_id: row.mutation_item_id,
    }
}

impl WebRunFailure {
    fn from_fetch_error(error: PublicHttpUrlError) -> Self {
        if error.is_terminal_policy_rejection() {
            Self::invalid_url()
        } else {
            Self::inaccessible()
        }
    }

    fn inaccessible() -> Self {
        Self {
            failure_code: WebRunFailureCode::Inaccessible.as_str().to_string(),
            candidate_reason: Some(WebClassificationReason::Inaccessible.as_str().to_string()),
            final_url: None,
            content_type: None,
            http_status: None,
        }
    }

    fn inaccessible_with_response(
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

    fn invalid_url() -> Self {
        Self {
            failure_code: WebRunFailureCode::InvalidUrl.as_str().to_string(),
            candidate_reason: Some(WebClassificationReason::InvalidUrl.as_str().to_string()),
            final_url: None,
            content_type: None,
            http_status: None,
        }
    }

    fn unsupported_content(
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
        Some("text/csv" | "application/csv") => "table.csv",
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
                .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| fallback.to_string())
}

pub(super) fn is_direct_image_web_resource(final_url: &str, content_type: Option<&str>) -> bool {
    content_type
        .and_then(normalized_mime_type)
        .is_some_and(|mime_type| mime_type.starts_with("image/"))
        || url_extension(final_url).is_some_and(|extension| {
            matches!(
                extension.as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "bmp"
                    | "webp"
                    | "svg"
                    | "tif"
                    | "tiff"
                    | "heic"
                    | "heif"
            )
        })
}

fn normalized_mime_type(content_type: &str) -> Option<String> {
    let mime_type = content_type.split(';').next()?.trim().to_ascii_lowercase();
    (!mime_type.is_empty()).then_some(mime_type)
}

fn url_extension(final_url: &str) -> Option<String> {
    reqwest::Url::parse(final_url)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
                .and_then(|segment| segment.rsplit_once('.').map(|(_, extension)| extension))
                .map(str::trim)
                .filter(|extension| !extension.is_empty())
                .map(str::to_ascii_lowercase)
        })
        .or_else(|| {
            final_url
                .split(['?', '#'])
                .next()
                .and_then(|path| path.rsplit('/').next())
                .and_then(|segment| segment.rsplit_once('.').map(|(_, extension)| extension))
                .map(str::trim)
                .filter(|extension| !extension.is_empty())
                .map(str::to_ascii_lowercase)
        })
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
            .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "index.html")
            .map(ToString::to_string);
        path_title.or_else(|| url.host_str().map(ToString::to_string))
    })
}

#[cfg(test)]
mod tests {
    use super::{PublicHttpUrlError, WebRunFailure, is_direct_image_web_resource};

    #[test]
    fn web_fetch_failures_preserve_policy_vs_transient_classification() {
        let policy_failure =
            WebRunFailure::from_fetch_error(PublicHttpUrlError::CredentialsForbidden);
        assert_eq!(policy_failure.failure_code, "invalid_url");
        assert_eq!(policy_failure.candidate_reason.as_deref(), Some("invalid_url"));

        let transient_failure =
            WebRunFailure::from_fetch_error(PublicHttpUrlError::ResolveTimedOut);
        assert_eq!(transient_failure.failure_code, "inaccessible");
        assert_eq!(transient_failure.candidate_reason.as_deref(), Some("inaccessible"));
    }

    #[test]
    fn direct_image_web_resources_are_not_document_materialization_targets() {
        assert!(is_direct_image_web_resource(
            "https://docs.example.test/assets/diagram.PNG?cache=1",
            None,
        ));
        assert!(is_direct_image_web_resource(
            "https://docs.example.test/download?id=42",
            Some("image/webp; charset=binary"),
        ));
        assert!(!is_direct_image_web_resource(
            "https://docs.example.test/guide.pdf",
            Some("application/pdf"),
        ));
        assert!(!is_direct_image_web_resource(
            "https://docs.example.test/page",
            Some("text/html; charset=utf-8"),
        ));
    }
}
