use axum::{
    Json, Router,
    extract::{Path, State},
};
use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ui_admin::{
        AdminMemberModel, AdminOverviewModel, AdminPricingCatalogEntryModel,
        AdminProviderProfileModel, AdminProviderValidationCheckModel, AdminProviderValidationModel,
        AdminSettingItemModel, AdminSupportedProviderModel, ApiTokenRowModel,
        CreateApiTokenResultModel, LibraryAccessRowModel,
    },
    domains::usage_governance::PricingCoverageSummary,
    infra::{repositories, ui_queries},
    interfaces::http::{
        auth::{hash_token, mint_plaintext_token, preview_token},
        router_support::ApiError,
        runtime_providers,
        ui_support::{UiSessionContext, load_active_ui_context, require_admin_role},
    },
    services::{pricing_catalog, runtime_ingestion},
};

#[derive(Debug, Deserialize)]
struct CreateApiTokenRequest {
    label: String,
    scopes: Vec<String>,
    expires_in_days: Option<i64>,
}

#[derive(Debug, Serialize)]
struct ApiTokensResponse {
    rows: Vec<ApiTokenRowModel>,
}

#[derive(Debug, Serialize)]
struct MembersResponse {
    rows: Vec<AdminMemberModel>,
}

#[derive(Debug, Serialize)]
struct LibraryAccessResponse {
    rows: Vec<LibraryAccessRowModel>,
}

