use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, put},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        catalog::{CatalogLibrary, CatalogLifecycleState, CatalogWorkspace},
        recognition::LibraryRecognitionPolicy,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_LIBRARY_WRITE, POLICY_MCP_DISCOVERY, POLICY_WORKSPACE_ADMIN,
            authorize_library_discovery, authorize_workspace_discovery, load_library_and_authorize,
            load_workspace_and_authorize,
        },
        router_support::{ApiError, RequestId},
    },
    services::{
        catalog_service::{
            CreateLibraryCommand, CreateWorkspaceCommand, UpdateLibraryCommand,
            UpdateLibraryRecognitionPolicyCommand, UpdateLibraryWebIngestPolicyCommand,
        },
        iam::audit::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
    },
    shared::web::ingest::WebIngestPolicy,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogWorkspaceResponse {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_state: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogLibraryIngestionReadinessResponse {
    pub ready: bool,
    pub missing_binding_purposes: Vec<AiBindingPurpose>,
}

#[derive(Debug, Serialize)]
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
    pub lifecycle_state: String,
    pub ingestion_readiness: CatalogLibraryIngestionReadinessResponse,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCatalogWorkspaceRequest {
    pub slug: Option<String>,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCatalogLibraryRequest {
    pub slug: Option<String>,
    pub display_name: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCatalogLibraryRequest {
    pub slug: Option<String>,
    pub display_name: String,
    pub description: Option<String>,
    pub extraction_prompt: Option<String>,
    pub lifecycle_state: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateLibraryWebIngestPolicyRequest {
    pub url_filter: crate::shared::web::ingest::WebIngestUrlFilter,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct UpdateLibraryRecognitionPolicyRequest {
    pub raster_image_engine: crate::domains::recognition::RecognitionEngine,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/catalog/workspaces", get(list_workspaces).post(create_workspace))
        .route("/catalog/workspaces/{workspace_id}", get(get_workspace).delete(delete_workspace))
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
}

#[tracing::instrument(level = "info", name = "http.list_workspaces", skip_all, fields(item_count))]
async fn list_workspaces(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<Vec<CatalogWorkspaceResponse>>, ApiError> {
    let span = tracing::Span::current();
    auth.require_discover_any_workspace(POLICY_MCP_DISCOVERY)?;
    let workspaces = state.canonical_services.catalog.list_workspaces(&state, None).await?;
    let items: Vec<_> = workspaces
        .into_iter()
        .filter(|workspace| authorize_workspace_discovery(&auth, workspace.id).is_ok())
        .map(map_workspace)
        .collect();
    span.record("item_count", items.len());
    Ok(Json(items))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_workspace",
    skip_all,
    fields(workspace_id = %workspace_id)
)]
async fn get_workspace(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<CatalogWorkspaceResponse>, ApiError> {
    authorize_workspace_discovery(&auth, workspace_id)?;
    let workspace = state.canonical_services.catalog.get_workspace(&state, workspace_id).await?;
    Ok(Json(map_workspace(workspace)))
}

#[tracing::instrument(level = "info", name = "http.create_workspace", skip_all)]
async fn create_workspace(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateCatalogWorkspaceRequest>,
) -> Result<Json<CatalogWorkspaceResponse>, ApiError> {
    if !auth.is_system_admin {
        record_catalog_audit_event(
            &state,
            &auth,
            request_id.map(|value| value.0.0),
            "catalog.workspace.create",
            "rejected",
            Some("workspace create denied".to_string()),
            Some(format!("principal {} was denied workspace creation", auth.principal_id)),
            Vec::new(),
        )
        .await;
        return Err(ApiError::Unauthorized);
    }

    let workspace = state
        .canonical_services
        .catalog
        .create_workspace(
            &state,
            CreateWorkspaceCommand {
                slug: payload.slug,
                display_name: payload.display_name,
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;

    record_catalog_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "catalog.workspace.create",
        "succeeded",
        Some(format!("workspace {} created", workspace.display_name)),
        Some(format!("principal {} created workspace {}", auth.principal_id, workspace.id)),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "workspace".to_string(),
            subject_id: workspace.id,
            workspace_id: Some(workspace.id),
            library_id: None,
            document_id: None,
        }],
    )
    .await;

    Ok(Json(map_workspace(workspace)))
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_workspace",
    skip_all,
    fields(workspace_id = %workspace_id)
)]
async fn delete_workspace(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(workspace_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    if !auth.is_system_admin {
        record_catalog_audit_event(
            &state,
            &auth,
            request_id.map(|value| value.0.0),
            "catalog.workspace.delete",
            "rejected",
            Some("workspace delete denied".to_string()),
            Some(format!("principal {} was denied workspace deletion", auth.principal_id)),
            Vec::new(),
        )
        .await;
        return Err(ApiError::Unauthorized);
    }

    let workspace = state.canonical_services.catalog.delete_workspace(&state, workspace_id).await?;

    record_catalog_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "catalog.workspace.delete",
        "succeeded",
        Some(format!("workspace {} deleted", workspace.display_name)),
        Some(format!("principal {} deleted workspace {}", auth.principal_id, workspace.id)),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "workspace".to_string(),
            subject_id: workspace.id,
            workspace_id: Some(workspace.id),
            library_id: None,
            document_id: None,
        }],
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[tracing::instrument(
    level = "info",
    name = "http.list_libraries",
    skip_all,
    fields(workspace_id = %workspace_id, item_count)
)]
async fn list_libraries(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Vec<CatalogLibraryResponse>>, ApiError> {
    let span = tracing::Span::current();
    authorize_workspace_discovery(&auth, workspace_id)?;
    let libraries = state.canonical_services.catalog.list_libraries(&state, workspace_id).await?;
    let items: Vec<_> = libraries
        .into_iter()
        .filter(|library| {
            authorize_library_discovery(&auth, library.workspace_id, library.id).is_ok()
        })
        .map(map_library)
        .collect();
    span.record("item_count", items.len());
    Ok(Json(items))
}

#[tracing::instrument(
    level = "info",
    name = "http.create_library",
    skip_all,
    fields(workspace_id = %workspace_id)
)]
async fn create_library(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(workspace_id): Path<Uuid>,
    Json(payload): Json<CreateCatalogLibraryRequest>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    load_workspace_and_authorize(&auth, &state, workspace_id, POLICY_WORKSPACE_ADMIN).await?;

    let library = state
        .canonical_services
        .catalog
        .create_library(
            &state,
            CreateLibraryCommand {
                workspace_id,
                slug: payload.slug,
                display_name: payload.display_name,
                description: payload.description,
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;

    record_catalog_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "catalog.library.create",
        "succeeded",
        Some(format!("library {} created", library.display_name)),
        Some(format!(
            "principal {} created library {} in workspace {}",
            auth.principal_id, library.id, library.workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "library".to_string(),
            subject_id: library.id,
            workspace_id: Some(library.workspace_id),
            library_id: Some(library.id),
            document_id: None,
        }],
    )
    .await;

    Ok(Json(map_library(library)))
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_library",
    skip_all,
    fields(workspace_id = %workspace_id, library_id = %library_id)
)]
async fn delete_library(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path((workspace_id, library_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    load_workspace_and_authorize(&auth, &state, workspace_id, POLICY_WORKSPACE_ADMIN).await?;
    let library = state.canonical_services.catalog.get_library(&state, library_id).await?;
    if library.workspace_id != workspace_id {
        return Err(ApiError::resource_not_found("library", library_id));
    }

    let deleted_library =
        state.canonical_services.catalog.delete_library(&state, library_id).await?;

    record_catalog_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "catalog.library.delete",
        "succeeded",
        Some(format!("library {} deleted", deleted_library.display_name)),
        Some(format!(
            "principal {} deleted library {} in workspace {}",
            auth.principal_id, deleted_library.id, deleted_library.workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "library".to_string(),
            subject_id: deleted_library.id,
            workspace_id: Some(deleted_library.workspace_id),
            library_id: Some(deleted_library.id),
            document_id: None,
        }],
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

#[tracing::instrument(
    level = "info",
    name = "http.get_library",
    skip_all,
    fields(library_id = %library_id)
)]
async fn get_library(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    let library = state.canonical_services.catalog.get_library(&state, library_id).await?;
    authorize_library_discovery(&auth, library.workspace_id, library.id)?;
    Ok(Json(map_library(library)))
}

#[tracing::instrument(
    level = "info",
    name = "http.update_library",
    skip_all,
    fields(library_id = %library_id)
)]
async fn update_library(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<UpdateCatalogLibraryRequest>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    let existing = state.canonical_services.catalog.get_library(&state, library_id).await?;
    load_workspace_and_authorize(&auth, &state, existing.workspace_id, POLICY_WORKSPACE_ADMIN)
        .await?;

    let lifecycle_state = payload
        .lifecycle_state
        .as_deref()
        .unwrap_or(lifecycle_state_label(&existing.lifecycle_state));
    let library = state
        .canonical_services
        .catalog
        .update_library(
            &state,
            UpdateLibraryCommand {
                library_id,
                slug: payload.slug,
                display_name: payload.display_name,
                description: payload.description,
                extraction_prompt: payload.extraction_prompt,
                lifecycle_state: parse_lifecycle_state_input(lifecycle_state)?,
            },
        )
        .await?;

    record_catalog_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "catalog.library.update",
        "succeeded",
        Some(format!("library {} updated", library.display_name)),
        Some(format!(
            "principal {} updated library {} in workspace {}",
            auth.principal_id, library.id, library.workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "library".to_string(),
            subject_id: library.id,
            workspace_id: Some(library.workspace_id),
            library_id: Some(library.id),
            document_id: None,
        }],
    )
    .await;

    Ok(Json(map_library(library)))
}

#[tracing::instrument(
    level = "info",
    name = "http.update_library_web_ingest_policy",
    skip_all,
    fields(library_id = %library_id)
)]
async fn update_library_web_ingest_policy(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<UpdateLibraryWebIngestPolicyRequest>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    let existing =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;

    let library = state
        .canonical_services
        .catalog
        .update_library_web_ingest_policy(
            &state,
            UpdateLibraryWebIngestPolicyCommand {
                library_id,
                web_ingest_policy: WebIngestPolicy { url_filter: payload.url_filter },
            },
        )
        .await?;

    record_catalog_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "catalog.library.web_ingest_policy.update",
        "succeeded",
        Some(format!("library {} web ingest policy updated", library.display_name)),
        Some(format!(
            "principal {} updated web ingest policy for library {} in workspace {}",
            auth.principal_id, library.id, library.workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "library".to_string(),
            subject_id: library.id,
            workspace_id: Some(existing.workspace_id),
            library_id: Some(library.id),
            document_id: None,
        }],
    )
    .await;

    Ok(Json(map_library(library)))
}

