mod fetch;
mod jobs;
mod recursive;
mod runs;
mod single_page;
mod support;
#[cfg(test)]
mod tests;

use support::{
    MappedCounts, WebRunFilterPatterns, extraction_title, fallback_title_from_url,
    is_direct_image_web_resource, map_web_page_row, map_web_run_counts_row, map_web_run_receipt,
    parse_run_url_filter, resolved_web_mime_type, source_file_name_from_url,
    web_run_source_identity,
};

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
        content_mutation_repository,
        ingest_repository::{
            self, UpdateWebIngestRun, WebDiscoveredPageRow, WebIngestRunRow, WebRunCountsRow,
        },
    },
    interfaces::http::router_support::ApiError,
    services::{
        content::service::{
            MaterializeWebCaptureCommand, PersistedRevisionSourceGuard, UpdateMutationCommand,
        },
        content::storage::ContentStorageService,
        ops::service::UpdateAsyncOperationCommand,
    },
    shared::{
        extraction::html_main_content::{
            extract_html_preferred_url, payload_looks_like_html_document,
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
/// path) uses these fields to build a new `RevisionAdmission` with
/// the updated blob reference so the next ingest run operates on the new
/// bytes rather than the old captured snapshot.
#[derive(Debug, Clone)]
pub struct RefetchedWebDocumentSource {
    pub file_name: String,
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
