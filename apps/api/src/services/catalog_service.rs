use std::collections::{HashMap, HashSet};

use tracing::error;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::recognition::LibraryRecognitionPolicy,
    domains::retrieval::RetrievalConfig,
    domains::{
        ai::AiBindingPurpose,
        catalog::{
            CatalogLibrary, CatalogLibraryIngestionReadiness, CatalogLibraryRuntimeReadiness,
            CatalogLifecycleState, CatalogWorkspace, ChunkingTemplate,
        },
        ops::OpsAsyncOperationStatus,
    },
    infra::repositories::{ai_repository, catalog_repository},
    interfaces::http::router_support::{
        ApiError, map_library_create_error, map_workspace_create_error,
    },
    services::ops::service::{CreateAsyncOperationCommand, UpdateAsyncOperationCommand},
    shared::slugs::slugify,
    shared::web::ingest::{WebIngestPolicy, validate_web_ingest_policy},
};

mod libraries;
mod workspaces;

#[cfg(test)]
mod tests;

pub(crate) const INGEST_REQUIRED_BINDINGS: &[AiBindingPurpose] =
    &[AiBindingPurpose::ExtractGraph, AiBindingPurpose::EmbedChunk];
const RUNTIME_REQUIRED_BINDINGS: &[AiBindingPurpose] = &[
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::EmbedChunk,
    AiBindingPurpose::QueryCompile,
    AiBindingPurpose::QueryAnswer,
];

#[derive(Debug, Clone)]
struct CatalogLibraryBindingReadiness {
    ingestion: CatalogLibraryIngestionReadiness,
    runtime: CatalogLibraryRuntimeReadiness,
}

#[derive(Debug)]
pub struct CatalogLibraryPage {
    pub items: Vec<CatalogLibrary>,
    pub total_count: i64,
}

pub struct SearchCatalogLibraryPage<'a> {
    pub workspace_id: Uuid,
    pub search: Option<&'a str>,
    pub visible_library_ids: Option<&'a [Uuid]>,
    pub pinned_library_id: Option<Uuid>,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Debug, Clone)]
pub struct CreateWorkspaceCommand {
    pub slug: Option<String>,
    pub display_name: String,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateWorkspaceCommand {
    pub workspace_id: Uuid,
    pub slug: Option<String>,
    pub display_name: String,
    pub lifecycle_state: CatalogLifecycleState,
}

#[derive(Debug, Clone)]
pub struct CreateLibraryCommand {
    pub workspace_id: Uuid,
    pub slug: Option<String>,
    pub display_name: String,
    pub description: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug)]
pub(crate) struct ProvisioningWorkspaceCreate {
    pub row: Option<catalog_repository::CatalogWorkspaceRow>,
    pub slug: String,
    pub display_name: String,
}

#[derive(Debug)]
pub(crate) struct ProvisioningLibraryCreate {
    pub row: Option<catalog_repository::CatalogLibraryRow>,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateLibraryCommand {
    pub library_id: Uuid,
    pub slug: Option<String>,
    pub display_name: String,
    pub description: Option<String>,
    pub extraction_prompt: Option<String>,
    pub lifecycle_state: CatalogLifecycleState,
    pub include_document_hint_in_mcp_answers: bool,
}

#[derive(Debug, Clone)]
pub struct UpdateLibraryWebIngestPolicyCommand {
    pub library_id: Uuid,
    pub web_ingest_policy: WebIngestPolicy,
}

#[derive(Debug, Clone)]
pub struct UpdateLibraryRecognitionPolicyCommand {
    pub library_id: Uuid,
    pub recognition_policy: LibraryRecognitionPolicy,
}

#[derive(Debug, Clone)]
pub struct UpdateLibraryRetrievalConfigCommand {
    pub library_id: Uuid,
    pub retrieval_config: RetrievalConfig,
}

#[derive(Debug, Clone)]
pub struct CatalogDeletionAdmission {
    pub operation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub display_name: String,
    pub should_start_worker: bool,
}

#[derive(Clone, Default)]
pub struct CatalogService;

#[derive(Debug, Clone, Copy)]
enum CatalogLifecycleError {
    DisabledVocabulary,
    InvalidValue,
}

impl CatalogLifecycleError {
    fn into_request_error(self) -> ApiError {
        match self {
            Self::DisabledVocabulary => {
                ApiError::BadRequest("unsupported catalog lifecycle state".to_string())
            }
            Self::InvalidValue => ApiError::Internal,
        }
    }

