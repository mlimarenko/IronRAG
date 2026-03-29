use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query::{
        QueryChunkReference, QueryConversation, QueryConversationDetail, QueryExecution,
        QueryExecutionDetail, QueryGraphEdgeReference, QueryGraphNodeReference, QueryTurn,
        RuntimeQueryMode,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_QUERY_READ, POLICY_QUERY_RUN, load_library_and_authorize,
            load_query_execution_and_authorize, load_query_session_and_authorize,
        },
        router_support::ApiError,
    },
    services::{
        audit_service::AppendAuditEventCommand,
        query_service::{CreateConversationCommand, ExecuteConversationTurnCommand},
    },
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListSessionsQuery {
    library_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    workspace_id: Uuid,
    library_id: Uuid,
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionTurnRequest {
    content_text: String,
    mode: RuntimeQueryMode,
    top_k: Option<usize>,
    include_debug: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuerySessionDetailResponse {
    session: QueryConversation,
    turns: Vec<QueryTurn>,
    executions: Vec<QueryExecution>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryExecutionDetailResponse {
    context_bundle_id: Uuid,
    execution: QueryExecution,
    request_turn: Option<QueryTurn>,
    response_turn: Option<QueryTurn>,
    chunk_references: Vec<QueryChunkReference>,
    entity_references: Vec<QueryGraphNodeReference>,
    relation_references: Vec<QueryGraphEdgeReference>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuerySessionTurnExecutionResponse {
    context_bundle_id: Uuid,
    session: QueryConversation,
    request_turn: QueryTurn,
    response_turn: Option<QueryTurn>,
    execution: QueryExecution,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/query/sessions", get(list_sessions).post(create_session))
        .route("/query/sessions/{session_id}", get(get_session))
        .route("/query/sessions/{session_id}/turns", axum::routing::post(create_session_turn))
        .route("/query/executions/{execution_id}", get(get_execution))
}

async fn list_sessions(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListSessionsQuery>,
) -> Result<Json<Vec<QueryConversation>>, ApiError> {
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_QUERY_READ).await?;
    let conversations =
        state.canonical_services.query.list_conversations(&state, library_id).await?;
    Ok(Json(conversations))
}

async fn create_session(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateSessionRequest>,
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
    let _ = state
        .canonical_services
        .audit
        .append_event(
            &state,
            AppendAuditEventCommand {
                actor_principal_id: Some(auth.principal_id),
                surface_kind: "rest".to_string(),
                action_kind: "query.session.create".to_string(),
                request_id: None,
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some("query session created".to_string()),
                internal_message: Some(format!(
                    "principal {} created query session {} in library {}",
                    auth.principal_id, conversation.id, conversation.library_id
                )),
                subjects: vec![state.canonical_services.audit.query_session_subject(
                    conversation.id,
                    conversation.workspace_id,
                    conversation.library_id,
                )],
            },
        )
        .await;
    Ok(Json(conversation))
}

async fn get_session(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<QuerySessionDetailResponse>, ApiError> {
    let _ = load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_READ).await?;
    let detail = state.canonical_services.query.get_conversation(&state, session_id).await?;
    Ok(Json(map_session_detail(detail)))
}

async fn create_session_turn(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(payload): Json<CreateSessionTurnRequest>,
) -> Result<Json<QuerySessionTurnExecutionResponse>, ApiError> {
    let _ = load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_RUN).await?;
    let outcome = state
        .canonical_services
        .query
        .execute_turn(
            &state,
            ExecuteConversationTurnCommand {
                conversation_id: session_id,
                author_principal_id: Some(auth.principal_id),
                content_text: payload.content_text,
                mode: payload.mode,
                top_k: payload.top_k.unwrap_or(8),
                include_debug: payload.include_debug.unwrap_or(false),
            },
        )
        .await?;
    let async_operation = state
        .canonical_services
        .ops
        .get_latest_async_operation_by_subject(&state, "query_execution", outcome.execution.id)
        .await?;
    let mut subjects = vec![
        state.canonical_services.audit.query_session_subject(
            outcome.conversation.id,
            outcome.conversation.workspace_id,
            outcome.conversation.library_id,
        ),
        state.canonical_services.audit.query_execution_subject(
            outcome.execution.id,
            outcome.execution.workspace_id,
            outcome.execution.library_id,
        ),
        state.canonical_services.audit.knowledge_bundle_subject(
            outcome.context_bundle_id,
            outcome.execution.workspace_id,
            outcome.execution.library_id,
        ),
    ];
    if let Some(operation) = async_operation {
        subjects.push(state.canonical_services.audit.async_operation_subject(
            operation.id,
            outcome.execution.workspace_id,
            outcome.execution.library_id,
        ));
    }
    let _ = state
        .canonical_services
        .audit
        .append_event(
            &state,
            AppendAuditEventCommand {
                actor_principal_id: Some(auth.principal_id),
                surface_kind: "rest".to_string(),
                action_kind: "query.execution.run".to_string(),
                request_id: None,
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some("query execution completed".to_string()),
                internal_message: Some(format!(
                    "principal {} executed query session {}, execution {}, bundle {}",
                    auth.principal_id,
                    outcome.conversation.id,
                    outcome.execution.id,
                    outcome.context_bundle_id
                )),
                subjects,
            },
        )
        .await;
    Ok(Json(QuerySessionTurnExecutionResponse {
        context_bundle_id: outcome.context_bundle_id,
        session: outcome.conversation,
        request_turn: outcome.request_turn,
        response_turn: outcome.response_turn,
        execution: outcome.execution,
    }))
}

async fn get_execution(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(execution_id): Path<Uuid>,
) -> Result<Json<QueryExecutionDetailResponse>, ApiError> {
    let _ =
        load_query_execution_and_authorize(&auth, &state, execution_id, POLICY_QUERY_READ).await?;
    let detail = state.canonical_services.query.get_execution(&state, execution_id).await?;
    Ok(Json(map_execution_detail(detail)))
}

fn map_session_detail(detail: QueryConversationDetail) -> QuerySessionDetailResponse {
    QuerySessionDetailResponse {
        session: detail.conversation,
        turns: detail.turns,
        executions: detail.executions,
    }
}

fn map_execution_detail(detail: QueryExecutionDetail) -> QueryExecutionDetailResponse {
    QueryExecutionDetailResponse {
        context_bundle_id: detail.execution.context_bundle_id,
        execution: detail.execution,
        request_turn: detail.request_turn,
        response_turn: detail.response_turn,
        chunk_references: detail.chunk_references,
        entity_references: detail.graph_node_references,
        relation_references: detail.graph_edge_references,
    }
}
