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

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AssistantTurnStreamEvent {
    Activity { event: AssistantActivityEvent },
    Completed { detail: Box<ironrag_contracts::assistant::AssistantExecutionDetail> },
    Failed { message: String },
}

#[derive(Debug, Serialize)]
struct AssistantActivityEvent {
    #[serde(rename = "type")]
    event_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    deadline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    iteration: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_final_answer: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    child_execution_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_preview: Option<String>,
}

const ASSISTANT_TURN_ACTIVITY_INTERVAL: Duration = Duration::from_secs(5);
const ASSISTANT_TURN_STREAM_BUFFER: usize = 512;
const ASSISTANT_TURN_TERMINAL_EVENT_RESERVE: usize = 8;
const ASSISTANT_ACTIVITY_DRAIN_GRACE: Duration = Duration::from_millis(250);
const ASSISTANT_PANIC_FAILURE_SEND_GRACE: Duration = Duration::from_secs(1);
const UI_VISIBLE_REFERENCE_LIMIT: usize = 12;

struct UiReferenceProjection {
    chunk_references: Vec<ironrag_contracts::assistant::AssistantChunkReference>,
    prepared_segment_references:
        Vec<ironrag_contracts::assistant::AssistantPreparedSegmentReference>,
    technical_fact_references: Vec<ironrag_contracts::assistant::AssistantTechnicalFactReference>,
    entity_references: Vec<ironrag_contracts::assistant::AssistantEntityReference>,
    relation_references: Vec<ironrag_contracts::assistant::AssistantRelationReference>,
    summary: ironrag_contracts::assistant::AssistantReferenceSummary,
}

enum UiRankedReference {
    Chunk(ironrag_contracts::assistant::AssistantChunkReference),
    PreparedSegment(ironrag_contracts::assistant::AssistantPreparedSegmentReference),
    TechnicalFact(ironrag_contracts::assistant::AssistantTechnicalFactReference),
    Entity(ironrag_contracts::assistant::AssistantEntityReference),
    Relation(ironrag_contracts::assistant::AssistantRelationReference),
}

impl UiRankedReference {
    const fn rank(&self) -> i32 {
        match self {
            Self::Chunk(reference) => reference.rank,
            Self::PreparedSegment(reference) => reference.rank,
            Self::TechnicalFact(reference) => reference.rank,
            Self::Entity(reference) => reference.rank,
            Self::Relation(reference) => reference.rank,
        }
    }

    const fn kind_order(&self) -> u8 {
        match self {
            Self::Chunk(_) => 0,
            Self::PreparedSegment(_) => 1,
            Self::TechnicalFact(_) => 2,
            Self::Entity(_) => 3,
            Self::Relation(_) => 4,
        }
    }
}

fn project_ui_reference_arrays(
    chunk_references: Vec<ironrag_contracts::assistant::AssistantChunkReference>,
    prepared_segment_references: Vec<
        ironrag_contracts::assistant::AssistantPreparedSegmentReference,
    >,
    technical_fact_references: Vec<ironrag_contracts::assistant::AssistantTechnicalFactReference>,
    entity_references: Vec<ironrag_contracts::assistant::AssistantEntityReference>,
    relation_references: Vec<ironrag_contracts::assistant::AssistantRelationReference>,
    total_count_hint: Option<usize>,
) -> UiReferenceProjection {
    let visible_count = chunk_references
        .len()
        .saturating_add(prepared_segment_references.len())
        .saturating_add(technical_fact_references.len())
        .saturating_add(entity_references.len())
        .saturating_add(relation_references.len());
    let total_count = total_count_hint.unwrap_or(visible_count).max(visible_count);
    let mut ranked = Vec::with_capacity(visible_count);

    ranked.extend(
        chunk_references.into_iter().enumerate().map(|(original_order, reference)| {
            (original_order, UiRankedReference::Chunk(reference))
        }),
    );
    ranked.extend(prepared_segment_references.into_iter().enumerate().map(
        |(original_order, reference)| {
            (original_order, UiRankedReference::PreparedSegment(reference))
        },
    ));
    ranked.extend(technical_fact_references.into_iter().enumerate().map(
        |(original_order, reference)| (original_order, UiRankedReference::TechnicalFact(reference)),
    ));
    ranked.extend(
        entity_references.into_iter().enumerate().map(|(original_order, reference)| {
            (original_order, UiRankedReference::Entity(reference))
        }),
    );
    ranked.extend(relation_references.into_iter().enumerate().map(
        |(original_order, reference)| (original_order, UiRankedReference::Relation(reference)),
    ));
    ranked.sort_by_key(|(original_order, reference)| {
        (reference.rank(), reference.kind_order(), *original_order)
    });

    let returned_count = ranked.len().min(UI_VISIBLE_REFERENCE_LIMIT);
    let mut projected = UiReferenceProjection {
        chunk_references: Vec::new(),
        prepared_segment_references: Vec::new(),
        technical_fact_references: Vec::new(),
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        summary: ironrag_contracts::assistant::AssistantReferenceSummary {
            total_count,
            returned_count,
            truncated: total_count > returned_count,
        },
    };
    for (_, reference) in ranked.into_iter().take(UI_VISIBLE_REFERENCE_LIMIT) {
        match reference {
            UiRankedReference::Chunk(reference) => projected.chunk_references.push(reference),
            UiRankedReference::PreparedSegment(reference) => {
                projected.prepared_segment_references.push(reference);
            }
            UiRankedReference::TechnicalFact(reference) => {
                projected.technical_fact_references.push(reference);
            }
            UiRankedReference::Entity(reference) => projected.entity_references.push(reference),
            UiRankedReference::Relation(reference) => projected.relation_references.push(reference),
        }
    }
    projected
}

fn project_ui_evidence_bundle(
    evidence: ironrag_contracts::assistant::AssistantEvidenceBundle,
) -> ironrag_contracts::assistant::AssistantEvidenceBundle {
    let ironrag_contracts::assistant::AssistantEvidenceBundle {
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        entity_references,
        relation_references,
        reference_summary,
        verification_state,
        verification_warnings,
        answer_disposition,
        clarification,
        runtime_summary,
        runtime_stage_summaries,
    } = evidence;
    let projection = project_ui_reference_arrays(
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        entity_references,
        relation_references,
        reference_summary.map(|summary| summary.total_count),
    );
    ironrag_contracts::assistant::AssistantEvidenceBundle {
        chunk_references: projection.chunk_references,
        prepared_segment_references: projection.prepared_segment_references,
        technical_fact_references: projection.technical_fact_references,
        entity_references: projection.entity_references,
        relation_references: projection.relation_references,
        reference_summary: Some(projection.summary),
        verification_state,
        verification_warnings,
        answer_disposition,
        clarification,
        runtime_summary,
        runtime_stage_summaries,
    }
}

