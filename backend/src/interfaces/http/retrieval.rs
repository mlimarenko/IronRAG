use axum::{
    Json, Router,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        query_intelligence::{ContextAssemblyMetadata, QueryPlanningMetadata, RerankMetadata},
        query_modes::RuntimeQueryMode,
        runtime_query::RuntimeQueryReference,
    },
    infra::repositories,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_QUERY_READ, POLICY_QUERY_RUN, load_project_and_authorize},
        router_support::ApiError,
    },
    services::{
        chat_sessions::ChatSessionsService,
        pricing_catalog,
        query_runtime::{
            PersistedRuntimeQuery, RuntimeAnswerQueryResult, RuntimeQueryRequest,
            execute_answer_query, persist_answer_query_result,
        },
    },
};

type UsageTokens = (Option<i32>, Option<i32>, Option<i32>);

#[derive(Serialize)]
pub struct RetrievalRunSummary {
    pub id: Uuid,
    pub project_id: Uuid,
    pub session_id: Option<Uuid>,
    pub query_text: String,
    pub model_profile_id: Option<Uuid>,
    pub top_k: i32,
    pub response_text: Option<String>,
    pub answer_status: String,
    pub weak_grounding: bool,
}

#[derive(Serialize)]
pub struct RetrievalRunDetail {
    pub id: Uuid,
    pub project_id: Uuid,
    pub session_id: Option<Uuid>,
    pub query_text: String,
    pub model_profile_id: Option<Uuid>,
    pub top_k: i32,
    pub response_text: Option<String>,
    pub answer_status: String,
    pub weak_grounding: bool,
    pub references: Vec<String>,
    pub matched_chunk_ids: Vec<Uuid>,
    pub warning: Option<String>,
    pub warning_kind: Option<String>,
    pub debug_json: serde_json::Value,
}

#[derive(Serialize)]
pub struct QueryResponse {
    pub retrieval_run_id: Uuid,
    pub query_id: Uuid,
    pub project_id: Uuid,
    pub session_id: Uuid,
    pub user_message_id: Uuid,
    pub assistant_message_id: Uuid,
    pub answer: String,
    pub references: Vec<String>,
    pub structured_references: Vec<StructuredReferenceResponse>,
    pub mode: String,
    pub grounding_status: String,
    pub provider: ProviderDescriptorResponse,
    pub planning: QueryPlanningMetadata,
    pub rerank: RerankMetadata,
    pub context_assembly: ContextAssemblyMetadata,
    pub answer_status: String,
    pub weak_grounding: bool,
    pub warning: Option<String>,
    pub warning_kind: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct StructuredReferenceResponse {
    pub kind: String,
    pub reference_id: Uuid,
    pub excerpt: Option<String>,
    pub rank: usize,
    pub score: Option<f32>,
}

#[derive(Clone, Serialize)]
pub struct ProviderDescriptorResponse {
    pub provider_kind: String,
    pub model_name: String,
}

#[derive(Deserialize)]
pub struct ProjectScopedQuery {
    pub project_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct CreateRetrievalRunRequest {
    pub project_id: Uuid,
    pub query_text: String,
    pub model_profile_id: Option<Uuid>,
    pub top_k: Option<i32>,
    pub response_text: Option<String>,
}

#[derive(Deserialize)]
pub struct QueryRequest {
    pub project_id: Uuid,
    pub session_id: Option<Uuid>,
    pub query_text: String,
    pub mode: Option<RuntimeQueryMode>,
    pub model_profile_id: Option<Uuid>,
    pub embedding_model_profile_id: Option<Uuid>,
    pub top_k: Option<i32>,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route(
            "/retrieval-runs",
            axum::routing::get(list_retrieval_runs).post(create_retrieval_run),
        )
        .route("/retrieval-runs/{id}", axum::routing::get(get_retrieval_run_detail))
        .route("/query", axum::routing::post(run_query))
}

async fn list_retrieval_runs(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ProjectScopedQuery>,
) -> Result<Json<Vec<RetrievalRunSummary>>, ApiError> {
    auth.require_any_scope(POLICY_QUERY_READ)?;
    if let Some(project_id) = query.project_id {
        load_project_and_authorize(&auth, &state, project_id, POLICY_QUERY_READ).await?;
    }

    let rows = repositories::list_retrieval_runs(&state.persistence.postgres, query.project_id)
        .await
        .map_err(|error| {
            error!(
                auth_token_id = %auth.token_id,
                project_id = ?query.project_id,
                ?error,
                "failed to list retrieval runs",
            );
            ApiError::Internal
        })?;
    let items: Vec<RetrievalRunSummary> = if auth.token_kind == "instance_admin" {
        rows
    } else {
        let workspace_id = auth.workspace_id.ok_or(ApiError::Unauthorized)?;
        let mut visible = Vec::new();
        for row in rows {
            let project =
                repositories::get_project_by_id(&state.persistence.postgres, row.project_id)
                    .await
                    .map_err(|error| {
                        error!(
                            auth_token_id = %auth.token_id,
                            workspace_id = %workspace_id,
                            project_id = %row.project_id,
                            ?error,
                            "failed to load project while filtering retrieval runs",
                        );
                        ApiError::Internal
                    })?
                    .ok_or_else(|| {
                        ApiError::NotFound(format!("project {} not found", row.project_id))
                    })?;
            if project.workspace_id == workspace_id {
                visible.push(row);
            }
        }
        visible
    }
    .into_iter()
    .map(|row| {
        let (answer_status, weak_grounding, _, _, _warning, _warning_kind) =
            extract_retrieval_debug(&row.debug_json);
        RetrievalRunSummary {
            id: row.id,
            project_id: row.project_id,
            session_id: row.session_id,
            query_text: row.query_text,
            model_profile_id: row.model_profile_id,
            top_k: row.top_k,
            response_text: row.response_text,
            answer_status,
            weak_grounding,
        }
    })
    .collect();

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = ?auth.workspace_id,
        project_id = ?query.project_id,
        retrieval_run_count = items.len(),
        "listed retrieval runs",
    );

