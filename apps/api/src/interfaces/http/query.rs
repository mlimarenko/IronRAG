use std::{convert::Infallible, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use chrono::Utc;
use futures::stream;
use ironrag_contracts;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::agent_runtime::{
        RuntimeExecutionSummary, RuntimePolicyDecisionSummary, RuntimePolicySummary,
    },
    domains::query::{
        PreparedSegmentReference, QueryChunkReference, QueryConversation, QueryConversationDetail,
        QueryExecution, QueryExecutionDetail, QueryGraphEdgeReference, QueryGraphNodeReference,
        QueryRuntimeStageSummary, QueryTurn, QueryVerificationState, QueryVerificationWarning,
        TechnicalFactReference,
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
        query::service::{
            CreateConversationCommand, ExecuteConversationTurnCommand, QueryTurnProgressEvent,
        },
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
    /// When omitted, inferred from the library's parent workspace.
    workspace_id: Option<Uuid>,
    library_id: Uuid,
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionTurnRequest {
    content_text: String,
    top_k: Option<usize>,
    include_debug: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryTurnStreamRuntimePayload {
    runtime: RuntimeExecutionSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryTurnStreamDeltaPayload {
    delta: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryTurnStreamToolCallStartedPayload {
    iteration: usize,
    call_id: String,
    name: String,
    arguments_preview: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryTurnStreamToolCallCompletedPayload {
    iteration: usize,
    call_id: String,
    name: String,
    is_error: bool,
    result_preview: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryTurnStreamErrorPayload {
    error: String,
    error_kind: &'static str,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/query/sessions", get(list_sessions).post(create_session))
        .route("/query/sessions/{session_id}", get(get_session))
        .route("/query/sessions/{session_id}/turns", axum::routing::post(create_session_turn))
        .route("/query/executions/{execution_id}", get(get_execution))
        .route("/query/executions/{execution_id}/llm-context", get(get_execution_llm_context))
        .route("/query/assistant/system-prompt", get(get_assistant_system_prompt))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssistantSystemPromptQuery {
    library_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssistantSystemPromptResponse {
    /// Raw template with the `{LIBRARY_REF}` placeholder. This is what
    /// external MCP clients (Claude Desktop, Codex, Cursor, Continue.dev,
    /// …) should paste into their own system prompt when attaching
    /// IronRAG's MCP server, so every agent — in-app or external — shares
    /// the same grounding discipline.
    template: String,
    /// Template rendered with the canonical `<workspace>/<library>` ref
    /// of the requested `libraryId`, when one was passed. Same text the
    /// in-app assistant agent uses for that library.
    rendered: Option<String>,
    library_id: Option<Uuid>,
}

/// Publish the canonical MCP assistant system prompt.
///
/// This is the single source of truth for two surfaces:
///   * our in-app assistant (`agent_loop::run_assistant_turn`);
///   * the admin UI's "MCP client setup" card, which serves the same
///     text verbatim for operators to copy into their own agents.
///
/// Any drift between the in-app agent and external agents would
/// silently change grounding behavior per client — so the text lives
/// in `services::query::assistant_prompt` and every consumer reads
/// from there.
#[tracing::instrument(
    level = "info",
    name = "http.query.get_assistant_system_prompt",
    skip_all,
    fields(library_id = ?query.library_id)
)]
async fn get_assistant_system_prompt(
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
        template: crate::services::query::assistant_prompt::ASSISTANT_SYSTEM_PROMPT_TEMPLATE
            .to_string(),
        rendered,
        library_id: query.library_id,
    }))
}

#[tracing::instrument(
    level = "info",
    name = "http.query.list_sessions",
    skip_all,
    fields(library_id = ?query.library_id, item_count)
)]
async fn list_sessions(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<ListSessionsQuery>,
) -> Result<Json<Vec<ironrag_contracts::assistant::AssistantSessionListItem>>, ApiError> {
    let span = tracing::Span::current();
    let library_id = query
        .library_id
        .ok_or_else(|| ApiError::BadRequest("libraryId is required".to_string()))?;
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_QUERY_READ).await?;
    let conversations =
        state.canonical_services.query.list_conversations(&state, library_id).await?;
    let items: Vec<_> =
        conversations.into_iter().map(map_session_list_item_with_defaults).collect();
    span.record("item_count", items.len());
    Ok(Json(items))
}

#[tracing::instrument(
    level = "info",
    name = "http.query.create_session",
    skip_all,
    fields(library_id = %payload.library_id)
)]
async fn create_session(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<Json<QueryConversation>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, payload.library_id, POLICY_QUERY_RUN).await?;
    // workspace_id is now optional — infer from the library when omitted.
    let workspace_id = payload.workspace_id.unwrap_or(library.workspace_id);
    if library.workspace_id != workspace_id {
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
                workspace_id,
                library_id: payload.library_id,
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
        .await
    {
        tracing::warn!(stage = "audit", error = %error, "audit append failed");
    }
    Ok(Json(conversation))
}

