use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    interfaces::http::{
        auth::{AuthContext, TokenSummary},
        authorization::{
            POLICY_PROVIDERS_ADMIN, POLICY_WORKSPACE_ADMIN, authorize_workspace_scope,
        },
        projects::ProjectSummary,
        providers::{ModelProfileSummary, ProviderAccountSummary},
        router_support::ApiError,
    },
};

const DEFAULT_EXAMPLE_LIMIT: usize = 5;
const MAX_EXAMPLE_LIMIT: usize = 25;
const KNOWN_AVAILABLE_SCOPES: &[&str] = &[
    "workspace:admin",
    "providers:admin",
    "projects:write",
    "documents:read",
    "documents:write",
    "query:run",
    "usage:read",
];

#[derive(Serialize)]
pub struct IntegrationExampleProjectSummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct IntegrationsWorkspaceSummary {
    pub workspace_id: Uuid,
    pub example_projects: Vec<IntegrationExampleProjectSummary>,
    pub api_tokens: Vec<TokenSummary>,
}

#[derive(Serialize)]
pub struct IntegrationsProductSnapshot {
    pub workspace_id: Uuid,
    pub provider_accounts: Vec<ProviderAccountSummary>,
    pub model_profiles: Vec<ModelProfileSummary>,
    pub projects: Vec<ProjectSummary>,
    pub available_scopes: Vec<String>,
    pub generated_at: DateTime<Utc>,
    pub warning: Option<String>,
}

#[derive(Serialize)]
pub struct IntegrationsProductResponse {
    pub snapshot: IntegrationsProductSnapshot,
}

#[derive(Deserialize)]
pub struct ExampleProjectsQuery {
    pub limit: Option<usize>,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/integrations/{workspace_id}", get(get_integrations_workspace_summary))
        .route("/integrations/{workspace_id}/examples", get(list_workspace_example_projects))
        .route(
            "/integrations/{workspace_id}/tokens/{id}",
            axum::routing::delete(revoke_workspace_token),
        )
        .route("/integrations-products/{workspace_id}", get(get_integrations_product))
}

async fn get_integrations_workspace_summary(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<IntegrationsWorkspaceSummary>, ApiError> {
    authorize_workspace_scope(&auth, workspace_id, POLICY_PROVIDERS_ADMIN).await?;

    let example_projects =
        load_example_projects(&state, workspace_id, DEFAULT_EXAMPLE_LIMIT).await?;
    let api_tokens = load_workspace_tokens(&state, workspace_id).await?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %workspace_id,
        example_project_count = example_projects.len(),
        api_token_count = api_tokens.len(),
        "loaded integrations workspace summary",
    );

    Ok(Json(IntegrationsWorkspaceSummary { workspace_id, example_projects, api_tokens }))
}

async fn list_workspace_example_projects(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    Query(query): Query<ExampleProjectsQuery>,
) -> Result<Json<Vec<IntegrationExampleProjectSummary>>, ApiError> {
    authorize_workspace_scope(&auth, workspace_id, POLICY_PROVIDERS_ADMIN).await?;

    let requested = query.limit.unwrap_or(DEFAULT_EXAMPLE_LIMIT);
    let limit = requested.clamp(1, MAX_EXAMPLE_LIMIT);
    let projects = load_example_projects(&state, workspace_id, limit).await?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %workspace_id,
        requested_limit = requested,
        effective_limit = limit,
        project_count = projects.len(),
        "listed integrations example projects",
    );

    Ok(Json(projects))
}

async fn revoke_workspace_token(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((workspace_id, id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    authorize_workspace_scope(&auth, workspace_id, POLICY_WORKSPACE_ADMIN).await?;

    let row = repositories::get_api_token_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            error!(
                auth_token_id = %auth.token_id,
                workspace_id = %workspace_id,
                api_token_id = %id,
                ?error,
                "failed to load workspace token for revoke",
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("api_token {id} not found")))?;

    if row.workspace_id != Some(workspace_id) {
        warn!(
            auth_token_id = %auth.token_id,
            workspace_id = %workspace_id,
            api_token_id = %id,
            token_workspace_id = ?row.workspace_id,
            "rejecting workspace token revoke for mismatched workspace",
        );
        return Err(ApiError::NotFound(format!(
            "api_token {id} not found in workspace {workspace_id}"
        )));
    }

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %workspace_id,
        api_token_id = %row.id,
        token_kind = %row.token_kind,
        previous_status = %row.status,
        "accepted workspace token revoke request",
    );

    repositories::revoke_api_token(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            error!(
                auth_token_id = %auth.token_id,
                workspace_id = %workspace_id,
                api_token_id = %id,
                ?error,
                "failed to revoke workspace token",
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("api_token {id} not found")))?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %workspace_id,
        api_token_id = %row.id,
        token_kind = %row.token_kind,
        "revoked workspace token",
    );

    Ok(StatusCode::NO_CONTENT)
}