#[tracing::instrument(
    level = "info",
    name = "http.update_library_recognition_policy",
    skip_all,
    fields(library_id = %library_id)
)]
async fn update_library_recognition_policy(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<UpdateLibraryRecognitionPolicyRequest>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    let existing =
        load_library_and_authorize(&auth, &state, library_id, POLICY_LIBRARY_WRITE).await?;

    let library = state
        .canonical_services
        .catalog
        .update_library_recognition_policy(
            &state,
            UpdateLibraryRecognitionPolicyCommand {
                library_id,
                recognition_policy: LibraryRecognitionPolicy {
                    raster_image_engine: payload.raster_image_engine,
                },
            },
        )
        .await?;

    record_catalog_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "catalog.library.recognition_policy.update",
        "succeeded",
        Some(format!("library {} recognition policy updated", library.display_name)),
        Some(format!(
            "principal {} updated recognition policy for library {} in workspace {}",
            auth.principal_id, library.id, library.workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "library".to_string(),
            subject_id: library.id,
            workspace_id: Some(existing.workspace_id),
            library_id: Some(library.id),
            document_id: None,
        }],
    )
    .await;

    Ok(Json(map_library(library)))
}

fn map_workspace(workspace: CatalogWorkspace) -> CatalogWorkspaceResponse {
    CatalogWorkspaceResponse {
        id: workspace.id,
        slug: workspace.slug,
        display_name: workspace.display_name,
        lifecycle_state: lifecycle_state_label(&workspace.lifecycle_state).to_string(),
    }
}