#[tracing::instrument(
    level = "info",
    name = "http.query.get_session",
    skip_all,
    fields(session_id = %session_id)
)]
async fn get_session(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<ironrag_contracts::assistant::AssistantHydratedConversation>, ApiError> {
    let _ = load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_READ).await?;
    let detail = state.canonical_services.query.get_conversation(&state, session_id).await?;
    Ok(Json(map_session_detail(detail)))
}

#[tracing::instrument(
    level = "info",
    name = "http.create_session_turn",
    skip_all,
    fields(session_id = %session_id, elapsed_ms)
)]
async fn create_session_turn(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    Json(payload): Json<CreateSessionTurnRequest>,
) -> Result<Response, ApiError> {
    let started_at = std::time::Instant::now();
    let span = tracing::Span::current();
    let _ = load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_RUN).await?;
    if accepts_event_stream(&headers) {
        return Ok(create_session_turn_stream(auth, state, session_id, payload).into_response());
    }
    let outcome = state
        .canonical_services
        .query
        .execute_turn(
            &state,
            ExecuteConversationTurnCommand {
                conversation_id: session_id,
                author_principal_id: Some(auth.principal_id),
                content_text: payload.content_text,
                external_prior_turns: Vec::new(),
                top_k: payload.top_k.unwrap_or(8),
                include_debug: payload.include_debug.unwrap_or(false),
                auth: auth.clone(),
            },
        )
        .await?;
    append_query_execution_audit(state.clone(), auth.principal_id, &outcome).await;
    span.record("elapsed_ms", started_at.elapsed().as_millis() as u64);
    Ok(Json(map_turn_execution_response(outcome)).into_response())
}

#[tracing::instrument(
    level = "info",
    name = "http.get_execution",
    skip_all,
    fields(execution_id = %execution_id)
)]
async fn get_execution(
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
/// provider for this assistant execution, if it is still in the
/// in-memory debug cache. The cache is volatile (bounded FIFO) so old
/// executions return 404 — that is by design, not an error the UI
/// should treat as fatal.
#[tracing::instrument(
    level = "info",
    name = "http.query.get_execution_llm_context",
    skip_all,
    fields(execution_id = %execution_id)
)]
async fn get_execution_llm_context(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(execution_id): Path<Uuid>,
) -> Result<Json<crate::services::query::llm_context_debug::LlmContextSnapshot>, ApiError> {
    let _ =
        load_query_execution_and_authorize(&auth, &state, execution_id, POLICY_QUERY_READ).await?;
    state
        .llm_context_debug
        .get(execution_id)
        .map(Json)
        .ok_or_else(|| ApiError::resource_not_found("llm_context_snapshot", execution_id))
}

