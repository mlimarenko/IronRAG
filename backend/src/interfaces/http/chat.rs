use axum::{
    Json, Router,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        query_modes::RuntimeQueryMode,
        ui_chat::{
            ChatSessionDetailModel, ChatSessionSettingsModel, ChatSessionSummaryModel,
            ChatThreadMessageModel, ChatThreadProviderModel, ChatThreadReferenceModel,
        },
    },
    infra::repositories,
    interfaces::http::{
        router_support::ApiError,
        ui_support::{UiActiveContext, UiSessionContext, load_active_ui_context},
    },
    services::{
        chat_sessions::ChatSessionsService,
        query_runtime::{parse_runtime_query_enrichment, parse_runtime_query_warning},
    },
};

const MAX_CHAT_TITLE_CHARS: usize = 120;
const MAX_SYSTEM_PROMPT_CHARS: usize = 4_000;

#[derive(Deserialize)]
pub struct ChatSessionsQuery {
    pub project_id: Uuid,
}

#[derive(Deserialize)]
pub struct CreateChatSessionRequest {
    pub project_id: Uuid,
    pub title: Option<String>,
    pub system_prompt: Option<String>,
    pub preferred_mode: Option<RuntimeQueryMode>,
}

#[derive(Deserialize)]
pub struct UpdateChatSessionRequest {
    pub title: Option<String>,
    pub system_prompt: Option<String>,
    pub preferred_mode: Option<RuntimeQueryMode>,
    pub restore_default: Option<bool>,
}

#[derive(Serialize)]
pub struct ChatSessionEnvelope {
    pub session: ChatSessionDetailModel,
    pub settings: ChatSessionSettingsModel,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/chat/sessions", axum::routing::get(list_chat_sessions).post(create_chat_session))
        .route(
            "/chat/sessions/{id}",
            axum::routing::get(get_chat_session).patch(update_chat_session),
        )
        .route("/chat/sessions/{id}/messages", axum::routing::get(list_chat_messages))
}

fn normalize_title(value: &str, fallback: &str) -> String {
    let trimmed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = trimmed.trim();
    if title.is_empty() {
        return fallback.to_string();
    }

    let truncated = title.chars().take(MAX_CHAT_TITLE_CHARS).collect::<String>();
    if title.chars().count() > MAX_CHAT_TITLE_CHARS {
        format!("{}...", truncated.trim_end())
    } else {
        truncated
    }
}

fn validate_system_prompt(prompt: &str) -> Result<(), ApiError> {
    if prompt.trim().is_empty() {
        return Err(ApiError::BadRequest("system_prompt must not be empty".into()));
    }
    if prompt.chars().count() > MAX_SYSTEM_PROMPT_CHARS {
        return Err(ApiError::BadRequest(format!(
            "system_prompt must be at most {MAX_SYSTEM_PROMPT_CHARS} characters",
        )));
    }
    Ok(())
}

fn normalize_preview(value: Option<&str>) -> Option<String> {
    value.and_then(|preview| {
        let normalized = preview.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() { None } else { Some(normalized) }
    })
}

fn map_session_summary(row: repositories::ChatSessionListRow) -> ChatSessionSummaryModel {
    ChatSessionSummaryModel {
        session_id: row.id.to_string(),
        title: row.title,
        message_count: row.message_count,
        last_message_preview: normalize_preview(row.last_message_preview.as_deref()),
        updated_at: row.updated_at.to_rfc3339(),
        prompt_state: row.prompt_state,
        preferred_mode: row.preferred_mode,
        is_empty: row.message_count == 0,
    }
}

fn map_session_detail(row: repositories::ChatSessionDetailRow) -> ChatSessionDetailModel {
    ChatSessionDetailModel {
        session_id: row.id.to_string(),
        title: row.title,
        message_count: row.message_count,
        last_message_preview: normalize_preview(row.last_message_preview.as_deref()),
        created_at: row.created_at.to_rfc3339(),
        updated_at: row.updated_at.to_rfc3339(),
        prompt_state: row.prompt_state,
        preferred_mode: row.preferred_mode,
        is_empty: row.message_count == 0,
    }
}

fn map_session_settings(row: &repositories::ChatSessionDetailRow) -> ChatSessionSettingsModel {
    ChatSessionSettingsModel {
        session_id: row.id.to_string(),
        system_prompt: row.system_prompt.clone(),
        prompt_state: row.prompt_state.clone(),
        preferred_mode: row.preferred_mode.clone(),
        default_prompt_available: true,
    }
}