fn project_ui_execution_detail(
    detail: ironrag_contracts::assistant::AssistantExecutionDetail,
) -> ironrag_contracts::assistant::AssistantExecutionDetail {
    let ironrag_contracts::assistant::AssistantExecutionDetail {
        context_bundle_id,
        execution,
        runtime_summary,
        runtime_stage_summaries,
        request_turn,
        response_turn,
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        entity_references,
        relation_references,
        reference_summary,
        verification_state,
        verification_warnings,
        answer_disposition,
        clarification,
    } = detail;
    let projection = project_ui_reference_arrays(
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        entity_references,
        relation_references,
        reference_summary.map(|summary| summary.total_count),
    );
    ironrag_contracts::assistant::AssistantExecutionDetail {
        context_bundle_id,
        execution,
        runtime_summary,
        runtime_stage_summaries,
        request_turn,
        response_turn,
        chunk_references: projection.chunk_references,
        prepared_segment_references: projection.prepared_segment_references,
        technical_fact_references: projection.technical_fact_references,
        entity_references: projection.entity_references,
        relation_references: projection.relation_references,
        reference_summary: Some(projection.summary),
        verification_state,
        verification_warnings,
        answer_disposition,
        clarification,
    }
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
        state.canonical_services.query.list_conversations(&state, library_id).await?;
    let mut items = Vec::with_capacity(conversations.len());
    for conversation in conversations {
        let turn_count = state
            .canonical_services
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
        (status = 201, description = "Newly created query conversation", body = QueryConversation, headers(("Location" = String, description = "Canonical URI of the created session"))),
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
        .canonical_services
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
        .canonical_services
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
                subjects: vec![state.canonical_services.audit.query_session_subject(
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
    let mut response = (StatusCode::CREATED, Json(conversation)).into_response();
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
    let turns = state.canonical_services.query.list_turns(&state, session_id).await?;
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
    let turn = state.canonical_services.query.get_turn(&state, session_id, turn_id).await?;
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
    let detail = state.canonical_services.query.get_conversation(&state, session_id).await?;
    Ok(Json(map_session_detail(&state, detail).await?))
}

#[utoipa::path(
    post,
    path = "/v1/query/sessions/{sessionId}/turns",
    tag = "query",
    operation_id = "createQuerySessionTurn",
    summary = "Run one UI assistant turn.",
    description = "Executes one user message through the same MCP-style tool loop used to simulate an external agent in the web UI. The model receives the available answer-surface tool schemas, chooses one or more tool calls, may run independent calls in parallel, reads the tool results, and then writes the final answer. For normal JSON clients the endpoint returns the completed `AssistantExecutionDetail`. When the request `Accept` header includes `text/event-stream`, the same endpoint streams `assistant_turn` SSE events: model requests, model responses, tool-call start/finish activity, periodic working heartbeats, and finally a terminal `completed` or `failed` event.",
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

#[tracing::instrument(
    level = "info",
    name = "http.create_session_turn_event_stream",
    skip_all,
    fields(session_id = %session_id)
)]
async fn create_session_turn_event_stream(
    auth: AuthContext,
    state: AppState,
    session_id: Uuid,
    payload: CreateSessionTurnRequest,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let _ = load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_RUN).await?;
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<AssistantTurnStreamEvent>(ASSISTANT_TURN_STREAM_BUFFER);
    let state_for_task = state.clone();
    let auth_for_task = auth.clone();

    tokio::spawn(async move {
        let panic_sender = sender.clone();
        let producer = async move {
            send_assistant_activity(
                &sender,
                AssistantActivityEvent {
                    event_type: "started",
                    deadline_ms: Some(ASSISTANT_AGENT_LOOP_DEADLINE_MS),
                    iteration: None,
                    provider_kind: None,
                    model_name: None,
                    tool_call_count: None,
                    has_final_answer: None,
                    tool_name: None,
                    elapsed_ms: None,
                    is_error: None,
                    child_execution_id: None,
                    result_preview: None,
                },
            );

            let (agent_activity_tx, mut agent_activity_rx) =
                tokio::sync::mpsc::channel::<AgentLoopActivityEvent>(256);
            let agent_activity_sender = sender.clone();
            let mut agent_activity_task = tokio::spawn(async move {
                while let Some(event) = agent_activity_rx.recv().await {
                    send_assistant_activity(&agent_activity_sender, map_agent_loop_activity(event));
                }
            });

            let progress_started_at = Instant::now();
            let progress_sender = sender.clone();
            let progress_task = tokio::spawn(async move {
                loop {
                    sleep(ASSISTANT_TURN_ACTIVITY_INTERVAL).await;
                    send_assistant_activity(
                        &progress_sender,
                        AssistantActivityEvent {
                            event_type: "working",
                            deadline_ms: None,
                            iteration: None,
                            provider_kind: None,
                            model_name: None,
                            tool_call_count: None,
                            has_final_answer: None,
                            tool_name: None,
                            elapsed_ms: Some(progress_started_at.elapsed().as_millis() as u64),
                            is_error: None,
                            child_execution_id: None,
                            result_preview: None,
                        },
                    );
                }
            });

            let result = execute_ui_session_turn(
                state_for_task.clone(),
                auth_for_task.clone(),
                session_id,
                payload,
                Some(agent_activity_tx),
            )
            .await;
            progress_task.abort();
            if tokio::time::timeout(ASSISTANT_ACTIVITY_DRAIN_GRACE, &mut agent_activity_task)
                .await
                .is_err()
            {
                agent_activity_task.abort();
            }

            match result {
                Ok(outcome) => {
                    send_assistant_activity(
                        &sender,
                        AssistantActivityEvent {
                            event_type: "model_response",
                            deadline_ms: None,
                            iteration: None,
                            provider_kind: None,
                            model_name: None,
                            tool_call_count: None,
                            has_final_answer: Some(true),
                            tool_name: None,
                            elapsed_ms: Some(progress_started_at.elapsed().as_millis() as u64),
                            is_error: Some(false),
                            child_execution_id: Some(outcome.execution.id),
                            result_preview: Some(format!(
                                "verification={}",
                                verification_state_stream_label(&outcome.verification_state)
                            )),
                        },
                    );
                    append_query_execution_audit(
                        state_for_task.clone(),
                        auth_for_task.principal_id,
                        "ui",
                        &outcome,
                    )
                    .await;
                    send_assistant_activity(
                        &sender,
                        AssistantActivityEvent {
                            event_type: "persisting",
                            deadline_ms: None,
                            iteration: None,
                            provider_kind: None,
                            model_name: None,
                            tool_call_count: None,
                            has_final_answer: None,
                            tool_name: None,
                            elapsed_ms: None,
                            is_error: None,
                            child_execution_id: None,
                            result_preview: None,
                        },
                    );
                    send_required_turn_stream_event(
                        &sender,
                        AssistantTurnStreamEvent::Completed {
                            detail: Box::new(map_ui_turn_execution_response(outcome)),
                        },
                    )
                    .await;
                }
                Err(error) => {
                    send_assistant_activity(
                        &sender,
                        AssistantActivityEvent {
                            event_type: "model_response",
                            deadline_ms: None,
                            iteration: None,
                            provider_kind: None,
                            model_name: None,
                            tool_call_count: None,
                            has_final_answer: Some(false),
                            tool_name: None,
                            elapsed_ms: Some(progress_started_at.elapsed().as_millis() as u64),
                            is_error: Some(true),
                            child_execution_id: None,
                            result_preview: Some(error.to_string()),
                        },
                    );
                    send_required_turn_stream_event(
                        &sender,
                        AssistantTurnStreamEvent::Failed { message: error.to_string() },
                    )
                    .await;
                }
            }
        };
        if let Err(panic) = Box::pin(AssertUnwindSafe(producer).catch_unwind()).await {
            tracing::error!(
                panic = %panic_payload_message(panic.as_ref()),
                "assistant turn stream producer panicked"
            );
            let _ = tokio::time::timeout(
                ASSISTANT_PANIC_FAILURE_SEND_GRACE,
                send_required_turn_stream_event(
                    &panic_sender,
                    AssistantTurnStreamEvent::Failed {
                        message: "assistant turn stream failed unexpectedly".to_string(),
                    },
                ),
            )
            .await;
        }
    });

    let stream = stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|payload| {
            let event = Event::default()
                .event("assistant_turn")
                .json_data(payload)
                .unwrap_or_else(|error| {
                    Event::default()
                        .event("assistant_turn")
                        .data(format!(
                            r#"{{"type":"failed","message":"failed to serialize stream event: {error}"}}"#
                        ))
                });
            (Ok(event), receiver)
        })
    });

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(10)).text("keep-alive")))
}