fn map_session_list_item_with_defaults(
    session: QueryConversation,
) -> ironrag_contracts::assistant::AssistantSessionListItem {
    map_session_list_item_with_turn_count(session, 0)
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

fn map_session_detail(
    detail: QueryConversationDetail,
) -> ironrag_contracts::assistant::AssistantHydratedConversation {
    ironrag_contracts::assistant::AssistantHydratedConversation {
        session: map_session_list_item_with_turn_count(detail.conversation, detail.turns.len()),
        messages: detail.turns.into_iter().map(map_turn_to_message).collect(),
    }
}

fn map_execution_detail(
    detail: QueryExecutionDetail,
) -> ironrag_contracts::assistant::AssistantExecutionDetail {
    ironrag_contracts::assistant::AssistantExecutionDetail {
        context_bundle_id: detail.execution.context_bundle_id,
        execution: map_execution(detail.execution),
        runtime_summary: map_runtime_summary(detail.runtime_summary),
        runtime_stage_summaries: detail
            .runtime_stage_summaries
            .into_iter()
            .map(map_runtime_stage_summary)
            .collect(),
        request_turn: detail.request_turn.map(map_turn),
        response_turn: detail.response_turn.map(map_turn),
        chunk_references: detail.chunk_references.into_iter().map(map_chunk_reference).collect(),
        prepared_segment_references: detail
            .prepared_segment_references
            .into_iter()
            .map(map_prepared_segment_reference)
            .collect(),
        technical_fact_references: detail
            .technical_fact_references
            .into_iter()
            .map(map_technical_fact_reference)
            .collect(),
        entity_references: detail
            .graph_node_references
            .into_iter()
            .map(map_graph_node_reference)
            .collect(),
        relation_references: detail
            .graph_edge_references
            .into_iter()
            .map(map_graph_edge_reference)
            .collect(),
        verification_state: map_verification_state(detail.verification_state),
        verification_warnings: detail
            .verification_warnings
            .into_iter()
            .map(map_verification_warning)
            .collect(),
    }
}

fn accepts_event_stream(headers: &HeaderMap) -> bool {
    headers.get(header::ACCEPT).and_then(|value| value.to_str().ok()).is_some_and(|value| {
        value.split(',').any(|item| item.trim().starts_with("text/event-stream"))
    })
}

fn map_turn_execution_response(
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
        verification_state: map_verification_state(outcome.verification_state),
        verification_warnings: outcome
            .verification_warnings
            .into_iter()
            .map(map_verification_warning)
            .collect(),
    }
}

fn map_turn_to_message(
    turn: QueryTurn,
) -> ironrag_contracts::assistant::AssistantConversationMessage {
    ironrag_contracts::assistant::AssistantConversationMessage {
        id: turn.id,
        role: map_turn_role(turn.turn_kind),
        content: turn.content_text,
        timestamp: turn.created_at,
        execution_id: turn.execution_id,
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
        source_uri: reference.source_uri,
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

const fn map_graph_edge_reference(
    reference: QueryGraphEdgeReference,
) -> ironrag_contracts::assistant::AssistantRelationReference {
    ironrag_contracts::assistant::AssistantRelationReference {
        execution_id: reference.execution_id,
        edge_id: reference.edge_id,
        rank: reference.rank,
        score: reference.score,
        predicate: String::new(),
        normalized_assertion: None,
    }
}

async fn append_query_execution_audit(
    state: AppState,
    principal_id: Uuid,
    outcome: &crate::services::query::service::QueryTurnExecutionResult,
) {
    if let Err(error) = state
        .canonical_services
        .audit
        .append_query_execution_event(
            &state,
            AppendQueryExecutionAuditCommand {
                actor_principal_id: principal_id,
                surface_kind: "rest".to_string(),
                request_id: None,
                query_session_id: outcome.conversation.id,
                query_execution_id: outcome.execution.id,
                runtime_execution_id: outcome.execution.runtime_execution_id,
                context_bundle_id: outcome.context_bundle_id,
                workspace_id: outcome.execution.workspace_id,
                library_id: outcome.execution.library_id,
                question_preview: Some(outcome.request_turn.content_text.clone()),
            },
        )
        .await
    {
        tracing::warn!(stage = "audit", error = %error, "audit append failed");
    }
}

fn create_session_turn_stream(
    auth: AuthContext,
    state: AppState,
    session_id: Uuid,
    payload: CreateSessionTurnRequest,
) -> Sse<impl stream::Stream<Item = Result<Event, Infallible>>> {
    let principal_id = auth.principal_id;
    let (sender, receiver) = mpsc::unbounded_channel::<QueryTurnStreamFrame>();
    let state_for_task = state.clone();
    tokio::spawn(async move {
        let (progress_sender, mut progress_receiver) =
            mpsc::unbounded_channel::<QueryTurnProgressEvent>();
        let frame_sender = sender.clone();
        let progress_bridge = tokio::spawn(async move {
            while let Some(event) = progress_receiver.recv().await {
                if frame_sender.send(QueryTurnStreamFrame::from(event)).is_err() {
                    break;
                }
            }
        });
        let outcome = state_for_task
            .canonical_services
            .query
            .execute_turn_stream(
                &state_for_task,
                ExecuteConversationTurnCommand {
                    conversation_id: session_id,
                    author_principal_id: Some(principal_id),
                    content_text: payload.content_text,
                    external_prior_turns: Vec::new(),
                    top_k: payload.top_k.unwrap_or(8),
                    include_debug: payload.include_debug.unwrap_or(false),
                    auth: auth.clone(),
                },
                progress_sender,
            )
            .await;
        let _ = progress_bridge.await;

        match outcome {
            Ok(outcome) => {
                append_query_execution_audit(state_for_task.clone(), principal_id, &outcome).await;
                let _ = sender
                    .send(QueryTurnStreamFrame::Completed(map_turn_execution_response(outcome)));
            }
            Err(error) => {
                let _ = sender.send(QueryTurnStreamFrame::Error(QueryTurnStreamErrorPayload {
                    error: error.to_string(),
                    error_kind: error.kind(),
                }));
            }
        }
    });

    let stream = stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|frame| (Ok(frame.into_event()), receiver))
    });

    // Keep-alive interval is intentionally short. Firefox Enhanced
    // Tracking Protection (and some corporate proxies) time out idle
    // fetch streams around 10 s; retrieval + context assembly can eat
    // 15-20 s before the first real `runtime`/`delta` frame, so we
    // need a filler frame under that ceiling. 3 s is cheap — comment
    // frames are ~15 bytes — and keeps the long-running turn stream
    // intact even when the retrieval stage takes its full budget.
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(3)).text("keep-alive"))
}

