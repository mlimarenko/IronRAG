pub mod snapshot;

mod deletion;
mod libraries;
mod mapping;
mod workspaces;

use deletion::*;
pub(crate) use deletion::{
    CatalogDeletionWorker, spawn_library_deletion_worker, spawn_workspace_deletion_worker,
};
pub use libraries::*;
use mapping::*;
pub use workspaces::*;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, put},
};
use serde::{Deserialize, Serialize};
use tracing::Instrument;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        catalog::{CatalogLibrary, CatalogLifecycleState, CatalogWorkspace},
        recognition::LibraryRecognitionPolicy,
        retrieval::RetrievalConfig,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_LIBRARY_WRITE, POLICY_MCP_DISCOVERY, authorize_library_discovery,
            authorize_workspace_discovery, library_discovery_ids, load_library_and_authorize,
            load_workspace_and_authorize_library_write,
        },
        router_support::{ApiError, RequestId, RestAuditEvent, record_rest_audit_event},
    },
    services::{
        catalog_service::{
            CatalogDeletionAdmission, CatalogService, CreateLibraryCommand, CreateWorkspaceCommand,
            SearchCatalogLibraryPage, UpdateLibraryCommand, UpdateLibraryRecognitionPolicyCommand,
            UpdateLibraryRetrievalConfigCommand, UpdateLibraryWebIngestPolicyCommand,
            UpdateWorkspaceCommand,
        },
        iam::audit::AppendAuditEventSubjectCommand,
    },
    shared::web::ingest::WebIngestPolicy,
};

const DEFAULT_CATALOG_LIBRARY_PAGE_LIMIT: u32 = 50;
const MAX_CATALOG_LIBRARY_PAGE_LIMIT: u32 = 100;
const MAX_CATALOG_LIBRARY_SEARCH_CHARS: usize = 200;

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogWorkspaceResponse {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_state: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogLibraryIngestionReadinessResponse {
    pub ready: bool,
    pub missing_binding_purposes: Vec<AiBindingPurpose>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogLibraryRuntimeReadinessResponse {
    pub missing_binding_purposes: Vec<AiBindingPurpose>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogLibraryResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub extraction_prompt: Option<String>,
    pub web_ingest_policy: WebIngestPolicy,
    pub recognition_policy: LibraryRecognitionPolicy,
    pub retrieval_config: RetrievalConfig,
    pub lifecycle_state: String,
    pub include_document_hint_in_mcp_answers: bool,
    pub ingestion_readiness: CatalogLibraryIngestionReadinessResponse,
    pub runtime_readiness: CatalogLibraryRuntimeReadinessResponse,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogLibraryPageResponse {
    pub items: Vec<CatalogLibraryResponse>,
    pub total_count: i64,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct CatalogLibraryListQuery {
    pub search: Option<String>,
    pub offset: Option<u32>,
    pub limit: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
struct CatalogLibraryPageWindow {
    search: Option<String>,
    offset: u32,
    limit: u32,
}

fn normalize_catalog_library_page(
    query: CatalogLibraryListQuery,
) -> Result<CatalogLibraryPageWindow, ApiError> {
    let search =
        query.search.map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
    if search.as_ref().is_some_and(|value| value.chars().count() > MAX_CATALOG_LIBRARY_SEARCH_CHARS)
    {
        return Err(ApiError::BadRequest("library search is too long".to_string()));
    }
    Ok(CatalogLibraryPageWindow {
        search,
        offset: query.offset.unwrap_or_default(),
        limit: query
            .limit
            .unwrap_or(DEFAULT_CATALOG_LIBRARY_PAGE_LIMIT)
            .clamp(1, MAX_CATALOG_LIBRARY_PAGE_LIMIT),
    })
}

fn cap_catalog_library_page<T>(mut items: Vec<T>, limit: u32) -> Vec<T> {
    items.truncate(limit as usize);
    items
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogDeletionAcceptedResponse {
    pub operation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateCatalogWorkspaceRequest {
    pub slug: Option<String>,
    pub display_name: String,
}

/// Partial update for a workspace. Every field is optional (true PATCH
/// semantics, unlike [`UpdateCatalogLibraryRequest`] below which still
/// requires `displayName`) — an absent field leaves the current value
/// unchanged.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCatalogWorkspaceRequest {
    pub slug: Option<String>,
    pub display_name: Option<String>,
    pub lifecycle_state: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateCatalogLibraryRequest {
    pub slug: Option<String>,
    pub display_name: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCatalogLibraryRequest {
    pub slug: Option<String>,
    pub display_name: String,
    pub description: Option<String>,
    pub extraction_prompt: Option<String>,
    pub lifecycle_state: Option<String>,
    pub include_document_hint_in_mcp_answers: Option<bool>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateLibraryWebIngestPolicyRequest {
    pub crawl_filter: crate::shared::web::ingest::WebIngestUrlFilter,
    pub materialization_filter: crate::shared::web::ingest::WebIngestUrlFilter,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct UpdateLibraryRecognitionPolicyRequest {
    pub raster_image_engine: crate::domains::recognition::RecognitionEngine,
}

/// Request body for replacing a library's declarative retrieval configuration.
/// Carries the full [`RetrievalConfig`]; absent keys resolve to defaults that
/// reproduce today's hardcoded lexical behaviour.
pub type UpdateLibraryRetrievalConfigRequest = RetrievalConfig;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/catalog/workspaces", get(list_workspaces).post(create_workspace))
        .route(
            "/catalog/workspaces/{workspace_id}",
            get(get_workspace).patch(update_workspace).delete(delete_workspace),
        )
        .route(
            "/catalog/workspaces/{workspace_id}/libraries",
            get(list_libraries).post(create_library),
        )
        .route("/catalog/workspaces/{workspace_id}/libraries/{library_id}", delete(delete_library))
        .route("/catalog/libraries/{library_id}", get(get_library).put(update_library))
        .route(
            "/catalog/libraries/{library_id}/web-ingest-policy",
            put(update_library_web_ingest_policy),
        )
        .route(
            "/catalog/libraries/{library_id}/recognition-policy",
            put(update_library_recognition_policy),
        )
        .route(
            "/catalog/libraries/{library_id}/retrieval-config",
            get(get_library_retrieval_config).put(update_library_retrieval_config),
        )
        .merge(snapshot::routes())
}

#[cfg(test)]
mod tests;