    Ok(Json(items))
}

async fn create_retrieval_run(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateRetrievalRunRequest>,
) -> Result<Json<RetrievalRunSummary>, ApiError> {
    auth.require_any_scope(POLICY_QUERY_RUN)?;
    if payload.query_text.trim().is_empty() {
        warn!(
            auth_token_id = %auth.token_id,
            project_id = %payload.project_id,
            model_profile_id = ?payload.model_profile_id,
            "rejecting retrieval run creation with empty query_text",
        );
        return Err(ApiError::BadRequest("query_text must not be empty".into()));
    }
    let top_k = payload.top_k.unwrap_or(8);
    if top_k <= 0 {
        warn!(
            auth_token_id = %auth.token_id,
            project_id = %payload.project_id,
            model_profile_id = ?payload.model_profile_id,
            top_k,
            "rejecting retrieval run creation with non-positive top_k",
        );
        return Err(ApiError::BadRequest("top_k must be greater than zero".into()));
    }

    let project =
        load_project_and_authorize(&auth, &state, payload.project_id, POLICY_QUERY_RUN).await?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %payload.project_id,
        model_profile_id = ?payload.model_profile_id,
        top_k,
        query_len = payload.query_text.trim().chars().count(),
        response_present = payload.response_text.is_some(),
        "accepted retrieval run request",
    );

    let row = repositories::create_retrieval_run(
        &state.persistence.postgres,
        payload.project_id,
        None,
        &payload.query_text,
        payload.model_profile_id,
        top_k,
        payload.response_text.as_deref(),
        serde_json::json!({"mode":"manual"}),
    )
    .await
    .map_err(|error| {
        error!(
            auth_token_id = %auth.token_id,
            workspace_id = %project.workspace_id,
            project_id = %payload.project_id,
            model_profile_id = ?payload.model_profile_id,
            top_k,
            ?error,
            "failed to create retrieval run",
        );
        ApiError::Internal
    })?;

    let (answer_status, weak_grounding, _, _, _, _) = extract_retrieval_debug(&row.debug_json);

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %row.project_id,
        retrieval_run_id = %row.id,
        model_profile_id = ?row.model_profile_id,
        top_k = row.top_k,
        answer_status,
        weak_grounding,
        "created retrieval run",
    );

    Ok(Json(RetrievalRunSummary {
        id: row.id,
        project_id: row.project_id,
        session_id: row.session_id,
        query_text: row.query_text,
        model_profile_id: row.model_profile_id,
        top_k: row.top_k,
        response_text: row.response_text,
        answer_status,
        weak_grounding,
    }))
}