enum QueryTurnStreamFrame {
    Runtime(QueryTurnStreamRuntimePayload),
    Delta(QueryTurnStreamDeltaPayload),
    ToolCallStarted(QueryTurnStreamToolCallStartedPayload),
    ToolCallCompleted(QueryTurnStreamToolCallCompletedPayload),
    Completed(ironrag_contracts::assistant::AssistantExecutionDetail),
    Error(QueryTurnStreamErrorPayload),
}

impl From<QueryTurnProgressEvent> for QueryTurnStreamFrame {
    fn from(value: QueryTurnProgressEvent) -> Self {
        match value {
            QueryTurnProgressEvent::Runtime(runtime) => {
                Self::Runtime(QueryTurnStreamRuntimePayload { runtime })
            }
            QueryTurnProgressEvent::AnswerDelta(delta) => {
                Self::Delta(QueryTurnStreamDeltaPayload { delta })
            }
            QueryTurnProgressEvent::AssistantToolCallStarted {
                iteration,
                call_id,
                name,
                arguments_preview,
            } => Self::ToolCallStarted(QueryTurnStreamToolCallStartedPayload {
                iteration,
                call_id,
                name,
                arguments_preview,
            }),
            QueryTurnProgressEvent::AssistantToolCallCompleted {
                iteration,
                call_id,
                name,
                is_error,
                result_preview,
            } => Self::ToolCallCompleted(QueryTurnStreamToolCallCompletedPayload {
                iteration,
                call_id,
                name,
                is_error,
                result_preview,
            }),
        }
    }
}

impl QueryTurnStreamFrame {
    fn into_event(self) -> Event {
        match self {
            Self::Runtime(payload) => serialize_sse_event("runtime", &payload),
            Self::Delta(payload) => serialize_sse_event("delta", &payload),
            Self::ToolCallStarted(payload) => serialize_sse_event("tool_call_started", &payload),
            Self::ToolCallCompleted(payload) => {
                serialize_sse_event("tool_call_completed", &payload)
            }
            Self::Completed(payload) => serialize_sse_event("completed", &payload),
            Self::Error(payload) => serialize_sse_event("error", &payload),
        }
    }
}

fn serialize_sse_event(event_name: &'static str, payload: &impl Serialize) -> Event {
    match serde_json::to_string(payload) {
        Ok(data) => Event::default().event(event_name).data(data),
        Err(error) => Event::default().event("error").data(
            serde_json::json!({
                "error": format!("failed to serialize query stream event: {error}"),
                "errorKind": "internal",
            })
            .to_string(),
        ),
    }
}
