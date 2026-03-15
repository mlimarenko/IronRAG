use axum::{
    Json, Router,
    extract::{Path, Query, State},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{repositories, vector_search},
    integrations::llm::{ChatRequest, EmbeddingRequest},
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_QUERY_READ, POLICY_QUERY_RUN, load_project_and_authorize},
        router_support::ApiError,
    },
    shared::similarity::cosine_similarity,
};

type RankedChunk = (Uuid, f32, repositories::ChunkRow);

type ModelSelection = (String, String, Option<Uuid>);

type UsageTokens = (Option<i32>, Option<i32>, Option<i32>);

struct QueryExecutionResult {
    provider_kind: String,
    model_name: String,
    provider_account_id: Option<Uuid>,
    output_text: String,
    usage_json: serde_json::Value,
    matched_chunks: Vec<repositories::ChunkRow>,
    references: Vec<String>,
    top_k: i32,
    lexical_chunk_count: usize,
    semantic_chunk_count: usize,
}

#[derive(Serialize)]
pub struct RetrievalRunSummary {
    pub id: Uuid,
    pub project_id: Uuid,
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
    pub query_text: String,
    pub model_profile_id: Option<Uuid>,
    pub top_k: i32,
    pub response_text: Option<String>,
    pub answer_status: String,
    pub weak_grounding: bool,
    pub references: Vec<String>,
    pub matched_chunk_ids: Vec<Uuid>,
    pub warning: Option<String>,
    pub debug_json: serde_json::Value,
}