fn map_message(
    row: repositories::ChatThreadMessageRow,
) -> Result<ChatThreadMessageModel, serde_json::Error> {
    let debug_json = row.retrieval_debug_json.unwrap_or_else(|| serde_json::json!({}));
    let fallback_mode = debug_json
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("hybrid")
        .parse::<RuntimeQueryMode>()
        .unwrap_or(RuntimeQueryMode::Hybrid);
    let enrichment = if row.retrieval_run_id.is_some() {
        Some(parse_runtime_query_enrichment(&debug_json, fallback_mode))
    } else {
        None
    };
    let (warning, warning_kind) = parse_runtime_query_warning(&debug_json);
    let references = debug_json
        .get("structured_references")
        .cloned()
        .map(serde_json::from_value::<Vec<StructuredReferencePayload>>)
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .map(|reference| ChatThreadReferenceModel {
            kind: reference.kind,
            reference_id: reference.reference_id.to_string(),
            excerpt: reference.excerpt,
            rank: reference.rank,
            score: reference.score,
        })
        .collect::<Vec<_>>();

    Ok(ChatThreadMessageModel {
        id: row.id.to_string(),
        role: row.role,
        content: row.content,
        created_at: row.created_at.to_rfc3339(),
        query_id: debug_json
            .get("query_id")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        mode: debug_json.get("mode").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
        grounding_status: debug_json
            .get("grounding_status")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        provider: debug_json.get("provider_kind").and_then(serde_json::Value::as_str).map(
            |provider_kind| ChatThreadProviderModel {
                provider_kind: provider_kind.to_string(),
                model_name: debug_json
                    .get("model_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
        ),
        references,
        planning: enrichment.as_ref().map(|value| value.planning.clone()),
        rerank: enrichment.as_ref().map(|value| value.rerank.clone()),
        context_assembly: enrichment.as_ref().map(|value| value.context_assembly.clone()),
        warning,
        warning_kind,
    })
}

async fn load_session_detail(
    state: &AppState,
    id: Uuid,
) -> Result<repositories::ChatSessionDetailRow, ApiError> {
    repositories::get_chat_session_detail_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            error!(session_id = %id, ?error, "failed to load chat session detail");
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("chat_session {id} not found")))
}

async fn load_chat_ui_context(
    state: &AppState,
    ui_session: &UiSessionContext,
    requested_project_id: Option<Uuid>,
) -> Result<UiActiveContext, ApiError> {
    let active = load_active_ui_context(state, ui_session).await?;
    if requested_project_id.is_some_and(|project_id| project_id != active.project.id) {
        return Err(ApiError::Unauthorized);
    }
    Ok(active)
}

fn ensure_session_matches_active_project(
    session: &repositories::ChatSessionDetailRow,
    active: &UiActiveContext,
) -> Result<(), ApiError> {
    if session.project_id != active.project.id {
        return Err(ApiError::Unauthorized);
    }
    Ok(())
}

async fn create_chat_session(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateChatSessionRequest>,
) -> Result<Json<ChatSessionEnvelope>, ApiError> {
    let active = load_chat_ui_context(&state, &ui_session, Some(payload.project_id)).await?;
    let service = ChatSessionsService::new();
    let title = normalize_title(
        payload.title.as_deref().unwrap_or(service.placeholder_title()),
        service.placeholder_title(),
    );
    let system_prompt = service.normalize_system_prompt(
        payload.system_prompt.as_deref().unwrap_or(&service.default_system_prompt()),
    );
    validate_system_prompt(&system_prompt)?;
    let prompt_state = service.derive_prompt_state(&system_prompt);
    let preferred_mode = payload.preferred_mode.unwrap_or(service.recommended_mode());

    let session = repositories::create_seeded_chat_session(
        &state.persistence.postgres,
        active.project.workspace_id,
        active.project.id,
        &title,
        &system_prompt,
        prompt_state.as_str(),
        preferred_mode.as_str(),
    )
    .await
    .map_err(|error| {
        error!(
            ui_session_id = %ui_session.session_id,
            user_id = %ui_session.user_id,
            workspace_id = %active.project.workspace_id,
            project_id = %active.project.id,
            ?error,
            "failed to create chat session",
        );
        ApiError::Internal
    })?;

    let detail = load_session_detail(&state, session.id).await?;

    info!(
        ui_session_id = %ui_session.session_id,
        user_id = %ui_session.user_id,
        workspace_id = %active.project.workspace_id,
        project_id = %active.project.id,
        session_id = %session.id,
        "created chat session",
    );

    Ok(Json(ChatSessionEnvelope {
        session: map_session_detail(detail.clone()),
        settings: map_session_settings(&detail),
    }))
}