async fn get_retrieval_run_detail(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<RetrievalRunDetail>, ApiError> {
    auth.require_any_scope(POLICY_QUERY_READ)?;

    let row = repositories::get_retrieval_run_by_id(&state.persistence.postgres, id)
        .await
        .map_err(|error| {
            error!(
                auth_token_id = %auth.token_id,
                retrieval_run_id = %id,
                ?error,
                "failed to load retrieval run detail",
            );
            ApiError::Internal
        })?
        .ok_or_else(|| ApiError::NotFound(format!("retrieval_run {id} not found")))?;

    let project =
        load_project_and_authorize(&auth, &state, row.project_id, POLICY_QUERY_READ).await?;

    let (answer_status, weak_grounding, references, matched_chunk_ids, warning, warning_kind) =
        extract_retrieval_debug(&row.debug_json);

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %row.project_id,
        retrieval_run_id = %row.id,
        model_profile_id = ?row.model_profile_id,
        top_k = row.top_k,
        answer_status,
        weak_grounding,
        reference_count = references.len(),
        matched_chunk_count = matched_chunk_ids.len(),
        "loaded retrieval run detail",
    );

    Ok(Json(RetrievalRunDetail {
        id: row.id,
        project_id: row.project_id,
        session_id: row.session_id,
        query_text: row.query_text,
        model_profile_id: row.model_profile_id,
        top_k: row.top_k,
        response_text: row.response_text,
        answer_status,
        weak_grounding,
        references,
        matched_chunk_ids,
        warning,
        warning_kind,
        debug_json: row.debug_json,
    }))
}

async fn run_query(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    let project =
        load_project_and_authorize(&auth, &state, payload.project_id, POLICY_QUERY_RUN).await?;
    Ok(Json(run_query_with_workspace(&state, project.workspace_id, &payload, auth.token_id).await?))
}

