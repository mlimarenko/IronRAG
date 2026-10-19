mod mappers;
mod turn_stream;
mod ui_projection;

pub(crate) use mappers::*;
use turn_stream::*;
use ui_projection::*;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use chrono::Utc;
use futures::{FutureExt as _, stream};
use ironrag_contracts;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, convert::Infallible, panic::AssertUnwindSafe, time::Duration};
use tokio::sync::mpsc::Sender;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::agent_runtime::{
        RuntimeExecutionSummary, RuntimePolicyDecisionSummary, RuntimePolicySummary,
        RuntimeSurfaceKind,
    },
    domains::query::{
        PreparedSegmentReference, QueryAnswerCandidate, QueryChunkReference, QueryClarification,
        QueryConversation, QueryConversationDetail, QueryExecution, QueryExecutionDetail,
        QueryGraphEdgeReference, QueryGraphNodeReference, QueryRuntimeStageSummary, QueryTurn,
        QueryVerificationState, QueryVerificationWarning, TechnicalFactReference, resolve_top_k,
    },
    infra::repositories::catalog_repository,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_QUERY_READ, POLICY_QUERY_RUN, load_library_and_authorize,
            load_query_execution_and_authorize, load_query_session_and_authorize,
        },
        router_support::ApiError,
    },
    services::{
        iam::audit::{AppendAuditEventCommand, AppendQueryExecutionAuditCommand},
        mcp::access::library_catalog_ref,
        query::{
            agent_loop::AgentLoopActivityEvent,
            service::{
                ASSISTANT_AGENT_LOOP_DEADLINE_MS, CreateConversationCommand,
                ExecuteConversationTurnCommand,
            },
        },
    },
};

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuerySessionListResponse {
    pub items: Vec<ironrag_contracts::assistant::AssistantSessionListItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub total: i64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryTurnListResponse {
    pub items: Vec<ironrag_contracts::assistant::AssistantTurn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub total: i64,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    /// Optional display title; the server derives one from the first turn
    /// when omitted.
    title: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionTurnRequest {
    content_text: String,
    include_debug: Option<bool>,
    top_k: Option<usize>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/query/libraries/{library_id}/sessions", get(list_sessions).post(create_session))
        .route("/query/sessions/{session_id}", get(get_session))
        .route(
            "/query/sessions/{session_id}/turns",
            get(list_session_turns).post(create_session_turn),
        )
        .route("/query/sessions/{session_id}/turns/{turn_id}", get(get_session_turn))
        .route("/query/executions/{execution_id}", get(get_execution))
        .route("/query/executions/{execution_id}/llm-context", get(get_execution_llm_context))
        .route("/query/system-prompt", get(get_assistant_system_prompt))
}

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AssistantSystemPromptQuery {
    pub library_id: Option<Uuid>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantSystemPromptResponse {
    /// Raw template with the `{LIBRARY_REF}` placeholder. This is what
    /// transport-agnostic external MCP clients should paste into their
    /// own system prompt when attaching `IronRAG`'s MCP server. Documented
    /// clients include Claude Desktop, Claude Code, Cursor, Codex, VS Code
    /// with Continue/Cline/Roo, Zed, and Hermes, so every agent — in-app
    /// or external — shares the same grounding discipline.
    template: String,
    /// Template rendered with the `<workspace>/<library>` ref
    /// of the requested `libraryId`, when one was passed. Same text the
    /// public MCP clients should use for that library.
    rendered: Option<String>,
    library_id: Option<Uuid>,
}

/// Publish the MCP assistant system prompt.
///
/// This is the single source of truth for external MCP clients and the
/// admin UI's "MCP client setup" card, which serves the same text
/// verbatim for operators to copy into their own agents.
///
/// Any drift between MCP client setup surfaces would silently change
/// grounding behavior per client, so the text lives in
/// `services::query::assistant_prompt` and every consumer reads from
/// there.
#[tracing::instrument(
    level = "info",
    name = "http.query.get_assistant_system_prompt",
    skip_all,
    fields(library_id = ?query.library_id)
)]
#[utoipa::path(
    get,
    path = "/v1/query/system-prompt",
    tag = "query",
    operation_id = "getAssistantSystemPrompt",
    summary = "Get the recommended MCP assistant system prompt.",
    description = "Returns the exact prompt text used by the built-in UI assistant and recommended for external MCP clients. The template teaches a generic tool-using agent how to choose IronRAG tools, pass conversation history, iterate over results, prefer high-signal grounded-answer probes for content questions, and avoid answering from catalog titles alone. Pass `libraryId` when the caller wants the same template rendered with a concrete `<workspace>/<library>` reference for copy-paste setup. Omit it to fetch only the reusable template with the `{LIBRARY_REF}` placeholder.",
    params(AssistantSystemPromptQuery),
    responses(
        (status = 200, description = "Assistant system prompt template plus the version rendered for the active library", body = AssistantSystemPromptResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the requested library"),
    ),
)]
pub async fn get_assistant_system_prompt(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<AssistantSystemPromptQuery>,
) -> Result<Json<AssistantSystemPromptResponse>, ApiError> {
    let rendered = if let Some(library_id) = query.library_id {
        let library =
            load_library_and_authorize(&auth, &state, library_id, POLICY_QUERY_READ).await?;
        let workspace = catalog_repository::get_workspace_by_id(
            &state.persistence.postgres,
            library.workspace_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("workspace", library.workspace_id))?;
        let library_ref = library_catalog_ref(&workspace.slug, &library.slug);
        Some(crate::services::query::assistant_prompt::render(&library_ref, None))
    } else {
        None
    };
    Ok(Json(AssistantSystemPromptResponse {
        template: crate::services::query::assistant_prompt::template(),
        rendered,
        library_id: query.library_id,
    }))
}