async fn get_integrations_product(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<IntegrationsProductResponse>, ApiError> {
    authorize_workspace_scope(&auth, workspace_id, POLICY_PROVIDERS_ADMIN).await?;

    let provider_accounts =
        repositories::list_provider_accounts(&state.persistence.postgres, Some(workspace_id))
            .await
            .map_err(|error| {
                error!(auth_token_id = %auth.token_id, workspace_id = %workspace_id, ?error, "failed to list provider accounts for integrations product");
                ApiError::Internal
            })?
            .into_iter()
            .map(|row| ProviderAccountSummary {
                id: row.id,
                workspace_id: row.workspace_id,
                provider_kind: row.provider_kind,
                label: row.label,
                status: row.status,
            })
            .collect::<Vec<_>>();

    let model_profiles =
        repositories::list_model_profiles(&state.persistence.postgres, Some(workspace_id))
            .await
            .map_err(|error| {
                error!(auth_token_id = %auth.token_id, workspace_id = %workspace_id, ?error, "failed to list model profiles for integrations product");
                ApiError::Internal
            })?
            .into_iter()
            .map(|row| ModelProfileSummary {
                id: row.id,
                workspace_id: row.workspace_id,
                provider_account_id: row.provider_account_id,
                profile_kind: row.profile_kind,
                model_name: row.model_name,
            })
            .collect::<Vec<_>>();

    let projects = repositories::list_projects(&state.persistence.postgres, Some(workspace_id))
        .await
        .map_err(|error| {
            error!(auth_token_id = %auth.token_id, workspace_id = %workspace_id, ?error, "failed to list projects for integrations product");
            ApiError::Internal
        })?
        .into_iter()
        .map(|row| ProjectSummary {
            id: row.id,
            workspace_id: row.workspace_id,
            slug: row.slug,
            name: row.name,
            description: row.description,
        })
        .collect::<Vec<_>>();

    let tokens = load_workspace_tokens(&state, workspace_id).await?;

    let mut available_scopes =
        KNOWN_AVAILABLE_SCOPES.iter().map(|scope| (*scope).to_string()).collect::<Vec<_>>();
    for token in &tokens {
        for scope in &token.scopes {
            if !available_scopes.iter().any(|value| value == scope) {
                available_scopes.push(scope.clone());
            }
        }
    }
    available_scopes.sort();

    let warning = match (
        provider_accounts.is_empty(),
        model_profiles.is_empty(),
        tokens.is_empty(),
        projects.is_empty(),
    ) {
        (true, _, _, _) => Some("No provider accounts configured for this workspace yet.".into()),
        (false, true, _, _) => {
            Some("Provider accounts exist, but no model profiles are configured yet.".into())
        }
        (false, false, true, _) => Some(
            "The integrations surface is live, but this workspace has no visible API tokens yet."
                .into(),
        ),
        (false, false, false, true) => {
            Some("The integrations surface is live, but this workspace has no projects yet.".into())
        }
        _ => None,
    };

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %workspace_id,
        provider_account_count = provider_accounts.len(),
        model_profile_count = model_profiles.len(),
        project_count = projects.len(),
        token_count = tokens.len(),
        warning_present = warning.is_some(),
        "loaded integrations product snapshot",
    );

    Ok(Json(IntegrationsProductResponse {
        snapshot: IntegrationsProductSnapshot {
            workspace_id,
            provider_accounts,
            model_profiles,
            projects,
            available_scopes,
            generated_at: Utc::now(),
            warning,
        },
    }))
}

async fn load_example_projects(
    state: &AppState,
    workspace_id: Uuid,
    limit: usize,
) -> Result<Vec<IntegrationExampleProjectSummary>, ApiError> {
    let projects = repositories::list_projects(&state.persistence.postgres, Some(workspace_id))
        .await
        .map_err(|error| {
            error!(
                workspace_id = %workspace_id,
                limit,
                ?error,
                "failed to load integration example projects",
            );
            ApiError::Internal
        })?
        .into_iter()
        .take(limit)
        .map(|row| IntegrationExampleProjectSummary {
            id: row.id,
            workspace_id: row.workspace_id,
            slug: row.slug,
            name: row.name,
        })
        .collect();

    Ok(projects)
}

async fn load_workspace_tokens(
    state: &AppState,
    workspace_id: Uuid,
) -> Result<Vec<TokenSummary>, ApiError> {
    let tokens = repositories::list_api_tokens(&state.persistence.postgres, Some(workspace_id))
        .await
        .map_err(|error| {
            error!(
                workspace_id = %workspace_id,
                ?error,
                "failed to load workspace api tokens",
            );
            ApiError::Internal
        })?
        .into_iter()
        .map(map_token_summary)
        .collect();

    Ok(tokens)
}

fn map_token_summary(row: repositories::ApiTokenRow) -> TokenSummary {
    let scopes = serde_json::from_value(row.scope_json).unwrap_or_default();

    TokenSummary {
        id: row.id,
        workspace_id: row.workspace_id,
        token_kind: row.token_kind,
        label: row.label,
        status: row.status,
        scopes,
        last_used_at: row.last_used_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}