pub(crate) async fn run_query_with_workspace(
    state: &AppState,
    workspace_id: Uuid,
    payload: &QueryRequest,
    auth_token_id: Uuid,
) -> Result<QueryResponse, ApiError> {
    let top_k = payload.top_k.unwrap_or(8);
    if let Err(error) = validate_query_payload(payload) {
        warn!(
            auth_token_id = %auth_token_id,
            workspace_id = %workspace_id,
            project_id = %payload.project_id,
            model_profile_id = ?payload.model_profile_id,
            embedding_model_profile_id = ?payload.embedding_model_profile_id,
            top_k,
            query_len = payload.query_text.trim().chars().count(),
            error = %error,
            "rejecting query request",
        );
        return Err(error);
    }
    let started_at = Instant::now();

    info!(
        auth_token_id = %auth_token_id,
        workspace_id = %workspace_id,
        project_id = %payload.project_id,
        model_profile_id = ?payload.model_profile_id,
        embedding_model_profile_id = ?payload.embedding_model_profile_id,
        top_k,
        query_len = payload.query_text.trim().chars().count(),
        "accepted query request",
    );

    let session = match resolve_query_session(state, workspace_id, payload).await {
        Ok(session) => session,
        Err(error) => {
            log_query_request_failure(
                workspace_id,
                payload,
                top_k,
                started_at.elapsed().as_millis(),
                "resolve_query_session",
                auth_token_id,
                &error,
            );
            return Err(error);
        }
    };

    let runtime_request = RuntimeQueryRequest {
        library_id: payload.project_id,
        question: payload.query_text.trim().to_string(),
        system_prompt: Some(session.system_prompt.clone()),
        mode: payload.mode.unwrap_or_else(|| {
            session.preferred_mode.parse::<RuntimeQueryMode>().unwrap_or(RuntimeQueryMode::Hybrid)
        }),
        top_k: usize::try_from(top_k).unwrap_or(8).clamp(1, 12),
        include_debug: true,
    };

    let query_result = match execute_answer_query(state, &runtime_request).await {
        Ok(result) => result,
        Err(error) => {
            let error_chain = error
                .chain()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .join(" | caused by: ");
            error!(
                auth_token_id = %auth_token_id,
                workspace_id = %workspace_id,
                project_id = %payload.project_id,
                session_id = ?payload.session_id,
                mode = %runtime_request.mode.as_str(),
                top_k = runtime_request.top_k,
                question_len = runtime_request.question.chars().count(),
                raw_error = %error,
                error_chain,
                "runtime answer query execution failed before api mapping",
            );
            let api_error = map_runtime_query_error(&error);
            log_query_request_failure(
                workspace_id,
                payload,
                top_k,
                started_at.elapsed().as_millis(),
                "execute_answer_query",
                auth_token_id,
                &api_error,
            );
            return Err(api_error);
        }
    };

    let persisted_query =
        match persist_answer_query_result(state, &runtime_request, &query_result).await {
            Ok(persisted) => persisted,
            Err(error) => {
                let api_error = map_runtime_query_error(&error);
                log_query_request_failure(
                    workspace_id,
                    payload,
                    top_k,
                    started_at.elapsed().as_millis(),
                    "persist_answer_query_result",
                    auth_token_id,
                    &api_error,
                );
                return Err(api_error);
            }
        };

    let structured_reference_count = query_result.structured.references.len();
    let (prompt_tokens, completion_tokens, total_tokens) =
        extract_usage_tokens(&query_result.usage_json);
    let response = match persist_runtime_query_artifacts(
        state,
        workspace_id,
        payload,
        &query_result,
        &persisted_query,
        session.id,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            log_query_request_failure(
                workspace_id,
                payload,
                top_k,
                started_at.elapsed().as_millis(),
                "persist_runtime_query_artifacts",
                auth_token_id,
                &error,
            );
            return Err(error);
        }
    };

    let chat_sessions = ChatSessionsService::new();
    if chat_sessions.is_placeholder_title(&session.title) {
        let next_title = chat_sessions.derive_title_from_question(&payload.query_text);
        if next_title != session.title {
            repositories::update_chat_session_title(
                &state.persistence.postgres,
                session.id,
                &next_title,
            )
            .await
            .map_err(|error| {
                error!(
                    auth_token_id = %auth_token_id,
                    workspace_id = %workspace_id,
                    project_id = %payload.project_id,
                    session_id = %session.id,
                    ?error,
                    "failed to update chat session title after first question",
                );
                ApiError::Internal
            })?;
        }
    }

    if response.0.weak_grounding {
        warn!(
            auth_token_id = %auth_token_id,
            workspace_id = %workspace_id,
            project_id = %payload.project_id,
            retrieval_run_id = %response.0.retrieval_run_id,
            query_id = %response.0.query_id,
            session_id = %response.0.session_id,
            provider_kind = %query_result.provider.provider_kind.as_str(),
            model_name = %query_result.provider.model_name,
            answer_status = %response.0.answer_status,
            mode = %response.0.mode,
            grounding_status = %response.0.grounding_status,
            reference_count = response.0.references.len(),
            structured_reference_count,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            latency_ms = started_at.elapsed().as_millis(),
            "query completed with weak grounding",
        );
    } else {
        info!(
            auth_token_id = %auth_token_id,
            workspace_id = %workspace_id,
            project_id = %payload.project_id,
            retrieval_run_id = %response.0.retrieval_run_id,
            query_id = %response.0.query_id,
            session_id = %response.0.session_id,
            provider_kind = %query_result.provider.provider_kind.as_str(),
            model_name = %query_result.provider.model_name,
            answer_status = %response.0.answer_status,
            mode = %response.0.mode,
            grounding_status = %response.0.grounding_status,
            reference_count = response.0.references.len(),
            structured_reference_count,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            latency_ms = started_at.elapsed().as_millis(),
            "query completed",
        );
    }

    Ok(response.0)
}

fn validate_query_payload(payload: &QueryRequest) -> Result<(), ApiError> {
    if payload.query_text.trim().is_empty() {
        return Err(ApiError::BadRequest("query_text must not be empty".into()));
    }
    if payload.top_k.unwrap_or(8) <= 0 {
        return Err(ApiError::BadRequest("top_k must be greater than zero".into()));
    }
    Ok(())
}