fn map_library(library: CatalogLibrary) -> CatalogLibraryResponse {
    CatalogLibraryResponse {
        id: library.id,
        workspace_id: library.workspace_id,
        slug: library.slug,
        display_name: library.display_name,
        description: library.description,
        extraction_prompt: library.extraction_prompt,
        web_ingest_policy: library.web_ingest_policy,
        recognition_policy: library.recognition_policy,
        lifecycle_state: lifecycle_state_label(&library.lifecycle_state).to_string(),
        ingestion_readiness: CatalogLibraryIngestionReadinessResponse {
            ready: library.ingestion_readiness.ready,
            missing_binding_purposes: library.ingestion_readiness.missing_binding_purposes,
        },
    }
}

fn parse_lifecycle_state_input(value: &str) -> Result<CatalogLifecycleState, ApiError> {
    match value {
        "active" => Ok(CatalogLifecycleState::Active),
        "archived" => Ok(CatalogLifecycleState::Archived),
        other => Err(ApiError::BadRequest(format!("invalid lifecycle state: {other}"))),
    }
}

const fn lifecycle_state_label(value: &CatalogLifecycleState) -> &'static str {
    match value {
        CatalogLifecycleState::Active => "active",
        CatalogLifecycleState::Disabled => "disabled",
        CatalogLifecycleState::Archived => "archived",
    }
}

async fn record_catalog_audit_event(
    state: &AppState,
    auth: &AuthContext,
    request_id: Option<String>,
    action_kind: &str,
    result_kind: &str,
    redacted_message: Option<String>,
    internal_message: Option<String>,
    subjects: Vec<AppendAuditEventSubjectCommand>,
) {
    if let Err(error) = state
        .canonical_services
        .audit
        .append_event(
            state,
            AppendAuditEventCommand {
                actor_principal_id: Some(auth.principal_id),
                surface_kind: "rest".to_string(),
                action_kind: action_kind.to_string(),
                request_id,
                trace_id: None,
                result_kind: result_kind.to_string(),
                redacted_message,
                internal_message,
                subjects,
            },
        )
        .await
    {
        tracing::warn!(stage = "audit", error = %error, "audit append failed");
    }
}
