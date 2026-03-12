use axum::{Json, Router, extract::State, routing::get};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    interfaces::http::{auth::AuthContext, router_support::ApiError},
};

#[derive(Serialize)]
pub struct WorkspaceSummary {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub status: String,
}

#[derive(Deserialize)]
pub struct CreateWorkspaceRequest {
    pub slug: String,
    pub name: String,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new().route("/workspaces", get(list_workspaces).post(create_workspace))
}

async fn list_workspaces(
    State(state): State<AppState>,
) -> Result<Json<Vec<WorkspaceSummary>>, ApiError> {
    let items = repositories::list_workspaces(&state.persistence.postgres)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|row| WorkspaceSummary {
            id: row.id,
            slug: row.slug,
            name: row.name,
            status: row.status,
        })
        .collect();

    Ok(Json(items))
}

async fn create_workspace(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateWorkspaceRequest>,
) -> Result<Json<WorkspaceSummary>, ApiError> {
    auth.require_any_scope(&["workspace:admin"])?;
    let row =
        repositories::create_workspace(&state.persistence.postgres, &payload.slug, &payload.name)
            .await
            .map_err(|_| ApiError::Internal)?;

    Ok(Json(WorkspaceSummary { id: row.id, slug: row.slug, name: row.name, status: row.status }))
}