#[tracing::instrument(
    level = "info",
    name = "http.query.list_sessions",
    skip_all,
    fields(library_id = %library_id, item_count)
)]
#[utoipa::path(
    get,
    path = "/v1/query/libraries/{libraryId}/sessions",
    tag = "query",
    operation_id = "listQuerySessions",
    summary = "List assistant sessions for one library.",
    description = "Returns the chat sessions visible to the caller for the requested library, most recently updated first. The web UI uses this endpoint to populate the assistant sidebar and restore recent conversations. The session list is retention-bounded per library, so the full set always fits on one page and `nextCursor` is always `null`.",
    params(("libraryId" = uuid::Uuid, Path, description = "Library that owns the session collection")),
    responses(
        (status = 200, description = "Query session list page", body = QuerySessionListResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
        (status = 404, description = "Library not found"),
    ),
)]
pub async fn list_sessions(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<QuerySessionListResponse>, ApiError> {
    let span = tracing::Span::current();
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_QUERY_READ).await?;
    let conversations =
        state.application_services.query.list_conversations(&state, library_id).await?;
    let mut items = Vec::with_capacity(conversations.len());
    for conversation in conversations {
        let turn_count = state
            .application_services
            .query
            .count_conversation_turns(&state, conversation.id)
            .await?;
        items.push(map_session_list_item_with_turn_count(
            conversation,
            usize::try_from(turn_count).unwrap_or(usize::MAX),
        ));
    }
    span.record("item_count", items.len());
    let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
    Ok(Json(QuerySessionListResponse { items, next_cursor: None, total }))
}

#[utoipa::path(
    post,
    path = "/v1/query/libraries/{libraryId}/sessions",
    tag = "query",
    operation_id = "createQuerySession",
    summary = "Create an assistant session.",
    description = "Creates a persistent assistant conversation scoped to one library. The session stores the user and assistant turns, execution ids, verifier state, citations, runtime traces, and debug snapshots produced by later turns.",
    params(("libraryId" = uuid::Uuid, Path, description = "Library that owns the new session")),
    request_body(content = CreateSessionRequest, description = "Optional display title for the new assistant session."),
    responses(
        (status = 201, description = "Newly created assistant session in the same shape session lists use", body = ironrag_contracts::assistant::AssistantSessionListItem, headers(("Location" = String, description = "Location URI of the created session"))),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
        (status = 404, description = "Library not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.query.create_session",
    skip_all,
    fields(library_id = %library_id)
)]
pub async fn create_session(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<Response, ApiError> {
    let library = load_library_and_authorize(&auth, &state, library_id, POLICY_QUERY_RUN).await?;
    let conversation = state
        .application_services
        .query
        .create_conversation(
            &state,
            CreateConversationCommand {
                workspace_id: library.workspace_id,
                library_id: library.id,
                created_by_principal_id: Some(auth.principal_id),
                title: payload.title,
                request_surface: "ui".to_string(),
            },
        )
        .await?;
    if let Err(error) = state
        .application_services
        .audit
        .append_event(
            &state,
            AppendAuditEventCommand {
                actor_principal_id: Some(auth.principal_id),
                surface_kind: "ui".to_string(),
                action_kind: "query.session.create".to_string(),
                request_id: None,
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some("query session created".to_string()),
                internal_message: Some(format!(
                    "principal {} created query session {} in library {}",
                    auth.principal_id, conversation.id, conversation.library_id
                )),
                subjects: vec![state.application_services.audit.query_session_subject(
                    conversation.id,
                    conversation.workspace_id,
                    conversation.library_id,
                )],
            },
        )
        .await
    {
        tracing::warn!(stage = "audit", error = %error, "audit append failed");
    }
    let location = format!("/v1/query/sessions/{}", conversation.id);
    // One session representation everywhere: return the same camelCase list
    // item shape the session list and detail endpoints use, instead of the
    // raw snake_case domain entity.
    let item = map_session_list_item_with_turn_count(conversation, 0);
    let mut response = (StatusCode::CREATED, Json(item)).into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(&location) {
        response.headers_mut().insert(axum::http::header::LOCATION, value);
    }
    Ok(response)
}

#[tracing::instrument(
    level = "info",
    name = "http.query.list_session_turns",
    skip_all,
    fields(session_id = %session_id, item_count)
)]
#[utoipa::path(
    get,
    path = "/v1/query/sessions/{sessionId}/turns",
    tag = "query",
    operation_id = "listQuerySessionTurns",
    summary = "List turns recorded for one assistant session.",
    description = "Returns the bounded, chronological turn history for one assistant session: recorded content, author, and the execution that produced or consumed each turn (without hydrated evidence). Use `GET /v1/query/sessions/{sessionId}/turns/{turnId}` for one turn, or `GET /v1/query/sessions/{sessionId}` for the fully hydrated conversation with evidence.",
    params(("sessionId" = uuid::Uuid, Path, description = "Query session identifier")),
    responses(
        (status = 200, description = "Query turn list page", body = QueryTurnListResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the session"),
        (status = 404, description = "Session not found"),
    ),
)]
pub async fn list_session_turns(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<QueryTurnListResponse>, ApiError> {
    let span = tracing::Span::current();
    let _ = load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_READ).await?;
    let turns = state.application_services.query.list_turns(&state, session_id).await?;
    let items: Vec<_> = turns.into_iter().map(map_turn).collect();
    span.record("item_count", items.len());
    let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
    Ok(Json(QueryTurnListResponse { items, next_cursor: None, total }))
}