fn log_query_request_failure(
    workspace_id: Uuid,
    payload: &QueryRequest,
    top_k: i32,
    latency_ms: u128,
    phase: &str,
    auth_token_id: Uuid,
    error: &ApiError,
) {
    match error {
        ApiError::Internal => {
            error!(
                auth_token_id = %auth_token_id,
                workspace_id = %workspace_id,
                project_id = %payload.project_id,
                session_id = ?payload.session_id,
                model_profile_id = ?payload.model_profile_id,
                embedding_model_profile_id = ?payload.embedding_model_profile_id,
                top_k,
                query_len = payload.query_text.trim().chars().count(),
                latency_ms,
                phase,
                error = %error,
                "query request failed",
            );
        }
        _ => {
            warn!(
                auth_token_id = %auth_token_id,
                workspace_id = %workspace_id,
                project_id = %payload.project_id,
                session_id = ?payload.session_id,
                model_profile_id = ?payload.model_profile_id,
                embedding_model_profile_id = ?payload.embedding_model_profile_id,
                top_k,
                query_len = payload.query_text.trim().chars().count(),
                latency_ms,
                phase,
                error = %error,
                "query request failed",
            );
        }
    }
}

fn llm_provider_label(provider_kind: &str) -> &str {
    match provider_kind {
        "openai" => "OpenAI",
        "deepseek" => "DeepSeek",
        "qwen" => "Qwen",
        _ => provider_kind,
    }
}

fn map_llm_gateway_error(error: &anyhow::Error, provider_kind: &str) -> ApiError {
    let message = error.to_string();
    let provider_label = llm_provider_label(provider_kind);

    if message.contains("missing OpenAI API key")
        || message.contains("missing DeepSeek API key")
        || message.contains("missing Qwen API key")
    {
        return ApiError::Conflict(format!(
            "AI assistant is unavailable because the {provider_label} API key is not configured",
        ));
    }

    if message.contains("unsupported provider kind:") {
        return ApiError::Conflict(format!(
            "AI assistant is unavailable because provider `{provider_kind}` is not supported",
        ));
    }

    if message.contains("status=401") || message.contains("status=403") {
        return ApiError::Conflict(format!(
            "AI assistant is unavailable because the {provider_label} credentials are invalid or expired",
        ));
    }

    ApiError::Internal
}

async fn resolve_query_session(
    state: &AppState,
    workspace_id: Uuid,
    payload: &QueryRequest,
) -> Result<repositories::ChatSessionRow, ApiError> {
    let chat_sessions = ChatSessionsService::new();
    match payload.session_id {
        Some(session_id) => {
            let session = repositories::get_chat_session_by_id(&state.persistence.postgres, session_id)
                .await
                .map_err(|error| {
                    error!(
                        project_id = %payload.project_id,
                        session_id = %session_id,
                        ?error,
                        "failed to load chat session for query",
                    );
                    ApiError::Internal
                })?
                .ok_or_else(|| ApiError::NotFound(format!("chat_session {session_id} not found")))?;
            if session.project_id != payload.project_id {
                return Err(ApiError::BadRequest("session_id does not belong to project_id".into()));
            }
            Ok(session)
        }
        None => repositories::create_seeded_chat_session(
            &state.persistence.postgres,
            workspace_id,
            payload.project_id,
            chat_sessions.placeholder_title(),
            &chat_sessions.default_system_prompt(),
            chat_sessions
                .derive_prompt_state(&chat_sessions.default_system_prompt())
                .as_str(),
            chat_sessions.recommended_mode().as_str(),
        )
        .await
        .map_err(|error| {
            error!(project_id = %payload.project_id, ?error, "failed to create chat session for query");
            ApiError::Internal
        }),
    }
}