async fn execute_ui_session_turn(
    state: AppState,
    auth: AuthContext,
    session_id: Uuid,
    payload: CreateSessionTurnRequest,
    agent_activity_tx: Option<tokio::sync::mpsc::Sender<AgentLoopActivityEvent>>,
) -> Result<crate::services::query::service::QueryTurnExecutionResult, ApiError> {
    let _ = load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_RUN).await?;
    let query_service = state.canonical_services.query.clone();
    query_service
        .execute_assistant_agent_turn(
            &state,
            &auth,
            ExecuteConversationTurnCommand {
                conversation_id: session_id,
                author_principal_id: Some(auth.principal_id),
                surface_kind: RuntimeSurfaceKind::Ui,
                content_text: payload.content_text,
                external_prior_turns: Vec::new(),
                top_k: resolve_query_turn_top_k(payload.top_k),
                include_debug: payload.include_debug.unwrap_or(false),
            },
            agent_activity_tx,
        )
        .await
}

async fn send_required_turn_stream_event(
    sender: &Sender<AssistantTurnStreamEvent>,
    event: AssistantTurnStreamEvent,
) {
    let _ = sender.send(event).await;
}

fn send_assistant_activity(
    sender: &Sender<AssistantTurnStreamEvent>,
    event: AssistantActivityEvent,
) {
    if sender.capacity() <= ASSISTANT_TURN_TERMINAL_EVENT_RESERVE {
        return;
    }
    let _ = sender.try_send(AssistantTurnStreamEvent::Activity { event });
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_string()
}

fn map_agent_loop_activity(event: AgentLoopActivityEvent) -> AssistantActivityEvent {
    match event {
        AgentLoopActivityEvent::ModelRequest { iteration, provider_kind, model_name } => {
            AssistantActivityEvent {
                event_type: "model_request",
                deadline_ms: None,
                iteration: Some(iteration),
                provider_kind: Some(provider_kind),
                model_name: Some(model_name),
                tool_call_count: None,
                has_final_answer: None,
                tool_name: None,
                elapsed_ms: None,
                is_error: None,
                child_execution_id: None,
                result_preview: None,
            }
        }
        AgentLoopActivityEvent::ModelResponse {
            iteration,
            provider_kind,
            model_name,
            tool_call_count,
            has_final_answer,
        } => AssistantActivityEvent {
            event_type: "model_response",
            deadline_ms: None,
            iteration: Some(iteration),
            provider_kind: Some(provider_kind),
            model_name: Some(model_name),
            tool_call_count: Some(tool_call_count),
            has_final_answer: Some(has_final_answer),
            tool_name: None,
            elapsed_ms: None,
            is_error: None,
            child_execution_id: None,
            result_preview: None,
        },
        AgentLoopActivityEvent::ToolCallStarted { iteration, tool_name } => {
            AssistantActivityEvent {
                event_type: "tool_call_started",
                deadline_ms: None,
                iteration: Some(iteration),
                provider_kind: None,
                model_name: None,
                tool_call_count: None,
                has_final_answer: None,
                tool_name: Some(tool_name),
                elapsed_ms: None,
                is_error: None,
                child_execution_id: None,
                result_preview: None,
            }
        }
        AgentLoopActivityEvent::ToolCallFinished {
            iteration,
            tool_name,
            elapsed_ms,
            is_error,
            child_execution_id,
            result_preview,
        } => AssistantActivityEvent {
            event_type: "tool_call_finished",
            deadline_ms: None,
            iteration: Some(iteration),
            provider_kind: None,
            model_name: None,
            tool_call_count: None,
            has_final_answer: None,
            tool_name: Some(tool_name),
            elapsed_ms: Some(elapsed_ms),
            is_error: Some(is_error),
            child_execution_id,
            result_preview,
        },
    }
}

const fn verification_state_stream_label(state: &QueryVerificationState) -> &'static str {
    match state {
        QueryVerificationState::NotRun => "not_run",
        QueryVerificationState::Verified => "verified",
        QueryVerificationState::PartiallySupported => "partially_supported",
        QueryVerificationState::Conflicting => "conflicting",
        QueryVerificationState::InsufficientEvidence => "insufficient_evidence",
        QueryVerificationState::Failed => "failed",
    }
}

