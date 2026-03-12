use axum::{
    Json, Router,
    extract::{Query, State},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{repositories, vector_search},
    integrations::llm::{ChatRequest, EmbeddingRequest},
    interfaces::http::{auth::AuthContext, router_support::ApiError},
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
}

#[derive(Serialize)]
pub struct RetrievalRunSummary {
    pub id: Uuid,
    pub project_id: Uuid,
    pub query_text: String,
    pub model_profile_id: Option<Uuid>,
    pub top_k: i32,
    pub response_text: Option<String>,
}

#[derive(Serialize)]
pub struct QueryResponse {
    pub retrieval_run_id: Uuid,
    pub project_id: Uuid,
    pub answer: String,
    pub references: Vec<String>,
    pub mode: String,
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
        .route("/query", axum::routing::post(run_query))
}

async fn list_retrieval_runs(
    State(state): State<AppState>,
    Query(query): Query<ProjectScopedQuery>,
) -> Result<Json<Vec<RetrievalRunSummary>>, ApiError> {
    let items = repositories::list_retrieval_runs(&state.persistence.postgres, query.project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .into_iter()
        .map(|row| RetrievalRunSummary {
            id: row.id,
            project_id: row.project_id,
            query_text: row.query_text,
            model_profile_id: row.model_profile_id,
            top_k: row.top_k,
            response_text: row.response_text,
        })
        .collect();

    Ok(Json(items))
}

async fn create_retrieval_run(
    _auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateRetrievalRunRequest>,
) -> Result<Json<RetrievalRunSummary>, ApiError> {
    if payload.query_text.trim().is_empty() {
        return Err(ApiError::BadRequest("query_text must not be empty".into()));
    }

    let row = repositories::create_retrieval_run(
        &state.persistence.postgres,
        payload.project_id,
        &payload.query_text,
        payload.model_profile_id,
        payload.top_k.unwrap_or(8),
        payload.response_text.as_deref(),
        serde_json::json!({"mode":"manual"}),
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(RetrievalRunSummary {
        id: row.id,
        project_id: row.project_id,
        query_text: row.query_text,
        model_profile_id: row.model_profile_id,
        top_k: row.top_k,
        response_text: row.response_text,
    }))
}

async fn run_query(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    auth.require_any_scope(&["query:run", "workspace:admin"])?;
    validate_query_payload(&payload)?;

    let query_result = execute_query(&state, &payload).await?;
    persist_query_artifacts(&state, &payload, &query_result).await
}

fn validate_query_payload(payload: &QueryRequest) -> Result<(), ApiError> {
    if payload.query_text.trim().is_empty() {
        return Err(ApiError::BadRequest("query_text must not be empty".into()));
    }
    Ok(())
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
    .map_err(|_| ApiError::Internal)?;
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
    let matched_chunks = rank_chunks(lexical_chunks, semantic_chunks, top_k);
    let context_block = build_context_block(&matched_chunks);

    let gateway_response = state
        .llm_gateway
        .generate(ChatRequest {
            provider_kind: provider_kind.clone(),
            model_name: model_name.clone(),
            prompt: format!("{context_block}User question: {}", payload.query_text),
        })
        .await
        .map_err(|_| ApiError::Internal)?;
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
    })
}

async fn persist_query_artifacts(
    state: &AppState,
    payload: &QueryRequest,
    query_result: &QueryExecutionResult,
) -> Result<Json<QueryResponse>, ApiError> {
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
        }),
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    let workspace_id =
        repositories::get_project_by_id(&state.persistence.postgres, payload.project_id)
            .await
            .map_err(|_| ApiError::Internal)?
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
    .map_err(|_| ApiError::Internal)?;

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
    .map_err(|_| ApiError::Internal)?;

    Ok(Json(QueryResponse {
        retrieval_run_id: row.id,
        project_id: row.project_id,
        answer: query_result.output_text.clone(),
        references: query_result.references.clone(),
        mode: "gateway_live".into(),
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
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::NotFound("model_profile not found".into()))?;
            let provider = repositories::get_provider_account_by_id(
                &state.persistence.postgres,
                profile.provider_account_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::NotFound("provider_account not found".into()))?;

            Ok((provider.provider_kind, profile.model_name, Some(profile.provider_account_id)))
        }
        None => Ok(("openai".to_string(), "unbound-foundation-model".to_string(), None)),
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
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::NotFound("embedding model_profile not found".into()))?;
            let provider = repositories::get_provider_account_by_id(
                &state.persistence.postgres,
                profile.provider_account_id,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::NotFound("embedding provider_account not found".into()))?;

            Ok((provider.provider_kind, profile.model_name, Some(profile.provider_account_id)))
        }
        None => Ok((default_provider_kind.to_string(), default_model_name.to_string(), None)),
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
            search_semantic_chunks(state, project_id, top_k, &query_embedding.embedding).await
        }
        Err(_) => Ok(Vec::new()),
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
        Ok(scored_rows) if !scored_rows.is_empty() => Ok(scored_rows
            .into_iter()
            .map(|row| {
                let score = row.cosine_similarity_score();
                let chunk = row.into_chunk();
                (chunk.id, score, chunk)
            })
            .collect()),
        _ => fallback_semantic_chunks(state, project_id, top_k, query_embedding).await,
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
    .map_err(|_| ApiError::Internal)?;
    let chunk_rows =
        repositories::list_chunks_by_project(&state.persistence.postgres, project_id, 500)
            .await
            .map_err(|_| ApiError::Internal)?;

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
    Ok(scored.into_iter().take(limit).map(|(score, chunk)| (chunk.id, score, chunk)).collect())
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
