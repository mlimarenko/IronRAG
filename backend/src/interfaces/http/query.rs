use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        query::{
            QueryChunkReference, QueryConversation, QueryConversationDetail, QueryExecution,
            QueryExecutionDetail, QueryGraphEdgeReference, QueryGraphNodeReference, QueryTurn,
        },
        query_modes::RuntimeQueryMode,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_QUERY_READ, POLICY_QUERY_RUN, load_library_and_authorize},
        router_support::ApiError,
    },
    services::query_service::{CreateConversationCommand, ExecuteConversationTurnCommand},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListConversationsQuery {
    library_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateConversationRequest {
    workspace_id: Uuid,
    library_id: Uuid,
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTurnRequest {
    content_text: String,
    mode: RuntimeQueryMode,
    top_k: Option<usize>,
    include_debug: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryConversationDetailResponse {
    conversation: QueryConversation,
    turns: Vec<QueryTurn>,
    executions: Vec<QueryExecution>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryExecutionDetailResponse {
    execution: QueryExecution,
    request_turn: Option<QueryTurn>,
    response_turn: Option<QueryTurn>,
    chunk_references: Vec<QueryChunkReference>,
    graph_node_references: Vec<QueryGraphNodeReference>,
    graph_edge_references: Vec<QueryGraphEdgeReference>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryTurnExecutionResponse {
    conversation: QueryConversation,
    request_turn: QueryTurn,
    response_turn: Option<QueryTurn>,
    execution: QueryExecution,
    chunk_references: Vec<QueryChunkReference>,
    graph_node_references: Vec<QueryGraphNodeReference>,
    graph_edge_references: Vec<QueryGraphEdgeReference>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/query/conversations", get(list_conversations).post(create_conversation))
        .route("/query/conversations/{conversation_id}", get(get_conversation))
        .route("/query/conversations/{conversation_id}/turns", axum::routing::post(create_turn))
        .route("/query/executions/{execution_id}", get(get_execution))
}

async fn list_conversations(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListConversationsQuery>,
) -> Result<Json<Vec<QueryConversation>>, ApiError> {
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_QUERY_READ).await?;
    let conversations =
        state.canonical_services.query.list_conversations(&state, library_id).await?;
    Ok(Json(conversations))
}

async fn create_conversation(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateConversationRequest>,
) -> Result<Json<QueryConversation>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, payload.library_id, POLICY_QUERY_RUN).await?;
    if library.workspace_id != payload.workspace_id {
        return Err(ApiError::BadRequest(
            "workspaceId does not match the target library".to_string(),
        ));
    }
    let conversation = state
        .canonical_services
        .query
        .create_conversation(
            &state,
            CreateConversationCommand {
                workspace_id: payload.workspace_id,
                library_id: payload.library_id,
                created_by_principal_id: Some(auth.principal_id),
                title: payload.title,
            },
        )
        .await?;
    Ok(Json(conversation))
}

async fn get_conversation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(conversation_id): Path<Uuid>,
) -> Result<Json<QueryConversationDetailResponse>, ApiError> {
    let detail = state.canonical_services.query.get_conversation(&state, conversation_id).await?;
    let _ = load_library_and_authorize(
        &auth,
        &state,
        detail.conversation.library_id,
        POLICY_QUERY_READ,
    )
    .await?;
    Ok(Json(map_conversation_detail(detail)))
}

async fn create_turn(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(conversation_id): Path<Uuid>,
    Json(payload): Json<CreateTurnRequest>,
) -> Result<Json<QueryTurnExecutionResponse>, ApiError> {
    let conversation =
        state.canonical_services.query.get_conversation(&state, conversation_id).await?;
    let _ = load_library_and_authorize(
        &auth,
        &state,
        conversation.conversation.library_id,
        POLICY_QUERY_RUN,
    )
    .await?;
    let outcome = state
        .canonical_services
        .query
        .execute_turn(
            &state,
            ExecuteConversationTurnCommand {
                conversation_id,
                author_principal_id: Some(auth.principal_id),
                content_text: payload.content_text,
                mode: payload.mode,
                top_k: payload.top_k.unwrap_or(8),
                include_debug: payload.include_debug.unwrap_or(false),
            },
        )
        .await?;
    Ok(Json(QueryTurnExecutionResponse {
        conversation: outcome.conversation,
        request_turn: outcome.request_turn,
        response_turn: outcome.response_turn,
        execution: outcome.execution,
        chunk_references: outcome.chunk_references,
        graph_node_references: outcome.graph_node_references,
        graph_edge_references: outcome.graph_edge_references,
    }))
}

async fn get_execution(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(execution_id): Path<Uuid>,
) -> Result<Json<QueryExecutionDetailResponse>, ApiError> {
    let detail = state.canonical_services.query.get_execution(&state, execution_id).await?;
    let _ =
        load_library_and_authorize(&auth, &state, detail.execution.library_id, POLICY_QUERY_READ)
            .await?;
    Ok(Json(map_execution_detail(detail)))
}

fn map_conversation_detail(detail: QueryConversationDetail) -> QueryConversationDetailResponse {
    QueryConversationDetailResponse {
        conversation: detail.conversation,
        turns: detail.turns,
        executions: detail.executions,
    }
}

fn map_execution_detail(detail: QueryExecutionDetail) -> QueryExecutionDetailResponse {
    QueryExecutionDetailResponse {
        execution: detail.execution,
        request_turn: detail.request_turn,
        response_turn: detail.response_turn,
        chunk_references: detail.chunk_references,
        graph_node_references: detail.graph_node_references,
        graph_edge_references: detail.graph_edge_references,
    }
}
