use anyhow::Context;
use axum::{
    Json, Router,
    extract::{Path, State},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::provider_profiles::SupportedProviderKind,
    infra::repositories::{self, RuntimeProviderProfileRow},
    integrations::provider_catalog::{
        CAPABILITY_CHAT, CAPABILITY_EMBEDDINGS, CAPABILITY_VISION, ROLE_ANSWER, ROLE_EMBEDDING,
        ROLE_INDEXING, ROLE_VISION, SupportedProviderCatalogEntry, model_is_available_for_role,
        provider_is_configured, provider_supports_capability, supported_provider_catalog,
    },
    interfaces::http::{
        auth::AuthContext, authorization::POLICY_PROVIDERS_ADMIN, router_support::ApiError,
        runtime_support::load_library_and_authorize,
    },
    services::provider_validation,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportedProviderResponse {
    pub provider_kind: String,
    pub supported_capabilities: Vec<String>,
    pub default_models: BTreeMap<String, String>,
    pub available_models: BTreeMap<String, Vec<String>>,
    pub is_configured: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogResponse {
    pub providers: Vec<SupportedProviderResponse>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryProviderProfileResponse {
    pub library_id: Uuid,
    pub indexing_provider_kind: String,
    pub indexing_model_name: String,
    pub embedding_provider_kind: String,
    pub embedding_model_name: String,
    pub answer_provider_kind: String,
    pub answer_model_name: String,
    pub vision_provider_kind: String,
    pub vision_model_name: String,
    pub last_validated_at: Option<String>,
    pub last_validation_status: Option<String>,
    pub last_validation_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibraryProviderProfileUpdate {
    indexing_provider_kind: String,
    indexing_model_name: String,
    embedding_provider_kind: String,
    embedding_model_name: String,
    answer_provider_kind: String,
    answer_model_name: String,
    vision_provider_kind: String,
    vision_model_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderValidationRequest {
    provider_kind: String,
    model_name: String,
    capability: String,
    library_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderValidationResponse {
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub status: String,
    pub checked_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryProviderValidationResponse {
    pub library_id: Uuid,
    pub status: String,
    pub checked_at: String,
    pub checks: Vec<ProviderValidationResponse>,
    pub profile: LibraryProviderProfileResponse,
}

#[derive(Debug, Clone)]
pub(crate) struct NormalizedProviderProfileUpdate {
    pub indexing_provider_kind: SupportedProviderKind,
    pub indexing_model_name: String,
    pub embedding_provider_kind: SupportedProviderKind,
    pub embedding_model_name: String,
    pub answer_provider_kind: SupportedProviderKind,
    pub answer_model_name: String,
    pub vision_provider_kind: SupportedProviderKind,
    pub vision_model_name: String,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/runtime/providers", axum::routing::get(list_supported_providers))
        .route(
            "/runtime/libraries/{library_id}/provider-profile",
            axum::routing::get(get_library_provider_profile).put(update_library_provider_profile),
        )
        .route("/runtime/providers/validate", axum::routing::post(validate_provider_connectivity))
}

async fn list_supported_providers(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<ProviderCatalogResponse>, ApiError> {
    auth.require_any_scope(POLICY_PROVIDERS_ADMIN)?;
    Ok(Json(ProviderCatalogResponse { providers: map_supported_provider_catalog(&state) }))
}

async fn get_library_provider_profile(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<LibraryProviderProfileResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_PROVIDERS_ADMIN).await?;
    let profile = load_or_create_provider_profile_row(&state, library_id).await?;
    Ok(Json(map_library_provider_profile(library_id, &profile)))
}

async fn update_library_provider_profile(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<LibraryProviderProfileUpdate>,
) -> Result<Json<LibraryProviderProfileResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_PROVIDERS_ADMIN).await?;
    let normalized = normalize_provider_profile_update(payload)?;
    ensure_profile_selection_available(
        &state,
        normalized.indexing_provider_kind,
        ROLE_INDEXING,
        &normalized.indexing_model_name,
        "indexingProviderKind",
        "indexingModelName",
    )?;
    ensure_profile_selection_available(
        &state,
        normalized.embedding_provider_kind,
        ROLE_EMBEDDING,
        &normalized.embedding_model_name,
        "embeddingProviderKind",
        "embeddingModelName",
    )?;
    ensure_profile_selection_available(
        &state,
        normalized.answer_provider_kind,
        ROLE_ANSWER,
        &normalized.answer_model_name,
        "answerProviderKind",
        "answerModelName",
    )?;
    ensure_profile_selection_available(
        &state,
        normalized.vision_provider_kind,
        ROLE_VISION,
        &normalized.vision_model_name,
        "visionProviderKind",
        "visionModelName",
    )?;
    let row = repositories::upsert_runtime_provider_profile(
        &state.persistence.postgres,
        library_id,
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

    Ok(Json(map_library_provider_profile(library_id, &row)))
}

async fn validate_provider_connectivity(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<ProviderValidationRequest>,
) -> Result<Json<ProviderValidationResponse>, ApiError> {
    auth.require_any_scope(POLICY_PROVIDERS_ADMIN)?;
    if let Some(library_id) = payload.library_id {
        let _ =
            load_library_and_authorize(&auth, &state, library_id, POLICY_PROVIDERS_ADMIN).await?;
    }

    let request = normalize_validation_request(payload)?;
    let response = validate_provider_combination(
        &state,
        request.library_id,
        request.provider_kind.as_str(),
        &request.model_name,
        &request.capability,
    )
    .await?;

    Ok(Json(response))
}

pub(crate) fn map_supported_provider_catalog(state: &AppState) -> Vec<SupportedProviderResponse> {
    supported_provider_catalog(&state.settings, &state.runtime_provider_defaults)
        .into_iter()
        .map(map_supported_provider_entry)
        .collect()
}

pub(crate) fn map_supported_provider_entry(
    entry: SupportedProviderCatalogEntry,
) -> SupportedProviderResponse {
    SupportedProviderResponse {
        provider_kind: entry.provider_kind.as_str().to_string(),
        supported_capabilities: entry.supported_capabilities,
        default_models: entry.default_models,
        available_models: entry.available_models,
        is_configured: entry.is_configured,
    }
}

pub(crate) fn map_library_provider_profile(
    library_id: Uuid,
    row: &RuntimeProviderProfileRow,
) -> LibraryProviderProfileResponse {
    LibraryProviderProfileResponse {
        library_id,
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

pub(crate) async fn load_or_create_provider_profile_row(
    state: &AppState,
    library_id: Uuid,
) -> Result<RuntimeProviderProfileRow, ApiError> {
    if let Some(row) =
        repositories::get_runtime_provider_profile(&state.persistence.postgres, library_id)
            .await
            .map_err(|_| ApiError::Internal)?
    {
        return Ok(row);
    }

    let defaults = &state.runtime_provider_defaults;
    repositories::upsert_runtime_provider_profile(
        &state.persistence.postgres,
        library_id,
        defaults.indexing.provider_kind.as_str(),
        &defaults.indexing.model_name,
        defaults.embedding.provider_kind.as_str(),
        &defaults.embedding.model_name,
        defaults.answer.provider_kind.as_str(),
        &defaults.answer.model_name,
        defaults.vision.provider_kind.as_str(),
        &defaults.vision.model_name,
    )
    .await
    .map_err(|_| ApiError::Internal)
}

pub(crate) async fn validate_library_provider_profile(
    state: &AppState,
    library_id: Uuid,
) -> Result<LibraryProviderValidationResponse, ApiError> {
    let profile = load_or_create_provider_profile_row(state, library_id).await?;
    let checks = vec![
        (
            profile.indexing_provider_kind.clone(),
            profile.indexing_model_name.clone(),
            CAPABILITY_CHAT,
        ),
        (
            profile.embedding_provider_kind.clone(),
            profile.embedding_model_name.clone(),
            CAPABILITY_EMBEDDINGS,
        ),
        (profile.answer_provider_kind.clone(), profile.answer_model_name.clone(), CAPABILITY_CHAT),
        (
            profile.vision_provider_kind.clone(),
            profile.vision_model_name.clone(),
            CAPABILITY_VISION,
        ),
    ];

    let mut results = Vec::with_capacity(checks.len());
    let mut first_error = None;
    for (provider_kind, model_name, capability) in checks {
        let result = validate_provider_combination(
            state,
            Some(library_id),
            &provider_kind,
            &model_name,
            capability,
        )
        .await?;
        if first_error.is_none() {
            first_error = result.error.clone();
        }
        results.push(result);
    }

    let status =
        if results.iter().all(|item| item.status == "passed") { "passed" } else { "failed" };

    let updated_profile = repositories::update_runtime_provider_profile_validation(
        &state.persistence.postgres,
        library_id,
        status,
        first_error.as_deref(),
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(LibraryProviderValidationResponse {
        library_id,
        status: status.to_string(),
        checked_at: Utc::now().to_rfc3339(),
        checks: results,
        profile: map_library_provider_profile(library_id, &updated_profile),
    })
}

pub(crate) async fn validate_provider_combination(
    state: &AppState,
    library_id: Option<Uuid>,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
) -> Result<ProviderValidationResponse, ApiError> {
    if !state.graph_runtime.live_validation_enabled {
        return Err(ApiError::Conflict(
            "live provider validation is disabled on this backend".into(),
        ));
    }

    let checked_at = Utc::now();
    let validation_result = match parse_supported_provider_kind(provider_kind) {
        Ok(parsed_provider_kind) => {
            if !provider_is_configured(&state.settings, parsed_provider_kind) {
                Err(anyhow::anyhow!("provider {provider_kind} is not configured on this backend"))
            } else if !provider_supports_capability(parsed_provider_kind, capability) {
                Err(anyhow::anyhow!(
                    "provider {provider_kind} does not support {capability} capability"
                ))
            } else if !model_is_available_for_capability(
                state,
                parsed_provider_kind,
                capability,
                model_name,
            ) {
                Err(anyhow::anyhow!(
                    "model {model_name} is not available for provider {provider_kind} capability {capability}"
                ))
            } else {
                run_capability_validation(state, provider_kind, model_name, capability).await
            }
        }
        Err(error) => Err(anyhow::anyhow!(error)),
    };

    let (status, error_message) = match validation_result {
        Ok(()) => ("passed".to_string(), None),
        Err(error) => ("failed".to_string(), Some(error.to_string())),
    };

    repositories::append_runtime_provider_validation_log(
        &state.persistence.postgres,
        library_id,
        provider_kind,
        model_name,
        capability,
        &status,
        error_message.as_deref(),
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    if let Some(library_id) = library_id {
        load_or_create_provider_profile_row(state, library_id).await?;
        repositories::update_runtime_provider_profile_validation(
            &state.persistence.postgres,
            library_id,
            &status,
            error_message.as_deref(),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
    }

    Ok(ProviderValidationResponse {
        provider_kind: provider_kind.to_string(),
        model_name: model_name.to_string(),
        capability: capability.to_string(),
        status,
        checked_at: checked_at.to_rfc3339(),
        error: error_message,
    })
}

async fn run_capability_validation(
    state: &AppState,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
) -> anyhow::Result<()> {
    match capability {
        "chat" => provider_validation::validate_chat_provider(state, provider_kind, model_name)
            .await
            .with_context(|| format!("chat validation failed for {provider_kind}:{model_name}")),
        "embeddings" => {
            provider_validation::validate_embedding_provider(state, provider_kind, model_name)
                .await
                .with_context(|| {
                    format!("embedding validation failed for {provider_kind}:{model_name}")
                })
        }
        "vision" => provider_validation::validate_vision_provider(state, provider_kind, model_name)
            .await
            .with_context(|| format!("vision validation failed for {provider_kind}:{model_name}")),
        other => Err(anyhow::anyhow!("unsupported provider capability: {other}")),
    }
}

fn normalize_provider_profile_update(
    payload: LibraryProviderProfileUpdate,
) -> Result<NormalizedProviderProfileUpdate, ApiError> {
    normalize_provider_profile_fields(
        &payload.indexing_provider_kind,
        &payload.indexing_model_name,
        &payload.embedding_provider_kind,
        &payload.embedding_model_name,
        &payload.answer_provider_kind,
        &payload.answer_model_name,
        &payload.vision_provider_kind,
        &payload.vision_model_name,
    )
}

fn normalize_validation_request(
    payload: ProviderValidationRequest,
) -> Result<ProviderValidationRequest, ApiError> {
    let parsed_provider_kind = parse_supported_provider_kind(&payload.provider_kind)?;
    let provider_kind = parsed_provider_kind.as_str().to_string();
    let model_name = normalize_model_name(&payload.model_name, "modelName")?;
    let capability = normalize_capability(&payload.capability)?;
    ensure_provider_supports_capability(parsed_provider_kind, &capability, "providerKind")?;
    Ok(ProviderValidationRequest {
        provider_kind,
        model_name,
        capability,
        library_id: payload.library_id,
    })
}

pub(crate) fn normalize_provider_profile_fields(
    indexing_provider_kind: &str,
    indexing_model_name: &str,
    embedding_provider_kind: &str,
    embedding_model_name: &str,
    answer_provider_kind: &str,
    answer_model_name: &str,
    vision_provider_kind: &str,
    vision_model_name: &str,
) -> Result<NormalizedProviderProfileUpdate, ApiError> {
    let indexing_provider_kind = parse_supported_provider_kind(indexing_provider_kind)?;
    let embedding_provider_kind = parse_supported_provider_kind(embedding_provider_kind)?;
    let answer_provider_kind = parse_supported_provider_kind(answer_provider_kind)?;
    let vision_provider_kind = parse_supported_provider_kind(vision_provider_kind)?;

    ensure_provider_supports_capability(
        indexing_provider_kind,
        CAPABILITY_CHAT,
        "indexingProviderKind",
    )?;
    ensure_provider_supports_capability(
        embedding_provider_kind,
        CAPABILITY_EMBEDDINGS,
        "embeddingProviderKind",
    )?;
    ensure_provider_supports_capability(
        answer_provider_kind,
        CAPABILITY_CHAT,
        "answerProviderKind",
    )?;
    ensure_provider_supports_capability(
        vision_provider_kind,
        CAPABILITY_VISION,
        "visionProviderKind",
    )?;

    Ok(NormalizedProviderProfileUpdate {
        indexing_provider_kind,
        indexing_model_name: normalize_model_name(indexing_model_name, "indexingModelName")?,
        embedding_provider_kind,
        embedding_model_name: normalize_model_name(embedding_model_name, "embeddingModelName")?,
        answer_provider_kind,
        answer_model_name: normalize_model_name(answer_model_name, "answerModelName")?,
        vision_provider_kind,
        vision_model_name: normalize_model_name(vision_model_name, "visionModelName")?,
    })
}

pub(crate) fn ensure_profile_selection_available(
    state: &AppState,
    provider_kind: SupportedProviderKind,
    role: &str,
    model_name: &str,
    provider_field_name: &str,
    model_field_name: &str,
) -> Result<(), ApiError> {
    if !provider_is_configured(&state.settings, provider_kind) {
        return Err(ApiError::BadRequest(format!(
            "{provider_field_name} provider '{}' is not configured on this backend",
            provider_kind.as_str(),
        )));
    }

    if model_is_available_for_role(
        &state.settings,
        &state.runtime_provider_defaults,
        provider_kind,
        role,
        model_name,
    ) {
        return Ok(());
    }

    Err(ApiError::BadRequest(format!(
        "{model_field_name} model '{model_name}' is not available for provider '{}' role {role}",
        provider_kind.as_str(),
    )))
}

pub(crate) fn parse_supported_provider_kind(
    value: &str,
) -> Result<SupportedProviderKind, ApiError> {
    value.parse().map_err(ApiError::BadRequest)
}

pub(crate) fn normalize_model_name(value: &str, field_name: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ApiError::BadRequest(format!("{field_name} is required")));
    }
    Ok(value.to_string())
}

pub(crate) fn normalize_capability(value: &str) -> Result<String, ApiError> {
    match value.trim().to_ascii_lowercase().as_str() {
        CAPABILITY_CHAT => Ok(CAPABILITY_CHAT.to_string()),
        CAPABILITY_EMBEDDINGS => Ok(CAPABILITY_EMBEDDINGS.to_string()),
        CAPABILITY_VISION => Ok(CAPABILITY_VISION.to_string()),
        other => Err(ApiError::BadRequest(format!("unsupported provider capability: {other}"))),
    }
}

pub(crate) fn ensure_provider_supports_capability(
    provider_kind: SupportedProviderKind,
    capability: &str,
    field_name: &str,
) -> Result<(), ApiError> {
    if provider_supports_capability(provider_kind, capability) {
        return Ok(());
    }

    Err(ApiError::BadRequest(format!(
        "{field_name} provider '{}' does not support {capability}",
        provider_kind.as_str(),
    )))
}

fn model_is_available_for_capability(
    state: &AppState,
    provider_kind: SupportedProviderKind,
    capability: &str,
    model_name: &str,
) -> bool {
    match capability {
        CAPABILITY_CHAT => {
            model_is_available_for_role(
                &state.settings,
                &state.runtime_provider_defaults,
                provider_kind,
                ROLE_INDEXING,
                model_name,
            ) || model_is_available_for_role(
                &state.settings,
                &state.runtime_provider_defaults,
                provider_kind,
                ROLE_ANSWER,
                model_name,
            )
        }
        CAPABILITY_EMBEDDINGS => model_is_available_for_role(
            &state.settings,
            &state.runtime_provider_defaults,
            provider_kind,
            ROLE_EMBEDDING,
            model_name,
        ),
        CAPABILITY_VISION => model_is_available_for_role(
            &state.settings,
            &state.runtime_provider_defaults,
            provider_kind,
            ROLE_VISION,
            model_name,
        ),
        _ => false,
    }
}

pub(crate) fn latest_validation_timestamp(
    results: &[ProviderValidationResponse],
) -> Option<DateTime<Utc>> {
    results
        .iter()
        .filter_map(|item| DateTime::parse_from_rfc3339(&item.checked_at).ok())
        .map(|value| value.with_timezone(&Utc))
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blank_model_name() {
        let error = normalize_model_name("   ", "modelName").expect_err("blank model rejected");
        match error {
            ApiError::BadRequest(message) => assert!(message.contains("modelName")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn normalizes_validation_capability() {
        assert_eq!(normalize_capability("Embeddings").expect("normalize capability"), "embeddings");
    }

    #[test]
    fn rejects_unsupported_provider_capability_combo() {
        let error = normalize_provider_profile_fields(
            "deepseek",
            "deepseek-chat",
            "deepseek",
            "deepseek-embedding",
            "deepseek",
            "deepseek-reasoner",
            "openai",
            "gpt-5-mini",
        )
        .expect_err("invalid provider profile rejected");

        match error {
            ApiError::BadRequest(message) => {
                assert!(message.contains("embeddingProviderKind"));
                assert!(message.contains("does not support embeddings"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn accepts_qwen_for_all_runtime_roles() {
        let normalized = normalize_provider_profile_fields(
            "qwen",
            "qwen-plus",
            "qwen",
            "text-embedding-v4",
            "qwen",
            "qwen-max",
            "qwen",
            "qwen-vl-max",
        )
        .expect("qwen profile accepted");

        assert_eq!(normalized.indexing_provider_kind, SupportedProviderKind::Qwen);
        assert_eq!(normalized.embedding_provider_kind, SupportedProviderKind::Qwen);
        assert_eq!(normalized.answer_provider_kind, SupportedProviderKind::Qwen);
        assert_eq!(normalized.vision_provider_kind, SupportedProviderKind::Qwen);
    }

    #[test]
    fn maps_library_provider_profile_response() {
        let now = Utc::now();
        let row = RuntimeProviderProfileRow {
            project_id: Uuid::nil(),
            indexing_provider_kind: "openai".to_string(),
            indexing_model_name: "gpt-5-mini".to_string(),
            embedding_provider_kind: "openai".to_string(),
            embedding_model_name: "text-embedding-3-large".to_string(),
            answer_provider_kind: "deepseek".to_string(),
            answer_model_name: "deepseek-reasoner".to_string(),
            vision_provider_kind: "openai".to_string(),
            vision_model_name: "gpt-5-mini".to_string(),
            last_validated_at: Some(now),
            last_validation_status: Some("passed".to_string()),
            last_validation_error: None,
            created_at: now,
            updated_at: now,
        };

        let response = map_library_provider_profile(Uuid::nil(), &row);
        assert_eq!(response.library_id, Uuid::nil());
        assert_eq!(response.answer_provider_kind, "deepseek");
        assert_eq!(response.last_validation_status.as_deref(), Some("passed"));
    }
}