#[tracing::instrument(
    level = "info",
    name = "http.query.get_session_turn",
    skip_all,
    fields(session_id = %session_id, turn_id = %turn_id)
)]
#[utoipa::path(
    get,
    path = "/v1/query/sessions/{sessionId}/turns/{turnId}",
    tag = "query",
    operation_id = "getQuerySessionTurn",
    summary = "Load one assistant session turn.",
    description = "Returns a single immutable turn from an assistant session: recorded content, author, timestamp, and the execution that produced or consumed it. Turns are append-only historical records — there is no item-level update or delete, only `POST .../turns` to record a new one and `DELETE /v1/query/sessions/{sessionId}` to erase the whole session, preserving the same audit trail that protects execution traces.",
    params(
        ("sessionId" = uuid::Uuid, Path, description = "Query session identifier"),
        ("turnId" = uuid::Uuid, Path, description = "Query turn identifier"),
    ),
    responses(
        (status = 200, description = "Assistant session turn", body = ironrag_contracts::assistant::AssistantTurn),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the session"),
        (status = 404, description = "Session or turn not found"),
    ),
)]
pub async fn get_session_turn(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((session_id, turn_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ironrag_contracts::assistant::AssistantTurn>, ApiError> {
    let _ = load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_READ).await?;
    let turn = state.application_services.query.get_turn(&state, session_id, turn_id).await?;
    Ok(Json(map_turn(turn)))
}

#[utoipa::path(
    get,
    path = "/v1/query/sessions/{sessionId}",
    tag = "query",
    operation_id = "getQuerySession",
    summary = "Load one assistant session with turns.",
    description = "Returns the hydrated conversation used by the UI chat pane: session metadata, user turns, assistant turns, execution identifiers, citations, and verification state. Use this after selecting a session from the list or after a page reload to reconstruct the visible conversation.",
    params(("sessionId" = uuid::Uuid, Path, description = "Query session identifier")),
    responses(
        (status = 200, description = "Hydrated assistant conversation with turns", body = ironrag_contracts::assistant::AssistantHydratedConversation),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the session"),
        (status = 404, description = "Session not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.query.get_session",
    skip_all,
    fields(session_id = %session_id)
)]
pub async fn get_session(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<ironrag_contracts::assistant::AssistantHydratedConversation>, ApiError> {
    let _ = load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_READ).await?;
    let detail = state.application_services.query.get_conversation(&state, session_id).await?;
    Ok(Json(map_session_detail(&state, detail).await?))
}

#[utoipa::path(
    post,
    path = "/v1/query/sessions/{sessionId}/turns",
    tag = "query",
    operation_id = "createQuerySessionTurn",
    summary = "Run one UI assistant turn.",
    description = "Executes one user message through the same MCP-style tool loop used to simulate an external agent in the web UI. The model receives the available answer-surface tool schemas, chooses one or more tool calls, may run independent calls in parallel, reads the tool results, and then writes the final answer. For normal JSON clients the endpoint returns the completed `AssistantExecutionDetail`. When the request `Accept` header includes `text/event-stream`, the same endpoint streams `assistant_turn` SSE events: model requests, model responses, tool-call start/finish activity, periodic working heartbeats, and finally a terminal `completed` or `failed` event. A `failed` event carries `code`, the same stable slug this API's problem responses use, alongside the human-readable `message`.",
    params(("sessionId" = uuid::Uuid, Path, description = "Query session identifier")),
    request_body(content = CreateSessionTurnRequest, description = "User message plus optional retrieval/debug controls for a new assistant turn."),
    responses(
        (status = 200, description = "Turn execution result with grounded answer + evidence references", body = ironrag_contracts::assistant::AssistantExecutionDetail),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the session"),
        (status = 404, description = "Session not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.create_session_turn",
    skip_all,
    fields(session_id = %session_id, elapsed_ms)
)]
pub async fn create_session_turn(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    Json(payload): Json<CreateSessionTurnRequest>,
) -> Result<Response, ApiError> {
    let started_at = std::time::Instant::now();
    let span = tracing::Span::current();
    if accepts_event_stream(&headers) {
        let stream = create_session_turn_event_stream(auth, state, session_id, payload).await?;
        return Ok(stream.into_response());
    }
    let outcome =
        execute_ui_session_turn(state.clone(), auth.clone(), session_id, payload, None).await?;
    append_query_execution_audit(state.clone(), auth.principal_id, "ui", &outcome).await;
    span.record("elapsed_ms", started_at.elapsed().as_millis() as u64);
    Ok(Json(map_ui_turn_execution_response(outcome)).into_response())
}

pub(crate) fn resolve_query_turn_top_k(requested_top_k: Option<usize>) -> usize {
    resolve_top_k(requested_top_k)
}

#[utoipa::path(
    get,
    path = "/v1/query/executions/{executionId}",
    tag = "query",
    operation_id = "getQueryExecution",
    summary = "Inspect one assistant execution.",
    description = "Loads the persisted execution detail for a completed or failed assistant turn. This is the main trace endpoint for the debug inspector and external operators: it includes request/response turns, citations, selected chunks, prepared segments, graph references, verifier verdict, runtime stage summary, policy decisions, and child tool executions when the turn used the agent loop.",
    params(("executionId" = uuid::Uuid, Path, description = "Query execution identifier")),
    responses(
        (status = 200, description = "Assistant execution detail with retrieval/answer/verification stages", body = ironrag_contracts::assistant::AssistantExecutionDetail),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the execution"),
        (status = 404, description = "Execution not found"),
    ),
)]
#[tracing::instrument(
    level = "info",
    name = "http.get_execution",
    skip_all,
    fields(execution_id = %execution_id)
)]
pub async fn get_execution(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(execution_id): Path<Uuid>,
) -> Result<Json<ironrag_contracts::assistant::AssistantExecutionDetail>, ApiError> {
    let _ =
        load_query_execution_and_authorize(&auth, &state, execution_id, POLICY_QUERY_READ).await?;
    let detail = state.application_services.query.get_execution(&state, execution_id).await?;
    Ok(Json(map_execution_detail(detail)))
}