#[derive(Debug, Serialize)]
struct SettingsResponse {
    items: Vec<AdminSettingItemModel>,
    provider_catalog: Vec<AdminSupportedProviderModel>,
    provider_profile: AdminProviderProfileModel,
    provider_validation: AdminProviderValidationModel,
    pricing_catalog: Vec<AdminPricingCatalogEntryModel>,
    pricing_coverage: PricingCoverageSummary,
    live_validation_enabled: bool,
    supported_provider_kinds: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProviderProfileRequest {
    indexing_provider_kind: String,
    indexing_model_name: String,
    embedding_provider_kind: String,
    embedding_model_name: String,
    answer_provider_kind: String,
    answer_model_name: String,
    vision_provider_kind: String,
    vision_model_name: String,
}

#[derive(Debug, Serialize)]
struct ProviderProfileResponse {
    profile: AdminProviderProfileModel,
}

#[derive(Debug, Serialize)]
struct ProviderValidationResponse {
    profile: AdminProviderProfileModel,
    validation: AdminProviderValidationModel,
}

fn map_token_row(row: repositories::ApiTokenRow) -> ApiTokenRowModel {
    let scopes = serde_json::from_value::<Vec<String>>(row.scope_json).unwrap_or_default();
    ApiTokenRowModel {
        id: row.id.to_string(),
        label: row.label,
        masked_token: row.token_preview.unwrap_or_else(|| "Stored token".to_string()),
        scopes,
        created_at: row.created_at.to_rfc3339(),
        last_used_at: row.last_used_at.map(|value| value.to_rfc3339()),
        expires_at: row.expires_at.map(|value| value.to_rfc3339()),
        can_revoke: row.status == "active",
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ui/admin/overview", axum::routing::get(get_admin_overview))
        .route("/ui/admin/api-tokens", axum::routing::get(list_api_tokens).post(create_api_token))
        .route("/ui/admin/api-tokens/{id}", axum::routing::delete(revoke_api_token))
        .route("/ui/admin/members", axum::routing::get(list_members))
        .route("/ui/admin/library-access", axum::routing::get(list_library_access))
        .route("/ui/admin/settings", axum::routing::get(get_settings))
        .route("/ui/admin/settings/provider-profile", axum::routing::put(update_provider_profile))
        .route(
            "/ui/admin/settings/provider-profile/validate",
            axum::routing::post(validate_provider_profile),
        )
}

async fn get_admin_overview(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
) -> Result<Json<AdminOverviewModel>, ApiError> {
    require_admin_role(&ui_session.role_label)?;
    let active = load_active_ui_context(&state, &ui_session).await?;
    let overview = ui_queries::load_admin_overview(&state.persistence.postgres, &active.workspace)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(overview))
}

async fn list_api_tokens(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
) -> Result<Json<ApiTokensResponse>, ApiError> {
    require_admin_role(&ui_session.role_label)?;
    let active = load_active_ui_context(&state, &ui_session).await?;
    let rows = ui_queries::load_admin_api_tokens(&state.persistence.postgres, active.workspace.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(ApiTokensResponse { rows }))
}

async fn create_api_token(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateApiTokenRequest>,
) -> Result<Json<CreateApiTokenResultModel>, ApiError> {
    require_admin_role(&ui_session.role_label)?;
    let active = load_active_ui_context(&state, &ui_session).await?;
    let label = payload.label.trim();
    if label.is_empty() {
        return Err(ApiError::BadRequest("token label is required".into()));
    }
    if payload.scopes.is_empty() {
        return Err(ApiError::BadRequest("at least one token scope is required".into()));
    }

    let plaintext_token = mint_plaintext_token();
    let token_hash = hash_token(&plaintext_token);
    let token_preview = preview_token(&plaintext_token);
    let scope_json = serde_json::to_value(&payload.scopes).map_err(|_| ApiError::Internal)?;
    let expires_at = payload
        .expires_in_days
        .and_then(|days| if days <= 0 { None } else { Some(Utc::now() + Duration::days(days)) });

    let row = repositories::create_api_token(
        &state.persistence.postgres,
        Some(active.workspace.id),
        "workspace_token",
        label,
        &token_hash,
        Some(&token_preview),
        scope_json,
        expires_at,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    info!(
        user_id = %ui_session.user_id,
        workspace_id = %active.workspace.id,
        api_token_id = %row.id,
        "created ui admin api token"
    );

    Ok(Json(CreateApiTokenResultModel { row: map_token_row(row), plaintext_token }))
}

async fn revoke_api_token(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiTokenRowModel>, ApiError> {
    require_admin_role(&ui_session.role_label)?;
    let active = load_active_ui_context(&state, &ui_session).await?;
    let row = repositories::get_api_token_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::NotFound(format!("api token {id} not found")))?;

    if row.workspace_id != Some(active.workspace.id) {
        return Err(ApiError::NotFound(format!("api token {id} not found")));
    }

    let revoked = repositories::revoke_api_token(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            error!(api_token_id = %id, ?error, "failed to revoke ui admin token");
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("api token {id} not found")))?;

    Ok(Json(map_token_row(revoked)))
}

async fn list_members(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
) -> Result<Json<MembersResponse>, ApiError> {
    require_admin_role(&ui_session.role_label)?;
    let active = load_active_ui_context(&state, &ui_session).await?;
    let rows = ui_queries::load_admin_members(&state.persistence.postgres, active.workspace.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(MembersResponse { rows }))
}

async fn list_library_access(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
) -> Result<Json<LibraryAccessResponse>, ApiError> {
    require_admin_role(&ui_session.role_label)?;
    let active = load_active_ui_context(&state, &ui_session).await?;
    let rows =
        ui_queries::load_admin_library_access(&state.persistence.postgres, active.workspace.id)
            .await
            .map_err(|_| ApiError::Internal)?;
    Ok(Json(LibraryAccessResponse { rows }))
}

async fn get_settings(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
) -> Result<Json<SettingsResponse>, ApiError> {
    require_admin_role(&ui_session.role_label)?;
    let active = load_active_ui_context(&state, &ui_session).await?;
    let provider_profile_row =
        runtime_providers::load_or_create_provider_profile_row(&state, active.project.id).await?;
    let provider_catalog = runtime_providers::map_supported_provider_catalog(&state)
        .into_iter()
        .map(|entry| AdminSupportedProviderModel {
            provider_kind: entry.provider_kind,
            supported_capabilities: entry.supported_capabilities,
            default_models: entry.default_models,
            available_models: entry.available_models,
            is_configured: entry.is_configured,
        })
        .collect::<Vec<_>>();
    let items = ui_queries::build_admin_settings_items(
        &active.workspace,
        &state.ui_runtime.default_locale,
        &state.ui_runtime.frontend_origin,
        state.ui_session_cookie.ttl_hours,
        state.ui_runtime.upload_max_size_mb,
    );
    let provider_profile = map_admin_provider_profile(&active.project.name, &provider_profile_row);
    let provider_validation = AdminProviderValidationModel {
        status: provider_profile.last_validation_status.clone(),
        checked_at: provider_profile.last_validated_at.clone(),
        error: provider_profile.last_validation_error.clone(),
        checks: Vec::new(),
    };
    let pricing_catalog = pricing_catalog::list_pricing_entries(&state, None)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .filter(|row| row.workspace_id.is_none() || row.workspace_id == Some(active.workspace.id))
        .map(map_admin_pricing_catalog_entry)
        .collect::<Vec<_>>();
    let pricing_coverage =
        build_pricing_coverage_summary(&state, active.workspace.id, &provider_profile_row).await?;
    let supported_provider_kinds =
        provider_catalog.iter().map(|entry| entry.provider_kind.clone()).collect::<Vec<_>>();

    Ok(Json(SettingsResponse {
        items,
        provider_catalog,
        provider_profile,
        provider_validation,
        pricing_catalog,
        pricing_coverage,
        live_validation_enabled: state.graph_runtime.live_validation_enabled,
        supported_provider_kinds,
    }))
}

async fn update_provider_profile(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Json(payload): Json<UpdateProviderProfileRequest>,
) -> Result<Json<ProviderProfileResponse>, ApiError> {
    require_admin_role(&ui_session.role_label)?;
    let active = load_active_ui_context(&state, &ui_session).await?;
    let normalized = runtime_providers::normalize_provider_profile_fields(
        &payload.indexing_provider_kind,
        &payload.indexing_model_name,
        &payload.embedding_provider_kind,
        &payload.embedding_model_name,
        &payload.answer_provider_kind,
        &payload.answer_model_name,
        &payload.vision_provider_kind,
        &payload.vision_model_name,
    )?;
    runtime_providers::ensure_profile_selection_available(
        &state,
        normalized.indexing_provider_kind,
        crate::integrations::provider_catalog::ROLE_INDEXING,
        &normalized.indexing_model_name,
        "indexingProviderKind",
        "indexingModelName",
    )?;
    runtime_providers::ensure_profile_selection_available(
        &state,
        normalized.embedding_provider_kind,
        crate::integrations::provider_catalog::ROLE_EMBEDDING,
        &normalized.embedding_model_name,
        "embeddingProviderKind",
        "embeddingModelName",
    )?;
    runtime_providers::ensure_profile_selection_available(
        &state,
        normalized.answer_provider_kind,
        crate::integrations::provider_catalog::ROLE_ANSWER,
        &normalized.answer_model_name,
        "answerProviderKind",
        "answerModelName",
    )?;
    runtime_providers::ensure_profile_selection_available(
        &state,
        normalized.vision_provider_kind,
        crate::integrations::provider_catalog::ROLE_VISION,
        &normalized.vision_model_name,
        "visionProviderKind",
        "visionModelName",
    )?;
    let row = repositories::upsert_runtime_provider_profile(
        &state.persistence.postgres,
        active.project.id,
        normalized.indexing_provider_kind.as_str(),
        &normalized.indexing_model_name,
        normalized.embedding_provider_kind.as_str(),
        &normalized.embedding_model_name,
        normalized.answer_provider_kind.as_str(),
        &normalized.answer_model_name,
        normalized.vision_provider_kind.as_str(),
        &normalized.vision_model_name,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(ProviderProfileResponse {
        profile: map_admin_provider_profile(&active.project.name, &row),
    }))
}

async fn validate_provider_profile(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
) -> Result<Json<ProviderValidationResponse>, ApiError> {
    require_admin_role(&ui_session.role_label)?;
    let active = load_active_ui_context(&state, &ui_session).await?;
    let result =
        runtime_providers::validate_library_provider_profile(&state, active.project.id).await?;

    Ok(Json(ProviderValidationResponse {
        profile: map_admin_provider_profile_from_runtime(&active.project.name, &result.profile),
        validation: AdminProviderValidationModel {
            status: Some(result.status),
            checked_at: Some(
                runtime_providers::latest_validation_timestamp(&result.checks)
                    .unwrap_or_else(Utc::now)
                    .to_rfc3339(),
            ),
            error: result.checks.iter().find_map(|check| check.error.clone()),
            checks: result
                .checks
                .into_iter()
                .map(|check| AdminProviderValidationCheckModel {
                    provider_kind: check.provider_kind,
                    model_name: check.model_name,
                    capability: check.capability,
                    status: check.status,
                    checked_at: check.checked_at,
                    error: check.error,
                })
                .collect(),
        },
    }))
}

fn map_admin_provider_profile(
    library_name: &str,
    row: &repositories::RuntimeProviderProfileRow,
) -> AdminProviderProfileModel {
    AdminProviderProfileModel {
        library_id: row.project_id.to_string(),
        library_name: library_name.to_string(),
        indexing_provider_kind: row.indexing_provider_kind.clone(),
        indexing_model_name: row.indexing_model_name.clone(),
        embedding_provider_kind: row.embedding_provider_kind.clone(),
        embedding_model_name: row.embedding_model_name.clone(),
        answer_provider_kind: row.answer_provider_kind.clone(),
        answer_model_name: row.answer_model_name.clone(),
        vision_provider_kind: row.vision_provider_kind.clone(),
        vision_model_name: row.vision_model_name.clone(),
        last_validated_at: row.last_validated_at.map(|value| value.to_rfc3339()),
        last_validation_status: row.last_validation_status.clone(),
        last_validation_error: row.last_validation_error.clone(),
    }
}

fn map_admin_provider_profile_from_runtime(
    library_name: &str,
    profile: &runtime_providers::LibraryProviderProfileResponse,
) -> AdminProviderProfileModel {
    AdminProviderProfileModel {
        library_id: profile.library_id.to_string(),
        library_name: library_name.to_string(),
        indexing_provider_kind: profile.indexing_provider_kind.clone(),
        indexing_model_name: profile.indexing_model_name.clone(),
        embedding_provider_kind: profile.embedding_provider_kind.clone(),
        embedding_model_name: profile.embedding_model_name.clone(),
        answer_provider_kind: profile.answer_provider_kind.clone(),
        answer_model_name: profile.answer_model_name.clone(),
        vision_provider_kind: profile.vision_provider_kind.clone(),
        vision_model_name: profile.vision_model_name.clone(),
        last_validated_at: profile.last_validated_at.clone(),
        last_validation_status: profile.last_validation_status.clone(),
        last_validation_error: profile.last_validation_error.clone(),
    }
}

fn map_admin_pricing_catalog_entry(
    row: repositories::ModelPricingCatalogEntryRow,
) -> AdminPricingCatalogEntryModel {
    AdminPricingCatalogEntryModel {
        id: row.id.to_string(),
        workspace_id: row.workspace_id.map(|value| value.to_string()),
        provider_kind: row.provider_kind,
        model_name: row.model_name,
        capability: row.capability,
        billing_unit: row.billing_unit,
        input_price: row.input_price.map(decimal_to_string),
        output_price: row.output_price.map(decimal_to_string),
        currency: row.currency,
        status: row.status,
        source_kind: row.source_kind,
        note: row.note,
        effective_from: row.effective_from.to_rfc3339(),
        effective_to: row.effective_to.map(|value| value.to_rfc3339()),
    }
}

async fn build_pricing_coverage_summary(
    state: &AppState,
    workspace_id: Uuid,
    profile_row: &repositories::RuntimeProviderProfileRow,
) -> Result<PricingCoverageSummary, ApiError> {
    let effective_profile = runtime_ingestion::map_runtime_provider_profile_row(profile_row);
    pricing_catalog::build_pricing_coverage_summary(state, workspace_id, &effective_profile)
        .await
        .map_err(|_| ApiError::Internal)
}

fn decimal_to_string(value: Decimal) -> String {
    value.normalize().to_string()
}
