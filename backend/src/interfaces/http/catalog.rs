use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        catalog::{CatalogLibrary, CatalogLifecycleState, CatalogWorkspace},
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_MCP_DISCOVERY, POLICY_WORKSPACE_ADMIN, authorize_library_discovery,
            authorize_workspace_discovery, load_workspace_and_authorize,
        },
        router_support::{ApiError, RequestId},
    },
    services::{
        audit_service::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
        catalog_service::{CreateLibraryCommand, CreateWorkspaceCommand},
    },
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

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/catalog/workspaces", get(list_workspaces).post(create_workspace))
        .route("/catalog/workspaces/{workspace_id}", get(get_workspace))
        .route(
            "/catalog/workspaces/{workspace_id}/libraries",
            get(list_libraries).post(create_library),
        )
        .route("/catalog/libraries/{library_id}", get(get_library))
}

async fn list_workspaces(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<Vec<CatalogWorkspaceResponse>>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let workspaces = state.canonical_services.catalog.list_workspaces(&state, None).await?;
    Ok(Json(
        workspaces
            .into_iter()
            .filter(|workspace| authorize_workspace_discovery(&auth, workspace.id).is_ok())
            .map(map_workspace)
            .collect(),
    ))
}

async fn get_workspace(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<CatalogWorkspaceResponse>, ApiError> {
    authorize_workspace_discovery(&auth, workspace_id)?;
    let workspace = state.canonical_services.catalog.get_workspace(&state, workspace_id).await?;
    Ok(Json(map_workspace(workspace)))
}

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

async fn list_libraries(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Vec<CatalogLibraryResponse>>, ApiError> {
    authorize_workspace_discovery(&auth, workspace_id)?;
    let libraries = state.canonical_services.catalog.list_libraries(&state, workspace_id).await?;
    Ok(Json(
        libraries
            .into_iter()
            .filter(|library| {
                authorize_library_discovery(&auth, library.workspace_id, library.id).is_ok()
            })
            .map(map_library)
            .collect(),
    ))
}

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

async fn get_library(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<CatalogLibraryResponse>, ApiError> {
    let library = state.canonical_services.catalog.get_library(&state, library_id).await?;
    authorize_library_discovery(&auth, library.workspace_id, library.id)?;
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
        lifecycle_state: lifecycle_state_label(&library.lifecycle_state).to_string(),
        ingestion_readiness: CatalogLibraryIngestionReadinessResponse {
            ready: library.ingestion_readiness.ready,
            missing_binding_purposes: library.ingestion_readiness.missing_binding_purposes,
        },
    }
}

fn lifecycle_state_label(value: &CatalogLifecycleState) -> &'static str {
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
    let _ = state
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
        .await;
}
