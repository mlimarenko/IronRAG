//! `grounded_answer` - MCP answer tool that runs `IronRAG`'s
//! grounded-answer pipeline for one library question.
//!
//! The implementation is deliberately a thin translator over the
//! compiled query service (`state.application_services.query`). The
//! handler creates an ephemeral conversation, delegates to
//! `execute_grounded_answer_turn`, and reshapes the result into the MCP tool-call
//! payload. The web UI assistant calls this through the in-process MCP
//! dispatcher when its model chooses `grounded_answer`; external clients
//! call the same tool contract through JSON-RPC.
//!
//! Phase 1 scope:
//!   - input: optional `library`, `query`, optional `conversationTurns`,
//!     optional `topK`, optional `includeDebug`
//!   - output: grounded answer text plus the authoritative execution details
//!     `AssistantExecutionDetail`, `runtimeExecutionId`,
//!     `conversationId`, `executionId`

mod args;
mod payload;
mod preserve_spans;

use args::*;
pub use payload::*;
pub(crate) use preserve_spans::*;

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::{
    domains::{
        query::{
            DEFAULT_TOP_K, MAX_TOP_K, QueryTurnKind, resolve_contextual_grounded_answer_top_k,
        },
        query_ir::{QueryIR, QueryLanguage, literal_text_is_identifier_shaped},
    },
    interfaces::http::{authorization::POLICY_QUERY_RUN, router_support::ApiError},
    mcp_types::{
        MCP_COMPACT_DEFAULT_REFERENCES, MCP_COMPACT_MAX_REFERENCES,
        McpGroundedAnswerResponseProfile,
    },
    services::{
        iam::audit::AppendQueryExecutionAuditCommand,
        mcp::agent_policy,
        query::{
            completion_policy::{
                AnswerCompletionAssessment, AnswerCompletionContract,
                GroundedAnswerCompletionEnvelope,
            },
            service::{
                CreateConversationCommand, ExecuteConversationTurnCommand,
                ExternalConversationTurn, QUERY_CONVERSATION_TITLE_LIMIT, QueryTurnExecutionResult,
            },
        },
    },
    shared::text_tokens::backtick_literal_spans,
};

use super::super::{McpToolDescriptor, McpToolResult, ok_tool_result, tool_error_result};
use super::ToolCallContext;

const MCP_GROUNDED_MAX_LIBRARY_CHARS: usize = 256;
const MCP_GROUNDED_MAX_QUERY_CHARS: usize = 4_000;
const MCP_GROUNDED_MAX_CONVERSATION_TURNS: usize = 20;
const MCP_GROUNDED_MAX_CONVERSATION_TURN_CHARS: usize = 1_200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GroundedAnswerConversationRouting {
    runtime_surface: crate::domains::agent_runtime::RuntimeSurfaceKind,
    storage_surface: &'static str,
}

