use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{operational::HealthState, usage_governance::UsageGovernanceSummary},
    infra::repositories,
    interfaces::http::{auth::AuthContext, router_support::ApiError},
    shared::status::summarize_health_state,
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

#[derive(Serialize)]
pub struct WorkspaceGovernanceSummary {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub status: String,
    pub projects: usize,
    pub provider_accounts: usize,
    pub model_profiles: usize,
    pub api_tokens: usize,
    pub health_state: HealthState,
    pub usage: UsageGovernanceSummary,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/workspaces", get(list_workspaces).post(create_workspace))
        .route("/workspaces/{id}/governance", get(get_workspace_governance))
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

async fn get_workspace_governance(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkspaceGovernanceSummary>, ApiError> {
    auth.require_any_scope(&["workspace:admin", "usage:read", "providers:admin"])?;

    let workspace = repositories::list_workspaces(&state.persistence.postgres)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .find(|row| row.id == id)
        .ok_or_else(|| ApiError::NotFound(format!("workspace {id} not found")))?;

    let projects = repositories::list_projects(&state.persistence.postgres, Some(id))
        .await
        .map_err(|_| ApiError::Internal)?;
    let provider_accounts =
        repositories::list_provider_accounts(&state.persistence.postgres, Some(id))
            .await
            .map_err(|_| ApiError::Internal)?;
    let model_profiles = repositories::list_model_profiles(&state.persistence.postgres, Some(id))
        .await
        .map_err(|_| ApiError::Internal)?;
    let api_tokens = repositories::list_api_tokens(&state.persistence.postgres, Some(id))
        .await
        .map_err(|_| ApiError::Internal)?;

    let totals = repositories::get_usage_cost_totals(&state.persistence.postgres, None)
        .await
        .map_err(|_| ApiError::Internal)?;

    let health_state = summarize_health_state(&[
        if provider_accounts.is_empty() { HealthState::Degraded } else { HealthState::Healthy },
        if model_profiles.is_empty() { HealthState::Degraded } else { HealthState::Healthy },
    ]);

    Ok(Json(WorkspaceGovernanceSummary {
        id: workspace.id,
        slug: workspace.slug,
        name: workspace.name,
        status: workspace.status,
        projects: projects.len(),
        provider_accounts: provider_accounts.len(),
        model_profiles: model_profiles.len(),
        api_tokens: api_tokens.len(),
        health_state,
        usage: UsageGovernanceSummary {
            usage_events: totals.usage_events,
            prompt_tokens: totals.prompt_tokens.unwrap_or(0),
            completion_tokens: totals.completion_tokens.unwrap_or(0),
            total_tokens: totals.total_tokens.unwrap_or(0),
            estimated_cost: totals.estimated_cost,
        },
    }))
}
