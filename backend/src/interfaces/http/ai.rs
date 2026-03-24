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
        AiBindingPurpose, BindingValidation, LibraryModelBinding, ModelCatalogEntry, ModelPreset,
        PriceCatalogEntry, ProviderCatalogEntry, ProviderCredential,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_MCP_DISCOVERY, POLICY_PROVIDERS_ADMIN, authorize_library_permission,
            authorize_workspace_discovery, authorize_workspace_permission,
            load_library_and_authorize, load_library_binding_and_authorize,
        },
        router_support::{ApiError, RequestId},
    },
    services::ai_catalog_service::{
        CreateBindingValidationCommand, CreateLibraryBindingCommand, CreateModelPresetCommand,
        CreateProviderCredentialCommand, CreateWorkspacePriceOverrideCommand,
        UpdateLibraryBindingCommand, UpdateModelPresetCommand, UpdateProviderCredentialCommand,
        UpdateWorkspacePriceOverrideCommand,
    },
    services::audit_service::{AppendAuditEventCommand, AppendAuditEventSubjectCommand},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsQuery {
    pub provider_catalog_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricesQuery {
    pub model_catalog_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsQuery {
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPresetsQuery {
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProviderCredentialRequest {
    pub workspace_id: Uuid,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub api_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProviderCredentialRequest {
    pub label: String,
    pub api_key: Option<String>,
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
    pub workspace_id: Uuid,
    pub model_catalog_id: Uuid,
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
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
    pub extra_parameters_json: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateLibraryBindingRequest {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_purpose: AiBindingPurpose,
    pub provider_credential_id: Uuid,
    pub model_preset_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateLibraryBindingRequest {
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntryResponse {
    pub id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub capability_kind: String,
    pub modality_kind: String,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceCatalogEntryResponse {
    pub id: Uuid,
    pub model_catalog_id: Uuid,
    pub billing_unit: String,
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
    pub workspace_id: Uuid,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub api_key_summary: String,
    pub credential_state: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPresetResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
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
pub struct LibraryModelBindingResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
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
        .route("/ai/libraries/{library_id}/bindings", get(list_library_bindings))
        .route("/ai/library-bindings", post(create_library_binding))
        .route("/ai/library-bindings/{binding_id}", put(update_library_binding))
        .route("/ai/library-bindings/{binding_id}/validate", post(validate_library_binding))
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
    let entries = state
        .canonical_services
        .ai_catalog
        .list_model_catalog(&state, query.provider_catalog_id)
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
    Query(query): Query<ModelPresetsQuery>,
) -> Result<Json<Vec<ModelPresetResponse>>, ApiError> {
    let workspace_id = resolve_workspace_scope(&auth, query.workspace_id)?;
    authorize_workspace_permission(&auth, workspace_id, POLICY_PROVIDERS_ADMIN)?;
    let entries =
        state.canonical_services.ai_catalog.list_model_presets(&state, workspace_id).await?;
    Ok(Json(entries.into_iter().map(map_model_preset).collect()))
}

async fn list_credentials(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<CredentialsQuery>,
) -> Result<Json<Vec<ProviderCredentialResponse>>, ApiError> {
    let workspace_id = resolve_workspace_scope(&auth, query.workspace_id)?;
    authorize_workspace_permission(&auth, workspace_id, POLICY_PROVIDERS_ADMIN)?;
    let entries =
        state.canonical_services.ai_catalog.list_provider_credentials(&state, workspace_id).await?;
    Ok(Json(entries.into_iter().map(map_provider_credential).collect()))
}

async fn create_credential(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateProviderCredentialRequest>,
) -> Result<Json<ProviderCredentialResponse>, ApiError> {
    let request_id = request_id.map(|value| value.0.0);
    if let Err(error) =
        authorize_workspace_permission(&auth, payload.workspace_id, POLICY_PROVIDERS_ADMIN)
    {
        record_ai_audit_event(
            &state,
            &auth,
            request_id.clone(),
            "ai.provider_credential.create",
            "rejected",
            Some("provider credential create denied".to_string()),
            Some(format!(
                "principal {} was denied provider credential create for workspace {}",
                auth.principal_id, payload.workspace_id
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
        return Err(error);
    }
    let entry = state
        .canonical_services
        .ai_catalog
        .create_provider_credential(
            &state,
            CreateProviderCredentialCommand {
                workspace_id: payload.workspace_id,
                provider_catalog_id: payload.provider_catalog_id,
                label: payload.label,
                api_key: payload.api_key,
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
            "principal {} created provider credential {} in workspace {}",
            auth.principal_id, entry.id, entry.workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "provider_credential".to_string(),
            subject_id: entry.id,
            workspace_id: Some(entry.workspace_id),
            library_id: None,
            document_id: None,
        }],
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
    authorize_workspace_permission(&auth, credential.workspace_id, POLICY_PROVIDERS_ADMIN)?;
    let entry = state
        .canonical_services
        .ai_catalog
        .update_provider_credential(
            &state,
            UpdateProviderCredentialCommand {
                credential_id,
                label: payload.label,
                api_key: payload.api_key,
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
            "principal {} updated provider credential {} in workspace {}",
            auth.principal_id, entry.id, entry.workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "provider_credential".to_string(),
            subject_id: entry.id,
            workspace_id: Some(entry.workspace_id),
            library_id: None,
            document_id: None,
        }],
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
    authorize_workspace_permission(&auth, payload.workspace_id, POLICY_PROVIDERS_ADMIN)?;
    let entry = state
        .canonical_services
        .ai_catalog
        .create_model_preset(
            &state,
            CreateModelPresetCommand {
                workspace_id: payload.workspace_id,
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
            "principal {} created model preset {} in workspace {}",
            auth.principal_id, entry.id, entry.workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "model_preset".to_string(),
            subject_id: entry.id,
            workspace_id: Some(entry.workspace_id),
            library_id: None,
            document_id: None,
        }],
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
    authorize_workspace_permission(&auth, preset.workspace_id, POLICY_PROVIDERS_ADMIN)?;
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
            "principal {} updated model preset {} in workspace {}",
            auth.principal_id, entry.id, entry.workspace_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "model_preset".to_string(),
            subject_id: entry.id,
            workspace_id: Some(entry.workspace_id),
            library_id: None,
            document_id: None,
        }],
    )
    .await;
    Ok(Json(map_model_preset(entry)))
}

async fn list_library_bindings(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<LibraryModelBindingResponse>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_PROVIDERS_ADMIN).await?;
    let entries =
        state.canonical_services.ai_catalog.list_library_bindings(&state, library_id).await?;
    Ok(Json(entries.into_iter().map(map_library_binding).collect()))
}

async fn create_library_binding(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Json(payload): Json<CreateLibraryBindingRequest>,
) -> Result<Json<LibraryModelBindingResponse>, ApiError> {
    let request_id = request_id.map(|value| value.0.0);
    if let Err(error) =
        authorize_workspace_permission(&auth, payload.workspace_id, POLICY_PROVIDERS_ADMIN)
    {
        record_ai_audit_event(
            &state,
            &auth,
            request_id.clone(),
            "ai.library_binding.create",
            "rejected",
            Some("library binding create denied".to_string()),
            Some(format!(
                "principal {} was denied library binding create for workspace {} library {}",
                auth.principal_id, payload.workspace_id, payload.library_id
            )),
            vec![AppendAuditEventSubjectCommand {
                subject_kind: "workspace".to_string(),
                subject_id: payload.workspace_id,
                workspace_id: Some(payload.workspace_id),
                library_id: Some(payload.library_id),
                document_id: None,
            }],
        )
        .await;
        return Err(error);
    }
    let library = state.canonical_services.catalog.get_library(&state, payload.library_id).await?;
    if library.workspace_id != payload.workspace_id {
        return Err(ApiError::BadRequest("libraryId does not belong to workspaceId".to_string()));
    }
    if let Err(error) = authorize_library_permission(
        &auth,
        library.workspace_id,
        library.id,
        POLICY_PROVIDERS_ADMIN,
    ) {
        record_ai_audit_event(
            &state,
            &auth,
            request_id.clone(),
            "ai.library_binding.create",
            "rejected",
            Some("library binding create denied".to_string()),
            Some(format!(
                "principal {} was denied library binding create for workspace {} library {}",
                auth.principal_id, library.workspace_id, library.id
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
        return Err(error);
    }
    let entry = state
        .canonical_services
        .ai_catalog
        .create_library_binding(
            &state,
            CreateLibraryBindingCommand {
                workspace_id: payload.workspace_id,
                library_id: payload.library_id,
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
        request_id,
        "ai.library_binding.create",
        "succeeded",
        Some(format!("library binding {} created", entry.id)),
        Some(format!(
            "principal {} created library binding {} for library {}",
            auth.principal_id, entry.id, entry.library_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "library_binding".to_string(),
            subject_id: entry.id,
            workspace_id: Some(entry.workspace_id),
            library_id: Some(entry.library_id),
            document_id: None,
        }],
    )
    .await;
    Ok(Json(map_library_binding(entry)))
}

async fn update_library_binding(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(binding_id): Path<Uuid>,
    Json(payload): Json<UpdateLibraryBindingRequest>,
) -> Result<Json<LibraryModelBindingResponse>, ApiError> {
    let binding =
        load_library_binding_and_authorize(&auth, &state, binding_id, POLICY_PROVIDERS_ADMIN)
            .await?;
    let entry = state
        .canonical_services
        .ai_catalog
        .update_library_binding(
            &state,
            UpdateLibraryBindingCommand {
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
        "ai.library_binding.update",
        "succeeded",
        Some(format!("library binding {} updated", entry.id)),
        Some(format!(
            "principal {} updated library binding {} for library {}",
            auth.principal_id, entry.id, entry.library_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "library_binding".to_string(),
            subject_id: entry.id,
            workspace_id: Some(binding.workspace_id),
            library_id: Some(binding.library_id),
            document_id: None,
        }],
    )
    .await;
    Ok(Json(map_library_binding(entry)))
}

async fn validate_library_binding(
    auth: AuthContext,
    State(state): State<AppState>,
    request_id: Option<axum::Extension<RequestId>>,
    Path(binding_id): Path<Uuid>,
) -> Result<Json<BindingValidationResponse>, ApiError> {
    let binding =
        load_library_binding_and_authorize(&auth, &state, binding_id, POLICY_PROVIDERS_ADMIN)
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
        "ai.library_binding.validate",
        "succeeded",
        Some(format!("library binding {} validation requested", binding_id)),
        Some(format!(
            "principal {} requested validation for library binding {}",
            auth.principal_id, binding_id
        )),
        vec![AppendAuditEventSubjectCommand {
            subject_kind: "library_binding".to_string(),
            subject_id: binding_id,
            workspace_id: Some(binding.workspace_id),
            library_id: Some(binding.library_id),
            document_id: None,
        }],
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

fn map_provider(entry: ProviderCatalogEntry) -> ProviderCatalogEntryResponse {
    ProviderCatalogEntryResponse {
        id: entry.id,
        provider_kind: entry.provider_kind,
        display_name: entry.display_name,
        api_style: entry.api_style,
        lifecycle_state: entry.lifecycle_state,
    }
}

fn map_model(entry: ModelCatalogEntry) -> ModelCatalogEntryResponse {
    ModelCatalogEntryResponse {
        id: entry.id,
        provider_catalog_id: entry.provider_catalog_id,
        model_name: entry.model_name,
        capability_kind: entry.capability_kind,
        modality_kind: entry.modality_kind,
        context_window: entry.context_window,
        max_output_tokens: entry.max_output_tokens,
    }
}

fn map_price(entry: PriceCatalogEntry) -> PriceCatalogEntryResponse {
    PriceCatalogEntryResponse {
        id: entry.id,
        model_catalog_id: entry.model_catalog_id,
        billing_unit: entry.billing_unit,
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
        workspace_id: entry.workspace_id,
        provider_catalog_id: entry.provider_catalog_id,
        label: entry.label,
        api_key_summary: summarize_api_key(&entry.api_key),
        credential_state: entry.credential_state,
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

fn summarize_api_key(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "stored".to_string();
    }
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
        workspace_id: entry.workspace_id,
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

fn map_library_binding(entry: LibraryModelBinding) -> LibraryModelBindingResponse {
    LibraryModelBindingResponse {
        id: entry.id,
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

fn resolve_workspace_scope(
    auth: &AuthContext,
    requested_workspace_id: Option<Uuid>,
) -> Result<Uuid, ApiError> {
    match requested_workspace_id {
        Some(workspace_id) => {
            if !auth.can_access_workspace(workspace_id) {
                return Err(ApiError::Unauthorized);
            }
            Ok(workspace_id)
        }
        None if auth.visible_workspace_ids.len() == 1 => {
            auth.visible_workspace_ids.iter().copied().next().ok_or(ApiError::Unauthorized)
        }
        None => auth
            .workspace_id
            .filter(|workspace_id| auth.can_access_workspace(*workspace_id))
            .ok_or(ApiError::Unauthorized),
    }
}
