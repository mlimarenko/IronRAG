use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    integrations::provider_catalog::supported_provider_catalog,
    interfaces::http::{auth::AuthContext, router_support::ApiError},
};

#[derive(Serialize)]
pub struct ProviderAccountSummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider_kind: String,
    pub label: String,
    pub status: String,
}

#[derive(Serialize)]
pub struct ModelProfileSummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider_account_id: Uuid,
    pub profile_kind: String,
    pub model_name: String,
}

#[derive(Serialize)]
pub struct ProviderGovernanceSummary {
    pub workspace_id: Uuid,
    pub provider_accounts: Vec<ProviderAccountSummary>,
    pub model_profiles: Vec<ModelProfileSummary>,
    pub warning: Option<String>,
}

#[derive(Deserialize)]
pub struct WorkspaceScopedQuery {
    pub workspace_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct CreateProviderAccountRequest {
    pub workspace_id: Uuid,
    pub provider_kind: String,
    pub label: String,
    pub api_base_url: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateModelProfileRequest {
    pub workspace_id: Uuid,
    pub provider_account_id: Uuid,
    pub profile_kind: String,
    pub model_name: String,
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<i32>,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/provider-accounts", get(list_provider_accounts).post(create_provider_account))
        .route("/model-profiles", get(list_model_profiles).post(create_model_profile))
        .route("/provider-governance/{workspace_id}", get(get_provider_governance))
}

async fn list_provider_accounts(
    State(state): State<AppState>,
    Query(query): Query<WorkspaceScopedQuery>,
) -> Result<Json<Vec<ProviderAccountSummary>>, ApiError> {
    let items =
        repositories::list_provider_accounts(&state.persistence.postgres, query.workspace_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .into_iter()
            .map(|row| ProviderAccountSummary {
                id: row.id,
                workspace_id: row.workspace_id,
                provider_kind: row.provider_kind,
                label: row.label,
                status: row.status,
            })
            .collect();

    Ok(Json(items))
}

async fn create_provider_account(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateProviderAccountRequest>,
) -> Result<Json<ProviderAccountSummary>, ApiError> {
    auth.require_any_scope(&["providers:admin", "workspace:admin"])?;
    auth.require_workspace_access(payload.workspace_id)?;
    let supported_kinds =
        supported_provider_catalog(&state.settings, &state.runtime_provider_defaults)
            .into_iter()
            .map(|entry| entry.provider_kind.as_str())
            .collect::<Vec<_>>();
    if !supported_kinds.iter().any(|kind| kind == &payload.provider_kind.trim()) {
        return Err(ApiError::BadRequest(format!(
            "unsupported provider kind: {}",
            payload.provider_kind
        )));
    }

    let row = repositories::create_provider_account(
        &state.persistence.postgres,
        payload.workspace_id,
        &payload.provider_kind,
        &payload.label,
        payload.api_base_url.as_deref(),
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(ProviderAccountSummary {
        id: row.id,
        workspace_id: row.workspace_id,
        provider_kind: row.provider_kind,
        label: row.label,
        status: row.status,
    }))
}

async fn list_model_profiles(
    State(state): State<AppState>,
    Query(query): Query<WorkspaceScopedQuery>,
) -> Result<Json<Vec<ModelProfileSummary>>, ApiError> {
    let items = repositories::list_model_profiles(&state.persistence.postgres, query.workspace_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|row| ModelProfileSummary {
            id: row.id,
            workspace_id: row.workspace_id,
            provider_account_id: row.provider_account_id,
            profile_kind: row.profile_kind,
            model_name: row.model_name,
        })
        .collect();

    Ok(Json(items))
}

async fn create_model_profile(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateModelProfileRequest>,
) -> Result<Json<ModelProfileSummary>, ApiError> {
    auth.require_any_scope(&["providers:admin", "workspace:admin"])?;
    auth.require_workspace_access(payload.workspace_id)?;

    let provider_account = repositories::get_provider_account_by_id(
        &state.persistence.postgres,
        payload.provider_account_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?
    .ok_or_else(|| ApiError::NotFound("provider_account not found".into()))?;

    if provider_account.workspace_id != payload.workspace_id {
        return Err(ApiError::BadRequest(
            "provider_account_id does not belong to workspace_id".into(),
        ));
    }

    let row = repositories::create_model_profile(
        &state.persistence.postgres,
        payload.workspace_id,
        payload.provider_account_id,
        &payload.profile_kind,
        &payload.model_name,
        payload.temperature,
        payload.max_output_tokens,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(ModelProfileSummary {
        id: row.id,
        workspace_id: row.workspace_id,
        provider_account_id: row.provider_account_id,
        profile_kind: row.profile_kind,
        model_name: row.model_name,
    }))
}

async fn get_provider_governance(
    auth: AuthContext,
    State(state): State<AppState>,
    axum::extract::Path(workspace_id): axum::extract::Path<Uuid>,
) -> Result<Json<ProviderGovernanceSummary>, ApiError> {
    auth.require_any_scope(&["providers:admin", "workspace:admin"])?;
    auth.require_workspace_access(workspace_id)?;

    let provider_accounts =
        repositories::list_provider_accounts(&state.persistence.postgres, Some(workspace_id))
            .await
            .map_err(|_| ApiError::Internal)?
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
            .map_err(|_| ApiError::Internal)?
            .into_iter()
            .map(|row| ModelProfileSummary {
                id: row.id,
                workspace_id: row.workspace_id,
                provider_account_id: row.provider_account_id,
                profile_kind: row.profile_kind,
                model_name: row.model_name,
            })
            .collect::<Vec<_>>();

    let supported_provider_kinds =
        supported_provider_catalog(&state.settings, &state.runtime_provider_defaults)
            .into_iter()
            .map(|entry| entry.provider_kind.as_str().to_string())
            .collect::<Vec<_>>();
    let warning = if provider_accounts.is_empty() {
        Some("No provider accounts configured for this workspace yet".to_string())
    } else if model_profiles.is_empty() {
        Some("Provider accounts exist, but no model profiles are configured yet".to_string())
    } else if provider_accounts
        .iter()
        .any(|row| !supported_provider_kinds.iter().any(|kind| kind == &row.provider_kind))
    {
        Some(format!(
            "Only {} providers are supported by the runtime graph pipeline",
            supported_provider_kinds.join(", ")
        ))
    } else {
        None
    };

    Ok(Json(ProviderGovernanceSummary { workspace_id, provider_accounts, model_profiles, warning }))
}
