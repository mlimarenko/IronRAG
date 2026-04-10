use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post, put},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::{
        AiBindingAssignment, AiBindingPurpose, AiScopeKind, BindingValidation,
        ModelAvailabilityState, ModelPreset, PriceCatalogEntry, ProviderCatalogEntry,
        ProviderCredential, ResolvedModelCatalogEntry,
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
        AiScopeRef, CreateBindingAssignmentCommand, CreateBindingValidationCommand,
        CreateModelPresetCommand, CreateProviderCredentialCommand,
        CreateWorkspacePriceOverrideCommand, UpdateBindingAssignmentCommand,
        UpdateModelPresetCommand, UpdateProviderCredentialCommand,
        UpdateWorkspacePriceOverrideCommand,
    },
    services::iam::audit::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsQuery {
    pub provider_catalog_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub credential_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricesQuery {
    pub model_catalog_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiScopeQuery {
    pub scope_kind: Option<AiScopeKind>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProviderCredentialRequest {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProviderCredentialRequest {
    pub label: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub credential_state: String,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateWorkspacePriceOverrideRequest {
    pub model_catalog_id: Uuid,
    pub billing_unit: String,
    pub unit_price: Decimal,
    pub currency_code: String,
    pub effective_from: chrono::DateTime<chrono::Utc>,
    pub effective_to: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateModelPresetRequest {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub model_catalog_id: Uuid,
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    #[serde(default)]
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateModelPresetRequest {
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    #[serde(default)]
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBindingAssignmentRequest {
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub binding_purpose: AiBindingPurpose,
    pub provider_credential_id: Uuid,
    pub model_preset_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBindingAssignmentRequest {
    pub provider_credential_id: Uuid,
    pub model_preset_id: Uuid,
    pub binding_state: String,
}

#[derive(Debug, Serialize)]
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntryResponse {
    pub id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub capability_kind: String,
    pub modality_kind: String,
    pub allowed_binding_purposes: Vec<AiBindingPurpose>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub availability_state: ModelAvailabilityState,
    pub available_credential_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCredentialResponse {
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPresetResponse {
    pub id: Uuid,
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub model_catalog_id: Uuid,
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiBindingAssignmentResponse {
    pub id: Uuid,
    pub scope_kind: AiScopeKind,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub binding_purpose: AiBindingPurpose,
    pub provider_credential_id: Uuid,
    pub model_preset_id: Uuid,
    pub binding_state: String,
}

#[derive(Debug, Serialize)]
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
        .route("/ai/providers", get(list_providers))
        .route("/ai/models", get(list_models))
        .route("/ai/model-presets", get(list_model_presets).post(create_model_preset))
        .route("/ai/model-presets/{preset_id}", put(update_model_preset))
        .route("/ai/prices", get(list_prices).post(create_price_override))
        .route("/ai/prices/{price_id}", put(update_price_override))
        .route("/ai/credentials", get(list_credentials).post(create_credential))
        .route("/ai/credentials/{credential_id}", put(update_credential))
        .route("/ai/bindings", get(list_binding_assignments).post(create_binding_assignment))
        .route(
            "/ai/bindings/{binding_id}",
            put(update_binding_assignment).delete(delete_binding_assignment),
        )
        .route("/ai/bindings/{binding_id}/validate", post(validate_binding_assignment))
}

async fn list_providers(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<Vec<ProviderCatalogEntryResponse>>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let entries = state.canonical_services.ai_catalog.list_provider_catalog(&state).await?;
    Ok(Json(entries.into_iter().map(map_provider).collect()))
}

async fn list_models(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ModelsQuery>,
) -> Result<Json<Vec<ModelCatalogEntryResponse>>, ApiError> {
    auth.require_any_scope(POLICY_MCP_DISCOVERY)?;
    let (workspace_id, library_id) = if query.workspace_id.is_some()
        || query.library_id.is_some()
        || query.credential_id.is_some()
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
            query.credential_id,
        )
        .await?;
    Ok(Json(entries.into_iter().map(map_model).collect()))
}

async fn list_prices(
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

async fn list_model_presets(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AiScopeQuery>,
) -> Result<Json<Vec<ModelPresetResponse>>, ApiError> {
    let entries = if let Some(scope_kind) = query.scope_kind {
        let scope = authorize_exact_ai_scope(
            &auth,
            &state,
            scope_kind,
            query.workspace_id,
            query.library_id,
        )
        .await?;
        state.canonical_services.ai_catalog.list_model_presets_exact(&state, scope).await?
    } else {
        let (workspace_id, library_id) =
            authorize_visible_ai_context(&auth, &state, query.workspace_id, query.library_id)
                .await?;
        state
            .canonical_services
            .ai_catalog
            .list_visible_model_presets(&state, workspace_id, library_id)
            .await?
    };
    Ok(Json(entries.into_iter().map(map_model_preset).collect()))
}

async fn list_credentials(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AiScopeQuery>,
) -> Result<Json<Vec<ProviderCredentialResponse>>, ApiError> {
    let entries = if let Some(scope_kind) = query.scope_kind {
        let scope = authorize_exact_ai_scope(
            &auth,
            &state,
            scope_kind,
            query.workspace_id,
            query.library_id,
        )
        .await?;
        state.canonical_services.ai_catalog.list_provider_credentials_exact(&state, scope).await?
    } else {
        let (workspace_id, library_id) =
            authorize_visible_ai_context(&auth, &state, query.workspace_id, query.library_id)
                .await?;
        state
            .canonical_services
            .ai_catalog
            .list_visible_provider_credentials(&state, workspace_id, library_id)
            .await?
    };
    Ok(Json(entries.into_iter().map(map_provider_credential).collect()))
}

async fn list_binding_assignments(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AiScopeQuery>,
) -> Result<Json<Vec<AiBindingAssignmentResponse>>, ApiError> {
    let scope_kind = query.scope_kind.ok_or_else(|| {
        ApiError::BadRequest("scopeKind is required for binding queries".to_string())
    })?;
    let scope =
        authorize_exact_ai_scope(&auth, &state, scope_kind, query.workspace_id, query.library_id)
            .await?;
    let entries =
        state.canonical_services.ai_catalog.list_binding_assignments(&state, scope).await?;
    Ok(Json(entries.into_iter().map(map_binding_assignment).collect()))
}

async fn create_credential(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateProviderCredentialRequest>,
) -> Result<Json<ProviderCredentialResponse>, ApiError> {
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
        .create_provider_credential(
            &state,
            CreateProviderCredentialCommand {
                scope_kind: payload.scope_kind,
                workspace_id: scope.workspace_id,
                library_id: scope.library_id,
                provider_catalog_id: payload.provider_catalog_id,
                label: payload.label,
                api_key: payload.api_key,
                base_url: payload.base_url,
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
        Some(format!("provider credential {} created", entry.label)),
        Some(format!(
            "principal {} created provider credential {} in {}",
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
    Ok(Json(map_provider_credential(entry)))
}

async fn update_credential(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(credential_id): Path<Uuid>,
    Json(payload): Json<UpdateProviderCredentialRequest>,
) -> Result<Json<ProviderCredentialResponse>, ApiError> {
    let credential =
        state.canonical_services.ai_catalog.get_provider_credential(&state, credential_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        credential.scope_kind,
        credential.workspace_id,
        credential.library_id,
    )
    .await?;
    let entry = state
        .canonical_services
        .ai_catalog
        .update_provider_credential(
            &state,
            UpdateProviderCredentialCommand {
                credential_id,
                label: payload.label,
                api_key: payload.api_key,
                base_url: payload.base_url,
                credential_state: payload.credential_state,
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.provider_credential.update",
        "succeeded",
        Some(format!("provider credential {} updated", entry.label)),
        Some(format!(
            "principal {} updated provider credential {} in {}",
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
    Ok(Json(map_provider_credential(entry)))
}

async fn create_price_override(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateWorkspacePriceOverrideRequest>,
) -> Result<Json<PriceCatalogEntryResponse>, ApiError> {
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
    Ok(Json(map_price(entry)))
}

async fn update_price_override(
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

async fn create_model_preset(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateModelPresetRequest>,
) -> Result<Json<ModelPresetResponse>, ApiError> {
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
        .create_model_preset(
            &state,
            CreateModelPresetCommand {
                scope_kind: payload.scope_kind,
                workspace_id: scope.workspace_id,
                library_id: scope.library_id,
                model_catalog_id: payload.model_catalog_id,
                preset_name: payload.preset_name,
                system_prompt: payload.system_prompt,
                temperature: payload.temperature,
                top_p: payload.top_p,
                max_output_tokens_override: payload.max_output_tokens_override,
                extra_parameters_json: payload.extra_parameters_json,
                created_by_principal_id: Some(auth.principal_id),
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.model_preset.create",
        "succeeded",
        Some(format!("model preset {} created", entry.preset_name)),
        Some(format!(
            "principal {} created model preset {} in {}",
            auth.principal_id,
            entry.id,
            describe_scope(entry.scope_kind, entry.workspace_id, entry.library_id),
        )),
        vec![subject_from_scope("model_preset", entry.id, entry.workspace_id, entry.library_id)],
    )
    .await;
    Ok(Json(map_model_preset(entry)))
}

async fn update_model_preset(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(preset_id): Path<Uuid>,
    Json(payload): Json<UpdateModelPresetRequest>,
) -> Result<Json<ModelPresetResponse>, ApiError> {
    let preset = state.canonical_services.ai_catalog.get_model_preset(&state, preset_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        preset.scope_kind,
        preset.workspace_id,
        preset.library_id,
    )
    .await?;
    let entry = state
        .canonical_services
        .ai_catalog
        .update_model_preset(
            &state,
            UpdateModelPresetCommand {
                preset_id,
                preset_name: payload.preset_name,
                system_prompt: payload.system_prompt,
                temperature: payload.temperature,
                top_p: payload.top_p,
                max_output_tokens_override: payload.max_output_tokens_override,
                extra_parameters_json: payload.extra_parameters_json,
            },
        )
        .await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.model_preset.update",
        "succeeded",
        Some(format!("model preset {} updated", entry.preset_name)),
        Some(format!(
            "principal {} updated model preset {} in {}",
            auth.principal_id,
            entry.id,
            describe_scope(entry.scope_kind, entry.workspace_id, entry.library_id),
        )),
        vec![subject_from_scope("model_preset", entry.id, entry.workspace_id, entry.library_id)],
    )
    .await;
    Ok(Json(map_model_preset(entry)))
}

async fn create_binding_assignment(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateBindingAssignmentRequest>,
) -> Result<Json<AiBindingAssignmentResponse>, ApiError> {
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
        .create_binding_assignment(
            &state,
            CreateBindingAssignmentCommand {
                scope_kind: payload.scope_kind,
                workspace_id: scope.workspace_id,
                library_id: scope.library_id,
                binding_purpose: payload.binding_purpose,
                provider_credential_id: payload.provider_credential_id,
                model_preset_id: payload.model_preset_id,
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
        Some(format!("binding assignment {} created", entry.id)),
        Some(format!(
            "principal {} created binding assignment {} in {}",
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
    Ok(Json(map_binding_assignment(entry)))
}

async fn update_binding_assignment(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(binding_id): Path<Uuid>,
    Json(payload): Json<UpdateBindingAssignmentRequest>,
) -> Result<Json<AiBindingAssignmentResponse>, ApiError> {
    let binding =
        state.canonical_services.ai_catalog.get_binding_assignment(&state, binding_id).await?;
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
        .update_binding_assignment(
            &state,
            UpdateBindingAssignmentCommand {
                binding_id,
                provider_credential_id: payload.provider_credential_id,
                model_preset_id: payload.model_preset_id,
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
        Some(format!("binding assignment {} updated", entry.id)),
        Some(format!(
            "principal {} updated binding assignment {} in {}",
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
    Ok(Json(map_binding_assignment(entry)))
}

async fn delete_binding_assignment(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(binding_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let binding =
        state.canonical_services.ai_catalog.get_binding_assignment(&state, binding_id).await?;
    authorize_exact_ai_scope(
        &auth,
        &state,
        binding.scope_kind,
        binding.workspace_id,
        binding.library_id,
    )
    .await?;
    state.canonical_services.ai_catalog.delete_binding_assignment(&state, binding_id).await?;
    record_ai_audit_event(
        &state,
        &auth,
        request_id.map(|value| value.0.0),
        "ai.binding_assignment.delete",
        "succeeded",
        Some(format!("binding assignment {} deleted", binding_id)),
        Some(format!(
            "principal {} deleted binding assignment {} in {}",
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
    Ok(Json(serde_json::json!({ "deleted": true })))
}

async fn validate_binding_assignment(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(binding_id): Path<Uuid>,
) -> Result<Json<BindingValidationResponse>, ApiError> {
    let binding =
        state.canonical_services.ai_catalog.get_binding_assignment(&state, binding_id).await?;
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
        Some(format!("binding assignment {} validation requested", binding_id)),
        Some(format!(
            "principal {} requested validation for binding assignment {} in {}",
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
    Ok(Json(map_binding_validation(validation)))
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
    }
}

fn map_model(entry: ResolvedModelCatalogEntry) -> ModelCatalogEntryResponse {
    ModelCatalogEntryResponse {
        id: entry.model.id,
        provider_catalog_id: entry.model.provider_catalog_id,
        model_name: entry.model.model_name,
        capability_kind: entry.model.capability_kind,
        modality_kind: entry.model.modality_kind,
        allowed_binding_purposes: entry.model.allowed_binding_purposes,
        context_window: entry.model.context_window,
        max_output_tokens: entry.model.max_output_tokens,
        availability_state: entry.availability_state,
        available_credential_ids: entry.available_credential_ids,
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

fn map_provider_credential(entry: ProviderCredential) -> ProviderCredentialResponse {
    ProviderCredentialResponse {
        id: entry.id,
        scope_kind: entry.scope_kind,
        workspace_id: entry.workspace_id,
        library_id: entry.library_id,
        provider_catalog_id: entry.provider_catalog_id,
        label: entry.label,
        base_url: entry.base_url,
        api_key_summary: summarize_api_key(entry.api_key.as_deref()),
        credential_state: entry.credential_state,
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

fn summarize_api_key(value: Option<&str>) -> String {
    let Some(trimmed) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return "not_configured".to_string();
    };
    let prefix: String = trimmed.chars().take(4).collect();
    let suffix =
        trimmed.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<String>();
    if trimmed.chars().count() <= 8 {
        format!("{prefix}••••")
    } else {
        format!("{prefix}••••{suffix}")
    }
}

fn map_model_preset(entry: ModelPreset) -> ModelPresetResponse {
    ModelPresetResponse {
        id: entry.id,
        scope_kind: entry.scope_kind,
        workspace_id: entry.workspace_id,
        library_id: entry.library_id,
        model_catalog_id: entry.model_catalog_id,
        preset_name: entry.preset_name,
        system_prompt: entry.system_prompt,
        temperature: entry.temperature,
        top_p: entry.top_p,
        max_output_tokens_override: entry.max_output_tokens_override,
        extra_parameters_json: entry.extra_parameters_json,
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

fn map_binding_assignment(entry: AiBindingAssignment) -> AiBindingAssignmentResponse {
    AiBindingAssignmentResponse {
        id: entry.id,
        scope_kind: entry.scope_kind,
        workspace_id: entry.workspace_id,
        library_id: entry.library_id,
        binding_purpose: entry.binding_purpose,
        provider_credential_id: entry.provider_credential_id,
        model_preset_id: entry.model_preset_id,
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

async fn authorize_visible_ai_context(
    auth: &AuthContext,
    state: &AppState,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<(Option<Uuid>, Option<Uuid>), ApiError> {
    if let Some(library_id) = library_id {
        let library =
            load_library_and_authorize(auth, state, library_id, POLICY_PROVIDERS_ADMIN).await?;
        if let Some(workspace_id) = workspace_id {
            if workspace_id != library.workspace_id {
                return Err(ApiError::BadRequest(
                    "libraryId does not belong to workspaceId".to_string(),
                ));
            }
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
            if let Some(workspace_id) = workspace_id {
                if workspace_id != library.workspace_id {
                    return Err(ApiError::BadRequest(
                        "libraryId does not belong to workspaceId".to_string(),
                    ));
                }
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
            workspace_id.map(|value| value.to_string()).unwrap_or_else(|| "unknown".to_string())
        ),
        AiScopeKind::Library => format!(
            "library {}",
            library_id.map(|value| value.to_string()).unwrap_or_else(|| "unknown".to_string())
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