async fn list_chat_sessions(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Query(query): Query<ChatSessionsQuery>,
) -> Result<Json<Vec<ChatSessionSummaryModel>>, ApiError> {
    let active = load_chat_ui_context(&state, &ui_session, Some(query.project_id)).await?;
    let items =
        repositories::list_chat_sessions_by_project(&state.persistence.postgres, active.project.id)
            .await
            .map_err(|error| {
                error!(
                    ui_session_id = %ui_session.session_id,
                    user_id = %ui_session.user_id,
                    workspace_id = %active.project.workspace_id,
                    project_id = %active.project.id,
                    ?error,
                    "failed to list chat sessions",
                );
                ApiError::Internal
            })?
            .into_iter()
            .map(map_session_summary)
            .collect::<Vec<_>>();

    info!(
        ui_session_id = %ui_session.session_id,
        user_id = %ui_session.user_id,
        workspace_id = %active.project.workspace_id,
        project_id = %active.project.id,
        session_count = items.len(),
        "listed chat sessions",
    );

    Ok(Json(items))
}

async fn get_chat_session(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ChatSessionEnvelope>, ApiError> {
    let session = load_session_detail(&state, id).await?;
    let active = load_chat_ui_context(&state, &ui_session, Some(session.project_id)).await?;
    ensure_session_matches_active_project(&session, &active)?;

    Ok(Json(ChatSessionEnvelope {
        session: map_session_detail(session.clone()),
        settings: map_session_settings(&session),
    }))
}

async fn update_chat_session(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateChatSessionRequest>,
) -> Result<Json<ChatSessionEnvelope>, ApiError> {
    let current = load_session_detail(&state, id).await?;
    let active = load_chat_ui_context(&state, &ui_session, Some(current.project_id)).await?;
    ensure_session_matches_active_project(&current, &active)?;
    let service = ChatSessionsService::new();

    if let Some(title) = payload.title.as_deref() {
        let next_title = normalize_title(title, service.placeholder_title());
        if next_title != current.title {
            repositories::update_chat_session_title(&state.persistence.postgres, id, &next_title)
                .await
                .map_err(|error| {
                    error!(
                        ui_session_id = %ui_session.session_id,
                        user_id = %ui_session.user_id,
                        workspace_id = %active.project.workspace_id,
                        project_id = %current.project_id,
                        session_id = %id,
                        ?error,
                        "failed to update chat title",
                    );
                    ApiError::Internal
                })?;
        }
    }

    let next_prompt = if payload.restore_default.unwrap_or(false) {
        service.restore_default_prompt()
    } else {
        service.normalize_system_prompt(
            payload.system_prompt.as_deref().unwrap_or(current.system_prompt.as_str()),
        )
    };
    validate_system_prompt(&next_prompt)?;
    let next_mode = payload.preferred_mode.unwrap_or_else(|| {
        current.preferred_mode.parse::<RuntimeQueryMode>().unwrap_or(service.recommended_mode())
    });
    let prompt_state = service.derive_prompt_state(&next_prompt);

    repositories::update_chat_session_settings(
        &state.persistence.postgres,
        id,
        &next_prompt,
        prompt_state.as_str(),
        next_mode.as_str(),
    )
    .await
    .map_err(|error| {
        error!(
            ui_session_id = %ui_session.session_id,
            user_id = %ui_session.user_id,
            workspace_id = %active.project.workspace_id,
            project_id = %current.project_id,
            session_id = %id,
            ?error,
            "failed to update chat session settings",
        );
        ApiError::Internal
    })?;

    let session = load_session_detail(&state, id).await?;

    Ok(Json(ChatSessionEnvelope {
        session: map_session_detail(session.clone()),
        settings: map_session_settings(&session),
    }))
}

async fn list_chat_messages(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ChatThreadMessageModel>>, ApiError> {
    let session = load_session_detail(&state, id).await?;
    let active = load_chat_ui_context(&state, &ui_session, Some(session.project_id)).await?;
    ensure_session_matches_active_project(&session, &active)?;
    let items = repositories::list_chat_thread_messages_by_session(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            error!(
                ui_session_id = %ui_session.session_id,
                user_id = %ui_session.user_id,
                workspace_id = %active.project.workspace_id,
                session_id = %id,
                ?error,
                "failed to list chat messages",
            );
            ApiError::Internal
        })?
        .into_iter()
        .map(map_message)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            error!(
                ui_session_id = %ui_session.session_id,
                user_id = %ui_session.user_id,
                workspace_id = %active.project.workspace_id,
                session_id = %id,
                ?error,
                "failed to map chat messages",
            );
            ApiError::Internal
        })?;

    Ok(Json(items))
}

#[derive(Debug, Deserialize)]
struct StructuredReferencePayload {
    kind: String,
    reference_id: Uuid,
    excerpt: Option<String>,
    rank: usize,
    score: Option<f32>,
}
