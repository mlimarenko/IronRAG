use axum::{
    Json, Router,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{repositories, ui_queries},
    interfaces::http::{
        router_support::ApiError,
        ui_support::{UiSessionContext, parse_locale, require_admin_role},
    },
    shared::slugs::slugify,
};

#[derive(Serialize)]
pub struct WorkspaceOptionResponse {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct LibraryOptionResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct ShellUserResponse {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub role_label: String,
    pub initials: String,
}

#[derive(Serialize)]
pub struct ShellContextResponse {
    pub locale: String,
    pub admin_enabled: bool,
    pub current_user: ShellUserResponse,
    pub active_workspace: WorkspaceOptionResponse,
    pub active_library: LibraryOptionResponse,
    pub workspaces: Vec<WorkspaceOptionResponse>,
    pub libraries: Vec<LibraryOptionResponse>,
}

#[derive(Deserialize)]
pub struct UpdateContextRequest {
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub locale: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateWorkspaceRequest {
    pub name: String,
}

#[derive(Deserialize)]
pub struct CreateLibraryRequest {
    pub workspace_id: Uuid,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Deserialize)]
pub struct LibrariesQuery {
    pub workspace_id: Option<Uuid>,
}

fn map_workspace_option(row: repositories::WorkspaceRow) -> WorkspaceOptionResponse {
    WorkspaceOptionResponse { id: row.id, slug: row.slug, name: row.name }
}

fn map_library_option(row: repositories::ProjectRow) -> LibraryOptionResponse {
    LibraryOptionResponse {
        id: row.id,
        workspace_id: row.workspace_id,
        slug: row.slug,
        name: row.name,
    }
}

fn map_shell_context(context: ui_queries::ResolvedShellContext) -> ShellContextResponse {
    let current_user = ui_queries::map_ui_user(&context.user);

    ShellContextResponse {
        locale: context.session.locale,
        admin_enabled: matches!(context.user.role_label.to_lowercase().as_str(), "owner" | "admin"),
        current_user: ShellUserResponse {
            id: current_user.id,
            email: current_user.email,
            display_name: current_user.display_name,
            role_label: current_user.role_label,
            initials: current_user.initials,
        },
        active_workspace: map_workspace_option(context.active_workspace),
        active_library: map_library_option(context.active_project),
        workspaces: context.workspaces.into_iter().map(map_workspace_option).collect(),
        libraries: context.projects.into_iter().map(map_library_option).collect(),
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ui/context", axum::routing::get(get_context).put(update_context))
        .route("/ui/workspaces", axum::routing::get(list_workspaces).post(create_workspace))
        .route("/ui/libraries", axum::routing::get(list_libraries).post(create_library))
}

async fn load_resolved_context(
    state: &AppState,
    ui_session: &UiSessionContext,
) -> Result<ui_queries::ResolvedShellContext, ApiError> {
    let session =
        repositories::get_ui_session_by_id(&state.persistence.postgres, ui_session.session_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or(ApiError::Unauthorized)?;
    let user = repositories::get_ui_user_by_id(&state.persistence.postgres, ui_session.user_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or(ApiError::Unauthorized)?;
    ui_queries::resolve_shell_context(
        &state.persistence.postgres,
        session,
        user,
        state.settings.destructive_fresh_bootstrap_settings().allow_legacy_startup_side_effects,
    )
    .await
    .map_err(|_| ApiError::Internal)
}

async fn get_context(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
) -> Result<Json<ShellContextResponse>, ApiError> {
    let context = load_resolved_context(&state, &ui_session).await?;
    Ok(Json(map_shell_context(context)))
}

async fn update_context(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Json(payload): Json<UpdateContextRequest>,
) -> Result<Json<ShellContextResponse>, ApiError> {
    let context = load_resolved_context(&state, &ui_session).await?;
    let workspace_id = payload.workspace_id.unwrap_or(context.active_workspace.id);
    if !ui_queries::workspace_is_visible(&context.workspaces, workspace_id) {
        return Err(ApiError::Unauthorized);
    }
    let mut projects = repositories::list_projects_for_ui_user(
        &state.persistence.postgres,
        ui_session.user_id,
        workspace_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    if projects.is_empty()
        && state.settings.destructive_fresh_bootstrap_settings().allow_legacy_startup_side_effects
    {
        let project =
            repositories::find_or_create_default_project(&state.persistence.postgres, workspace_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        repositories::ensure_project_access_grant(
            &state.persistence.postgres,
            project.id,
            ui_session.user_id,
            "write",
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        projects = repositories::list_projects_for_ui_user(
            &state.persistence.postgres,
            ui_session.user_id,
            workspace_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
    }
    let library_id = match payload.library_id {
        Some(library_id) if ui_queries::project_is_visible(&projects, library_id) => library_id,
        Some(_) => return Err(ApiError::Unauthorized),
        None => {
            let preserved_library_id = (context.active_project.workspace_id == workspace_id
                && ui_queries::project_is_visible(&projects, context.active_project.id))
            .then_some(context.active_project.id);

            preserved_library_id
                .or_else(|| projects.first().map(|project| project.id))
                .ok_or_else(|| ApiError::Conflict("workspace has no visible libraries".into()))?
        }
    };
    let locale = parse_locale(
        payload.locale.as_deref().unwrap_or(&context.session.locale),
        &state.ui_runtime.default_locale,
    );
    repositories::touch_ui_session(
        &state.persistence.postgres,
        ui_session.session_id,
        Some(workspace_id),
        Some(library_id),
        &locale,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let updated = load_resolved_context(&state, &ui_session).await?;
    Ok(Json(map_shell_context(updated)))
}

async fn list_workspaces(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
) -> Result<Json<Vec<WorkspaceOptionResponse>>, ApiError> {
    let workspaces =
        repositories::list_workspaces_for_ui_user(&state.persistence.postgres, ui_session.user_id)
            .await
            .map_err(|_| ApiError::Internal)?;
    Ok(Json(workspaces.into_iter().map(map_workspace_option).collect()))
}

async fn create_workspace(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateWorkspaceRequest>,
) -> Result<Json<WorkspaceOptionResponse>, ApiError> {
    require_admin_role(&ui_session.role_label)?;
    let name = payload.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("workspace name is required".into()));
    }
    let workspace =
        repositories::create_workspace(&state.persistence.postgres, &slugify(name), name)
            .await
            .map_err(|_| ApiError::Internal)?;
    repositories::ensure_workspace_member(
        &state.persistence.postgres,
        workspace.id,
        ui_session.user_id,
        &ui_session.role_label,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let project =
        repositories::find_or_create_default_project(&state.persistence.postgres, workspace.id)
            .await
            .map_err(|_| ApiError::Internal)?;
    repositories::ensure_project_access_grant(
        &state.persistence.postgres,
        project.id,
        ui_session.user_id,
        "write",
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(Json(map_workspace_option(workspace)))
}

async fn list_libraries(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Query(query): Query<LibrariesQuery>,
) -> Result<Json<Vec<LibraryOptionResponse>>, ApiError> {
    let context = load_resolved_context(&state, &ui_session).await?;
    let workspace_id = query.workspace_id.unwrap_or(context.active_workspace.id);
    if !ui_queries::workspace_is_visible(&context.workspaces, workspace_id) {
        return Err(ApiError::Unauthorized);
    }
    let libraries = repositories::list_projects_for_ui_user(
        &state.persistence.postgres,
        ui_session.user_id,
        workspace_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(Json(libraries.into_iter().map(map_library_option).collect()))
}

async fn create_library(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateLibraryRequest>,
) -> Result<Json<LibraryOptionResponse>, ApiError> {
    require_admin_role(&ui_session.role_label)?;
    let context = load_resolved_context(&state, &ui_session).await?;
    if !ui_queries::workspace_is_visible(&context.workspaces, payload.workspace_id) {
        return Err(ApiError::Unauthorized);
    }
    let name = payload.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("library name is required".into()));
    }
    let project = repositories::create_project(
        &state.persistence.postgres,
        payload.workspace_id,
        &slugify(name),
        name,
        payload.description.as_deref(),
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    repositories::ensure_project_access_grant(
        &state.persistence.postgres,
        project.id,
        ui_session.user_id,
        "write",
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(Json(map_library_option(project)))
}