fn map_runtime_query_error(error: &anyhow::Error) -> ApiError {
    let message = error.to_string();

    if message.contains("missing OpenAI API key") {
        return map_llm_gateway_error(error, "openai");
    }
    if message.contains("missing DeepSeek API key") {
        return map_llm_gateway_error(error, "deepseek");
    }
    if message.contains("missing Qwen API key") {
        return map_llm_gateway_error(error, "qwen");
    }
    if message.contains("unsupported provider kind:") {
        return ApiError::Conflict(message);
    }
    if message.contains("status=401") || message.contains("status=403") {
        if message.to_ascii_lowercase().contains("deepseek") {
            return map_llm_gateway_error(error, "deepseek");
        }
        if message.to_ascii_lowercase().contains("qwen") {
            return map_llm_gateway_error(error, "qwen");
        }
        return map_llm_gateway_error(error, "openai");
    }
    if message.contains("failed to generate grounded answer")
        || message.contains("failed to embed runtime query")
    {
        return ApiError::Conflict(message);
    }

    ApiError::Internal
}

fn answer_status_for_grounding(grounding_status: &str) -> (&'static str, bool, Option<String>) {
    match grounding_status {
        "grounded" => ("grounded", false, None),
        "partial" => (
            "partially_grounded",
            true,
            Some(
                "The answer is only partially grounded. Inspect the references before using it as a source of truth."
                    .to_string(),
            ),
        ),
        "weak" => (
            "weakly_grounded",
            true,
            Some(
                "The answer was generated with limited graph evidence; inspect references and graph coverage."
                    .to_string(),
            ),
        ),
        _ => (
            "ungrounded",
            true,
            Some(
                "No grounded evidence was available in the active library yet. The answer may be incomplete."
                    .to_string(),
            ),
        ),
    }
}

fn map_structured_references(
    references: &[RuntimeQueryReference],
) -> Vec<StructuredReferenceResponse> {
    references
        .iter()
        .map(|reference| StructuredReferenceResponse {
            kind: reference.kind.clone(),
            reference_id: reference.reference_id,
            excerpt: reference.excerpt.clone(),
            rank: reference.rank,
            score: reference.score,
        })
        .collect()
}

fn format_reference_label(reference: &StructuredReferenceResponse) -> String {
    let label = match reference.kind.as_str() {
        "chunk" => "Chunk",
        "node" => "Node",
        "edge" => "Edge",
        _ => "Reference",
    };

    match reference.excerpt.as_deref() {
        Some(excerpt) if !excerpt.trim().is_empty() => {
            format!("{label} {} · {}", reference.rank, excerpt.trim())
        }
        _ => format!("{label} {} · {}", reference.rank, reference.reference_id),
    }
}

fn collect_matched_chunk_ids(references: &[StructuredReferenceResponse]) -> Vec<Uuid> {
    references
        .iter()
        .filter(|reference| reference.kind == "chunk")
        .map(|reference| reference.reference_id)
        .collect()
}

