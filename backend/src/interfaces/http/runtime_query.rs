use axum::{
    Json, Router,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        query_intelligence::{ContextAssemblyMetadata, QueryPlanningMetadata, RerankMetadata},
        query_modes::RuntimeQueryMode,
        runtime_query::{GroundingStatus, RuntimeQueryReference},
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_QUERY_READ, POLICY_QUERY_RUN},
        router_support::ApiError,
        runtime_support::load_library_and_authorize,
    },
    services::query_runtime::{
        RuntimeMatchedChunk, RuntimeMatchedEntity, RuntimeMatchedRelationship, RuntimeQueryRequest,
        execute_answer_query, execute_structured_query, load_persisted_query,
        parse_runtime_query_enrichment, parse_runtime_query_warning, persist_answer_query_result,
        persist_structured_query_result,
    },
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeQueryPayload {
    question: String,
    mode: RuntimeQueryMode,
    top_k: Option<usize>,
    include_debug: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderDescriptorResponse {
    provider_kind: String,
    model_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryReferenceResponse {
    kind: String,
    reference_id: Uuid,
    excerpt: Option<String>,
    rank: usize,
    score: Option<f32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MatchedEntityResponse {
    node_id: Uuid,
    label: String,
    node_type: String,
    score: Option<f32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MatchedRelationshipResponse {
    edge_id: Uuid,
    relation_type: String,
    from_node_id: Uuid,
    to_node_id: Uuid,
    score: Option<f32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MatchedChunkResponse {
    chunk_id: Uuid,
    document_id: Uuid,
    excerpt: String,
    score: Option<f32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnswerQueryResponse {
    query_id: Uuid,
    mode: RuntimeQueryMode,
    answer: String,
    grounding_status: GroundingStatus,
    references: Vec<QueryReferenceResponse>,
    provider: ProviderDescriptorResponse,
    planning: QueryPlanningMetadata,
    rerank: RerankMetadata,
    context_assembly: ContextAssemblyMetadata,
    warning: Option<String>,
    warning_kind: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StructuredQueryResponse {
    query_id: Uuid,
    mode: RuntimeQueryMode,
    entities: Vec<MatchedEntityResponse>,
    relationships: Vec<MatchedRelationshipResponse>,
    chunks: Vec<MatchedChunkResponse>,
    references: Vec<QueryReferenceResponse>,
    provider: ProviderDescriptorResponse,
    planning: QueryPlanningMetadata,
    rerank: RerankMetadata,
    context_assembly: ContextAssemblyMetadata,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryExecutionDetailResponse {
    query_id: Uuid,
    mode: RuntimeQueryMode,
    question: String,
    answer: Option<String>,
    grounding_status: GroundingStatus,
    references: Vec<QueryReferenceResponse>,
    provider: ProviderDescriptorResponse,
    planning: QueryPlanningMetadata,
    rerank: RerankMetadata,
    context_assembly: ContextAssemblyMetadata,
    warning: Option<String>,
    warning_kind: Option<String>,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route(
            "/runtime/libraries/{library_id}/queries/answer",
            axum::routing::post(run_answer_query),
        )
        .route(
            "/runtime/libraries/{library_id}/queries/data",
            axum::routing::post(run_structured_query),
        )
        .route(
            "/runtime/libraries/{library_id}/queries/{query_id}",
            axum::routing::get(get_query_execution),
        )
}

async fn run_answer_query(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<RuntimeQueryPayload>,
) -> Result<Json<AnswerQueryResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_QUERY_RUN).await?;
    let request = normalize_runtime_query_request(library_id, payload)?;
    let result = execute_answer_query(&state, &request).await.map_err(map_runtime_query_error)?;
    let persisted = persist_answer_query_result(&state, &request, &result)
        .await
        .map_err(map_runtime_query_error)?;

    Ok(Json(AnswerQueryResponse {
        query_id: persisted.execution.id,
        mode: result.structured.planned_mode,
        answer: result.answer,
        grounding_status: result.structured.grounding_status,
        references: map_references(&result.structured.references),
        provider: map_provider(&result.provider),
        planning: result.structured.enrichment.planning.clone(),
        rerank: result.structured.enrichment.rerank.clone(),
        context_assembly: result.structured.enrichment.context_assembly.clone(),
        warning: result.warning,
        warning_kind: result.warning_kind,
    }))
}

async fn run_structured_query(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<RuntimeQueryPayload>,
) -> Result<Json<StructuredQueryResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_QUERY_RUN).await?;
    let request = normalize_runtime_query_request(library_id, payload)?;
    let result =
        execute_structured_query(&state, &request).await.map_err(map_runtime_query_error)?;
    let persisted = persist_structured_query_result(&state, &request, &result)
        .await
        .map_err(map_runtime_query_error)?;

    Ok(Json(StructuredQueryResponse {
        query_id: persisted.execution.id,
        mode: result.planned_mode,
        entities: map_entities(&result.entities),
        relationships: map_relationships(&result.relationships),
        chunks: map_chunks(&result.chunks),
        references: map_references(&result.references),
        provider: map_provider(&result.provider),
        planning: result.enrichment.planning.clone(),
        rerank: result.enrichment.rerank.clone(),
        context_assembly: result.enrichment.context_assembly.clone(),
    }))
}

async fn get_query_execution(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, query_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<QueryExecutionDetailResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_QUERY_READ).await?;
    let Some(persisted) = load_persisted_query(&state, library_id, query_id)
        .await
        .map_err(map_runtime_query_error)?
    else {
        return Err(ApiError::NotFound(format!("runtime query {query_id} not found")));
    };

    let mode =
        persisted.execution.mode.parse::<RuntimeQueryMode>().map_err(|_| ApiError::Internal)?;
    let grounding_status = persisted
        .execution
        .grounding_status
        .parse::<GroundingStatus>()
        .map_err(|_| ApiError::Internal)?;
    let enrichment = parse_runtime_query_enrichment(&persisted.execution.debug_json, mode);
    let (warning, warning_kind) = parse_runtime_query_warning(&persisted.execution.debug_json);

    Ok(Json(QueryExecutionDetailResponse {
        query_id: persisted.execution.id,
        mode,
        question: persisted.execution.question,
        answer: persisted.execution.answer_text,
        grounding_status,
        references: persisted
            .references
            .into_iter()
            .map(|reference| QueryReferenceResponse {
                kind: reference.reference_kind,
                reference_id: reference.reference_id,
                excerpt: reference.excerpt,
                rank: usize::try_from(reference.rank).unwrap_or_default(),
                score: reference
                    .score
                    .and_then(|value| if value.is_finite() { Some(value as f32) } else { None }),
            })
            .collect(),
        provider: ProviderDescriptorResponse {
            provider_kind: persisted.execution.provider_kind,
            model_name: persisted.execution.model_name,
        },
        planning: enrichment.planning,
        rerank: enrichment.rerank,
        context_assembly: enrichment.context_assembly,
        warning,
        warning_kind,
    }))
}

fn normalize_runtime_query_request(
    library_id: Uuid,
    payload: RuntimeQueryPayload,
) -> Result<RuntimeQueryRequest, ApiError> {
    let question = payload.question.trim();
    if question.is_empty() {
        return Err(ApiError::BadRequest("question is required".into()));
    }
    Ok(RuntimeQueryRequest {
        library_id,
        question: question.to_string(),
        system_prompt: None,
        mode: payload.mode,
        top_k: payload.top_k.unwrap_or(8).clamp(1, 12),
        include_debug: payload.include_debug.unwrap_or(false),
    })
}

fn map_provider(
    provider: &crate::domains::provider_profiles::ProviderModelSelection,
) -> ProviderDescriptorResponse {
    ProviderDescriptorResponse {
        provider_kind: provider.provider_kind.as_str().to_string(),
        model_name: provider.model_name.clone(),
    }
}

fn map_references(references: &[RuntimeQueryReference]) -> Vec<QueryReferenceResponse> {
    references
        .iter()
        .map(|reference| QueryReferenceResponse {
            kind: reference.kind.clone(),
            reference_id: reference.reference_id,
            excerpt: reference.excerpt.clone(),
            rank: reference.rank,
            score: reference.score,
        })
        .collect()
}

fn map_entities(entities: &[RuntimeMatchedEntity]) -> Vec<MatchedEntityResponse> {
    entities
        .iter()
        .map(|entity| MatchedEntityResponse {
            node_id: entity.node_id,
            label: entity.label.clone(),
            node_type: entity.node_type.clone(),
            score: entity.score,
        })
        .collect()
}

fn map_relationships(
    relationships: &[RuntimeMatchedRelationship],
) -> Vec<MatchedRelationshipResponse> {
    relationships
        .iter()
        .map(|relationship| MatchedRelationshipResponse {
            edge_id: relationship.edge_id,
            relation_type: relationship.relation_type.clone(),
            from_node_id: relationship.from_node_id,
            to_node_id: relationship.to_node_id,
            score: relationship.score,
        })
        .collect()
}

fn map_chunks(chunks: &[RuntimeMatchedChunk]) -> Vec<MatchedChunkResponse> {
    chunks
        .iter()
        .map(|chunk| MatchedChunkResponse {
            chunk_id: chunk.chunk_id,
            document_id: chunk.document_id,
            excerpt: chunk.excerpt.clone(),
            score: chunk.score,
        })
        .collect()
}

fn map_runtime_query_error(error: anyhow::Error) -> ApiError {
    let message = error.to_string();
    if message.contains("missing OpenAI API key")
        || message.contains("missing DeepSeek API key")
        || message.contains("missing Qwen API key")
        || message.contains("failed to generate grounded answer")
        || message.contains("failed to embed runtime query")
    {
        ApiError::Conflict(message)
    } else {
        ApiError::Internal
    }
}