const fn grounded_answer_conversation_routing(
    runtime_surface: crate::domains::agent_runtime::RuntimeSurfaceKind,
) -> GroundedAnswerConversationRouting {
    GroundedAnswerConversationRouting {
        runtime_surface,
        // `query_conversation.request_surface = 'ui'` is reserved for chats
        // visible in the assistant session list. Tool-created child state is
        // transient even when the in-process UI agent initiated the call.
        storage_surface: crate::domains::agent_runtime::RuntimeSurfaceKind::Mcp.as_str(),
    }
}

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    if name != "grounded_answer" {
        return None;
    }
    Some(McpToolDescriptor {
        name: "grounded_answer",
        description: agent_policy::GROUNDED_ANSWER_TOOL_DESCRIPTION,
        input_schema: json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "library": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MCP_GROUNDED_MAX_LIBRARY_CHARS,
                    "description": "Optional target fully-qualified library ref. An explicit ref is resolved directly and the token MUST have query_run on it. When omitted, IronRAG infers the target only when the token has query_run on exactly one library; zero or multiple query-authorized libraries are rejected."
                },
                "query": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MCP_GROUNDED_MAX_QUERY_CHARS,
                    "description": "Natural-language question in the user's language. IronRAG's QueryCompiler turns it into a typed QueryIR (act, scope, target_types) before retrieval — no keyword pre-processing is required on the client side."
                },
                "conversationTurns": {
                    "type": "array",
                    "maxItems": MCP_GROUNDED_MAX_CONVERSATION_TURNS,
                    "description": "Optional rolling prior chat turns for ordinary chat continuity, follow-ups, and coreference resolution. Pass the actual earlier user/assistant turns in chronological order when the client's tool runtime has them. If the client cannot pass history, rewrite the latest follow-up into one self-contained question before calling the tool.",
                    "items": {
                        "type": "object",
                        "required": ["role", "content"],
                        "properties": {
                            "role": {
                                "type": "string",
                                "enum": ["user", "assistant"]
                            },
                            "content": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": MCP_GROUNDED_MAX_CONVERSATION_TURN_CHARS
                            }
                        }
                    }
                },
                "topK": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_TOP_K,
                    "description": format!("Optional retrieval breadth. Defaults to {DEFAULT_TOP_K}, matching the UI assistant. Larger values are rarely useful; the verifier keeps only cited hits.")
                },
                "includeDebug": {
                    "type": "boolean",
                    "description": "Optional flag. When true, the response uses the full profile and carries the same debug metadata the UI debug panel shows (runtime stage summaries, graph expansion, verifier trace). It cannot be combined with responseProfile=compact."
                },
                "responseProfile": {
                    "type": "string",
                    "enum": ["full", "compact"],
                    "default": "compact",
                    "description": "Optional response shape. Ordinary MCP and in-process UI agent calls use `compact`. `full` is reserved for explicit debug requests. Both profiles preserve readiness, verifier outcome, trace IDs, and exact spans."
                },
                "maxReferences": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": agent_policy::AGENT_COMPACT_REFERENCE_LIMIT,
                    "description": "Optional compact-profile reference limit (defaults to 8 when omitted). Valid only when responseProfile is `compact`."
                }
            }
        }),
    })
}

pub(crate) async fn call_tool(
    name: &str,
    context: ToolCallContext<'_>,
    arguments: &Value,
) -> Option<McpToolResult> {
    if name != "grounded_answer" {
        return None;
    }
    Some(Box::pin(grounded_answer(context, arguments)).await)
}