async fn persist_runtime_query_artifacts(
    state: &AppState,
    workspace_id: Uuid,
    payload: &QueryRequest,
    query_result: &RuntimeAnswerQueryResult,
    persisted_query: &PersistedRuntimeQuery,
    session_id: Uuid,
) -> Result<Json<QueryResponse>, ApiError> {
    let grounding_status = query_result.structured.grounding_status.clone().as_str().to_string();
    let (answer_status, weak_grounding, derived_warning) =
        answer_status_for_grounding(&grounding_status);
    let warning = query_result.warning.clone().or(derived_warning);
    let warning_kind = query_result.warning_kind.clone();
    let structured_references = map_structured_references(&query_result.structured.references);
    let references = structured_references.iter().map(format_reference_label).collect::<Vec<_>>();
    let matched_chunk_ids = collect_matched_chunk_ids(&structured_references);

    let user_message = repositories::create_chat_message(
        &state.persistence.postgres,
        session_id,
        payload.project_id,
        "user",
        &payload.query_text,
        None,
    )
    .await
    .map_err(|error| {
        error!(
            project_id = %payload.project_id,
            session_id = %session_id,
            ?error,
            "failed to persist user chat message",
        );
        ApiError::Internal
    })?;

    let row = repositories::create_retrieval_run(
        &state.persistence.postgres,
        payload.project_id,
        Some(session_id),
        &payload.query_text,
        payload.model_profile_id,
        payload.top_k.unwrap_or(8),
        Some(query_result.answer.as_str()),
        serde_json::json!({
            "mode": query_result.structured.planned_mode.as_str(),
            "requested_mode": query_result.structured.mode.as_str(),
            "provider_kind": query_result.provider.provider_kind.as_str(),
            "model_name": &query_result.provider.model_name,
            "usage": query_result.usage_json.clone(),
            "query_id": persisted_query.execution.id,
            "grounding_status": grounding_status.clone(),
            "structured_references": &structured_references,
            "references": &references,
            "matched_chunk_ids": &matched_chunk_ids,
            "planning": &query_result.structured.enrichment.planning,
            "rerank": &query_result.structured.enrichment.rerank,
            "context_assembly": &query_result.structured.enrichment.context_assembly,
            "answer_status": answer_status,
            "weak_grounding": weak_grounding,
            "warning": warning.clone(),
            "warning_kind": warning_kind.clone(),
            "session_id": session_id,
            "runtime_debug": query_result.structured.debug_json.clone(),
        }),
    )
    .await
    .map_err(|error| {
        error!(
            project_id = %payload.project_id,
            model_profile_id = ?payload.model_profile_id,
            provider_kind = %query_result.provider.provider_kind.as_str(),
            model_name = %query_result.provider.model_name,
            query_id = %persisted_query.execution.id,
            ?error,
            "failed to persist retrieval run artifacts",
        );
        ApiError::Internal
    })?;

    let assistant_message = repositories::create_chat_message(
        &state.persistence.postgres,
        session_id,
        payload.project_id,
        "assistant",
        query_result.answer.as_str(),
        Some(row.id),
    )
    .await
    .map_err(|error| {
        error!(
            project_id = %payload.project_id,
            session_id = %session_id,
            retrieval_run_id = %row.id,
            ?error,
            "failed to persist assistant chat message",
        );
        ApiError::Internal
    })?;

    let (prompt_tokens, completion_tokens, total_tokens) =
        extract_usage_tokens(&query_result.usage_json);
    let usage_event = repositories::create_usage_event(
        &state.persistence.postgres,
        &repositories::NewUsageEvent {
            workspace_id: Some(workspace_id),
            project_id: Some(payload.project_id),
            provider_account_id: None,
            model_profile_id: payload.model_profile_id,
            usage_kind: "runtime_query".to_string(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            raw_usage_json: query_result.usage_json.clone(),
        },
    )
    .await
    .map_err(|error| {
        error!(
            workspace_id = %workspace_id,
            project_id = %payload.project_id,
            model_profile_id = ?payload.model_profile_id,
            query_id = %persisted_query.execution.id,
            ?error,
            "failed to create query usage event",
        );
        ApiError::Internal
    })?;

    let provider_kind = query_result.provider.provider_kind.as_str();
    let pricing_resolution = pricing_catalog::resolve_usage_cost(
        state,
        pricing_catalog::UsageCostLookupRequest {
            workspace_id: Some(workspace_id),
            provider_kind: provider_kind.to_string(),
            model_name: query_result.provider.model_name.clone(),
            capability: "answer".to_string(),
            billing_unit: "per_1m_tokens".to_string(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            at: usage_event.created_at,
        },
    )
    .await
    .map_err(|error| {
        error!(
            workspace_id = %workspace_id,
            project_id = %payload.project_id,
            usage_event_id = %usage_event.id,
            provider_kind,
            model_name = %query_result.provider.model_name,
            ?error,
            "failed to resolve query pricing",
        );
        ApiError::Internal
    })?;

    if let Some(estimated_cost) = pricing_resolution.estimated_cost {
        repositories::create_cost_ledger(
            &state.persistence.postgres,
            Some(workspace_id),
            Some(payload.project_id),
            usage_event.id,
            provider_kind,
            &query_result.provider.model_name,
            estimated_cost,
            pricing_resolution.pricing_snapshot_json.clone(),
        )
        .await
        .map_err(|error| {
            error!(
                workspace_id = %workspace_id,
                project_id = %payload.project_id,
                usage_event_id = %usage_event.id,
                provider_kind,
                model_name = %query_result.provider.model_name,
                ?error,
                "failed to create query cost ledger entry",
            );
            ApiError::Internal
        })?;
    } else {
        warn!(
            workspace_id = %workspace_id,
            project_id = %payload.project_id,
            usage_event_id = %usage_event.id,
            provider_kind,
            model_name = %query_result.provider.model_name,
            pricing_status = pricing_catalog::pricing_status_label(&pricing_resolution.status),
            "skipping query cost ledger because pricing could not be resolved",
        );
    }

    info!(
        workspace_id = %workspace_id,
        project_id = %payload.project_id,
        session_id = %session_id,
        retrieval_run_id = %row.id,
        query_id = %persisted_query.execution.id,
        usage_event_id = %usage_event.id,
        provider_kind,
        model_name = %query_result.provider.model_name,
        answer_status,
        weak_grounding,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        pricing_status = pricing_catalog::pricing_status_label(&pricing_resolution.status),
        estimated_cost = ?pricing_resolution.estimated_cost,
        "persisted runtime query artifacts",
    );

    Ok(Json(QueryResponse {
        retrieval_run_id: row.id,
        query_id: persisted_query.execution.id,
        project_id: row.project_id,
        session_id,
        user_message_id: user_message.id,
        assistant_message_id: assistant_message.id,
        answer: query_result.answer.clone(),
        references,
        structured_references,
        mode: query_result.structured.planned_mode.as_str().to_string(),
        grounding_status,
        provider: ProviderDescriptorResponse {
            provider_kind: provider_kind.to_string(),
            model_name: query_result.provider.model_name.clone(),
        },
        planning: query_result.structured.enrichment.planning.clone(),
        rerank: query_result.structured.enrichment.rerank.clone(),
        context_assembly: query_result.structured.enrichment.context_assembly.clone(),
        answer_status: answer_status.to_string(),
        weak_grounding,
        warning,
        warning_kind,
    }))
}

fn extract_retrieval_debug(
    debug_json: &serde_json::Value,
) -> (String, bool, Vec<String>, Vec<Uuid>, Option<String>, Option<String>) {
    let answer_status = debug_json
        .get("answer_status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let weak_grounding =
        debug_json.get("weak_grounding").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let references = debug_json
        .get("references")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let matched_chunk_ids = debug_json
        .get("matched_chunk_ids")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter_map(|value| Uuid::parse_str(value).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let warning =
        debug_json.get("warning").and_then(serde_json::Value::as_str).map(ToOwned::to_owned);
    let warning_kind =
        debug_json.get("warning_kind").and_then(serde_json::Value::as_str).map(ToOwned::to_owned);

    (answer_status, weak_grounding, references, matched_chunk_ids, warning, warning_kind)
}

fn extract_usage_tokens(usage_json: &serde_json::Value) -> UsageTokens {
    let prompt_tokens = usage_json
        .get("prompt_tokens")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let completion_tokens = usage_json
        .get("completion_tokens")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let total_tokens = usage_json
        .get("total_tokens")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .or_else(|| match (prompt_tokens, completion_tokens) {
            (Some(prompt), Some(completion)) => Some(prompt + completion),
            _ => None,
        });

    (prompt_tokens, completion_tokens, total_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_missing_openai_key_to_conflict() {
        let error = anyhow::anyhow!("missing OpenAI API key");

        let api_error = map_llm_gateway_error(&error, "openai");

        assert!(matches!(
            api_error,
            ApiError::Conflict(message)
                if message == "AI assistant is unavailable because the OpenAI API key is not configured"
        ));
    }

    #[test]
    fn maps_invalid_provider_credentials_to_conflict() {
        let error = anyhow::anyhow!(
            "provider request failed: provider=openai status=401 Unauthorized body={{}}",
        );

        let api_error = map_llm_gateway_error(&error, "openai");

        assert!(matches!(
            api_error,
            ApiError::Conflict(message)
                if message
                    == "AI assistant is unavailable because the OpenAI credentials are invalid or expired"
        ));
    }

    #[test]
    fn maps_missing_qwen_key_to_conflict() {
        let error = anyhow::anyhow!("missing Qwen API key");

        let api_error = map_llm_gateway_error(&error, "qwen");

        assert!(matches!(
            api_error,
            ApiError::Conflict(message)
                if message == "AI assistant is unavailable because the Qwen API key is not configured"
        ));
    }

    #[test]
    fn keeps_unknown_gateway_failures_internal() {
        let error = anyhow::anyhow!("request timed out");

        let api_error = map_llm_gateway_error(&error, "openai");

        assert!(matches!(api_error, ApiError::Internal));
    }
}
