use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroize as _;

use crate::{
    app::state::AppState,
    domains::ai::{
        AiAccount, AiBinding, AiBindingPurpose, AiScopeKind, BindingValidation,
        ModelAvailabilityState, PriceCatalogEntry, ProviderCatalogEntry, ResolvedModelCatalogEntry,
    },
    domains::provider_profiles::{
        ProviderBaseUrlPolicy, ProviderCapabilities, ProviderCredentialPolicy,
        ProviderModelDiscovery, ProviderRuntimeProfile,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_MCP_DISCOVERY, POLICY_PROVIDERS_ADMIN, authorize_workspace_discovery,
            authorize_workspace_permission, load_library_and_authorize,
        },
        router_support::{ApiError, RequestId},
    },
    services::ai_catalog_service::{
        AiAccountSummary, AiScopeRef, CreateAiAccountCommand, CreateAiBindingCommand,
        CreateBindingValidationCommand, CreateModelCatalogCommand, CreateProviderCatalogCommand,
        CreateWorkspacePriceOverrideCommand, UpdateAiAccountCommand, UpdateAiBindingCommand,
        UpdateModelCatalogCommand, UpdateProviderCatalogCommand,
        UpdateWorkspacePriceOverrideCommand,
    },
    services::iam::audit::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
};

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ModelsQuery {
    pub provider_catalog_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub account_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct PricesQuery {
    pub model_catalog_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AiScopeQuery {
    pub scope_kind: Option<AiScopeKind>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateProviderCatalogRequest {
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub default_base_url: Option<String>,
    pub capability_flags_json: serde_json::Value,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProviderCatalogRequest {
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub default_base_url: Option<String>,
    pub capability_flags_json: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateModelCatalogRequest {
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub capability_kind: String,
    pub modality_kind: String,
    pub lifecycle_state: String,
    #[serde(default)]
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    #[serde(default)]
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateModelCatalogRequest {
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub capability_kind: String,
    pub modality_kind: String,
    pub lifecycle_state: String,
    #[serde(default)]
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub metadata_json: Option<serde_json::Value>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateAiAccountRequest {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

impl Drop for CreateAiAccountRequest {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for CreateAiAccountRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreateAiAccountRequest")
            .field("scope_kind", &self.scope_kind)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("provider_catalog_id", &self.provider_catalog_id)
            .field("label", &self.label)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAiAccountRequest {
    pub label: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub credential_state: String,
}

impl Drop for UpdateAiAccountRequest {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for UpdateAiAccountRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpdateAiAccountRequest")
            .field("label", &self.label)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .field("credential_state", &self.credential_state)
            .finish()
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkspacePriceOverrideRequest {
    pub workspace_id: Uuid,
    pub model_catalog_id: Uuid,
    pub billing_unit: String,
    pub unit_price: Decimal,
    pub currency_code: String,
    pub effective_from: chrono::DateTime<chrono::Utc>,
    pub effective_to: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateWorkspacePriceOverrideRequest {
    pub model_catalog_id: Uuid,
    pub billing_unit: String,
    pub unit_price: Decimal,
    pub currency_code: String,
    pub effective_from: chrono::DateTime<chrono::Utc>,
    pub effective_to: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateAiBindingRequest {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub binding_purpose: AiBindingPurpose,
    pub account_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    #[serde(default)]
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAiBindingRequest {
    pub account_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    #[serde(default)]
    pub extra_parameters_json: serde_json::Value,
    pub binding_state: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogEntryResponse {
    pub id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub default_base_url: Option<String>,
    pub api_key_required: bool,
    pub base_url_required: bool,
    pub credential_policy: ProviderCredentialPolicy,
    pub base_url_policy: ProviderBaseUrlPolicy,
    pub model_discovery: ProviderModelDiscovery,
    pub capabilities: ProviderCapabilities,
    pub runtime: ProviderRuntimeProfile,
    pub ui_hints: serde_json::Value,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntryResponse {
    pub id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub capability_kind: String,
    pub modality_kind: String,
    pub lifecycle_state: String,
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub availability_state: ModelAvailabilityState,
    pub available_account_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PriceCatalogEntryResponse {
    pub id: Uuid,
    pub model_catalog_id: Uuid,
    pub billing_unit: String,
    pub price_variant_key: String,
    pub request_input_tokens_min: Option<i32>,
    pub request_input_tokens_max: Option<i32>,
    pub unit_price: rust_decimal::Decimal,
    pub currency_code: String,
    pub effective_from: chrono::DateTime<chrono::Utc>,
    pub effective_to: Option<chrono::DateTime<chrono::Utc>>,
    pub catalog_scope: String,
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiAccountResponse {
    pub id: Uuid,
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub base_url: Option<String>,
    pub api_key_summary: String,
    pub credential_state: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiBindingResponse {
    pub id: Uuid,
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub binding_purpose: AiBindingPurpose,
    pub account_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
    pub binding_state: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BindingValidationResponse {
    pub id: Uuid,
    pub binding_id: Uuid,
    pub validation_state: String,
    pub checked_at: chrono::DateTime<chrono::Utc>,
    pub failure_code: Option<String>,
    pub message: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ai/providers", get(list_providers).post(create_provider))
        .route(
            "/ai/providers/{provider_id}",
            get(get_provider).patch(update_provider).delete(delete_provider),
        )
        .route("/ai/models", get(list_models).post(create_model))
        .route("/ai/models/{model_id}", get(get_model).patch(update_model).delete(delete_model))
        .route("/ai/prices", get(list_prices))
        .route("/ai/price-overrides", post(create_price_override))
        .route(
            "/ai/price-overrides/{price_id}",
            get(get_price_override).patch(update_price_override).delete(delete_price_override),
        )
        .route("/ai/accounts", get(list_accounts).post(create_account))
        .route(
            "/ai/accounts/{account_id}",
            get(get_account).patch(update_account).delete(delete_account),
        )
        .route("/ai/bindings", get(list_bindings).post(create_binding))
        .route(
            "/ai/bindings/{binding_id}",
            get(get_binding).patch(update_binding).delete(delete_binding),
        )
        .route(
            "/ai/bindings/{binding_id}/validations",
            get(list_binding_validations).post(create_binding_validation),
        )
        .route("/ai/bindings/{binding_id}/validations/{validation_id}", get(get_binding_validation))
}

/// Builds a sync-create `201 Created` response with a `Location` header
/// pointing at the newly created resource's own item-read endpoint (see
/// router_support conventions established by the Content domain reference
/// implementation).
fn created_response<T: Serialize>(location: &str, body: T) -> axum::response::Response {
    let mut response = (StatusCode::CREATED, Json(body)).into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}

#[utoipa::path(
    get,
    path = "/v1/ai/providers",
    tag = "ai",
    operation_id = "listAiProviders",
    responses(
        (status = 200, description = "Available AI providers", body = [ProviderCatalogEntryResponse]),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.list_providers", skip_all)]
pub async fn list_providers(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<Vec<ProviderCatalogEntryResponse>>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let entries = state.canonical_services.ai_catalog.list_provider_catalog(&state).await?;
    Ok(Json(entries.into_iter().map(map_provider).collect()))
}

#[utoipa::path(
    get,
    path = "/v1/ai/models",
    tag = "ai",
    operation_id = "listAiModels",
    params(ModelsQuery),
    responses(
        (status = 200, description = "Resolved AI models for the requested context", body = [ModelCatalogEntryResponse]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot view the requested workspace/library"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_models",
    skip_all,
    fields(workspace_id = ?query.workspace_id, library_id = ?query.library_id)
)]
pub async fn list_models(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ModelsQuery>,
) -> Result<Json<Vec<ModelCatalogEntryResponse>>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let (workspace_id, library_id) = if query.workspace_id.is_some()
        || query.library_id.is_some()
        || query.account_id.is_some()
    {
        authorize_visible_ai_context(&auth, &state, query.workspace_id, query.library_id).await?
    } else {
        (None, None)
    };
    let entries = state
        .canonical_services
        .ai_catalog
        .list_resolved_model_catalog(
            &state,
            query.provider_catalog_id,
            workspace_id,
            library_id,
            query.account_id,
        )
        .await?;
    Ok(Json(entries.into_iter().map(map_model).collect()))
}

#[utoipa::path(
    post,
    path = "/v1/ai/providers",
    tag = "ai",
    operation_id = "createAiProvider",
    request_body = CreateProviderCatalogRequest,
    responses(
        (status = 201, description = "Newly created AI provider catalog entry", body = ProviderCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.create_provider", skip_all)]
pub async fn create_provider(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateProviderCatalogRequest>,
) -> Result<axum::response::Response, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry = state
        .canonical_services
        .ai_catalog
        .create_provider_catalog(
            &state,
            CreateProviderCatalogCommand {
                provider_kind: payload.provider_kind,
                display_name: payload.display_name,
                api_style: payload.api_style,
                lifecycle_state: payload.lifecycle_state,
                default_base_url: payload.default_base_url,
                capability_flags_json: payload.capability_flags_json,
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.provider_catalog.create",
        "succeeded",
        Some(format!("AI provider {} created", entry.display_name)),
        Some(format!(
            "principal {} created AI provider catalog entry {}",
            auth.principal_id, entry.id
        )),
        vec![instance_subject("provider_catalog", entry.id)],
    )
    .await;
    Ok(created_response(&format!("/v1/ai/providers/{}", entry.id), map_provider(entry)))
}

#[utoipa::path(
    get,
    path = "/v1/ai/providers/{providerId}",
    tag = "ai",
    operation_id = "getAiProvider",
    params(("providerId" = uuid::Uuid, Path, description = "AI provider catalog identifier")),
    responses(
        (status = 200, description = "AI provider catalog entry", body = ProviderCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "Provider not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_provider", skip_all, fields(provider_id = %provider_id))]
pub async fn get_provider(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(provider_id): Path<Uuid>,
) -> Result<Json<ProviderCatalogEntryResponse>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let entry =
        state.canonical_services.ai_catalog.get_provider_catalog(&state, provider_id).await?;
    Ok(Json(map_provider(entry)))
}

#[utoipa::path(
    patch,
    path = "/v1/ai/providers/{providerId}",
    tag = "ai",
    operation_id = "updateAiProvider",
    params(("providerId" = uuid::Uuid, Path, description = "AI provider catalog identifier")),
    request_body = UpdateProviderCatalogRequest,
    responses(
        (status = 200, description = "Updated AI provider catalog entry", body = ProviderCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "Provider not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.update_provider", skip_all, fields(provider_id = %provider_id))]
pub async fn update_provider(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(provider_id): Path<Uuid>,
    Json(payload): Json<UpdateProviderCatalogRequest>,
) -> Result<Json<ProviderCatalogEntryResponse>, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry = state
        .canonical_services
        .ai_catalog
        .update_provider_catalog(
            &state,
            UpdateProviderCatalogCommand {
                provider_id,
                provider_kind: payload.provider_kind,
                display_name: payload.display_name,
                api_style: payload.api_style,
                lifecycle_state: payload.lifecycle_state,
                default_base_url: payload.default_base_url,
                capability_flags_json: payload.capability_flags_json,
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.provider_catalog.update",
        "succeeded",
        Some(format!("AI provider {} updated", entry.display_name)),
        Some(format!(
            "principal {} updated AI provider catalog entry {}",
            auth.principal_id, entry.id
        )),
        vec![instance_subject("provider_catalog", entry.id)],
    )
    .await;
    Ok(Json(map_provider(entry)))
}

/// Real hard delete; 409 when a model still references this provider. The
/// reversible pause is `PATCH {lifecycleState:"disabled"}` above — the two
/// are distinct operations (obtained by not deleting FK references but
/// blocking on them), not two paths to the same result.
#[utoipa::path(
    delete,
    path = "/v1/ai/providers/{providerId}",
    tag = "ai",
    operation_id = "deleteAiProvider",
    params(("providerId" = uuid::Uuid, Path, description = "AI provider catalog identifier")),
    responses(
        (status = 204, description = "Provider catalog entry deleted"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "Provider not found"),
        (status = 409, description = "Provider is still referenced by a model"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.delete_provider", skip_all, fields(provider_id = %provider_id))]
pub async fn delete_provider(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(provider_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry =
        state.canonical_services.ai_catalog.get_provider_catalog(&state, provider_id).await?;
    state.canonical_services.ai_catalog.delete_provider_catalog(&state, provider_id).await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.provider_catalog.delete",
        "succeeded",
        Some(format!("AI provider {} deleted", entry.display_name)),
        Some(format!(
            "principal {} deleted AI provider catalog entry {}",
            auth.principal_id, entry.id
        )),
        vec![instance_subject("provider_catalog", entry.id)],
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/v1/ai/models",
    tag = "ai",
    operation_id = "createAiModel",
    request_body = CreateModelCatalogRequest,
    responses(
        (status = 201, description = "Newly created AI model catalog entry", body = ModelCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.create_model", skip_all)]
pub async fn create_model(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateModelCatalogRequest>,
) -> Result<axum::response::Response, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry = state
        .canonical_services
        .ai_catalog
        .create_model_catalog(
            &state,
            CreateModelCatalogCommand {
                provider_catalog_id: payload.provider_catalog_id,
                model_name: payload.model_name,
                capability_kind: payload.capability_kind,
                modality_kind: payload.modality_kind,
                lifecycle_state: payload.lifecycle_state,
                allowed_binding_purposes: payload.allowed_binding_purposes,
                context_window: payload.context_window,
                max_output_tokens: payload.max_output_tokens,
                metadata_json: payload.metadata_json,
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.model_catalog.create",
        "succeeded",
        Some(format!("AI model {} created", entry.model_name)),
        Some(format!(
            "principal {} created AI model catalog entry {}",
            auth.principal_id, entry.id
        )),
        vec![instance_subject("model_catalog", entry.id)],
    )
    .await;
    let model_id = entry.id;
    let resolved =
        state.canonical_services.ai_catalog.get_resolved_model_catalog(&state, model_id).await?;
    Ok(created_response(&format!("/v1/ai/models/{model_id}"), map_model(resolved)))
}

#[utoipa::path(
    get,
    path = "/v1/ai/models/{modelId}",
    tag = "ai",
    operation_id = "getAiModel",
    params(("modelId" = uuid::Uuid, Path, description = "AI model catalog identifier")),
    responses(
        (status = 200, description = "AI model catalog entry with its resolved availability", body = ModelCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "Model not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_model", skip_all, fields(model_id = %model_id))]
pub async fn get_model(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(model_id): Path<Uuid>,
) -> Result<Json<ModelCatalogEntryResponse>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let resolved =
        state.canonical_services.ai_catalog.get_resolved_model_catalog(&state, model_id).await?;
    Ok(Json(map_model(resolved)))
}

#[utoipa::path(
    patch,
    path = "/v1/ai/models/{modelId}",
    tag = "ai",
    operation_id = "updateAiModel",
    params(("modelId" = uuid::Uuid, Path, description = "AI model catalog identifier")),
    request_body = UpdateModelCatalogRequest,
    responses(
        (status = 200, description = "Updated AI model catalog entry", body = ModelCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "Model not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.update_model", skip_all, fields(model_id = %model_id))]
pub async fn update_model(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(model_id): Path<Uuid>,
    Json(payload): Json<UpdateModelCatalogRequest>,
) -> Result<Json<ModelCatalogEntryResponse>, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry = state
        .canonical_services
        .ai_catalog
        .update_model_catalog(
            &state,
            UpdateModelCatalogCommand {
                model_id,
                provider_catalog_id: payload.provider_catalog_id,
                model_name: payload.model_name,
                capability_kind: payload.capability_kind,
                modality_kind: payload.modality_kind,
                lifecycle_state: payload.lifecycle_state,
                allowed_binding_purposes: payload.allowed_binding_purposes,
                context_window: payload.context_window,
                max_output_tokens: payload.max_output_tokens,
                metadata_json: payload.metadata_json,
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.model_catalog.update",
        "succeeded",
        Some(format!("AI model {} updated", entry.model_name)),
        Some(format!(
            "principal {} updated AI model catalog entry {}",
            auth.principal_id, entry.id
        )),
        vec![instance_subject("model_catalog", entry.id)],
    )
    .await;
    let resolved =
        state.canonical_services.ai_catalog.get_resolved_model_catalog(&state, model_id).await?;
    Ok(Json(map_model(resolved)))
}

/// Real hard delete; 409 when a price override or binding still references
/// this model. The reversible pause is `PATCH {lifecycleState:"disabled"}`
/// above — the two are distinct operations, not two paths to the same
/// result.
#[utoipa::path(
    delete,
    path = "/v1/ai/models/{modelId}",
    tag = "ai",
    operation_id = "deleteAiModel",
    params(("modelId" = uuid::Uuid, Path, description = "AI model catalog identifier")),
    responses(
        (status = 204, description = "Model catalog entry deleted"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not a system administrator"),
        (status = 404, description = "Model not found"),
        (status = 409, description = "Model is still referenced by a price override or binding"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.delete_model", skip_all, fields(model_id = %model_id))]
pub async fn delete_model(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(model_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    authorize_ai_catalog_admin(&auth)?;
    let entry = state.canonical_services.ai_catalog.get_model_catalog(&state, model_id).await?;
    state.canonical_services.ai_catalog.delete_model_catalog(&state, model_id).await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.model_catalog.delete",
        "succeeded",
        Some(format!("AI model {} deleted", entry.model_name)),
        Some(format!(
            "principal {} deleted AI model catalog entry {}",
            auth.principal_id, entry.id
        )),
        vec![instance_subject("model_catalog", entry.id)],
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/v1/ai/prices",
    tag = "ai",
    operation_id = "listAiPrices",
    params(PricesQuery),
    responses(
        (status = 200, description = "Price catalog entries for the requested model/workspace", body = [PriceCatalogEntryResponse]),
        (status = 401, description = "Caller is not authenticated"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_prices",
    skip_all,
    fields(workspace_id = ?query.workspace_id)
)]
pub async fn list_prices(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<PricesQuery>,
) -> Result<Json<Vec<PriceCatalogEntryResponse>>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let workspace_id = match query.workspace_id {
        Some(workspace_id) => {
            authorize_workspace_discovery(&auth, workspace_id)?;
            Some(workspace_id)
        }
        None => None,
    };
    let entries = state
        .canonical_services
        .ai_catalog
        .list_price_catalog(&state, query.model_catalog_id, workspace_id)
        .await?;
    Ok(Json(entries.into_iter().map(map_price).collect()))
}

#[utoipa::path(
    get,
    path = "/v1/ai/price-overrides/{overrideId}",
    tag = "ai",
    operation_id = "getAiPriceOverride",
    params(("overrideId" = uuid::Uuid, Path, description = "Price catalog entry identifier")),
    responses(
        (status = 200, description = "Price catalog entry", body = PriceCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 404, description = "Price override not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_price_override", skip_all, fields(price_id = %price_id))]
pub async fn get_price_override(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(price_id): Path<Uuid>,
) -> Result<Json<PriceCatalogEntryResponse>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let entry =
        state.canonical_services.ai_catalog.get_price_catalog_entry(&state, price_id).await?;
    Ok(Json(map_price(entry)))
}

#[utoipa::path(
    get,
    path = "/v1/ai/accounts",
    tag = "ai",
    operation_id = "listAiAccounts",
    params(AiScopeQuery),
    responses(
        (status = 200, description = "Visible AI accounts for the requested scope", body = [AiAccountResponse]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the requested scope"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_accounts",
    skip_all,
    fields(workspace_id = ?query.workspace_id, library_id = ?query.library_id)
)]
pub async fn list_accounts(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AiScopeQuery>,
) -> Result<Json<Vec<AiAccountResponse>>, ApiError> {
    let entries = if let Some(scope_kind) = query.scope_kind {
        let scope = authorize_exact_ai_scope(
            &auth,
            &state,
            scope_kind,
            query.workspace_id,
            query.library_id,
        )
        .await?;
        state.canonical_services.ai_catalog.list_account_summaries_exact(&state, scope).await?
    } else {
        let (workspace_id, library_id) =
            authorize_visible_ai_context(&auth, &state, query.workspace_id, query.library_id)
                .await?;
        state
            .canonical_services
            .ai_catalog
            .list_visible_account_summaries(&state, workspace_id, library_id)
            .await?
    };
    Ok(Json(entries.into_iter().map(map_account_summary).collect()))
}

#[utoipa::path(
    get,
    path = "/v1/ai/accounts/{accountId}",
    tag = "ai",
    operation_id = "getAiAccount",
    params(("accountId" = uuid::Uuid, Path, description = "AI account identifier")),
    responses(
        (status = 200, description = "AI account (secret redacted, same as the list view)", body = AiAccountResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot view accounts in the account's scope"),
        (status = 404, description = "Account not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_account", skip_all, fields(account_id = %account_id))]
pub async fn get_account(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<AiAccountResponse>, ApiError> {
    let account = state.canonical_services.ai_catalog.get_account(&state, account_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        account.scope_kind,
        account.workspace_id,
        account.library_id,
    )
    .await?;
    Ok(Json(map_account(account)))
}

#[utoipa::path(
    get,
    path = "/v1/ai/bindings",
    tag = "ai",
    operation_id = "listAiLibraryBindings",
    params(AiScopeQuery),
    responses(
        (status = 200, description = "Bindings for the requested scope", body = [AiBindingResponse]),
        (status = 400, description = "scopeKind query parameter is required"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the requested scope"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.list_bindings",
    skip_all,
    fields(workspace_id = ?query.workspace_id, library_id = ?query.library_id)
)]
pub async fn list_bindings(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AiScopeQuery>,
) -> Result<Json<Vec<AiBindingResponse>>, ApiError> {
    let scope_kind = query.scope_kind.ok_or_else(|| {
        ApiError::BadRequest("scopeKind is required for binding queries".to_string())
    })?;
    let scope =
        authorize_exact_ai_scope(&auth, &state, scope_kind, query.workspace_id, query.library_id)
            .await?;
    let entries = state.canonical_services.ai_catalog.list_bindings(&state, scope).await?;
    Ok(Json(entries.into_iter().map(map_binding).collect()))
}

#[utoipa::path(
    get,
    path = "/v1/ai/bindings/{bindingId}",
    tag = "ai",
    operation_id = "getAiLibraryBinding",
    params(("bindingId" = uuid::Uuid, Path, description = "Binding identifier")),
    responses(
        (status = 200, description = "Binding", body = AiBindingResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot view bindings in the binding's scope"),
        (status = 404, description = "Binding not found"),
    ),
)]
#[tracing::instrument(level = "info", name = "http.get_binding", skip_all, fields(binding_id = %binding_id))]
pub async fn get_binding(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(binding_id): Path<Uuid>,
) -> Result<Json<AiBindingResponse>, ApiError> {
    let binding = state.canonical_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    Ok(Json(map_binding(binding)))
}

#[utoipa::path(
    post,
    path = "/v1/ai/accounts",
    tag = "ai",
    operation_id = "createAiAccount",
    request_body = CreateAiAccountRequest,
    responses(
        (status = 201, description = "Newly created AI account", body = AiAccountResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer accounts in the requested scope"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.create_account",
    skip_all,
    fields(workspace_id = ?payload.workspace_id, library_id = ?payload.library_id)
)]
pub async fn create_account(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(mut payload): Json<CreateAiAccountRequest>,
) -> Result<axum::response::Response, ApiError> {
    let request_id = request_id.map(|value| value.0.0);
    let scope = authorize_exact_ai_scope(
        &auth,
        &state,
        payload.scope_kind,
        payload.workspace_id,
        payload.library_id,
    )
    .await?;
    let entry = state
        .canonical_services
        .ai_catalog
        .create_account(
            &state,
            CreateAiAccountCommand {
                scope_kind: payload.scope_kind,
                workspace_id: scope.workspace_id,
                library_id: scope.library_id,
                provider_catalog_id: payload.provider_catalog_id,
                label: std::mem::take(&mut payload.label),
                api_key: payload.api_key.take(),
                base_url: payload.base_url.take(),
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id,
        "ai.provider_credential.create",
        "succeeded",
        Some(format!("AI account {} created", entry.label)),
        Some(format!(
            "principal {} created AI account {} in {}",
            auth.principal_id,
            entry.id,
            describe_scope(entry.scope_kind, entry.workspace_id, entry.library_id),
        )),
        vec![subject_from_scope(
            "provider_credential",
            entry.id,
            entry.workspace_id,
            entry.library_id,
        )],
    )
    .await;
    let account_id = entry.id;
    Ok(created_response(&format!("/v1/ai/accounts/{account_id}"), map_account(entry)))
}

#[tracing::instrument(
    level = "info",
    name = "http.update_account",
    skip_all,
    fields(account_id = %account_id)
)]
#[utoipa::path(
    patch,
    path = "/v1/ai/accounts/{accountId}",
    tag = "ai",
    operation_id = "updateAiAccount",
    params(("accountId" = uuid::Uuid, Path, description = "AI account identifier")),
    request_body = UpdateAiAccountRequest,
    responses(
        (status = 200, description = "Updated AI account", body = AiAccountResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer accounts in the account's scope"),
        (status = 404, description = "Account not found"),
    ),
)]
pub async fn update_account(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(account_id): Path<Uuid>,
    Json(mut payload): Json<UpdateAiAccountRequest>,
) -> Result<Json<AiAccountResponse>, ApiError> {
    let account = state.canonical_services.ai_catalog.get_account(&state, account_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        account.scope_kind,
        account.workspace_id,
        account.library_id,
    )
    .await?;
    let entry = state
        .canonical_services
        .ai_catalog
        .update_account(
            &state,
            UpdateAiAccountCommand {
                account_id,
                label: std::mem::take(&mut payload.label),
                api_key: payload.api_key.take(),
                base_url: payload.base_url.take(),
                credential_state: std::mem::take(&mut payload.credential_state),
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.provider_credential.update",
        "succeeded",
        Some(format!("AI account {} updated", entry.label)),
        Some(format!(
            "principal {} updated AI account {} in {}",
            auth.principal_id,
            entry.id,
            describe_scope(entry.scope_kind, entry.workspace_id, entry.library_id),
        )),
        vec![subject_from_scope(
            "provider_credential",
            entry.id,
            entry.workspace_id,
            entry.library_id,
        )],
    )
    .await;
    Ok(Json(map_account(entry)))
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_account",
    skip_all,
    fields(account_id = %account_id)
)]
#[utoipa::path(
    delete,
    path = "/v1/ai/accounts/{accountId}",
    tag = "ai",
    operation_id = "deleteAiAccount",
    params(("accountId" = uuid::Uuid, Path, description = "AI account identifier")),
    responses(
        (status = 204, description = "Account deleted"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer accounts in the account's scope"),
        (status = 404, description = "Account not found"),
        (status = 409, description = "Account is still referenced"),
    ),
)]
pub async fn delete_account(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(account_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let account = state.canonical_services.ai_catalog.get_account(&state, account_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        account.scope_kind,
        account.workspace_id,
        account.library_id,
    )
    .await?;
    state.canonical_services.ai_catalog.delete_account(&state, account_id).await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.provider_credential.delete",
        "succeeded",
        Some(format!("AI account {} deleted", account.label)),
        Some(format!(
            "principal {} deleted AI account {} in {}",
            auth.principal_id,
            account_id,
            describe_scope(account.scope_kind, account.workspace_id, account.library_id),
        )),
        vec![subject_from_scope(
            "provider_credential",
            account_id,
            account.workspace_id,
            account.library_id,
        )],
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[tracing::instrument(
    level = "info",
    name = "http.create_price_override",
    skip_all,
    fields(workspace_id = %payload.workspace_id)
)]
#[utoipa::path(
    post,
    path = "/v1/ai/price-overrides",
    tag = "ai",
    operation_id = "createAiPriceOverride",
    request_body = CreateWorkspacePriceOverrideRequest,
    responses(
        (status = 201, description = "Newly created workspace price override", body = PriceCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer pricing for the workspace"),
    ),
)]
pub async fn create_price_override(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateWorkspacePriceOverrideRequest>,
) -> Result<axum::response::Response, ApiError> {
    authorize_workspace_permission(&auth, payload.workspace_id, POLICY_PROVIDERS_ADMIN)?;
    let entry = state
        .canonical_services
        .ai_catalog
        .create_workspace_price_override(
            &state,
            CreateWorkspacePriceOverrideCommand {
                workspace_id: payload.workspace_id,
                model_catalog_id: payload.model_catalog_id,
                billing_unit: payload.billing_unit,
                unit_price: payload.unit_price,
                currency_code: payload.currency_code,
                effective_from: payload.effective_from,
                effective_to: payload.effective_to,
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.price_override.create",
        "succeeded",
        Some(format!("workspace price override {} created", entry.id)),
        Some(format!(
            "principal {} created workspace price override {} in workspace {}",
            auth.principal_id, entry.id, payload.workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "workspace".to_string(),
            subject_id: payload.workspace_id,
            workspace_id: Some(payload.workspace_id),
            library_id: None,
            document_id: None,
        }],
    )
    .await;
    let price_id = entry.id;
    Ok(created_response(&format!("/v1/ai/price-overrides/{price_id}"), map_price(entry)))
}

#[tracing::instrument(
    level = "info",
    name = "http.update_price_override",
    skip_all,
    fields(price_id = %price_id)
)]
#[utoipa::path(
    patch,
    path = "/v1/ai/price-overrides/{overrideId}",
    tag = "ai",
    operation_id = "updateAiPriceOverride",
    params(("overrideId" = uuid::Uuid, Path, description = "Price catalog entry identifier")),
    request_body = UpdateWorkspacePriceOverrideRequest,
    responses(
        (status = 200, description = "Updated workspace price override", body = PriceCatalogEntryResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer pricing for the workspace"),
        (status = 404, description = "Price override not found"),
    ),
)]
pub async fn update_price_override(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(price_id): Path<Uuid>,
    Json(payload): Json<UpdateWorkspacePriceOverrideRequest>,
) -> Result<Json<PriceCatalogEntryResponse>, ApiError> {
    let price =
        state.canonical_services.ai_catalog.get_price_catalog_entry(&state, price_id).await?;
    let workspace_id = price
        .workspace_id
        .ok_or_else(|| ApiError::BadRequest("system catalog prices are read-only".to_string()))?;
    if price.catalog_scope != "workspace_override" {
        return Err(ApiError::BadRequest("system catalog prices are read-only".to_string()));
    }
    authorize_workspace_permission(&auth, workspace_id, POLICY_PROVIDERS_ADMIN)?;
    let entry = state
        .canonical_services
        .ai_catalog
        .update_workspace_price_override(
            &state,
            UpdateWorkspacePriceOverrideCommand {
                price_id,
                model_catalog_id: payload.model_catalog_id,
                billing_unit: payload.billing_unit,
                unit_price: payload.unit_price,
                currency_code: payload.currency_code,
                effective_from: payload.effective_from,
                effective_to: payload.effective_to,
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.price_override.update",
        "succeeded",
        Some(format!("workspace price override {} updated", entry.id)),
        Some(format!(
            "principal {} updated workspace price override {} in workspace {}",
            auth.principal_id, entry.id, workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "workspace".to_string(),
            subject_id: workspace_id,
            workspace_id: Some(workspace_id),
            library_id: None,
            document_id: None,
        }],
    )
    .await;
    Ok(Json(map_price(entry)))
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_price_override",
    skip_all,
    fields(price_id = %price_id)
)]
#[utoipa::path(
    delete,
    path = "/v1/ai/price-overrides/{overrideId}",
    tag = "ai",
    operation_id = "deleteAiPriceOverride",
    params(("overrideId" = uuid::Uuid, Path, description = "Price catalog entry identifier")),
    responses(
        (status = 204, description = "Price override deleted"),
        (status = 400, description = "System catalog prices are read-only"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer pricing for the workspace"),
        (status = 404, description = "Price override not found"),
        (status = 409, description = "Price override is still referenced"),
    ),
)]
pub async fn delete_price_override(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(price_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let price =
        state.canonical_services.ai_catalog.get_price_catalog_entry(&state, price_id).await?;
    let workspace_id = price
        .workspace_id
        .ok_or_else(|| ApiError::BadRequest("system catalog prices are read-only".to_string()))?;
    if price.catalog_scope != "workspace_override" {
        return Err(ApiError::BadRequest("system catalog prices are read-only".to_string()));
    }
    authorize_workspace_permission(&auth, workspace_id, POLICY_PROVIDERS_ADMIN)?;
    state.canonical_services.ai_catalog.delete_workspace_price_override(&state, price_id).await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.price_override.delete",
        "succeeded",
        Some(format!("workspace price override {price_id} deleted")),
        Some(format!(
            "principal {} deleted workspace price override {} in workspace {}",
            auth.principal_id, price_id, workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "workspace".to_string(),
            subject_id: workspace_id,
            workspace_id: Some(workspace_id),
            library_id: None,
            document_id: None,
        }],
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[tracing::instrument(
    level = "info",
    name = "http.create_binding",
    skip_all,
    fields(workspace_id = ?payload.workspace_id, library_id = ?payload.library_id)
)]
#[utoipa::path(
    post,
    path = "/v1/ai/bindings",
    tag = "ai",
    operation_id = "createAiLibraryBinding",
    request_body = CreateAiBindingRequest,
    responses(
        (status = 201, description = "Newly created binding", body = AiBindingResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer bindings in the requested scope"),
    ),
)]
pub async fn create_binding(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateAiBindingRequest>,
) -> Result<axum::response::Response, ApiError> {
    let scope = authorize_exact_ai_scope(
        &auth,
        &state,
        payload.scope_kind,
        payload.workspace_id,
        payload.library_id,
    )
    .await?;
    let entry = state
        .canonical_services
        .ai_catalog
        .create_binding(
            &state,
            CreateAiBindingCommand {
                scope_kind: payload.scope_kind,
                workspace_id: scope.workspace_id,
                library_id: scope.library_id,
                binding_purpose: payload.binding_purpose,
                account_id: payload.account_id,
                model_catalog_id: payload.model_catalog_id,
                system_prompt: payload.system_prompt,
                temperature: payload.temperature,
                top_p: payload.top_p,
                max_output_tokens_override: payload.max_output_tokens_override,
                extra_parameters_json: payload.extra_parameters_json,
                updated_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.binding_assignment.create",
        "succeeded",
        Some(format!("binding {} created", entry.id)),
        Some(format!(
            "principal {} created binding {} in {}",
            auth.principal_id,
            entry.id,
            describe_scope(entry.scope_kind, entry.workspace_id, entry.library_id),
        )),
        vec![subject_from_scope(
            "binding_assignment",
            entry.id,
            entry.workspace_id,
            entry.library_id,
        )],
    )
    .await;
    let binding_id = entry.id;
    Ok(created_response(&format!("/v1/ai/bindings/{binding_id}"), map_binding(entry)))
}

#[tracing::instrument(
    level = "info",
    name = "http.update_binding",
    skip_all,
    fields(binding_id = %binding_id)
)]
#[utoipa::path(
    patch,
    path = "/v1/ai/bindings/{bindingId}",
    tag = "ai",
    operation_id = "updateAiLibraryBinding",
    params(("bindingId" = uuid::Uuid, Path, description = "Binding identifier")),
    request_body = UpdateAiBindingRequest,
    responses(
        (status = 200, description = "Updated binding", body = AiBindingResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer bindings in the binding's scope"),
        (status = 404, description = "Binding not found"),
    ),
)]
pub async fn update_binding(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(binding_id): Path<Uuid>,
    Json(payload): Json<UpdateAiBindingRequest>,
) -> Result<Json<AiBindingResponse>, ApiError> {
    let binding = state.canonical_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    let entry = state
        .canonical_services
        .ai_catalog
        .update_binding(
            &state,
            UpdateAiBindingCommand {
                binding_id,
                account_id: payload.account_id,
                model_catalog_id: payload.model_catalog_id,
                system_prompt: payload.system_prompt,
                temperature: payload.temperature,
                top_p: payload.top_p,
                max_output_tokens_override: payload.max_output_tokens_override,
                extra_parameters_json: payload.extra_parameters_json,
                binding_state: payload.binding_state,
                updated_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.binding_assignment.update",
        "succeeded",
        Some(format!("binding {} updated", entry.id)),
        Some(format!(
            "principal {} updated binding {} in {}",
            auth.principal_id,
            entry.id,
            describe_scope(entry.scope_kind, entry.workspace_id, entry.library_id),
        )),
        vec![subject_from_scope(
            "binding_assignment",
            entry.id,
            entry.workspace_id,
            entry.library_id,
        )],
    )
    .await;
    Ok(Json(map_binding(entry)))
}

#[tracing::instrument(
    level = "info",
    name = "http.delete_binding",
    skip_all,
    fields(binding_id = %binding_id)
)]
#[utoipa::path(
    delete,
    path = "/v1/ai/bindings/{bindingId}",
    tag = "ai",
    operation_id = "deleteAiLibraryBinding",
    params(("bindingId" = uuid::Uuid, Path, description = "Binding identifier")),
    responses(
        (status = 204, description = "Binding deleted"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot administer bindings in the binding's scope"),
        (status = 404, description = "Binding not found"),
    ),
)]
pub async fn delete_binding(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(binding_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let binding = state.canonical_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    state.canonical_services.ai_catalog.delete_binding(&state, binding_id).await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.binding_assignment.delete",
        "succeeded",
        Some(format!("binding {binding_id} deleted")),
        Some(format!(
            "principal {} deleted binding {} in {}",
            auth.principal_id,
            binding_id,
            describe_scope(binding.scope_kind, binding.workspace_id, binding.library_id),
        )),
        vec![subject_from_scope(
            "binding_assignment",
            binding_id,
            binding.workspace_id,
            binding.library_id,
        )],
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Creates a binding-validation attempt. Resource-shaped replacement for the
/// former RPC-verb `POST .../validate`: the handler already created a real
/// record with a `pending` → `succeeded`/`failed` lifecycle (`ai_binding_validation`,
/// unchanged by this redesign); the defect was the unpollable verb-shaped
/// form, not the underlying action. `Location` points at the created
/// validation's own item-read endpoint below.
#[tracing::instrument(
    level = "info",
    name = "http.create_binding_validation",
    skip_all,
    fields(binding_id = %binding_id)
)]
#[utoipa::path(
    post,
    path = "/v1/ai/bindings/{bindingId}/validations",
    tag = "ai",
    operation_id = "createAiBindingValidation",
    params(("bindingId" = uuid::Uuid, Path, description = "Binding identifier")),
    responses(
        (status = 201, description = "Binding validation attempt created", body = BindingValidationResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot validate bindings in the binding's scope"),
        (status = 404, description = "Binding not found"),
    ),
)]
pub async fn create_binding_validation(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(binding_id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    let binding = state.canonical_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    let validation = state
        .canonical_services
        .ai_catalog
        .validate_binding(
            &state,
            CreateBindingValidationCommand {
                binding_id,
                validation_state: "pending".to_string(),
                failure_code: None,
                message: Some("validation requested".to_string()),
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.binding_assignment.validate",
        "succeeded",
        Some(format!("binding {binding_id} validation requested")),
        Some(format!(
            "principal {} requested validation for binding {} in {}",
            auth.principal_id,
            binding_id,
            describe_scope(binding.scope_kind, binding.workspace_id, binding.library_id),
        )),
        vec![subject_from_scope(
            "binding_assignment",
            binding_id,
            binding.workspace_id,
            binding.library_id,
        )],
    )
    .await;
    let validation_id = validation.id;
    Ok(created_response(
        &format!("/v1/ai/bindings/{binding_id}/validations/{validation_id}"),
        map_binding_validation(validation),
    ))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_binding_validation",
    skip_all,
    fields(binding_id = %binding_id, validation_id = %validation_id)
)]
#[utoipa::path(
    get,
    path = "/v1/ai/bindings/{bindingId}/validations/{validationId}",
    tag = "ai",
    operation_id = "getAiBindingValidation",
    params(
        ("bindingId" = uuid::Uuid, Path, description = "Binding identifier"),
        ("validationId" = uuid::Uuid, Path, description = "Binding validation attempt identifier"),
    ),
    responses(
        (status = 200, description = "Binding validation outcome", body = BindingValidationResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot view validations in the binding's scope"),
        (status = 404, description = "Binding or validation not found"),
    ),
)]
pub async fn get_binding_validation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((binding_id, validation_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<BindingValidationResponse>, ApiError> {
    let binding = state.canonical_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    let validation =
        state.canonical_services.ai_catalog.get_binding_validation(&state, validation_id).await?;
    if validation.binding_id != binding_id {
        return Err(ApiError::resource_not_found("binding_validation", validation_id));
    }
    Ok(Json(map_binding_validation(validation)))
}

#[tracing::instrument(
    level = "info",
    name = "http.list_binding_validations",
    skip_all,
    fields(binding_id = %binding_id, item_count)
)]
#[utoipa::path(
    get,
    path = "/v1/ai/bindings/{bindingId}/validations",
    tag = "ai",
    operation_id = "listAiBindingValidations",
    params(("bindingId" = uuid::Uuid, Path, description = "Binding identifier")),
    responses(
        (status = 200, description = "Validation attempt history for the binding, newest first", body = [BindingValidationResponse]),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller cannot view validations in the binding's scope"),
        (status = 404, description = "Binding not found"),
    ),
)]
pub async fn list_binding_validations(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(binding_id): Path<Uuid>,
) -> Result<Json<Vec<BindingValidationResponse>>, ApiError> {
    let span = tracing::Span::current();
    let binding = state.canonical_services.ai_catalog.get_binding(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    let validations =
        state.canonical_services.ai_catalog.list_binding_validations(&state, binding_id).await?;
    span.record("item_count", validations.len());
    Ok(Json(validations.into_iter().map(map_binding_validation).collect()))
}

async fn record_ai_audit_event(
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

fn map_provider(entry: ProviderCatalogEntry) -> ProviderCatalogEntryResponse {
    ProviderCatalogEntryResponse {
        id: entry.id,
        provider_kind: entry.provider_kind,
        display_name: entry.display_name,
        api_style: entry.api_style,
        lifecycle_state: entry.lifecycle_state,
        default_base_url: entry.default_base_url,
        api_key_required: entry.api_key_required,
        base_url_required: entry.base_url_required,
        credential_policy: entry.credential_policy,
        base_url_policy: entry.base_url_policy,
        model_discovery: entry.model_discovery,
        capabilities: entry.capabilities,
        runtime: entry.runtime,
        ui_hints: entry.ui_hints,
    }
}

fn map_model(entry: ResolvedModelCatalogEntry) -> ModelCatalogEntryResponse {
    ModelCatalogEntryResponse {
        id: entry.model.id,
        provider_catalog_id: entry.model.provider_catalog_id,
        model_name: entry.model.model_name,
        capability_kind: entry.model.capability_kind,
        modality_kind: entry.model.modality_kind,
        lifecycle_state: entry.model.lifecycle_state,
        allowed_binding_purposes: entry.model.allowed_binding_purposes,
        context_window: entry.model.context_window,
        max_output_tokens: entry.model.max_output_tokens,
        availability_state: entry.availability_state,
        available_account_ids: entry.available_account_ids,
    }
}

fn map_price(entry: PriceCatalogEntry) -> PriceCatalogEntryResponse {
    PriceCatalogEntryResponse {
        id: entry.id,
        model_catalog_id: entry.model_catalog_id,
        billing_unit: entry.billing_unit,
        price_variant_key: entry.price_variant_key,
        request_input_tokens_min: entry.request_input_tokens_min,
        request_input_tokens_max: entry.request_input_tokens_max,
        unit_price: entry.unit_price,
        currency_code: entry.currency_code,
        effective_from: entry.effective_from,
        effective_to: entry.effective_to,
        catalog_scope: entry.catalog_scope,
        workspace_id: entry.workspace_id,
    }
}

fn map_account(mut entry: AiAccount) -> AiAccountResponse {
    let api_key_summary = configured_api_key_summary(
        entry.api_key.as_deref().is_some_and(|value| !value.trim().is_empty()),
    );
    AiAccountResponse {
        id: entry.id,
        scope_kind: entry.scope_kind,
        workspace_id: entry.workspace_id,
        library_id: entry.library_id,
        provider_catalog_id: entry.provider_catalog_id,
        label: std::mem::take(&mut entry.label),
        base_url: entry.base_url.take(),
        api_key_summary,
        credential_state: std::mem::take(&mut entry.credential_state),
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

fn map_account_summary(mut entry: AiAccountSummary) -> AiAccountResponse {
    AiAccountResponse {
        id: entry.id,
        scope_kind: entry.scope_kind,
        workspace_id: entry.workspace_id,
        library_id: entry.library_id,
        provider_catalog_id: entry.provider_catalog_id,
        label: std::mem::take(&mut entry.label),
        base_url: entry.base_url.take(),
        api_key_summary: configured_api_key_summary(entry.has_api_key),
        credential_state: std::mem::take(&mut entry.credential_state),
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

fn configured_api_key_summary(configured: bool) -> String {
    if configured { "configured" } else { "not_configured" }.to_string()
}

fn map_binding(entry: AiBinding) -> AiBindingResponse {
    AiBindingResponse {
        id: entry.id,
        scope_kind: entry.scope_kind,
        workspace_id: entry.workspace_id,
        library_id: entry.library_id,
        binding_purpose: entry.binding_purpose,
        account_id: entry.account_id,
        model_catalog_id: entry.model_catalog_id,
        system_prompt: entry.system_prompt,
        temperature: entry.temperature,
        top_p: entry.top_p,
        max_output_tokens_override: entry.max_output_tokens_override,
        extra_parameters_json: entry.extra_parameters_json,
        binding_state: entry.binding_state,
    }
}

fn map_binding_validation(entry: BindingValidation) -> BindingValidationResponse {
    BindingValidationResponse {
        id: entry.id,
        binding_id: entry.binding_id,
        validation_state: entry.validation_state,
        checked_at: entry.checked_at,
        failure_code: entry.failure_code,
        message: entry.message,
    }
}

fn authorize_ai_catalog_admin(auth: &AuthContext) -> Result<(), ApiError> {
    auth.require_write_capability()?;
    if auth.is_system_admin {
        return Ok(());
    }
    Err(ApiError::forbidden("system administrator required"))
}

fn instance_subject(subject_kind: &str, subject_id: Uuid) -> AppendAuditEventSubjectCommand {
    AppendAuditEventSubjectCommand {
        subject_kind: subject_kind.to_string(),
        subject_id,
        workspace_id: None,
        library_id: None,
        document_id: None,
    }
}

async fn authorize_visible_ai_context(
    auth: &AuthContext,
    state: &AppState,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<(Option<Uuid>, Option<Uuid>), ApiError> {
    if let Some(library_id) = library_id {
        let library =
            load_library_and_authorize(auth, state, library_id, POLICY_PROVIDERS_ADMIN).await?;
        if let Some(workspace_id) = workspace_id
            && workspace_id != library.workspace_id
        {
            return Err(ApiError::BadRequest(
                "libraryId does not belong to workspaceId".to_string(),
            ));
        }
        return Ok((Some(library.workspace_id), Some(library.id)));
    }

    if let Some(workspace_id) = workspace_id {
        authorize_workspace_permission(auth, workspace_id, POLICY_PROVIDERS_ADMIN)?;
        return Ok((Some(workspace_id), None));
    }

    if auth.is_system_admin {
        return Ok((None, None));
    }

    Err(ApiError::BadRequest(
        "workspaceId or libraryId is required for AI configuration".to_string(),
    ))
}

async fn authorize_exact_ai_scope(
    auth: &AuthContext,
    state: &AppState,
    scope_kind: AiScopeKind,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<AiScopeRef, ApiError> {
    match scope_kind {
        AiScopeKind::Instance => {
            if workspace_id.is_some() || library_id.is_some() {
                return Err(ApiError::BadRequest(
                    "instance scope cannot include workspaceId or libraryId".to_string(),
                ));
            }
            if auth.is_system_admin {
                Ok(AiScopeRef { scope_kind, workspace_id: None, library_id: None })
            } else {
                Err(ApiError::Unauthorized)
            }
        }
        AiScopeKind::Workspace => {
            if library_id.is_some() {
                return Err(ApiError::BadRequest(
                    "workspace scope cannot include libraryId".to_string(),
                ));
            }
            let workspace_id = workspace_id.ok_or_else(|| {
                ApiError::BadRequest("workspaceId is required for workspace scope".to_string())
            })?;
            authorize_workspace_permission(auth, workspace_id, POLICY_PROVIDERS_ADMIN)?;
            Ok(AiScopeRef { scope_kind, workspace_id: Some(workspace_id), library_id: None })
        }
        AiScopeKind::Library => {
            let library_id = library_id.ok_or_else(|| {
                ApiError::BadRequest("libraryId is required for library scope".to_string())
            })?;
            let library =
                load_library_and_authorize(auth, state, library_id, POLICY_PROVIDERS_ADMIN).await?;
            if let Some(workspace_id) = workspace_id
                && workspace_id != library.workspace_id
            {
                return Err(ApiError::BadRequest(
                    "libraryId does not belong to workspaceId".to_string(),
                ));
            }
            Ok(AiScopeRef {
                scope_kind,
                workspace_id: Some(library.workspace_id),
                library_id: Some(library.id),
            })
        }
    }
}

fn describe_scope(
    scope_kind: AiScopeKind,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> String {
    match scope_kind {
        AiScopeKind::Instance => "instance scope".to_string(),
        AiScopeKind::Workspace => format!(
            "workspace {}",
            workspace_id.map_or_else(|| "unknown".to_string(), |value| value.to_string())
        ),
        AiScopeKind::Library => format!(
            "library {}",
            library_id.map_or_else(|| "unknown".to_string(), |value| value.to_string())
        ),
    }
}

fn subject_from_scope(
    subject_kind: &str,
    subject_id: Uuid,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> AppendAuditEventSubjectCommand {
    AppendAuditEventSubjectCommand {
        subject_kind: subject_kind.to_string(),
        subject_id,
        workspace_id,
        library_id,
        document_id: None,
    }
}

#[cfg(test)]
mod secret_surface_tests {
    use uuid::Uuid;

    use super::{
        AiScopeKind, CreateAiAccountRequest, UpdateAiAccountRequest, configured_api_key_summary,
    };

    #[test]
    fn account_request_debug_redacts_key_and_untrusted_base_url() {
        let create = CreateAiAccountRequest {
            scope_kind: AiScopeKind::Instance,
            workspace_id: None,
            library_id: None,
            provider_catalog_id: Uuid::now_v7(),
            label: "synthetic".into(),
            api_key: Some("provider-key-regression".into()),
            base_url: Some("https://user:url-secret@host.example/?token=query-secret".into()),
        };
        let update = UpdateAiAccountRequest {
            label: "synthetic".into(),
            api_key: Some("updated-key-regression".into()),
            base_url: Some("https://host.example/?token=updated-query-secret".into()),
            credential_state: "active".into(),
        };

        let debug = format!("{create:?} {update:?}");
        for secret in [
            "provider-key-regression",
            "updated-key-regression",
            "url-secret",
            "query-secret",
            "updated-query-secret",
        ] {
            assert!(!debug.contains(secret), "Debug exposed {secret}");
        }
    }

    #[test]
    fn account_key_summary_never_discloses_key_fragments() {
        assert_eq!(configured_api_key_summary(true), "configured");
        assert_eq!(configured_api_key_summary(false), "not_configured");
    }
}