fn accepts_event_stream(headers: &HeaderMap) -> bool {
    headers.get(header::ACCEPT).and_then(|value| value.to_str().ok()).is_some_and(|value| {
        value.split(',').any(|segment| segment.trim().eq_ignore_ascii_case("text/event-stream"))
    })
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
    let detail = state.canonical_services.query.get_execution(&state, execution_id).await?;
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
    crate::services::query::llm_context_debug::load_snapshot(
        &state.persistence.postgres,
        execution_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .map(Json)
    .ok_or_else(|| ApiError::resource_not_found("llm_context_snapshot", execution_id))
}

fn map_session_list_item_with_turn_count(
    session: QueryConversation,
    turn_count: usize,
) -> ironrag_contracts::assistant::AssistantSessionListItem {
    ironrag_contracts::assistant::AssistantSessionListItem {
        id: session.id,
        workspace_id: session.workspace_id,
        library_id: session.library_id,
        title: session.title.unwrap_or_default(),
        updated_at: session.updated_at,
        created_at: session.created_at,
        conversation_state: session.conversation_state.as_str().to_string(),
        turn_count: i32::try_from(turn_count).unwrap_or(i32::MAX),
    }
}

async fn map_session_detail(
    state: &AppState,
    detail: QueryConversationDetail,
) -> Result<ironrag_contracts::assistant::AssistantHydratedConversation, ApiError> {
    let QueryConversationDetail { conversation, turns, executions } = detail;
    let workspace_id = conversation.workspace_id;
    let library_id = conversation.library_id;
    let turn_count = turns.len();
    let mut evidence_by_turn_id =
        hydrate_session_message_evidence(state, &turns, workspace_id, library_id).await?;
    let pending_execution_by_request_turn_id: HashMap<Uuid, QueryExecution> = executions
        .into_iter()
        .filter(is_pending_session_execution)
        .filter_map(|execution| {
            execution.request_turn_id.map(|request_turn_id| (request_turn_id, execution))
        })
        .collect();
    let mut messages = Vec::with_capacity(turn_count + pending_execution_by_request_turn_id.len());
    for turn in turns {
        let turn_id = turn.id;
        let evidence = evidence_by_turn_id.remove(&turn_id);
        messages.push(map_turn_to_message(turn, evidence));
        if let Some(execution) = pending_execution_by_request_turn_id.get(&turn_id) {
            messages.push(map_pending_execution_to_message(execution));
        }
    }
    Ok(ironrag_contracts::assistant::AssistantHydratedConversation {
        session: map_session_list_item_with_turn_count(conversation, turn_count),
        messages,
    })
}

const fn is_pending_session_execution(execution: &QueryExecution) -> bool {
    execution.request_turn_id.is_some()
        && execution.response_turn_id.is_none()
        && !execution.lifecycle_state.is_terminal()
}

async fn hydrate_session_message_evidence(
    state: &AppState,
    turns: &[QueryTurn],
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<HashMap<Uuid, ironrag_contracts::assistant::AssistantEvidenceBundle>, ApiError> {
    let mut evidence_by_turn_id = HashMap::new();
    for turn in turns {
        if !matches!(turn.turn_kind, crate::domains::query::QueryTurnKind::Assistant) {
            continue;
        }
        let Some(execution_id) = turn.execution_id else {
            continue;
        };
        let detail = state.canonical_services.query.get_execution(state, execution_id).await?;
        if detail.execution.workspace_id != workspace_id
            || detail.execution.library_id != library_id
        {
            return Err(ApiError::internal_with_log(
                format!(
                    "query turn {} points to execution {} outside workspace/library scope",
                    turn.id, execution_id
                ),
                "query session evidence ownership mismatch",
            ));
        }
        evidence_by_turn_id.insert(turn.id, map_execution_detail_to_evidence(detail));
    }
    Ok(evidence_by_turn_id)
}

fn map_execution_detail(
    detail: QueryExecutionDetail,
) -> ironrag_contracts::assistant::AssistantExecutionDetail {
    let QueryExecutionDetail {
        execution,
        runtime_summary,
        runtime_stage_summaries,
        request_turn,
        response_turn,
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        graph_node_references,
        graph_edge_references,
        verification_state,
        verification_warnings,
        answer_disposition,
        clarification,
        query_ir: _,
    } = detail;
    let context_bundle_id = execution.context_bundle_id;
    let execution = map_execution(execution);
    let request_turn = request_turn.map(map_turn);
    let response_turn = response_turn.map(map_turn);
    let evidence = map_execution_evidence_parts(
        runtime_summary,
        runtime_stage_summaries,
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        graph_node_references,
        graph_edge_references,
        verification_state,
        verification_warnings,
        answer_disposition,
        clarification,
    );
    ironrag_contracts::assistant::AssistantExecutionDetail {
        context_bundle_id,
        execution,
        runtime_summary: evidence.runtime_summary,
        runtime_stage_summaries: evidence.runtime_stage_summaries,
        request_turn,
        response_turn,
        chunk_references: evidence.chunk_references,
        prepared_segment_references: evidence.prepared_segment_references,
        technical_fact_references: evidence.technical_fact_references,
        entity_references: evidence.entity_references,
        relation_references: evidence.relation_references,
        reference_summary: evidence.reference_summary,
        verification_state: evidence.verification_state,
        verification_warnings: evidence.verification_warnings,
        answer_disposition: evidence.answer_disposition,
        clarification: evidence.clarification,
    }
}

fn map_execution_detail_to_evidence(
    detail: QueryExecutionDetail,
) -> ironrag_contracts::assistant::AssistantEvidenceBundle {
    project_ui_evidence_bundle(map_execution_evidence_parts(
        detail.runtime_summary,
        detail.runtime_stage_summaries,
        detail.chunk_references,
        detail.prepared_segment_references,
        detail.technical_fact_references,
        detail.graph_node_references,
        detail.graph_edge_references,
        detail.verification_state,
        detail.verification_warnings,
        detail.answer_disposition,
        detail.clarification,
    ))
}

fn map_execution_evidence_parts(
    runtime_summary: RuntimeExecutionSummary,
    runtime_stage_summaries: Vec<QueryRuntimeStageSummary>,
    chunk_references: Vec<QueryChunkReference>,
    prepared_segment_references: Vec<PreparedSegmentReference>,
    technical_fact_references: Vec<TechnicalFactReference>,
    graph_node_references: Vec<QueryGraphNodeReference>,
    graph_edge_references: Vec<QueryGraphEdgeReference>,
    verification_state: QueryVerificationState,
    verification_warnings: Vec<QueryVerificationWarning>,
    answer_disposition: crate::domains::query::QueryAnswerDisposition,
    clarification: QueryClarification,
) -> ironrag_contracts::assistant::AssistantEvidenceBundle {
    ironrag_contracts::assistant::AssistantEvidenceBundle {
        chunk_references: chunk_references.into_iter().map(map_chunk_reference).collect(),
        prepared_segment_references: prepared_segment_references
            .into_iter()
            .map(map_prepared_segment_reference)
            .collect(),
        technical_fact_references: technical_fact_references
            .into_iter()
            .map(map_technical_fact_reference)
            .collect(),
        entity_references: graph_node_references
            .into_iter()
            .map(map_graph_node_reference)
            .collect(),
        relation_references: graph_edge_references
            .into_iter()
            .map(map_graph_edge_reference)
            .collect(),
        reference_summary: None,
        verification_state: map_verification_state(verification_state),
        verification_warnings: verification_warnings
            .into_iter()
            .map(map_verification_warning)
            .collect(),
        answer_disposition: map_answer_disposition(answer_disposition),
        clarification: map_query_clarification(clarification),
        runtime_summary: map_runtime_summary(runtime_summary),
        runtime_stage_summaries: runtime_stage_summaries
            .into_iter()
            .map(map_runtime_stage_summary)
            .collect(),
    }
}

pub(crate) fn map_turn_execution_response(
    outcome: crate::services::query::service::QueryTurnExecutionResult,
) -> ironrag_contracts::assistant::AssistantExecutionDetail {
    ironrag_contracts::assistant::AssistantExecutionDetail {
        context_bundle_id: outcome.context_bundle_id,
        execution: map_execution(outcome.execution),
        runtime_summary: map_runtime_summary(outcome.runtime_summary),
        runtime_stage_summaries: outcome
            .runtime_stage_summaries
            .into_iter()
            .map(map_runtime_stage_summary)
            .collect(),
        request_turn: Some(map_turn(outcome.request_turn)),
        response_turn: outcome.response_turn.map(map_turn),
        chunk_references: outcome.chunk_references.into_iter().map(map_chunk_reference).collect(),
        prepared_segment_references: outcome
            .prepared_segment_references
            .into_iter()
            .map(map_prepared_segment_reference)
            .collect(),
        technical_fact_references: outcome
            .technical_fact_references
            .into_iter()
            .map(map_technical_fact_reference)
            .collect(),
        entity_references: outcome
            .graph_node_references
            .into_iter()
            .map(map_graph_node_reference)
            .collect(),
        relation_references: outcome
            .graph_edge_references
            .into_iter()
            .map(map_graph_edge_reference)
            .collect(),
        reference_summary: None,
        verification_state: map_verification_state(outcome.verification_state),
        verification_warnings: outcome
            .verification_warnings
            .into_iter()
            .map(map_verification_warning)
            .collect(),
        answer_disposition: map_answer_disposition(outcome.answer_disposition),
        clarification: map_query_clarification(outcome.clarification),
    }
}

fn map_ui_turn_execution_response(
    outcome: crate::services::query::service::QueryTurnExecutionResult,
) -> ironrag_contracts::assistant::AssistantExecutionDetail {
    project_ui_execution_detail(map_turn_execution_response(outcome))
}

fn map_query_clarification(
    clarification: QueryClarification,
) -> ironrag_contracts::assistant::AssistantClarification {
    ironrag_contracts::assistant::AssistantClarification {
        required: clarification.required,
        question: clarification.question,
        answer_candidates: clarification
            .answer_candidates
            .into_iter()
            .map(map_query_answer_candidate)
            .collect(),
    }
}

fn map_query_answer_candidate(
    candidate: QueryAnswerCandidate,
) -> ironrag_contracts::assistant::AssistantAnswerCandidate {
    ironrag_contracts::assistant::AssistantAnswerCandidate {
        label: candidate.label,
        kind: candidate.kind,
        confidence: candidate.confidence,
        provenance: ironrag_contracts::assistant::AssistantAnswerCandidateProvenance {
            entity_id: candidate.provenance.entity_id,
            document_id: candidate.provenance.document_id,
            chunk_id: candidate.provenance.chunk_id,
        },
    }
}

fn map_turn_to_message(
    turn: QueryTurn,
    evidence: Option<ironrag_contracts::assistant::AssistantEvidenceBundle>,
) -> ironrag_contracts::assistant::AssistantConversationMessage {
    ironrag_contracts::assistant::AssistantConversationMessage {
        id: turn.id,
        role: map_turn_role(turn.turn_kind),
        content: turn.content_text,
        timestamp: turn.created_at,
        execution_id: turn.execution_id,
        evidence,
    }
}

const fn map_pending_execution_to_message(
    execution: &QueryExecution,
) -> ironrag_contracts::assistant::AssistantConversationMessage {
    ironrag_contracts::assistant::AssistantConversationMessage {
        id: execution.id,
        role: ironrag_contracts::assistant::AssistantTurnRole::Assistant,
        content: String::new(),
        timestamp: execution.started_at,
        execution_id: Some(execution.id),
        evidence: None,
    }
}

fn map_turn(turn: QueryTurn) -> ironrag_contracts::assistant::AssistantTurn {
    ironrag_contracts::assistant::AssistantTurn {
        id: turn.id,
        conversation_id: turn.conversation_id,
        turn_index: turn.turn_index,
        turn_kind: map_turn_role(turn.turn_kind),
        author_principal_id: turn.author_principal_id,
        content_text: turn.content_text,
        execution_id: turn.execution_id,
        created_at: turn.created_at,
    }
}

const fn map_turn_role(
    turn_kind: crate::domains::query::QueryTurnKind,
) -> ironrag_contracts::assistant::AssistantTurnRole {
    match turn_kind {
        crate::domains::query::QueryTurnKind::User => {
            ironrag_contracts::assistant::AssistantTurnRole::User
        }
        crate::domains::query::QueryTurnKind::Assistant => {
            ironrag_contracts::assistant::AssistantTurnRole::Assistant
        }
        crate::domains::query::QueryTurnKind::System => {
            ironrag_contracts::assistant::AssistantTurnRole::System
        }
        crate::domains::query::QueryTurnKind::Tool => {
            ironrag_contracts::assistant::AssistantTurnRole::Tool
        }
    }
}

fn map_execution(execution: QueryExecution) -> ironrag_contracts::assistant::AssistantExecution {
    ironrag_contracts::assistant::AssistantExecution {
        id: execution.id,
        workspace_id: execution.workspace_id,
        library_id: execution.library_id,
        conversation_id: execution.conversation_id,
        context_bundle_id: execution.context_bundle_id,
        request_turn_id: execution.request_turn_id,
        response_turn_id: execution.response_turn_id,
        binding_id: execution.binding_id,
        runtime_execution_id: execution.runtime_execution_id,
        lifecycle_state: execution.lifecycle_state.as_str().to_string(),
        active_stage: execution.active_stage.map(|stage| stage.as_str().to_string()),
        query_text: execution.query_text,
        failure_code: execution.failure_code,
        started_at: execution.started_at,
        completed_at: execution.completed_at,
    }
}

fn map_runtime_summary(
    runtime_summary: RuntimeExecutionSummary,
) -> ironrag_contracts::assistant::AssistantRuntimeSummary {
    let runtime_accepted_at = runtime_summary.accepted_at;
    ironrag_contracts::assistant::AssistantRuntimeSummary {
        runtime_execution_id: runtime_summary.runtime_execution_id,
        lifecycle_state: runtime_summary.lifecycle_state.as_str().to_string(),
        active_stage: runtime_summary.active_stage.map(|stage| stage.as_str().to_string()),
        turn_budget: runtime_summary.turn_budget,
        turn_count: runtime_summary.turn_count,
        parallel_action_limit: runtime_summary.parallel_action_limit,
        failure_code: runtime_summary.failure_code,
        failure_summary_redacted: runtime_summary.failure_summary_redacted,
        policy_summary: map_policy_summary(runtime_summary.policy_summary, runtime_accepted_at),
        accepted_at: runtime_summary.accepted_at,
        completed_at: runtime_summary.completed_at,
    }
}

fn map_runtime_stage_summary(
    summary: QueryRuntimeStageSummary,
) -> ironrag_contracts::assistant::AssistantRuntimeStageSummary {
    ironrag_contracts::assistant::AssistantRuntimeStageSummary {
        stage_kind: summary.stage_kind.as_str().to_string(),
        stage_label: summary.stage_label,
        duration_ms: summary.duration_ms,
    }
}

fn map_policy_summary(
    policy_summary: RuntimePolicySummary,
    decision_timestamp: chrono::DateTime<Utc>,
) -> ironrag_contracts::assistant::AssistantPolicySummary {
    ironrag_contracts::assistant::AssistantPolicySummary {
        allow_count: policy_summary.allow_count.try_into().unwrap_or(i32::MAX),
        reject_count: policy_summary.reject_count.try_into().unwrap_or(i32::MAX),
        terminate_count: policy_summary.terminate_count.try_into().unwrap_or(i32::MAX),
        recent_decisions: policy_summary
            .recent_decisions
            .into_iter()
            .map(|decision| map_policy_decision_summary(decision, decision_timestamp))
            .collect(),
    }
}

fn map_policy_decision_summary(
    policy_decision: RuntimePolicyDecisionSummary,
    decision_timestamp: chrono::DateTime<Utc>,
) -> ironrag_contracts::assistant::AssistantPolicyDecisionSummary {
    ironrag_contracts::assistant::AssistantPolicyDecisionSummary {
        target_kind: policy_decision.target_kind.as_str().to_string(),
        decision_kind: policy_decision.decision_kind.as_str().to_string(),
        reason_code: policy_decision.reason_code,
        target_id: policy_decision.reason_summary_redacted,
        decided_at: decision_timestamp,
    }
}

const fn map_verification_state(
    state: QueryVerificationState,
) -> ironrag_contracts::assistant::AssistantVerificationState {
    match state {
        QueryVerificationState::NotRun => {
            ironrag_contracts::assistant::AssistantVerificationState::NotRun
        }
        QueryVerificationState::Verified => {
            ironrag_contracts::assistant::AssistantVerificationState::Verified
        }
        QueryVerificationState::PartiallySupported => {
            ironrag_contracts::assistant::AssistantVerificationState::PartiallySupported
        }
        QueryVerificationState::Conflicting => {
            ironrag_contracts::assistant::AssistantVerificationState::Conflicting
        }
        QueryVerificationState::InsufficientEvidence => {
            ironrag_contracts::assistant::AssistantVerificationState::InsufficientEvidence
        }
        QueryVerificationState::Failed => {
            ironrag_contracts::assistant::AssistantVerificationState::Failed
        }
    }
}

const fn map_answer_disposition(
    disposition: crate::domains::query::QueryAnswerDisposition,
) -> ironrag_contracts::assistant::AssistantAnswerDisposition {
    match disposition {
        crate::domains::query::QueryAnswerDisposition::NonTerminal => {
            ironrag_contracts::assistant::AssistantAnswerDisposition::NonTerminal
        }
        crate::domains::query::QueryAnswerDisposition::FactualReady => {
            ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady
        }
        crate::domains::query::QueryAnswerDisposition::SafeFallback => {
            ironrag_contracts::assistant::AssistantAnswerDisposition::SafeFallback
        }
        crate::domains::query::QueryAnswerDisposition::Clarification => {
            ironrag_contracts::assistant::AssistantAnswerDisposition::Clarification
        }
    }
}

fn map_verification_warning(
    warning: QueryVerificationWarning,
) -> ironrag_contracts::assistant::AssistantVerificationWarning {
    ironrag_contracts::assistant::AssistantVerificationWarning {
        code: warning.code,
        message: warning.message,
        related_segment_id: warning.related_segment_id,
        related_fact_id: warning.related_fact_id,
    }
}

const fn map_chunk_reference(
    reference: QueryChunkReference,
) -> ironrag_contracts::assistant::AssistantChunkReference {
    ironrag_contracts::assistant::AssistantChunkReference {
        execution_id: reference.execution_id,
        chunk_id: reference.chunk_id,
        rank: reference.rank,
        score: reference.score,
    }
}

fn map_prepared_segment_reference(
    reference: PreparedSegmentReference,
) -> ironrag_contracts::assistant::AssistantPreparedSegmentReference {
    ironrag_contracts::assistant::AssistantPreparedSegmentReference {
        execution_id: reference.execution_id,
        segment_id: reference.segment_id,
        revision_id: reference.revision_id,
        block_kind: reference.block_kind.as_str().to_string(),
        rank: reference.rank,
        score: reference.score,
        heading_trail: reference.heading_trail,
        section_path: reference.section_path,
        document_id: reference.document_id,
        document_title: reference.document_title,
        document_hint: reference.document_hint,
        source_access: reference.source_access.map(|access| {
            ironrag_contracts::assistant::AssistantContentSourceAccess {
                kind: match access.kind {
                    crate::domains::content::ContentSourceAccessKind::StoredDocument => {
                        "stored_document".to_string()
                    }
                    crate::domains::content::ContentSourceAccessKind::ExternalUrl => {
                        "external_url".to_string()
                    }
                },
                href: access.href,
            }
        }),
    }
}

fn map_technical_fact_reference(
    reference: TechnicalFactReference,
) -> ironrag_contracts::assistant::AssistantTechnicalFactReference {
    ironrag_contracts::assistant::AssistantTechnicalFactReference {
        execution_id: reference.execution_id,
        fact_id: reference.fact_id,
        revision_id: reference.revision_id,
        fact_kind: reference.fact_kind.as_str().to_string(),
        canonical_value: reference.canonical_value,
        display_value: reference.display_value,
        rank: reference.rank,
        score: reference.score,
    }
}

fn map_graph_node_reference(
    reference: QueryGraphNodeReference,
) -> ironrag_contracts::assistant::AssistantEntityReference {
    ironrag_contracts::assistant::AssistantEntityReference {
        execution_id: reference.execution_id,
        node_id: reference.node_id,
        rank: reference.rank,
        score: reference.score,
        label: reference.label,
        entity_type: reference.entity_type,
        summary: reference.summary,
    }
}

fn map_graph_edge_reference(
    reference: QueryGraphEdgeReference,
) -> ironrag_contracts::assistant::AssistantRelationReference {
    ironrag_contracts::assistant::AssistantRelationReference {
        execution_id: reference.execution_id,
        edge_id: reference.edge_id,
        rank: reference.rank,
        score: reference.score,
        predicate: reference.relation_type,
        normalized_assertion: reference.summary,
    }
}

fn append_query_execution_audit(
    state: AppState,
    principal_id: Uuid,
    surface_kind: &'static str,
    outcome: &crate::services::query::service::QueryTurnExecutionResult,
) -> futures::future::BoxFuture<'static, ()> {
    let command = AppendQueryExecutionAuditCommand {
        actor_principal_id: principal_id,
        surface_kind: surface_kind.to_string(),
        request_id: None,
        query_session_id: outcome.conversation.id,
        query_execution_id: outcome.execution.id,
        runtime_execution_id: outcome.execution.runtime_execution_id,
        context_bundle_id: outcome.context_bundle_id,
        workspace_id: outcome.execution.workspace_id,
        library_id: outcome.execution.library_id,
        question_preview: Some(outcome.request_turn.content_text.clone()),
    };
    let audit_service = state.canonical_services.audit.clone();
    Box::pin(async move {
        if let Err(error) = audit_service.append_query_execution_event(&state, command).await {
            tracing::warn!(stage = "audit", error = %error, "audit append failed");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity_event() -> AssistantActivityEvent {
        AssistantActivityEvent {
            event_type: "working",
            deadline_ms: None,
            iteration: None,
            provider_kind: None,
            model_name: None,
            tool_call_count: None,
            has_final_answer: None,
            tool_name: None,
            elapsed_ms: None,
            is_error: None,
            child_execution_id: None,
            result_preview: None,
        }
    }

    fn assistant_runtime_summary() -> ironrag_contracts::assistant::AssistantRuntimeSummary {
        let now = Utc::now();
        ironrag_contracts::assistant::AssistantRuntimeSummary {
            runtime_execution_id: Uuid::new_v4(),
            lifecycle_state: "completed".to_string(),
            active_stage: None,
            turn_budget: 1,
            turn_count: 1,
            parallel_action_limit: 1,
            failure_code: None,
            failure_summary_redacted: None,
            policy_summary: ironrag_contracts::assistant::AssistantPolicySummary {
                allow_count: 0,
                reject_count: 0,
                terminate_count: 0,
                recent_decisions: Vec::new(),
            },
            accepted_at: now,
            completed_at: Some(now),
        }
    }

    fn chunk_reference(
        rank: i32,
        id: u128,
    ) -> ironrag_contracts::assistant::AssistantChunkReference {
        ironrag_contracts::assistant::AssistantChunkReference {
            execution_id: Uuid::from_u128(1),
            chunk_id: Uuid::from_u128(id),
            rank,
            score: 1.0 / f64::from(rank.max(1)),
        }
    }

    fn prepared_segment_reference(
        rank: i32,
        id: u128,
    ) -> ironrag_contracts::assistant::AssistantPreparedSegmentReference {
        ironrag_contracts::assistant::AssistantPreparedSegmentReference {
            execution_id: Uuid::from_u128(1),
            segment_id: Uuid::from_u128(id),
            revision_id: Uuid::from_u128(id + 100),
            block_kind: "paragraph".to_string(),
            rank,
            score: 1.0 / f64::from(rank.max(1)),
            heading_trail: Vec::new(),
            section_path: Vec::new(),
            document_id: None,
            document_title: None,
            document_hint: None,
            source_access: None,
        }
    }

    fn technical_fact_reference(
        rank: i32,
        id: u128,
    ) -> ironrag_contracts::assistant::AssistantTechnicalFactReference {
        ironrag_contracts::assistant::AssistantTechnicalFactReference {
            execution_id: Uuid::from_u128(1),
            fact_id: Uuid::from_u128(id),
            revision_id: Uuid::from_u128(id + 100),
            fact_kind: "scalar".to_string(),
            canonical_value: id.to_string(),
            display_value: id.to_string(),
            rank,
            score: 1.0 / f64::from(rank.max(1)),
        }
    }

    fn entity_reference(
        rank: i32,
        id: u128,
    ) -> ironrag_contracts::assistant::AssistantEntityReference {
        ironrag_contracts::assistant::AssistantEntityReference {
            execution_id: Uuid::from_u128(1),
            node_id: Uuid::from_u128(id),
            rank,
            score: 1.0 / f64::from(rank.max(1)),
            label: id.to_string(),
            entity_type: None,
            summary: None,
        }
    }

    fn relation_reference(
        rank: i32,
        id: u128,
    ) -> ironrag_contracts::assistant::AssistantRelationReference {
        ironrag_contracts::assistant::AssistantRelationReference {
            execution_id: Uuid::from_u128(1),
            edge_id: Uuid::from_u128(id),
            rank,
            score: 1.0 / f64::from(rank.max(1)),
            predicate: id.to_string(),
            normalized_assertion: None,
        }
    }

    fn evidence_bundle_with_references(
        chunk_references: Vec<ironrag_contracts::assistant::AssistantChunkReference>,
        prepared_segment_references: Vec<
            ironrag_contracts::assistant::AssistantPreparedSegmentReference,
        >,
        technical_fact_references: Vec<
            ironrag_contracts::assistant::AssistantTechnicalFactReference,
        >,
        entity_references: Vec<ironrag_contracts::assistant::AssistantEntityReference>,
        relation_references: Vec<ironrag_contracts::assistant::AssistantRelationReference>,
    ) -> ironrag_contracts::assistant::AssistantEvidenceBundle {
        ironrag_contracts::assistant::AssistantEvidenceBundle {
            chunk_references,
            prepared_segment_references,
            technical_fact_references,
            entity_references,
            relation_references,
            reference_summary: None,
            verification_state: ironrag_contracts::assistant::AssistantVerificationState::Verified,
            verification_warnings: Vec::new(),
            answer_disposition:
                ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady,
            clarification: ironrag_contracts::assistant::AssistantClarification::default(),
            runtime_summary: assistant_runtime_summary(),
            runtime_stage_summaries: Vec::new(),
        }
    }

    fn execution_detail_from_evidence(
        evidence: ironrag_contracts::assistant::AssistantEvidenceBundle,
    ) -> ironrag_contracts::assistant::AssistantExecutionDetail {
        let now = Utc::now();
        let context_bundle_id = Uuid::new_v4();
        let execution_id = Uuid::new_v4();
        ironrag_contracts::assistant::AssistantExecutionDetail {
            context_bundle_id,
            execution: ironrag_contracts::assistant::AssistantExecution {
                id: execution_id,
                workspace_id: Uuid::new_v4(),
                library_id: Uuid::new_v4(),
                conversation_id: Uuid::new_v4(),
                context_bundle_id,
                request_turn_id: None,
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id: Some(evidence.runtime_summary.runtime_execution_id),
                lifecycle_state: "completed".to_string(),
                active_stage: None,
                query_text: "neutral question".to_string(),
                failure_code: None,
                started_at: now,
                completed_at: Some(now),
            },
            runtime_summary: evidence.runtime_summary,
            runtime_stage_summaries: evidence.runtime_stage_summaries,
            request_turn: None,
            response_turn: None,
            chunk_references: evidence.chunk_references,
            prepared_segment_references: evidence.prepared_segment_references,
            technical_fact_references: evidence.technical_fact_references,
            entity_references: evidence.entity_references,
            relation_references: evidence.relation_references,
            reference_summary: evidence.reference_summary,
            verification_state: evidence.verification_state,
            verification_warnings: evidence.verification_warnings,
            answer_disposition: evidence.answer_disposition,
            clarification: evidence.clarification,
        }
    }

    fn query_execution_detail_with_chunks(count: usize) -> QueryExecutionDetail {
        let now = Utc::now();
        let execution = query_execution_with_state(
            crate::domains::agent_runtime::RuntimeLifecycleState::Completed,
            None,
            None,
        );
        let execution_id = execution.id;
        QueryExecutionDetail {
            runtime_summary: RuntimeExecutionSummary {
                runtime_execution_id: execution
                    .runtime_execution_id
                    .expect("test execution carries a runtime id"),
                lifecycle_state: crate::domains::agent_runtime::RuntimeLifecycleState::Completed,
                active_stage: None,
                turn_budget: 1,
                turn_count: 1,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
                policy_summary: RuntimePolicySummary::default(),
                accepted_at: now,
                completed_at: Some(now),
            },
            runtime_stage_summaries: Vec::new(),
            request_turn: None,
            response_turn: None,
            chunk_references: (0..count)
                .map(|index| QueryChunkReference {
                    execution_id,
                    chunk_id: Uuid::from_u128(1_000 + index as u128),
                    rank: i32::try_from(index + 1).expect("test rank fits i32"),
                    score: 1.0 / (index + 1) as f64,
                })
                .collect(),
            prepared_segment_references: Vec::new(),
            technical_fact_references: Vec::new(),
            graph_node_references: Vec::new(),
            graph_edge_references: Vec::new(),
            verification_state: QueryVerificationState::Verified,
            verification_warnings: Vec::new(),
            answer_disposition: crate::domains::query::QueryAnswerDisposition::FactualReady,
            clarification: QueryClarification::default(),
            query_ir: None,
            execution,
        }
    }

    fn query_execution_with_state(
        lifecycle_state: crate::domains::agent_runtime::RuntimeLifecycleState,
        request_turn_id: Option<Uuid>,
        response_turn_id: Option<Uuid>,
    ) -> QueryExecution {
        QueryExecution {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            library_id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            context_bundle_id: Uuid::new_v4(),
            request_turn_id,
            response_turn_id,
            binding_id: None,
            runtime_execution_id: Some(Uuid::new_v4()),
            lifecycle_state,
            active_stage: None,
            query_text: "neutral question".to_string(),
            failure_code: None,
            started_at: Utc::now(),
            completed_at: None,
        }
    }

    #[test]
    fn pending_session_execution_maps_to_empty_assistant_message() {
        let request_turn_id = Uuid::new_v4();
        let execution = query_execution_with_state(
            crate::domains::agent_runtime::RuntimeLifecycleState::Running,
            Some(request_turn_id),
            None,
        );

        assert!(is_pending_session_execution(&execution));

        let message = map_pending_execution_to_message(&execution);
        assert_eq!(message.id, execution.id);
        assert_eq!(message.role, ironrag_contracts::assistant::AssistantTurnRole::Assistant);
        assert!(message.content.is_empty());
        assert_eq!(message.timestamp, execution.started_at);
        assert_eq!(message.execution_id, Some(execution.id));
        assert!(message.evidence.is_none());
    }

    #[test]
    fn terminal_or_answered_execution_is_not_session_pending() {
        let request_turn_id = Uuid::new_v4();
        let response_turn_id = Uuid::new_v4();
        let answered = query_execution_with_state(
            crate::domains::agent_runtime::RuntimeLifecycleState::Running,
            Some(request_turn_id),
            Some(response_turn_id),
        );
        let failed = query_execution_with_state(
            crate::domains::agent_runtime::RuntimeLifecycleState::Failed,
            Some(request_turn_id),
            None,
        );
        let orphan = query_execution_with_state(
            crate::domains::agent_runtime::RuntimeLifecycleState::Running,
            None,
            None,
        );

        assert!(!is_pending_session_execution(&answered));
        assert!(!is_pending_session_execution(&failed));
        assert!(!is_pending_session_execution(&orphan));
    }

    #[tokio::test]
    async fn assistant_activity_stream_reserves_terminal_capacity() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<AssistantTurnStreamEvent>(
            ASSISTANT_TURN_TERMINAL_EVENT_RESERVE + 2,
        );

        for _ in 0..100 {
            send_assistant_activity(&sender, activity_event());
        }

        assert_eq!(sender.capacity(), ASSISTANT_TURN_TERMINAL_EVENT_RESERVE);
        tokio::time::timeout(
            Duration::from_millis(50),
            send_required_turn_stream_event(
                &sender,
                AssistantTurnStreamEvent::Failed { message: "panic".to_string() },
            ),
        )
        .await
        .expect("terminal event should not wait behind activity backlog");

        let mut saw_failure = false;
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(50), receiver.recv()).await
        {
            if matches!(event, AssistantTurnStreamEvent::Failed { .. }) {
                saw_failure = true;
                break;
            }
        }
        assert!(saw_failure);
    }

    #[test]
    fn map_turn_to_message_preserves_hydrated_evidence() {
        let now = Utc::now();
        let execution_id = Uuid::new_v4();
        let evidence = ironrag_contracts::assistant::AssistantEvidenceBundle {
            chunk_references: Vec::new(),
            prepared_segment_references: Vec::new(),
            technical_fact_references: Vec::new(),
            entity_references: Vec::new(),
            relation_references: Vec::new(),
            reference_summary: None,
            verification_state: ironrag_contracts::assistant::AssistantVerificationState::Verified,
            verification_warnings: Vec::new(),
            answer_disposition:
                ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady,
            clarification: ironrag_contracts::assistant::AssistantClarification::default(),
            runtime_summary: ironrag_contracts::assistant::AssistantRuntimeSummary {
                runtime_execution_id: Uuid::new_v4(),
                lifecycle_state: "completed".to_string(),
                active_stage: None,
                turn_budget: 4,
                turn_count: 1,
                parallel_action_limit: 2,
                failure_code: None,
                failure_summary_redacted: None,
                policy_summary: ironrag_contracts::assistant::AssistantPolicySummary {
                    allow_count: 0,
                    reject_count: 0,
                    terminate_count: 0,
                    recent_decisions: Vec::new(),
                },
                accepted_at: now,
                completed_at: Some(now),
            },
            runtime_stage_summaries: vec![
                ironrag_contracts::assistant::AssistantRuntimeStageSummary {
                    stage_kind: "retrieve".to_string(),
                    stage_label: "Retrieve".to_string(),
                    duration_ms: None,
                },
            ],
        };

        let message = map_turn_to_message(
            QueryTurn {
                id: Uuid::new_v4(),
                conversation_id: Uuid::new_v4(),
                turn_index: 1,
                turn_kind: crate::domains::query::QueryTurnKind::Assistant,
                author_principal_id: None,
                content_text: "answer".to_string(),
                execution_id: Some(execution_id),
                created_at: now,
            },
            Some(evidence),
        );

        let hydrated_evidence =
            message.evidence.expect("hydrated session messages must retain assistant evidence");
        assert_eq!(message.execution_id, Some(execution_id));
        assert_eq!(
            hydrated_evidence.verification_state,
            ironrag_contracts::assistant::AssistantVerificationState::Verified
        );
        assert_eq!(hydrated_evidence.runtime_stage_summaries.len(), 1);
        assert_eq!(hydrated_evidence.runtime_stage_summaries[0].stage_kind, "retrieve");
    }

    #[test]
    fn ui_reference_projection_caps_all_reference_kinds_globally() {
        let evidence = evidence_bundle_with_references(
            (0..8).map(|index| chunk_reference(20 + index, 10 + index as u128)).collect(),
            Vec::new(),
            (0..5).map(|index| technical_fact_reference(1 + index, 30 + index as u128)).collect(),
            Vec::new(),
            Vec::new(),
        );

        let projected = project_ui_evidence_bundle(evidence);
        let returned_count = projected.chunk_references.len()
            + projected.prepared_segment_references.len()
            + projected.technical_fact_references.len()
            + projected.entity_references.len()
            + projected.relation_references.len();

        assert_eq!(returned_count, UI_VISIBLE_REFERENCE_LIMIT);
        assert_eq!(projected.technical_fact_references.len(), 5);
        assert_eq!(projected.chunk_references.len(), 7);
        assert_eq!(
            projected.reference_summary,
            Some(ironrag_contracts::assistant::AssistantReferenceSummary {
                total_count: 13,
                returned_count: UI_VISIBLE_REFERENCE_LIMIT,
                truncated: true,
            })
        );
    }

    #[test]
    fn ui_reference_projection_keeps_small_sets_without_truncation() {
        let evidence = evidence_bundle_with_references(
            vec![chunk_reference(2, 10)],
            vec![prepared_segment_reference(1, 20)],
            vec![technical_fact_reference(3, 30)],
            vec![entity_reference(4, 40)],
            vec![relation_reference(5, 50)],
        );

        let projected = project_ui_evidence_bundle(evidence);

        assert_eq!(projected.chunk_references.len(), 1);
        assert_eq!(projected.prepared_segment_references.len(), 1);
        assert_eq!(projected.technical_fact_references.len(), 1);
        assert_eq!(projected.entity_references.len(), 1);
        assert_eq!(projected.relation_references.len(), 1);
        assert_eq!(
            projected.reference_summary,
            Some(ironrag_contracts::assistant::AssistantReferenceSummary {
                total_count: 5,
                returned_count: 5,
                truncated: false,
            })
        );
    }

    #[test]
    fn ui_reference_projection_breaks_rank_ties_by_kind_then_original_order() {
        let evidence = evidence_bundle_with_references(
            vec![chunk_reference(1, 12), chunk_reference(1, 11), chunk_reference(1, 10)],
            vec![
                prepared_segment_reference(1, 22),
                prepared_segment_reference(1, 21),
                prepared_segment_reference(1, 20),
            ],
            vec![
                technical_fact_reference(1, 32),
                technical_fact_reference(1, 31),
                technical_fact_reference(1, 30),
            ],
            vec![entity_reference(1, 42), entity_reference(1, 41), entity_reference(1, 40)],
            vec![relation_reference(1, 50)],
        );

        let projected = project_ui_evidence_bundle(evidence);

        assert_eq!(
            projected
                .chunk_references
                .iter()
                .map(|reference| reference.chunk_id)
                .collect::<Vec<_>>(),
            vec![Uuid::from_u128(12), Uuid::from_u128(11), Uuid::from_u128(10)]
        );
        assert_eq!(projected.entity_references.len(), 3);
        assert!(projected.relation_references.is_empty());
    }

    #[test]
    fn ui_completion_and_hydration_serialize_bounded_reference_summaries() {
        let evidence = evidence_bundle_with_references(
            (0..13).map(|index| chunk_reference(index + 1, 100 + index as u128)).collect(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let projected_evidence = project_ui_evidence_bundle(evidence.clone());
        let projected_detail =
            project_ui_execution_detail(execution_detail_from_evidence(evidence));

        let json_completion =
            serde_json::to_value(&projected_detail).expect("serialize completion");
        assert_eq!(json_completion["referenceSummary"]["totalCount"], 13);
        assert_eq!(json_completion["referenceSummary"]["returnedCount"], 12);
        assert_eq!(json_completion["chunkReferences"].as_array().map(Vec::len), Some(12));

        let stream_completion = serde_json::to_value(AssistantTurnStreamEvent::Completed {
            detail: Box::new(projected_detail),
        })
        .expect("serialize terminal stream event");
        assert_eq!(stream_completion["type"], "completed");
        assert_eq!(stream_completion["detail"]["referenceSummary"]["totalCount"], 13);
        assert_eq!(
            stream_completion["detail"]["chunkReferences"].as_array().map(Vec::len),
            Some(12)
        );

        let hydrated = serde_json::to_value(projected_evidence).expect("serialize hydration");
        assert_eq!(hydrated["referenceSummary"]["totalCount"], 13);
        assert_eq!(hydrated["referenceSummary"]["returnedCount"], 12);
    }

    #[test]
    fn unprojected_debug_detail_remains_unbounded_and_omits_ui_summary() {
        let evidence = evidence_bundle_with_references(
            (0..13).map(|index| chunk_reference(index + 1, 200 + index as u128)).collect(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let debug_detail = execution_detail_from_evidence(evidence);

        let serialized = serde_json::to_value(debug_detail).expect("serialize debug detail");

        assert_eq!(serialized["chunkReferences"].as_array().map(Vec::len), Some(13));
        assert!(serialized.get("referenceSummary").is_none());
    }

    #[test]
    fn session_hydration_projects_references_while_debug_mapping_stays_complete() {
        let durable_detail = query_execution_detail_with_chunks(13);

        let debug_detail = map_execution_detail(durable_detail.clone());
        let hydrated_evidence = map_execution_detail_to_evidence(durable_detail);

        assert_eq!(debug_detail.chunk_references.len(), 13);
        assert!(debug_detail.reference_summary.is_none());
        assert_eq!(hydrated_evidence.chunk_references.len(), 12);
        assert_eq!(
            hydrated_evidence.reference_summary,
            Some(ironrag_contracts::assistant::AssistantReferenceSummary {
                total_count: 13,
                returned_count: 12,
                truncated: true,
            })
        );
    }

    #[test]
    fn session_hydration_preserves_typed_clarification_and_candidates() {
        let mut durable_detail = query_execution_detail_with_chunks(0);
        let candidate = QueryAnswerCandidate {
            label: "Neutral variant".to_string(),
            kind: "document".to_string(),
            confidence: Some(0.75),
            provenance: crate::domains::query::QueryAnswerCandidateProvenance::default(),
        };
        durable_detail.answer_disposition =
            crate::domains::query::QueryAnswerDisposition::Clarification;
        durable_detail.clarification = QueryClarification {
            required: true,
            question: Some("Which neutral variant?".to_string()),
            answer_candidates: vec![candidate],
        };

        let hydrated = map_execution_detail_to_evidence(durable_detail);

        assert_eq!(
            hydrated.answer_disposition,
            ironrag_contracts::assistant::AssistantAnswerDisposition::Clarification,
        );
        assert!(hydrated.clarification.required);
        assert_eq!(hydrated.clarification.question.as_deref(), Some("Which neutral variant?"));
        assert_eq!(hydrated.clarification.answer_candidates.len(), 1);
        assert_eq!(hydrated.clarification.answer_candidates[0].label, "Neutral variant");
    }

    #[test]
    fn panic_payload_message_accepts_dyn_any_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("boom");

        assert_eq!(panic_payload_message(payload.as_ref()), "boom");
    }
}