    const fn into_persisted_error(self) -> ApiError {
        match self {
            Self::DisabledVocabulary | Self::InvalidValue => ApiError::Internal,
        }
    }
}

impl CatalogService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

fn normalize_display_name(value: &str, field_name: &'static str) -> Result<String, ApiError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(ApiError::BadRequest(format!("{field_name} must not be empty")));
    }
    Ok(normalized.to_string())
}

fn normalize_optional_slug(provided_slug: Option<&str>, display_name: &str) -> String {
    provided_slug
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| slugify(display_name), slugify)
}

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(ToString::to_string)
}

fn parse_lifecycle_state(value: &str) -> Result<CatalogLifecycleState, CatalogLifecycleError> {
    match value {
        "active" => Ok(CatalogLifecycleState::Active),
        "archived" => Ok(CatalogLifecycleState::Archived),
        "disabled" => Err(CatalogLifecycleError::DisabledVocabulary),
        _ => Err(CatalogLifecycleError::InvalidValue),
    }
}

const fn lifecycle_state_as_str(
    value: &CatalogLifecycleState,
) -> Result<&'static str, CatalogLifecycleError> {
    match value {
        CatalogLifecycleState::Active => Ok("active"),
        CatalogLifecycleState::Disabled => Err(CatalogLifecycleError::DisabledVocabulary),
        CatalogLifecycleState::Archived => Ok("archived"),
    }
}

fn map_catalog_delete_blockers(
    subject_kind: &'static str,
    blockers: catalog_repository::CatalogDeleteBlockers,
) -> ApiError {
    ApiError::Conflict(format!(
        "{subject_kind} deletion is blocked until durable webhook work is drained \
         (undispatched lifecycle events: {}, active delivery jobs: {}, \
         unresolved delivery attempts: {})",
        blockers.undispatched_webhook_lifecycle_events,
        blockers.active_webhook_delivery_jobs,
        blockers.unresolved_webhook_delivery_attempts,
    ))
}

async fn settle_catalog_delete_operation(
    state: &AppState,
    operation_id: Uuid,
    status: &str,
    failure_code: Option<String>,
) {
    if let Err(update_error) = state
        .application_services
        .ops
        .update_async_operation(
            state,
            UpdateAsyncOperationCommand {
                operation_id,
                status: status.to_string(),
                completed_at: Some(chrono::Utc::now()),
                failure_code,
            },
        )
        .await
    {
        error!(
            operation_id = %operation_id,
            error = %update_error,
            "failed to settle catalog deletion async operation"
        );
    }
}

const fn catalog_delete_operation_is_active(status: OpsAsyncOperationStatus) -> bool {
    matches!(status, OpsAsyncOperationStatus::Accepted | OpsAsyncOperationStatus::Processing)
}

async fn restore_stashed_directory(
    state: &AppState,
    stashed_directory: Option<&crate::services::content::storage::StashedContentDirectory>,
) {
    if let Some(stashed_directory) = stashed_directory
        && let Err(restore_error) =
            state.content_storage.restore_stashed_directory(stashed_directory).await
    {
        error!(
            original_path = %stashed_directory.original_path().display(),
            stashed_path = %stashed_directory.stashed_path().display(),
            error = ?restore_error,
            "failed to restore stashed content directory"
        );
    }
}

async fn purge_stashed_directory(
    state: &AppState,
    stashed_directory: Option<&crate::services::content::storage::StashedContentDirectory>,
) {
    if let Some(stashed_directory) = stashed_directory
        && let Err(purge_error) =
            state.content_storage.purge_stashed_directory(stashed_directory).await
    {
        error!(
            original_path = %stashed_directory.original_path().display(),
            stashed_path = %stashed_directory.stashed_path().display(),
            error = ?purge_error,
            "failed to purge stashed content directory"
        );
    }
}