/// Returns the raw LLM request/response chain that was sent to the
/// provider for this assistant execution.
#[tracing::instrument(
    level = "info",
    name = "http.query.get_execution_llm_context",
    skip_all,
    fields(execution_id = %execution_id)
)]
#[utoipa::path(
    get,
    path = "/v1/query/executions/{executionId}/llm-context",
    tag = "query",
    operation_id = "getQueryExecutionLlmContext",
    summary = "Inspect captured LLM context for one execution.",
    description = "Returns the durable model transcript captured for an assistant execution: system messages, prior conversation messages, tool definitions, tool-call arguments, tool results, final model responses, token usage, and stop reasons. The UI debug inspector uses this endpoint to show the full prompt/tool context that produced an answer. The endpoint is intended for debugging and audit, not for user-facing answer rendering.",
    params(("executionId" = uuid::Uuid, Path, description = "Query execution identifier")),
    responses(
        (status = 200, description = "Durable LLM request/response capture for the execution", body = crate::services::query::llm_context_debug::LlmContextSnapshot),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the execution"),
        (status = 404, description = "Execution not found or no LLM context snapshot was recorded"),
    ),
)]
pub async fn get_execution_llm_context(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(execution_id): Path<Uuid>,
) -> Result<Json<crate::services::query::llm_context_debug::LlmContextSnapshot>, ApiError> {
    let _ =
        load_query_execution_and_authorize(&auth, &state, execution_id, POLICY_QUERY_READ).await?;
    state.application_services.query.get_execution(&state, execution_id).await?;
    crate::services::query::llm_context_debug::load_snapshot(
        &state.persistence.postgres,
        execution_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .map(Json)
    .ok_or_else(|| ApiError::resource_not_found("llm_context_snapshot", execution_id))
}

#[cfg(test)]
mod tests;