async fn grounded_answer(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    let routing = grounded_answer_conversation_routing(context.surface_kind);
    let request = match prepare_grounded_answer_request(arguments) {
        Ok(request) => request,
        Err(error) => return tool_error_result(error),
    };

    // Scope check: the same POLICY_QUERY_RUN the UI handler uses for
    // `create_session` / `create_session_turn`. An MCP token without
    // query_run on the library gets a clean 401-equivalent tool error
    // instead of silently degrading to a stub answer.
    let library_result = match request.library.as_deref() {
        Some(library_ref) => {
            crate::services::mcp::access::load_library_by_catalog_ref(
                context.auth,
                context.state,
                library_ref,
                POLICY_QUERY_RUN,
            )
            .await
        }
        None => {
            crate::services::mcp::access::load_sole_query_authorized_library(
                context.auth,
                context.state,
            )
            .await
        }
    };
    let library = match library_result {
        Ok(library) => library,
        Err(error) => return tool_error_result(error),
    };

    // Transient execution state: `execute_grounded_answer_turn` is
    // conversation-scoped because the grounded-answer pipeline consumes
    // recent turns for coreference resolution. The durable audit
    // record is written separately below; completed tool-created conversation
    // rows use bounded transient storage and never appear in the UI session
    // list, even when the in-process UI agent initiated this call.
    let conversation = match context
        .state
        .application_services
        .query
        .create_conversation(
            context.state,
            CreateConversationCommand {
                workspace_id: library.workspace_id,
                library_id: library.id,
                created_by_principal_id: Some(context.auth.principal_id),
                title: Some(conversation_title(routing.runtime_surface.as_str(), &request.query)),
                request_surface: routing.storage_surface.to_string(),
            },
        )
        .await
    {
        Ok(conversation) => conversation,
        Err(error) => return tool_error_result(error),
    };

    let outcome = match Box::pin(
        context.state.application_services.query.execute_grounded_answer_turn(
            context.state,
            ExecuteConversationTurnCommand {
                conversation_id: conversation.id,
                author_principal_id: Some(context.auth.principal_id),
                surface_kind: routing.runtime_surface,
                content_text: request.query.clone(),
                external_prior_turns: request.external_prior_turns,
                top_k: request.top_k,
                include_debug: request.include_debug,
            },
        ),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            log_grounded_answer_execution_failure(
                context.request_id,
                library.id,
                conversation.id,
                &error,
            );
            enforce_transient_conversation_retention(&context, library.id, conversation.id).await;
            return tool_error_result(sanitize_grounded_answer_execution_error(error));
        }
    };

    append_grounded_answer_audit(&context, routing, &outcome).await;
    enforce_transient_conversation_retention(&context, library.id, conversation.id).await;
    grounded_answer_result(
        outcome,
        context.request_id,
        request.response_profile,
        request.compact_reference_limit,
    )
}

fn log_grounded_answer_execution_failure(
    request_id: &str,
    library_id: uuid::Uuid,
    conversation_id: uuid::Uuid,
    error: &ApiError,
) {
    tracing::error!(
        request_id,
        %library_id,
        %conversation_id,
        error_kind = error.kind(),
        error = %error,
        "grounded_answer execution failed"
    );
}