#[derive(Serialize)]
pub struct QueryResponse {
    pub retrieval_run_id: Uuid,
    pub project_id: Uuid,
    pub answer: String,
    pub references: Vec<String>,
    pub mode: String,
    pub answer_status: String,
    pub weak_grounding: bool,
    pub warning: Option<String>,
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
    pub query_text: String,
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
        let (answer_status, weak_grounding, _, _, _warning) =
            extract_retrieval_debug(&row.debug_json);
        RetrievalRunSummary {
            id: row.id,
            project_id: row.project_id,
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

    let (answer_status, weak_grounding, _, _, _) = extract_retrieval_debug(&row.debug_json);

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

    let (answer_status, weak_grounding, references, matched_chunk_ids, warning) =
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
        query_text: row.query_text,
        model_profile_id: row.model_profile_id,
        top_k: row.top_k,
        response_text: row.response_text,
        answer_status,
        weak_grounding,
        references,
        matched_chunk_ids,
        warning,
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
    let top_k = payload.top_k.unwrap_or(8);
    if let Err(error) = validate_query_payload(&payload) {
        warn!(
            auth_token_id = %auth.token_id,
            workspace_id = %project.workspace_id,
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
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %payload.project_id,
        model_profile_id = ?payload.model_profile_id,
        embedding_model_profile_id = ?payload.embedding_model_profile_id,
        top_k,
        query_len = payload.query_text.trim().chars().count(),
        "accepted query request",
    );

    let query_result = match execute_query(&state, &payload).await {
        Ok(result) => result,
        Err(error) => {
            log_query_request_failure(
                project.workspace_id,
                &payload,
                top_k,
                started_at.elapsed().as_millis(),
                "execute_query",
                auth.token_id,
                &error,
            );
            return Err(error);
        }
    };
    let matched_chunk_count = query_result.matched_chunks.len();
    let (prompt_tokens, completion_tokens, total_tokens) =
        extract_usage_tokens(&query_result.usage_json);
    let response = match persist_query_artifacts(&state, &payload, &query_result).await {
        Ok(response) => response,
        Err(error) => {
            log_query_request_failure(
                project.workspace_id,
                &payload,
                top_k,
                started_at.elapsed().as_millis(),
                "persist_query_artifacts",
                auth.token_id,
                &error,
            );
            return Err(error);
        }
    };

    if response.0.weak_grounding {
        warn!(
            auth_token_id = %auth.token_id,
            workspace_id = %project.workspace_id,
            project_id = %payload.project_id,
            retrieval_run_id = %response.0.retrieval_run_id,
            provider_kind = %query_result.provider_kind,
            model_name = %query_result.model_name,
            answer_status = %response.0.answer_status,
            lexical_chunk_count = query_result.lexical_chunk_count,
            semantic_chunk_count = query_result.semantic_chunk_count,
            matched_chunk_count,
            reference_count = response.0.references.len(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            latency_ms = started_at.elapsed().as_millis(),
            "query completed with weak grounding",
        );
    } else {
        info!(
            auth_token_id = %auth.token_id,
            workspace_id = %project.workspace_id,
            project_id = %payload.project_id,
            retrieval_run_id = %response.0.retrieval_run_id,
            provider_kind = %query_result.provider_kind,
            model_name = %query_result.model_name,
            answer_status = %response.0.answer_status,
            lexical_chunk_count = query_result.lexical_chunk_count,
            semantic_chunk_count = query_result.semantic_chunk_count,
            matched_chunk_count,
            reference_count = response.0.references.len(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            latency_ms = started_at.elapsed().as_millis(),
            "query completed",
        );
    }

    Ok(response)
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

async fn execute_query(
    state: &AppState,
    payload: &QueryRequest,
) -> Result<QueryExecutionResult, ApiError> {
    let top_k = payload.top_k.unwrap_or(8);
    let (provider_kind, model_name, provider_account_id) =
        resolve_chat_model(state, payload.model_profile_id).await?;
    let lexical_chunks = repositories::search_chunks_by_project(
        &state.persistence.postgres,
        payload.project_id,
        &payload.query_text,
        top_k,
    )
    .await
    .map_err(|error| {
        error!(
            project_id = %payload.project_id,
            top_k,
            ?error,
            "failed to search lexical retrieval chunks",
        );
        ApiError::Internal
    })?;
    let lexical_chunk_count = lexical_chunks.len();
    let (embedding_provider_kind, embedding_model_name, _) = resolve_embedding_model(
        state,
        payload.embedding_model_profile_id,
        &provider_kind,
        &model_name,
    )
    .await?;

    let semantic_chunks = collect_semantic_chunks(
        state,
        payload.project_id,
        &payload.query_text,
        top_k,
        &embedding_provider_kind,
        &embedding_model_name,
    )
    .await?;
    let semantic_chunk_count = semantic_chunks.len();
    let matched_chunks = rank_chunks(lexical_chunks, semantic_chunks, top_k);
    if matched_chunks.is_empty() {
        warn!(
            project_id = %payload.project_id,
            provider_kind = %provider_kind,
            model_name = %model_name,
            embedding_provider_kind = %embedding_provider_kind,
            embedding_model_name = %embedding_model_name,
            top_k,
            "query has no retrieval evidence; generating answer without matched chunks",
        );
    }
    info!(
        project_id = %payload.project_id,
        provider_kind = %provider_kind,
        model_name = %model_name,
        embedding_provider_kind = %embedding_provider_kind,
        embedding_model_name = %embedding_model_name,
        top_k,
        lexical_chunk_count,
        semantic_chunk_count,
        matched_chunk_count = matched_chunks.len(),
        "retrieval evidence prepared",
    );
    let context_block = build_context_block(&matched_chunks);

    let gateway_response = state
        .llm_gateway
        .generate(ChatRequest {
            provider_kind: provider_kind.clone(),
            model_name: model_name.clone(),
            prompt: format!("{context_block}User question: {}", payload.query_text),
        })
        .await
        .map_err(|error| {
            error!(
                project_id = %payload.project_id,
                provider_kind = %provider_kind,
                model_name = %model_name,
                matched_chunk_count = matched_chunks.len(),
                ?error,
                "llm query generation failed",
            );
            ApiError::Internal
        })?;
    let references = build_references(&matched_chunks);

    Ok(QueryExecutionResult {
        provider_kind,
        model_name,
        provider_account_id,
        output_text: gateway_response.output_text,
        usage_json: gateway_response.usage_json,
        matched_chunks,
        references,
        top_k,
        lexical_chunk_count,
        semantic_chunk_count,
    })
}

async fn persist_query_artifacts(
    state: &AppState,
    payload: &QueryRequest,
    query_result: &QueryExecutionResult,
) -> Result<Json<QueryResponse>, ApiError> {
    let weak_grounding =
        query_result.references.is_empty() || query_result.matched_chunks.len() < 2;
    let answer_status = if weak_grounding { "weakly_grounded" } else { "grounded" };
    let warning = weak_grounding.then_some(
        "The answer was generated with limited retrieval evidence; inspect references and project readiness.".to_string(),
    );

    let row = repositories::create_retrieval_run(
        &state.persistence.postgres,
        payload.project_id,
        &payload.query_text,
        payload.model_profile_id,
        query_result.top_k,
        Some(&query_result.output_text),
        serde_json::json!({
            "mode": "gateway_live",
            "provider_kind": query_result.provider_kind,
            "model_name": query_result.model_name,
            "usage": query_result.usage_json,
            "matched_chunk_ids": query_result.matched_chunks.iter().map(|chunk| chunk.id).collect::<Vec<_>>(),
            "references": query_result.references,
            "answer_status": answer_status,
            "weak_grounding": weak_grounding,
            "warning": warning,
        }),
    )
    .await
    .map_err(|error| {
        error!(
            project_id = %payload.project_id,
            model_profile_id = ?payload.model_profile_id,
            provider_kind = %query_result.provider_kind,
            model_name = %query_result.model_name,
            matched_chunk_count = query_result.matched_chunks.len(),
            ?error,
            "failed to persist retrieval run artifacts",
        );
        ApiError::Internal
    })?;

    let workspace_id =
        repositories::get_project_by_id(&state.persistence.postgres, payload.project_id)
            .await
            .map_err(|error| {
                error!(
                    project_id = %payload.project_id,
                    ?error,
                    "failed to load project while persisting query usage",
                );
                ApiError::Internal
            })?
            .map(|project| project.workspace_id);
    let (prompt_tokens, completion_tokens, total_tokens) =
        extract_usage_tokens(&query_result.usage_json);
    let usage_event = repositories::create_usage_event(
        &state.persistence.postgres,
        &repositories::NewUsageEvent {
            workspace_id,
            project_id: Some(payload.project_id),
            provider_account_id: query_result.provider_account_id,
            model_profile_id: payload.model_profile_id,
            usage_kind: "query".to_string(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            raw_usage_json: query_result.usage_json.clone(),
        },
    )
    .await
    .map_err(|error| {
        error!(
            workspace_id = ?workspace_id,
            project_id = %payload.project_id,
            provider_account_id = ?query_result.provider_account_id,
            model_profile_id = ?payload.model_profile_id,
            ?error,
            "failed to create query usage event",
        );
        ApiError::Internal
    })?;

    let (input_price_per_1m, output_price_per_1m) =
        usage_prices(state, &query_result.provider_kind);
    let estimated_cost = estimate_query_cost(
        prompt_tokens,
        completion_tokens,
        input_price_per_1m,
        output_price_per_1m,
    );

    repositories::create_cost_ledger(
        &state.persistence.postgres,
        workspace_id,
        Some(payload.project_id),
        usage_event.id,
        &query_result.provider_kind,
        &query_result.model_name,
        Decimal::from_f64_retain(estimated_cost).unwrap_or(Decimal::ZERO),
        serde_json::json!({
            "input_price_per_1m": input_price_per_1m,
            "output_price_per_1m": output_price_per_1m,
        }),
    )
    .await
    .map_err(|error| {
        error!(
            workspace_id = ?workspace_id,
            project_id = %payload.project_id,
            usage_event_id = %usage_event.id,
            provider_kind = %query_result.provider_kind,
            model_name = %query_result.model_name,
            estimated_cost_usd = estimated_cost,
            ?error,
            "failed to create query cost ledger entry",
        );
        ApiError::Internal
    })?;

    info!(
        workspace_id = ?workspace_id,
        project_id = %payload.project_id,
        retrieval_run_id = %row.id,
        usage_event_id = %usage_event.id,
        provider_kind = %query_result.provider_kind,
        model_name = %query_result.model_name,
        answer_status,
        weak_grounding,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_cost_usd = estimated_cost,
        "persisted query artifacts",
    );

    Ok(Json(QueryResponse {
        retrieval_run_id: row.id,
        project_id: row.project_id,
        answer: query_result.output_text.clone(),
        references: query_result.references.clone(),
        mode: "gateway_live".into(),
        answer_status: answer_status.to_string(),
        weak_grounding,
        warning,
    }))
}

async fn resolve_chat_model(
    state: &AppState,
    model_profile_id: Option<Uuid>,
) -> Result<ModelSelection, ApiError> {
    match model_profile_id {
        Some(model_profile_id) => {
            let profile = repositories::get_model_profile_by_id(
                &state.persistence.postgres,
                model_profile_id,
            )
            .await
            .map_err(|error| {
                error!(model_profile_id = %model_profile_id, ?error, "failed to load chat model profile");
                ApiError::Internal
            })?
            .ok_or_else(|| ApiError::NotFound("model_profile not found".into()))?;
            let provider = repositories::get_provider_account_by_id(
                &state.persistence.postgres,
                profile.provider_account_id,
            )
            .await
            .map_err(|error| {
                error!(
                    provider_account_id = %profile.provider_account_id,
                    model_profile_id = %model_profile_id,
                    ?error,
                    "failed to load chat provider account",
                );
                ApiError::Internal
            })?
            .ok_or_else(|| ApiError::NotFound("provider_account not found".into()))?;

            info!(
                model_profile_id = %model_profile_id,
                provider_account_id = %profile.provider_account_id,
                provider_kind = %provider.provider_kind,
                model_name = %profile.model_name,
                "resolved explicit chat model",
            );
            Ok((provider.provider_kind, profile.model_name, Some(profile.provider_account_id)))
        }
        None => {
            info!(
                provider_kind = "openai",
                model_name = "unbound-foundation-model",
                "using default chat model",
            );
            Ok(("openai".to_string(), "unbound-foundation-model".to_string(), None))
        }
    }
}

async fn resolve_embedding_model(
    state: &AppState,
    model_profile_id: Option<Uuid>,
    default_provider_kind: &str,
    default_model_name: &str,
) -> Result<ModelSelection, ApiError> {
    match model_profile_id {
        Some(model_profile_id) => {
            let profile = repositories::get_model_profile_by_id(
                &state.persistence.postgres,
                model_profile_id,
            )
            .await
            .map_err(|error| {
                error!(
                    model_profile_id = %model_profile_id,
                    ?error,
                    "failed to load embedding model profile",
                );
                ApiError::Internal
            })?
            .ok_or_else(|| ApiError::NotFound("embedding model_profile not found".into()))?;
            let provider = repositories::get_provider_account_by_id(
                &state.persistence.postgres,
                profile.provider_account_id,
            )
            .await
            .map_err(|error| {
                error!(
                    provider_account_id = %profile.provider_account_id,
                    model_profile_id = %model_profile_id,
                    ?error,
                    "failed to load embedding provider account",
                );
                ApiError::Internal
            })?
            .ok_or_else(|| ApiError::NotFound("embedding provider_account not found".into()))?;

            info!(
                model_profile_id = %model_profile_id,
                provider_account_id = %profile.provider_account_id,
                provider_kind = %provider.provider_kind,
                model_name = %profile.model_name,
                "resolved explicit embedding model",
            );
            Ok((provider.provider_kind, profile.model_name, Some(profile.provider_account_id)))
        }
        None => {
            info!(
                provider_kind = %default_provider_kind,
                model_name = %default_model_name,
                "using query model as embedding model fallback",
            );
            Ok((default_provider_kind.to_string(), default_model_name.to_string(), None))
        }
    }
}

async fn collect_semantic_chunks(
    state: &AppState,
    project_id: Uuid,
    query_text: &str,
    top_k: i32,
    provider_kind: &str,
    model_name: &str,
) -> Result<Vec<RankedChunk>, ApiError> {
    let embedding_result = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: provider_kind.to_string(),
            model_name: model_name.to_string(),
            input: query_text.to_string(),
        })
        .await;

    match embedding_result {
        Ok(query_embedding) => {
            info!(
                project_id = %project_id,
                provider_kind,
                model_name,
                top_k,
                embedding_dimensions = query_embedding.embedding.len(),
                "generated query embedding for semantic retrieval",
            );
            search_semantic_chunks(state, project_id, top_k, &query_embedding.embedding).await
        }
        Err(error) => {
            warn!(
                project_id = %project_id,
                provider_kind,
                model_name,
                ?error,
                "semantic retrieval embedding failed; continuing with lexical results",
            );
            Ok(Vec::new())
        }
    }
}

async fn search_semantic_chunks(
    state: &AppState,
    project_id: Uuid,
    top_k: i32,
    query_embedding: &[f32],
) -> Result<Vec<RankedChunk>, ApiError> {
    match vector_search::search_chunks_by_project_embedding(
        &state.persistence.postgres,
        project_id,
        query_embedding,
        top_k,
    )
    .await
    {
        Ok(scored_rows) if !scored_rows.is_empty() => {
            info!(
                project_id = %project_id,
                top_k,
                semantic_chunk_count = scored_rows.len(),
                "semantic retrieval used vector index",
            );
            Ok(scored_rows
                .into_iter()
                .map(|row| {
                    let score = row.cosine_similarity_score();
                    let chunk = row.into_chunk();
                    (chunk.id, score, chunk)
                })
                .collect())
        }
        Ok(_) => {
            warn!(
                project_id = %project_id,
                top_k,
                "semantic vector search returned no rows; falling back to persisted embeddings",
            );
            fallback_semantic_chunks(state, project_id, top_k, query_embedding).await
        }
        Err(error) => {
            warn!(
                project_id = %project_id,
                top_k,
                ?error,
                "semantic vector search failed; falling back to persisted embeddings",
            );
            fallback_semantic_chunks(state, project_id, top_k, query_embedding).await
        }
    }
}

async fn fallback_semantic_chunks(
    state: &AppState,
    project_id: Uuid,
    top_k: i32,
    query_embedding: &[f32],
) -> Result<Vec<RankedChunk>, ApiError> {
    let embedding_rows = repositories::list_chunk_embeddings_by_project(
        &state.persistence.postgres,
        project_id,
        500,
    )
    .await
    .map_err(|error| {
        error!(project_id = %project_id, ?error, "failed to load persisted chunk embeddings");
        ApiError::Internal
    })?;
    let chunk_rows =
        repositories::list_chunks_by_project(&state.persistence.postgres, project_id, 500)
            .await
            .map_err(|error| {
                error!(project_id = %project_id, ?error, "failed to load project chunks for fallback semantic retrieval");
                ApiError::Internal
            })?;
    let persisted_embedding_count = embedding_rows.len();
    let chunk_count = chunk_rows.len();

    let mut scored = Vec::new();
    for embedding_row in embedding_rows {
        let vector: Vec<f32> =
            serde_json::from_value(embedding_row.embedding_json).unwrap_or_default();
        if let Some(score) = cosine_similarity(query_embedding, &vector)
            && let Some(chunk) = chunk_rows.iter().find(|chunk| chunk.id == embedding_row.chunk_id)
        {
            scored.push((score, chunk.clone()));
        }
    }

    scored.sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap_or(std::cmp::Ordering::Equal));

    let limit = usize::try_from(top_k).unwrap_or_default();
    let result = scored
        .into_iter()
        .take(limit)
        .map(|(score, chunk)| (chunk.id, score, chunk))
        .collect::<Vec<_>>();
    if result.is_empty() {
        warn!(
            project_id = %project_id,
            top_k,
            persisted_embedding_count,
            chunk_count,
            "fallback semantic retrieval produced no matches",
        );
    } else {
        info!(
            project_id = %project_id,
            top_k,
            persisted_embedding_count,
            chunk_count,
            semantic_chunk_count = result.len(),
            "fallback semantic retrieval completed",
        );
    }
    Ok(result)
}

fn rank_chunks(
    lexical_chunks: Vec<repositories::ChunkRow>,
    semantic_chunks: Vec<RankedChunk>,
    top_k: i32,
) -> Vec<repositories::ChunkRow> {
    let mut ranked: Vec<RankedChunk> = lexical_chunks
        .into_iter()
        .enumerate()
        .map(|(idx, chunk)| {
            #[allow(clippy::cast_precision_loss, clippy::suboptimal_flops)]
            let lexical_score = (idx as f32).mul_add(-0.01_f32, 1.0_f32);
            (chunk.id, lexical_score, chunk)
        })
        .collect();

    for (chunk_id, semantic_score, chunk) in semantic_chunks {
        if let Some(existing) = ranked.iter_mut().find(|(id, _, _)| *id == chunk_id) {
            existing.1 = existing.1.max(semantic_score + 1.0_f32);
        } else {
            ranked.push((chunk_id, semantic_score + 1.0_f32, chunk));
        }
    }

    ranked.sort_by(|left, right| right.1.partial_cmp(&left.1).unwrap_or(std::cmp::Ordering::Equal));
    let limit = usize::try_from(top_k).unwrap_or_default();
    ranked.into_iter().take(limit).map(|(_, _, chunk)| chunk).collect()
}

fn build_context_block(matched_chunks: &[repositories::ChunkRow]) -> String {
    if matched_chunks.is_empty() {
        return String::new();
    }

    let joined = matched_chunks
        .iter()
        .map(|chunk| format!("[chunk:{}] {}", chunk.ordinal, chunk.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("Context:\n{joined}\n\n")
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
        .and_then(|value| i32::try_from(value).ok());
    (prompt_tokens, completion_tokens, total_tokens)
}

fn build_references(matched_chunks: &[repositories::ChunkRow]) -> Vec<String> {
    matched_chunks
        .iter()
        .map(|chunk| format!("document:{}:chunk:{}", chunk.document_id, chunk.ordinal))
        .collect()
}

fn usage_prices(state: &AppState, provider_kind: &str) -> (f64, f64) {
    match provider_kind {
        "openai" => {
            (state.settings.openai_input_price_per_1m, state.settings.openai_output_price_per_1m)
        }
        "deepseek" => (
            state.settings.deepseek_input_price_per_1m,
            state.settings.deepseek_output_price_per_1m,
        ),
        _ => (0.0, 0.0),
    }
}

fn estimate_query_cost(
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    input_price_per_1m: f64,
    output_price_per_1m: f64,
) -> f64 {
    let prompt_cost = (f64::from(prompt_tokens.unwrap_or(0)) / 1_000_000.0) * input_price_per_1m;
    let completion_cost =
        (f64::from(completion_tokens.unwrap_or(0)) / 1_000_000.0) * output_price_per_1m;
    prompt_cost + completion_cost
}

fn extract_retrieval_debug(
    debug_json: &serde_json::Value,
) -> (String, bool, Vec<String>, Vec<Uuid>, Option<String>) {
    let answer_status = debug_json
        .get("answer_status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("grounded")
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
                .map(ToString::to_string)
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
        debug_json.get("warning").and_then(serde_json::Value::as_str).map(ToString::to_string);

    (answer_status, weak_grounding, references, matched_chunk_ids, warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_validation_rejects_empty_text() {
        let payload = QueryRequest {
            project_id: Uuid::now_v7(),
            query_text: "   ".into(),
            model_profile_id: None,
            embedding_model_profile_id: None,
            top_k: Some(8),
        };

        assert!(matches!(
            validate_query_payload(&payload),
            Err(ApiError::BadRequest(message)) if message.contains("query_text")
        ));
    }

    #[test]
    fn query_validation_rejects_non_positive_top_k() {
        let payload = QueryRequest {
            project_id: Uuid::now_v7(),
            query_text: "hello".into(),
            model_profile_id: None,
            embedding_model_profile_id: None,
            top_k: Some(0),
        };

        assert!(matches!(
            validate_query_payload(&payload),
            Err(ApiError::BadRequest(message)) if message.contains("top_k")
        ));
    }

    #[test]
    fn retrieval_debug_extracts_expected_fields() {
        let chunk_id = Uuid::now_v7();
        let debug_json = serde_json::json!({
            "answer_status": "weakly_grounded",
            "weak_grounding": true,
            "references": ["document:1:chunk:2"],
            "matched_chunk_ids": [chunk_id],
            "warning": "limited evidence"
        });

        let (answer_status, weak_grounding, references, matched_chunk_ids, warning) =
            extract_retrieval_debug(&debug_json);

        assert_eq!(answer_status, "weakly_grounded");
        assert!(weak_grounding);
        assert_eq!(references, vec!["document:1:chunk:2".to_string()]);
        assert_eq!(matched_chunk_ids, vec![chunk_id]);
        assert_eq!(warning.as_deref(), Some("limited evidence"));
    }

    #[test]
    fn retrieval_debug_ignores_invalid_chunk_ids_and_uses_defaults() {
        let valid_chunk_id = Uuid::now_v7();
        let debug_json = serde_json::json!({
            "references": ["document:1:chunk:2", 42],
            "matched_chunk_ids": ["not-a-uuid", valid_chunk_id],
            "weak_grounding": false
        });

        let (answer_status, weak_grounding, references, matched_chunk_ids, warning) =
            extract_retrieval_debug(&debug_json);

        assert_eq!(answer_status, "grounded");
        assert!(!weak_grounding);
        assert_eq!(references, vec!["document:1:chunk:2".to_string()]);
        assert_eq!(matched_chunk_ids, vec![valid_chunk_id]);
        assert!(warning.is_none());
    }

    #[test]
    fn extract_usage_tokens_handles_bounds_and_missing_values() {
        let usage_json = serde_json::json!({
            "prompt_tokens": 123,
            "completion_tokens": i64::from(i32::MAX) + 1,
            "total_tokens": "nope"
        });

        let (prompt_tokens, completion_tokens, total_tokens) = extract_usage_tokens(&usage_json);

        assert_eq!(prompt_tokens, Some(123));
        assert_eq!(completion_tokens, None);
        assert_eq!(total_tokens, None);
    }

    #[test]
    fn build_context_block_formats_ranked_chunks() {
        let now = chrono::Utc::now();
        let chunk_a = repositories::ChunkRow {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            ordinal: 1,
            content: "First fact".into(),
            token_count: Some(10),
            metadata_json: serde_json::json!({}),
            created_at: now,
        };
        let chunk_b = repositories::ChunkRow {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            ordinal: 2,
            content: "Second fact".into(),
            token_count: Some(12),
            metadata_json: serde_json::json!({}),
            created_at: now,
        };

        let context = build_context_block(&[chunk_a, chunk_b]);

        assert!(context.starts_with("Context:\n"));
        assert!(context.contains("[chunk:1] First fact"));
        assert!(context.contains("[chunk:2] Second fact"));
        assert!(context.ends_with("\n\n"));
    }

    #[test]
    fn build_context_block_returns_empty_string_for_no_chunks() {
        assert!(build_context_block(&[]).is_empty());
    }

    #[test]
    fn build_references_uses_document_and_chunk_ordinals() {
        let chunk = repositories::ChunkRow {
            id: Uuid::now_v7(),
            document_id: Uuid::parse_str("aaaaaaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa").unwrap(),
            project_id: Uuid::now_v7(),
            ordinal: 7,
            content: "Fact".into(),
            token_count: None,
            metadata_json: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        };

        let references = build_references(&[chunk]);

        assert_eq!(
            references,
            vec!["document:aaaaaaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa:chunk:7".to_string()]
        );
    }

    #[test]
    fn rank_chunks_prefers_semantic_score_and_deduplicates() {
        let created_at = chrono::Utc::now();
        let shared_id = Uuid::now_v7();
        let lexical = repositories::ChunkRow {
            id: shared_id,
            document_id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            ordinal: 1,
            content: "Lexical".into(),
            token_count: None,
            metadata_json: serde_json::json!({}),
            created_at,
        };
        let semantic_duplicate = repositories::ChunkRow {
            id: shared_id,
            document_id: lexical.document_id,
            project_id: lexical.project_id,
            ordinal: 1,
            content: "Lexical".into(),
            token_count: None,
            metadata_json: serde_json::json!({}),
            created_at,
        };
        let semantic_only = repositories::ChunkRow {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            ordinal: 2,
            content: "Semantic only".into(),
            token_count: None,
            metadata_json: serde_json::json!({}),
            created_at,
        };

        let ranked = rank_chunks(
            vec![lexical],
            vec![
                (shared_id, 0.6, semantic_duplicate),
                (semantic_only.id, 0.8, semantic_only.clone()),
            ],
            5,
        );

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].id, semantic_only.id);
        assert_eq!(ranked[1].id, shared_id);
    }

    #[test]
    fn estimate_query_cost_defaults_missing_usage_to_zero() {
        let cost = estimate_query_cost(None, Some(250_000), 2.0, 8.0);

        assert!((cost - 2.0).abs() < f64::EPSILON);
    }
}