async fn append_grounded_answer_audit(
    context: &ToolCallContext<'_>,
    routing: GroundedAnswerConversationRouting,
    outcome: &QueryTurnExecutionResult,
) {
    if let Err(error) = context
        .state
        .application_services
        .audit
        .append_query_execution_event(
            context.state,
            AppendQueryExecutionAuditCommand {
                actor_principal_id: context.auth.principal_id,
                surface_kind: routing.runtime_surface.as_str().to_string(),
                request_id: Some(context.request_id.to_string()),
                query_session_id: outcome.conversation.id,
                query_execution_id: outcome.execution.id,
                runtime_execution_id: Some(outcome.execution.runtime_execution_id),
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

fn grounded_answer_result(
    outcome: QueryTurnExecutionResult,
    request_id: &str,
    response_profile: McpGroundedAnswerResponseProfile,
    compact_reference_limit: Option<usize>,
) -> McpToolResult {
    let answer_text =
        outcome.response_turn.as_ref().map(|turn| turn.content_text.clone()).unwrap_or_default();

    let Some(query_ir) = outcome.query_ir.as_ref() else {
        tracing::error!(
            request_id,
            query_execution_id = %outcome.execution.id,
            "grounded answer completed without compiled typed query IR"
        );
        return tool_error_result(ApiError::Internal);
    };
    let completion = AnswerCompletionContract::from_query_ir(query_ir).evaluate(&answer_text);
    let query_language = query_ir.language;
    let execution_detail = crate::interfaces::http::query::map_turn_execution_response(outcome);

    let mut result = grounded_answer_tool_result_with_profile_and_completion(
        &answer_text,
        &execution_detail,
        response_profile,
        compact_reference_limit,
        &completion,
    );
    attach_grounded_answer_query_language(&mut result, query_language);
    result
}

fn attach_grounded_answer_query_language(result: &mut McpToolResult, language: QueryLanguage) {
    if let Some(structured_content) = result.structured_content.as_object_mut() {
        structured_content.insert("queryLanguage".to_string(), json!(language));
    }
}

async fn enforce_transient_conversation_retention(
    context: &ToolCallContext<'_>,
    library_id: uuid::Uuid,
    protected_conversation_id: uuid::Uuid,
) {
    match context
        .state
        .application_services
        .query
        .enforce_transient_conversation_retention(
            context.state,
            library_id,
            protected_conversation_id,
        )
        .await
    {
        Ok(deleted_count) if deleted_count > 0 => {
            log_transient_conversations_pruned(library_id, deleted_count);
        }
        Ok(_) => {}
        Err(error) => log_transient_conversation_retention_failure(
            library_id,
            protected_conversation_id,
            &error,
        ),
    }
}

fn log_transient_conversations_pruned(library_id: uuid::Uuid, deleted_count: u64) {
    tracing::debug!(
        stage = "transient_tool_conversation_retention",
        %library_id,
        deleted_count,
        "pruned completed transient tool conversation state"
    );
}

fn log_transient_conversation_retention_failure(
    library_id: uuid::Uuid,
    protected_conversation_id: uuid::Uuid,
    error: &ApiError,
) {
    tracing::warn!(
        stage = "transient_tool_conversation_retention",
        %library_id,
        %protected_conversation_id,
        error_kind = error.kind(),
        "completed transient tool conversation retention enforcement failed"
    );
}

pub(crate) fn resolve_grounded_answer_top_k(
    requested_top_k: Option<usize>,
    has_contextual_turns: bool,
) -> usize {
    resolve_contextual_grounded_answer_top_k(requested_top_k, has_contextual_turns, MAX_TOP_K)
}

fn conversation_title(surface_kind: &str, query: &str) -> String {
    // Keep tool-created conversations visually distinct from ordinary
    // user sessions while preserving the real request surface.
    let prefix = format!("[{}]", surface_kind.to_ascii_uppercase());
    let trimmed = query.trim();
    let title = if trimmed.is_empty() {
        format!("{prefix} grounded_answer")
    } else {
        format!("{prefix} {trimmed}")
    };
    title.chars().take(QUERY_CONVERSATION_TITLE_LIMIT).collect()
}

/// Convert execution-stage failures into a stable, non-sensitive MCP error.
///
/// Query execution errors may contain the verbatim question, SQL context, or
/// an upstream provider chain. The detailed error is logged at the call site;
/// external callers receive only a bounded error kind and message. Known
/// retryable deadline, projection, provider, binding, and retrieval failures
/// remain distinguishable through stable safe kinds; every other execution
/// failure collapses to the generic redacted boundary.
fn sanitize_grounded_answer_execution_error(error: ApiError) -> ApiError {
    match error {
        ApiError::ServiceUnavailable { kind: "document_projection_converging", .. } => {
            ApiError::service_unavailable(
                "document projection is still updating; retry shortly",
                "document_projection_converging",
            )
        }
        ApiError::GatewayTimeout { .. } => ApiError::query_deadline_exceeded(),
        ApiError::ServiceUnavailable { kind: "query_provider_unavailable", .. }
        | ApiError::ProviderFailure(_) => ApiError::service_unavailable(
            "query provider is temporarily unavailable; retry shortly",
            "query_provider_unavailable",
        ),
        ApiError::ServiceUnavailable {
            kind: "query_binding_unavailable" | "query_binding_not_configured",
            ..
        } => ApiError::service_unavailable(
            "query AI binding is unavailable; retry after configuration is restored",
            "query_binding_unavailable",
        ),
        ApiError::ServiceUnavailable {
            kind: "query_retrieval_unavailable" | "query_dependency_unavailable",
            ..
        } => ApiError::service_unavailable(
            "query retrieval is temporarily unavailable; retry shortly",
            "query_retrieval_unavailable",
        ),
        _ => ApiError::service_unavailable(
            "grounded answer execution failed",
            "query_execution_failed",
        ),
    }
}

#[cfg(test)]
mod tests;
