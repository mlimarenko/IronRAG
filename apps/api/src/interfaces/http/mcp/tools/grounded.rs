//! `grounded_answer` - canonical MCP answer tool that runs `IronRAG`'s
//! grounded-answer pipeline for one library question.
//!
//! The implementation is deliberately a thin translator over the
//! canonical query service (`state.canonical_services.query`). The
//! handler creates an ephemeral conversation, delegates to
//! `execute_grounded_answer_turn`, and reshapes the result into the MCP tool-call
//! payload. The web UI assistant calls this through the in-process MCP
//! dispatcher when its model chooses `grounded_answer`; external clients
//! call the same tool contract through JSON-RPC.
//!
//! Phase 1 scope:
//!   - input: optional `library`, `query`, optional `conversationTurns`,
//!     optional `topK`, optional `includeDebug`
//!   - output: grounded answer text plus the canonical
//!     `AssistantExecutionDetail`, `runtimeExecutionId`,
//!     `conversationId`, `executionId`

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
                ExternalConversationTurn, QUERY_CONVERSATION_TITLE_LIMIT,
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
    let mut parsed: GroundedAnswerArgs = match serde_json::from_value(arguments.clone()) {
        Ok(parsed) => parsed,
        Err(error) => {
            return tool_error_result(ApiError::invalid_mcp_tool_call(format!(
                "invalid grounded_answer arguments: {error}"
            )));
        }
    };
    if let Err(error) = validate_and_normalize_grounded_answer_args(&mut parsed) {
        return tool_error_result(error);
    }
    let has_contextual_turns =
        parsed.conversation_turns.as_ref().is_some_and(|turns| !turns.is_empty());
    let response_profile =
        resolve_grounded_answer_response_profile(parsed.include_debug, parsed.response_profile);
    let compact_reference_limit =
        match resolve_compact_reference_limit(response_profile, parsed.max_references) {
            Ok(limit) => limit,
            Err(error) => return tool_error_result(error),
        };
    let external_prior_turns = match normalize_external_prior_turns(parsed.conversation_turns) {
        Ok(turns) => turns,
        Err(error) => return tool_error_result(error),
    };

    // Scope check: the same POLICY_QUERY_RUN the UI handler uses for
    // `create_session` / `create_session_turn`. An MCP token without
    // query_run on the library gets a clean 401-equivalent tool error
    // instead of silently degrading to a stub answer.
    let library_result = match parsed.library.as_deref() {
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
    // recent turns for coreference resolution. The canonical durable audit
    // record is written separately below; completed tool-created conversation
    // rows use bounded transient storage and never appear in the UI session
    // list, even when the in-process UI agent initiated this call.
    let conversation = match context
        .state
        .canonical_services
        .query
        .create_conversation(
            context.state,
            CreateConversationCommand {
                workspace_id: library.workspace_id,
                library_id: library.id,
                created_by_principal_id: Some(context.auth.principal_id),
                title: Some(conversation_title(routing.runtime_surface.as_str(), &parsed.query)),
                request_surface: routing.storage_surface.to_string(),
            },
        )
        .await
    {
        Ok(conversation) => conversation,
        Err(error) => return tool_error_result(error),
    };

    let outcome = match Box::pin(
        context.state.canonical_services.query.execute_grounded_answer_turn(
            context.state,
            ExecuteConversationTurnCommand {
                conversation_id: conversation.id,
                author_principal_id: Some(context.auth.principal_id),
                surface_kind: routing.runtime_surface,
                content_text: parsed.query.clone(),
                external_prior_turns,
                top_k: resolve_grounded_answer_top_k(parsed.top_k, has_contextual_turns),
                include_debug: parsed.include_debug.unwrap_or(false),
            },
        ),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::error!(
                request_id = context.request_id,
                library_id = %library.id,
                conversation_id = %conversation.id,
                error_kind = error.kind(),
                "grounded_answer execution failed"
            );
            enforce_transient_conversation_retention(&context, library.id, conversation.id).await;
            return tool_error_result(sanitize_grounded_answer_execution_error(error));
        }
    };

    if let Err(error) = context
        .state
        .canonical_services
        .audit
        .append_query_execution_event(
            context.state,
            AppendQueryExecutionAuditCommand {
                actor_principal_id: context.auth.principal_id,
                surface_kind: routing.runtime_surface.as_str().to_string(),
                request_id: Some(context.request_id.to_string()),
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

    enforce_transient_conversation_retention(&context, library.id, conversation.id).await;

    let answer_text =
        outcome.response_turn.as_ref().map(|turn| turn.content_text.clone()).unwrap_or_default();

    let Some(query_ir) = outcome.query_ir.as_ref() else {
        tracing::error!(
            request_id = context.request_id,
            query_execution_id = %outcome.execution.id,
            "grounded answer completed without canonical typed query IR"
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
        .canonical_services
        .query
        .enforce_transient_conversation_retention(
            context.state,
            library_id,
            protected_conversation_id,
        )
        .await
    {
        Ok(deleted_count) if deleted_count > 0 => tracing::debug!(
            stage = "transient_tool_conversation_retention",
            %library_id,
            deleted_count,
            "pruned completed transient tool conversation state"
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(
            stage = "transient_tool_conversation_retention",
            %library_id,
            %protected_conversation_id,
            error_kind = error.kind(),
            "completed transient tool conversation retention enforcement failed"
        ),
    }
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
        ApiError::ServiceUnavailable {
            kind: "query_content_projection_converging" | "document_projection_converging",
            ..
        } => ApiError::service_unavailable(
            "library content is converging; retry shortly",
            "query_content_projection_converging",
        ),
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

#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GroundedAnswerArgs {
    library: Option<String>,
    query: String,
    conversation_turns: Option<Vec<GroundedAnswerConversationTurn>>,
    top_k: Option<usize>,
    include_debug: Option<bool>,
    response_profile: Option<McpGroundedAnswerResponseProfile>,
    max_references: Option<usize>,
}

fn validate_and_normalize_grounded_answer_args(
    parsed: &mut GroundedAnswerArgs,
) -> Result<(), ApiError> {
    if let Some(library) = parsed.library.as_mut() {
        *library = library.trim().to_string();
        validate_non_empty_bounded_text("library", library, MCP_GROUNDED_MAX_LIBRARY_CHARS)?;
    }
    parsed.query = parsed.query.trim().to_string();
    validate_non_empty_bounded_text("query", &parsed.query, MCP_GROUNDED_MAX_QUERY_CHARS)?;
    if parsed.top_k.is_some_and(|top_k| !(1..=MAX_TOP_K).contains(&top_k)) {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "topK must be between 1 and {MAX_TOP_K}",
        )));
    }
    if parsed
        .conversation_turns
        .as_ref()
        .is_some_and(|turns| turns.len() > MCP_GROUNDED_MAX_CONVERSATION_TURNS)
    {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "conversationTurns must contain at most {MCP_GROUNDED_MAX_CONVERSATION_TURNS} items",
        )));
    }
    if let Some(turns) = parsed.conversation_turns.as_mut() {
        for (index, turn) in turns.iter_mut().enumerate() {
            turn.content = turn.content.trim().to_string();
            validate_non_empty_bounded_text(
                &format!("conversationTurns[{index}].content"),
                &turn.content,
                MCP_GROUNDED_MAX_CONVERSATION_TURN_CHARS,
            )?;
        }
    }
    if parsed.include_debug == Some(true)
        && parsed.response_profile == Some(McpGroundedAnswerResponseProfile::Compact)
    {
        return Err(ApiError::invalid_mcp_tool_call(
            "includeDebug=true requires responseProfile=full or an omitted responseProfile",
        ));
    }
    Ok(())
}

fn validate_non_empty_bounded_text(
    field: &str,
    value: &str,
    max_chars: usize,
) -> Result<(), ApiError> {
    let char_count = value.chars().count();
    if char_count == 0 {
        return Err(ApiError::invalid_mcp_tool_call(format!("{field} must not be empty")));
    }
    if char_count > max_chars {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "{field} must contain at most {max_chars} characters",
        )));
    }
    Ok(())
}

fn resolve_compact_reference_limit(
    response_profile: McpGroundedAnswerResponseProfile,
    requested_limit: Option<usize>,
) -> Result<Option<usize>, ApiError> {
    match (response_profile, requested_limit) {
        (McpGroundedAnswerResponseProfile::Full, None) => Ok(None),
        (McpGroundedAnswerResponseProfile::Full, Some(_)) => Err(ApiError::invalid_mcp_tool_call(
            "maxReferences is valid only when responseProfile is compact",
        )),
        (McpGroundedAnswerResponseProfile::Compact, None) => {
            Ok(Some(MCP_COMPACT_DEFAULT_REFERENCES))
        }
        (McpGroundedAnswerResponseProfile::Compact, Some(limit))
            if (1..=agent_policy::AGENT_COMPACT_REFERENCE_LIMIT).contains(&limit) =>
        {
            Ok(Some(limit))
        }
        (McpGroundedAnswerResponseProfile::Compact, Some(_)) => {
            Err(ApiError::invalid_mcp_tool_call(format!(
                "maxReferences must be between 1 and {}",
                agent_policy::AGENT_COMPACT_REFERENCE_LIMIT,
            )))
        }
    }
}

fn resolve_grounded_answer_response_profile(
    include_debug: Option<bool>,
    requested: Option<McpGroundedAnswerResponseProfile>,
) -> McpGroundedAnswerResponseProfile {
    if include_debug == Some(true) {
        McpGroundedAnswerResponseProfile::Full
    } else {
        requested.unwrap_or(McpGroundedAnswerResponseProfile::Compact)
    }
}

#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GroundedAnswerConversationTurn {
    role: GroundedAnswerConversationTurnRole,
    content: String,
}

#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
enum GroundedAnswerConversationTurnRole {
    User,
    Assistant,
}

fn normalize_external_prior_turns(
    turns: Option<Vec<GroundedAnswerConversationTurn>>,
) -> Result<Vec<ExternalConversationTurn>, ApiError> {
    turns
        .unwrap_or_default()
        .into_iter()
        .map(|turn| {
            let content_text = turn.content.trim().to_string();
            if content_text.is_empty() {
                return Err(ApiError::invalid_mcp_tool_call(
                    "invalid grounded_answer arguments: conversationTurns.content must not be empty"
                        .to_string(),
                ));
            }
            let turn_kind = match turn.role {
                GroundedAnswerConversationTurnRole::User => QueryTurnKind::User,
                GroundedAnswerConversationTurnRole::Assistant => QueryTurnKind::Assistant,
            };
            Ok(ExternalConversationTurn { turn_kind, content_text })
        })
        .collect()
}

// --- grounded_answer contract payload / response construction --------
//
// Split out of the former `interfaces/http/mcp.rs` god-file (plan
// §6.4): this is `grounded_answer`-specific text and contract-payload
// construction that had ended up living in the transport file instead
// of alongside the tool it belongs to. `grounded_answer_contract_payload`,
// `_with_profile`, and `_for_query_ir` are re-exported at
// `crate::interfaces::http::mcp` (see `mcp.rs`) since an external
// integration test (`tests/mcp_grounded_answer_contract.rs`) imports
// them at that path — the move keeps that public contract stable.

const GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT: usize = 128;
const GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS: usize = 240;
const GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT: usize = 24;
const GROUNDED_ANSWER_GRAPH_PRESERVE_MAX_RANK: i32 = 32;

/// Builds the JSON-serializable MCP `grounded_answer` tool result from
/// the same assistant execution detail returned by the UI query API.
///
/// The live MCP handler calls `grounded_answer_tool_result` directly.
/// This public JSON form gives integration tests a DB-free contract path
/// for snapshotting the MCP wrapper without duplicating the production
/// serializer. It is a test contract surface, not a stable application API.
#[doc(hidden)]
#[must_use]
pub fn grounded_answer_contract_payload(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> Value {
    json!(grounded_answer_tool_result(answer_text, execution_detail))
}

/// DB-free contract helper for the optional grounded-answer response profile.
/// Production callers validate `max_references` before reaching the serializer;
/// this helper defensively applies the public compact-profile bounds.
#[doc(hidden)]
#[must_use]
pub fn grounded_answer_contract_payload_with_profile(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    response_profile: McpGroundedAnswerResponseProfile,
    max_references: Option<usize>,
) -> Value {
    let compact_reference_limit = match response_profile {
        McpGroundedAnswerResponseProfile::Full => None,
        McpGroundedAnswerResponseProfile::Compact => Some(
            max_references
                .unwrap_or(MCP_COMPACT_DEFAULT_REFERENCES)
                .clamp(1, MCP_COMPACT_MAX_REFERENCES),
        ),
    };
    json!(grounded_answer_tool_result_with_profile(
        answer_text,
        execution_detail,
        response_profile,
        compact_reference_limit,
    ))
}

/// DB-free contract helper that applies the same typed completion policy as
/// the live grounded-answer MCP handler.
#[doc(hidden)]
#[must_use]
pub fn grounded_answer_contract_payload_for_query_ir(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    query_ir: &QueryIR,
    response_profile: McpGroundedAnswerResponseProfile,
    max_references: Option<usize>,
) -> Value {
    let compact_reference_limit = match response_profile {
        McpGroundedAnswerResponseProfile::Full => None,
        McpGroundedAnswerResponseProfile::Compact => Some(
            max_references
                .unwrap_or(MCP_COMPACT_DEFAULT_REFERENCES)
                .clamp(1, MCP_COMPACT_MAX_REFERENCES),
        ),
    };
    let completion = AnswerCompletionContract::from_query_ir(query_ir).evaluate(answer_text);
    let mut result = grounded_answer_tool_result_with_profile_and_completion(
        answer_text,
        execution_detail,
        response_profile,
        compact_reference_limit,
        &completion,
    );
    if let Some(structured_content) = result.structured_content.as_object_mut() {
        structured_content.insert("queryLanguage".to_string(), json!(query_ir.language));
    }
    json!(result)
}

pub(crate) fn grounded_answer_tool_result(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> McpToolResult {
    grounded_answer_tool_result_with_profile(
        answer_text,
        execution_detail,
        McpGroundedAnswerResponseProfile::Full,
        None,
    )
}

pub(crate) fn grounded_answer_tool_result_with_profile(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    response_profile: McpGroundedAnswerResponseProfile,
    compact_reference_limit: Option<usize>,
) -> McpToolResult {
    grounded_answer_tool_result_with_profile_and_completion(
        answer_text,
        execution_detail,
        response_profile,
        compact_reference_limit,
        &AnswerCompletionAssessment::complete(),
    )
}

pub(crate) fn grounded_answer_tool_result_with_profile_and_completion(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    response_profile: McpGroundedAnswerResponseProfile,
    compact_reference_limit: Option<usize>,
    completion: &AnswerCompletionAssessment,
) -> McpToolResult {
    ok_tool_result(
        &grounded_answer_human_text(answer_text),
        grounded_answer_structured_content(
            answer_text,
            execution_detail,
            response_profile,
            compact_reference_limit,
            completion,
        ),
    )
}

fn grounded_answer_human_text(answer_text: &str) -> String {
    if answer_text.is_empty() {
        "The grounded-answer pipeline returned no answer text (execution may have failed or degraded). Inspect runtimeExecutionId via get_runtime_execution_trace for details.".to_string()
    } else {
        answer_text.to_string()
    }
}

fn grounded_answer_structured_content(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    response_profile: McpGroundedAnswerResponseProfile,
    compact_reference_limit: Option<usize>,
    completion: &AnswerCompletionAssessment,
) -> Value {
    let envelope = GroundedAnswerCompletionEnvelope::new(
        execution_detail.answer_disposition,
        answer_text,
        completion.clone(),
        &execution_detail.execution.lifecycle_state,
        execution_detail.execution.failure_code.clone(),
    );
    match response_profile {
        McpGroundedAnswerResponseProfile::Full => {
            grounded_answer_full_structured_content(answer_text, execution_detail, &envelope)
        }
        McpGroundedAnswerResponseProfile::Compact => grounded_answer_compact_structured_content(
            answer_text,
            execution_detail,
            compact_reference_limit
                .unwrap_or(MCP_COMPACT_DEFAULT_REFERENCES)
                .clamp(1, MCP_COMPACT_MAX_REFERENCES),
            &envelope,
        ),
    }
}

fn grounded_answer_full_structured_content(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    envelope: &GroundedAnswerCompletionEnvelope,
) -> Value {
    let mut sanitized_execution_detail = json!(execution_detail);
    if let Some(references) = sanitized_execution_detail
        .get_mut("preparedSegmentReferences")
        .and_then(Value::as_array_mut)
    {
        for reference in references {
            if let Some(object) = reference.as_object_mut() {
                object.remove("sourceUri");
                object.remove("sourceAccess");
            }
        }
    }
    let clarification = &execution_detail.clarification;
    json!({
        "answerBody": answer_text,
        "responseProfile": "full",
        "executionDetail": sanitized_execution_detail,
        "finalAnswerReady": envelope.final_answer_ready,
        "finalizable": envelope.finalizable,
        "completion": &envelope.completion,
        "repairPolicy": &envelope.repair_policy,
        "readiness": &envelope.readiness,
        "mustPreserveSpans": grounded_answer_must_preserve_spans(
            answer_text,
            execution_detail,
            envelope.finalizable,
        ),
        // Typed disambiguation surface for agent callers: the prose answer
        // body stays authoritative, while `answerCandidates` lets an agent
        // route a follow-up without parsing the clarification tail. Empty
        // for unambiguous answers. `clarification` reports whether this turn
        // asked the user to choose and the question it asked.
        "answerCandidates": clarification.answer_candidates,
        "clarification": json!({
            "required": clarification.required,
            "question": clarification.question,
        }),
        "runtimeExecutionId": execution_detail.execution.runtime_execution_id,
        "executionId": execution_detail.execution.id,
        "conversationId": execution_detail.execution.conversation_id,
        "libraryId": execution_detail.execution.library_id,
        "workspaceId": execution_detail.execution.workspace_id,
        "lifecycleState": execution_detail.execution.lifecycle_state,
    })
}

const GROUNDED_ANSWER_COMPACT_WARNING_LIMIT: usize = 16;
const GROUNDED_ANSWER_COMPACT_PRESERVE_SPAN_LIMIT: usize = 16;
const GROUNDED_ANSWER_COMPACT_CANDIDATE_LIMIT: usize = 8;
const GROUNDED_ANSWER_COMPACT_TEXT_MAX_CHARS: usize = 240;

fn grounded_answer_compact_structured_content(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    reference_limit: usize,
    envelope: &GroundedAnswerCompletionEnvelope,
) -> Value {
    let clarification = &execution_detail.clarification;
    let (total_reference_count, references) =
        grounded_answer_compact_references(execution_detail, reference_limit);
    let warning_count = execution_detail.verification_warnings.len();
    let warnings = execution_detail
        .verification_warnings
        .iter()
        .take(GROUNDED_ANSWER_COMPACT_WARNING_LIMIT)
        .map(|warning| {
            json!({
                "code": warning.code,
                "message": grounded_answer_compact_text(&warning.message),
                "relatedSegmentId": warning.related_segment_id,
                "relatedFactId": warning.related_fact_id,
            })
        })
        .collect::<Vec<_>>();
    let preserve_spans =
        grounded_answer_must_preserve_spans(answer_text, execution_detail, envelope.finalizable);
    let preserve_span_count = preserve_spans.len();
    let bounded_preserve_spans = preserve_spans
        .into_iter()
        .take(GROUNDED_ANSWER_COMPACT_PRESERVE_SPAN_LIMIT)
        .collect::<Vec<_>>();
    let answer_candidate_count = clarification.answer_candidates.len();
    let answer_candidates = clarification
        .answer_candidates
        .iter()
        .take(GROUNDED_ANSWER_COMPACT_CANDIDATE_LIMIT)
        .collect::<Vec<_>>();

    json!({
        "answerBody": answer_text,
        "responseProfile": "compact",
        "finalAnswerReady": envelope.final_answer_ready,
        "finalizable": envelope.finalizable,
        "completion": &envelope.completion,
        "repairPolicy": &envelope.repair_policy,
        "readiness": &envelope.readiness,
        "verifier": {
            "state": execution_detail.verification_state,
            "warningCount": warning_count,
            "returnedWarningCount": warnings.len(),
            "warningsTruncated": warning_count > warnings.len(),
        },
        "warnings": warnings,
        "referenceSummary": {
            "totalCount": total_reference_count,
            "returnedCount": references.len(),
            "truncated": total_reference_count > references.len(),
            "references": references,
        },
        "preserveSpanSummary": {
            "totalCount": preserve_span_count,
            "returnedCount": bounded_preserve_spans.len(),
            "truncated": preserve_span_count > bounded_preserve_spans.len(),
        },
        "mustPreserveSpans": bounded_preserve_spans,
        "answerCandidateSummary": {
            "totalCount": answer_candidate_count,
            "returnedCount": answer_candidates.len(),
            "truncated": answer_candidate_count > answer_candidates.len(),
            "candidates": answer_candidates,
        },
        "clarification": {
            "required": clarification.required,
            "question": clarification.question,
        },
        "runtimeExecutionId": execution_detail.execution.runtime_execution_id,
        "executionId": execution_detail.execution.id,
        "conversationId": execution_detail.execution.conversation_id,
        "libraryId": execution_detail.execution.library_id,
        "workspaceId": execution_detail.execution.workspace_id,
        "lifecycleState": execution_detail.execution.lifecycle_state,
    })
}

fn grounded_answer_compact_references(
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    limit: usize,
) -> (usize, Vec<Value>) {
    let total_count = execution_detail
        .chunk_references
        .len()
        .saturating_add(execution_detail.prepared_segment_references.len())
        .saturating_add(execution_detail.technical_fact_references.len())
        .saturating_add(execution_detail.entity_references.len())
        .saturating_add(execution_detail.relation_references.len());
    let mut ranked = Vec::with_capacity(total_count.min(limit.saturating_mul(2)));

    ranked.extend(execution_detail.chunk_references.iter().map(|reference| {
        (
            reference.rank,
            0_u8,
            json!({
                "kind": "chunk",
                "chunkId": reference.chunk_id,
                "rank": reference.rank,
                "score": reference.score,
            }),
        )
    }));
    ranked.extend(execution_detail.prepared_segment_references.iter().map(|reference| {
        (
            reference.rank,
            1_u8,
            json!({
                "kind": "prepared_segment",
                "segmentId": reference.segment_id,
                "revisionId": reference.revision_id,
                "documentId": reference.document_id,
                "documentTitle": reference
                    .document_title
                    .as_deref()
                    .map(grounded_answer_compact_text),
                "blockKind": reference.block_kind,
                "rank": reference.rank,
                "score": reference.score,
            }),
        )
    }));
    ranked.extend(execution_detail.technical_fact_references.iter().map(|reference| {
        (
            reference.rank,
            2_u8,
            json!({
                "kind": "technical_fact",
                "factId": reference.fact_id,
                "revisionId": reference.revision_id,
                "factKind": reference.fact_kind,
                "displayValue": grounded_answer_compact_text(&reference.display_value),
                "rank": reference.rank,
                "score": reference.score,
            }),
        )
    }));
    ranked.extend(execution_detail.entity_references.iter().map(|reference| {
        (
            reference.rank,
            3_u8,
            json!({
                "kind": "entity",
                "nodeId": reference.node_id,
                "label": grounded_answer_compact_text(&reference.label),
                "entityType": reference.entity_type,
                "rank": reference.rank,
                "score": reference.score,
            }),
        )
    }));
    ranked.extend(execution_detail.relation_references.iter().map(|reference| {
        (
            reference.rank,
            4_u8,
            json!({
                "kind": "relation",
                "edgeId": reference.edge_id,
                "predicate": grounded_answer_compact_text(&reference.predicate),
                "assertion": reference
                    .normalized_assertion
                    .as_deref()
                    .map(grounded_answer_compact_text),
                "rank": reference.rank,
                "score": reference.score,
            }),
        )
    }));
    ranked.sort_by_key(|(rank, kind_order, _)| (*rank, *kind_order));

    (total_count, ranked.into_iter().take(limit).map(|(_, _, reference)| reference).collect())
}

fn grounded_answer_compact_text(value: &str) -> String {
    let mut chars = value.trim().chars();
    let mut compact =
        chars.by_ref().take(GROUNDED_ANSWER_COMPACT_TEXT_MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        compact.push('…');
    }
    compact
}

fn grounded_answer_must_preserve_spans(
    answer_text: &str,
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
    include_source_titles: bool,
) -> Vec<String> {
    let graph_spans = if include_source_titles {
        grounded_answer_graph_preserve_span_candidates(execution_detail)
    } else {
        Default::default()
    };
    let source_titles = include_source_titles.then_some(()).into_iter().flat_map(|()| {
        execution_detail
            .prepared_segment_references
            .iter()
            .filter_map(|reference| reference.document_title.as_deref())
    });
    grounded_answer_must_preserve_spans_for_evidence(answer_text, graph_spans, source_titles)
}

#[cfg(test)]
pub(crate) fn grounded_answer_must_preserve_spans_for_source_titles<'a>(
    answer_text: &str,
    source_titles: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    grounded_answer_must_preserve_spans_for_evidence(answer_text, std::iter::empty(), source_titles)
}

pub(crate) fn grounded_answer_must_preserve_spans_for_evidence<'a>(
    answer_text: &str,
    graph_spans: impl IntoIterator<Item = &'a str>,
    source_titles: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut spans = Vec::new();
    let mut seen = BTreeSet::new();
    for span in backtick_literal_spans(answer_text) {
        push_grounded_answer_preserve_span(&mut spans, &mut seen, &span);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    for span in adjacent_code_span_assignments(answer_text) {
        push_grounded_answer_preserve_candidate(&mut spans, &mut seen, &span);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    for graph_span in graph_spans {
        push_grounded_answer_preserve_evidence_span(&mut spans, &mut seen, graph_span);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    for title in source_titles {
        push_grounded_answer_preserve_source_title(&mut spans, &mut seen, title);
        if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
            break;
        }
    }
    spans
}

fn grounded_answer_graph_preserve_span_candidates(
    execution_detail: &ironrag_contracts::assistant::AssistantExecutionDetail,
) -> Vec<&str> {
    let mut candidates = Vec::new();
    for relation in execution_detail.relation_references.iter().filter(|reference| {
        reference.rank > 0 && reference.rank <= GROUNDED_ANSWER_GRAPH_PRESERVE_MAX_RANK
    }) {
        if let Some(assertion) = relation.normalized_assertion.as_deref() {
            candidates.push(assertion);
        }
        if candidates.len() >= GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT {
            return candidates;
        }
    }
    for entity in execution_detail.entity_references.iter().filter(|reference| {
        reference.rank > 0 && reference.rank <= GROUNDED_ANSWER_GRAPH_PRESERVE_MAX_RANK
    }) {
        candidates.push(entity.label.as_str());
        if let Some(summary) = entity.summary.as_deref() {
            candidates.push(summary);
        }
        if candidates.len() >= GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT {
            candidates.truncate(GROUNDED_ANSWER_GRAPH_PRESERVE_SPAN_LIMIT);
            return candidates;
        }
    }
    candidates
}

fn push_grounded_answer_preserve_evidence_span(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    evidence: &str,
) {
    if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
        return;
    }
    let evidence = evidence.trim();
    if evidence.is_empty()
        || evidence.chars().count() > GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS
        || !evidence.chars().any(char::is_alphanumeric)
    {
        return;
    }
    if seen.insert(evidence.to_string()) {
        spans.push(evidence.to_string());
    }
}

fn push_grounded_answer_preserve_source_title(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    title: &str,
) {
    if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
        return;
    }
    let title = title.trim();
    if title.is_empty()
        || title.chars().count() > GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS
        || !title.chars().any(char::is_alphanumeric)
    {
        return;
    }
    if seen.insert(title.to_string()) {
        spans.push(title.to_string());
    }
}

fn push_grounded_answer_preserve_span(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    span: &str,
) {
    let span = span.trim();
    if span.is_empty() {
        return;
    }
    if span.contains('\n') {
        let mut lines = span.lines();
        if let Some(first_line) = lines.next()
            && !is_probable_code_fence_info(first_line)
        {
            push_grounded_answer_preserve_line(spans, seen, first_line);
        }
        for line in lines {
            push_grounded_answer_preserve_line(spans, seen, line);
            if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
                break;
            }
        }
        return;
    }
    push_grounded_answer_preserve_candidate(spans, seen, span);
}

fn push_grounded_answer_preserve_line(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    line: &str,
) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    push_grounded_answer_preserve_candidate(spans, seen, line);
    if let Some((left, right)) = line.split_once('=') {
        push_grounded_answer_preserve_candidate(spans, seen, left.trim());
        push_grounded_answer_preserve_candidate(spans, seen, right.trim());
    }
}

fn push_grounded_answer_preserve_candidate(
    spans: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    candidate: &str,
) {
    if spans.len() >= GROUNDED_ANSWER_MUST_PRESERVE_SPAN_LIMIT {
        return;
    }
    let candidate = candidate.trim();
    if !is_grounded_answer_preserve_candidate(candidate) {
        return;
    }
    if seen.insert(candidate.to_string()) {
        spans.push(candidate.to_string());
    }
}

fn is_grounded_answer_preserve_candidate(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty()
        || candidate.chars().count() > GROUNDED_ANSWER_MUST_PRESERVE_SPAN_MAX_CHARS
        || !candidate.chars().any(char::is_alphanumeric)
    {
        return false;
    }
    if candidate.starts_with('/') || candidate.starts_with('\\') {
        return true;
    }
    if candidate.contains('/') || candidate.contains('\\') || candidate.contains("://") {
        return true;
    }
    if candidate.contains('=') {
        return candidate.split_once('=').is_some_and(|(left, right)| {
            !left.trim().is_empty()
                && !right.trim().is_empty()
                && left.trim().chars().any(char::is_alphanumeric)
        });
    }
    let unwrapped =
        candidate.trim_matches('[').trim_matches(']').trim_matches('`').trim_matches('"');
    literal_text_is_identifier_shaped(unwrapped) || is_plain_code_span(unwrapped)
}

fn is_plain_code_span(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate.chars().any(char::is_whitespace) {
        return false;
    }
    let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
    alnum_count >= 2
        && candidate.chars().all(|ch| {
            ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\' | ':' | '=')
        })
}

fn adjacent_code_span_assignments(text: &str) -> Vec<String> {
    let mut assignments = Vec::new();
    let mut seen = BTreeSet::new();
    let mut pending_key: Option<(String, usize)> = None;
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            pending_key = None;
            continue;
        }
        let spans = backtick_literal_spans(line);
        if spans.is_empty() {
            continue;
        }
        let keys = spans
            .iter()
            .map(String::as_str)
            .filter(|span| is_assignment_key_span(span))
            .collect::<Vec<_>>();
        let values = spans
            .iter()
            .map(String::as_str)
            .filter(|span| is_assignment_value_span(span))
            .collect::<Vec<_>>();
        append_pending_code_span_assignment(
            &mut assignments,
            &mut seen,
            pending_key.as_ref(),
            line_index,
            &keys,
            &values,
        );
        pending_key = current_code_span_assignment_key(
            &mut assignments,
            &mut seen,
            line_index,
            &keys,
            &values,
        )
        .or(pending_key);
    }
    assignments
}

fn append_pending_code_span_assignment(
    assignments: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    pending_key: Option<&(String, usize)>,
    line_index: usize,
    keys: &[&str],
    values: &[&str],
) {
    let Some((key, key_line)) = pending_key else {
        return;
    };
    if keys.is_empty() && line_index.saturating_sub(*key_line) <= 6 && values.len() == 1 {
        push_adjacent_code_span_assignment(assignments, seen, key, values[0]);
    }
}

fn current_code_span_assignment_key(
    assignments: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    line_index: usize,
    keys: &[&str],
    values: &[&str],
) -> Option<(String, usize)> {
    let [key] = keys else {
        return None;
    };
    if let [value] = values {
        push_adjacent_code_span_assignment(assignments, seen, key, value);
    }
    Some((key.trim().to_string(), line_index))
}

fn push_adjacent_code_span_assignment(
    assignments: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    key: &str,
    value: &str,
) {
    let assignment = format!("{} = {}", key.trim(), value.trim());
    if seen.insert(assignment.clone()) {
        assignments.push(assignment);
    }
}

fn is_assignment_key_span(span: &str) -> bool {
    let span = span.trim();
    if span.is_empty()
        || is_assignment_value_span(span)
        || span.starts_with('[')
        || span.starts_with('/')
        || span.starts_with('\\')
        || span.contains('/')
        || span.contains('\\')
        || span.contains("://")
        || span.contains('=')
    {
        return false;
    }
    let Some(first) = span.chars().next() else {
        return false;
    };
    first.is_alphabetic()
        && span.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && literal_text_is_identifier_shaped(span)
}

fn is_assignment_value_span(span: &str) -> bool {
    let span = span.trim();
    if span.is_empty() || span.chars().any(char::is_whitespace) {
        return false;
    }
    let lowered = span.to_ascii_lowercase();
    matches!(lowered.as_str(), "true" | "false")
        || span.contains("://")
        || span.starts_with('/')
        || span.starts_with('\\')
        || span.chars().all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+'))
}

fn is_probable_code_fence_info(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty()
        && line.chars().count() <= 32
        && line
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+' | '.' | '#'))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ironrag_contracts::assistant::{
        AssistantAnswerDisposition, AssistantChunkReference, AssistantContentSourceAccess,
        AssistantEntityReference, AssistantExecution, AssistantExecutionDetail,
        AssistantPolicySummary, AssistantPreparedSegmentReference, AssistantRelationReference,
        AssistantRuntimeSummary, AssistantTechnicalFactReference, AssistantVerificationState,
    };
    use uuid::Uuid;

    use crate::domains::query_ir::{
        QueryAct, QueryIR, QueryLanguage, QueryScope, SourceSliceDirection, SourceSliceFilter,
        SourceSliceSpec,
    };

    use super::*;

    #[test]
    fn grounded_answer_carries_the_explicit_typed_query_language() {
        let mut result = McpToolResult {
            content: Vec::new(),
            structured_content: json!({"answerBody": "Synthetic answer."}),
            is_error: false,
        };

        attach_grounded_answer_query_language(&mut result, QueryLanguage::Ru);

        assert_eq!(result.structured_content["queryLanguage"], json!("ru"));
    }

    #[test]
    fn grounded_answer_default_top_k_matches_ui_query_turn_default() {
        let library_ref = "alpha-workspace/adapter-library";
        let query = "Which endpoint does the demo adapter call for inventory sync?";

        let mcp_top_k = resolve_grounded_answer_top_k(None, false);
        let ui_top_k = crate::interfaces::http::query::resolve_query_turn_top_k(None);

        assert_eq!(mcp_top_k, ui_top_k, "top_k drift for library {library_ref} and query {query}");
        assert_eq!(mcp_top_k, DEFAULT_TOP_K);
        assert!(mcp_top_k >= 24);
    }

    #[test]
    fn grounded_answer_explicit_top_k_matches_ui_query_turn() {
        assert_eq!(
            resolve_grounded_answer_top_k(None, false),
            crate::interfaces::http::query::resolve_query_turn_top_k(None)
        );
        assert_eq!(
            resolve_grounded_answer_top_k(Some(6), false),
            crate::interfaces::http::query::resolve_query_turn_top_k(Some(6))
        );
    }

    #[test]
    fn omitted_response_profile_is_compact_for_ui_and_external_mcp() {
        for surface in [
            crate::domains::agent_runtime::RuntimeSurfaceKind::Ui,
            crate::domains::agent_runtime::RuntimeSurfaceKind::Mcp,
        ] {
            let routing = grounded_answer_conversation_routing(surface);
            assert_eq!(routing.runtime_surface, surface);
            assert_eq!(
                resolve_grounded_answer_response_profile(None, None),
                McpGroundedAnswerResponseProfile::Compact,
                "omitted profile drifted for {surface:?}",
            );
        }
        assert_eq!(
            resolve_grounded_answer_response_profile(Some(true), None),
            McpGroundedAnswerResponseProfile::Full,
        );
        assert_eq!(
            resolve_grounded_answer_response_profile(
                None,
                Some(McpGroundedAnswerResponseProfile::Full),
            ),
            McpGroundedAnswerResponseProfile::Full,
        );
    }

    #[test]
    fn grounded_answer_contextual_top_k_floor_matches_ui_agent_tool_default() {
        // A contextual follow-up floors retrieval breadth to the canonical
        // default so it never retrieves more narrowly than a fresh turn.
        assert_eq!(resolve_grounded_answer_top_k(Some(4), true), DEFAULT_TOP_K);
        assert_eq!(resolve_grounded_answer_top_k(Some(4), false), 4);
    }

    #[test]
    fn grounded_answer_descriptor_is_bounded_and_preserves_agent_policy_signals() {
        let descriptor = descriptor("grounded_answer").expect("descriptor");
        assert!(
            descriptor.description.len()
                <= crate::services::mcp::agent_policy::GROUNDED_ANSWER_DESCRIPTION_MAX_BYTES
        );
        for signal in [
            crate::services::mcp::agent_policy::AGENT_POLICY_VERSION,
            "exact current user question",
            "built-in UI dispatches it",
            "responseProfile=compact",
            "maxReferences<=8",
            "finalAnswerReady=true",
            "one exact-query repair",
            "repairPolicy",
            "maxAdditionalGroundedAnswerCalls",
            "clarification.required=true",
            "mustPreserveSpans",
        ] {
            assert!(descriptor.description.contains(signal), "missing descriptor signal: {signal}");
        }
    }

    #[test]
    fn grounded_answer_descriptor_advertises_compact_agent_profile() {
        let descriptor = descriptor("grounded_answer").expect("descriptor");
        let properties = &descriptor.input_schema["properties"];

        assert_eq!(properties["responseProfile"]["enum"], json!(["full", "compact"]));
        assert_eq!(properties["responseProfile"]["default"], json!("compact"));
        assert_eq!(properties["maxReferences"]["minimum"], json!(1));
        assert_eq!(
            properties["maxReferences"]["maximum"],
            json!(crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT)
        );
    }

    #[test]
    fn grounded_answer_descriptor_allows_only_unambiguous_library_inference() {
        let descriptor = descriptor("grounded_answer").expect("descriptor");
        let schema = &descriptor.input_schema;
        let library_description =
            schema["properties"]["library"]["description"].as_str().expect("library description");

        assert_eq!(schema["required"], json!(["query"]));
        assert!(library_description.contains("exactly one"));
        assert!(library_description.contains("query_run"));
        assert!(library_description.contains("zero or multiple"));
    }

    #[test]
    fn grounded_answer_args_accept_omitted_library_and_preserve_explicit_library() {
        let mut inferred = serde_json::from_value::<GroundedAnswerArgs>(json!({
            "query": "question"
        }))
        .expect("omitted library is valid before authorization-aware inference");
        validate_and_normalize_grounded_answer_args(&mut inferred)
            .expect("validate omitted library");
        assert_eq!(inferred.library.as_deref(), None);

        let mut explicit = serde_json::from_value::<GroundedAnswerArgs>(json!({
            "library": "  workspace/library  ",
            "query": "question"
        }))
        .expect("parse explicit library");
        validate_and_normalize_grounded_answer_args(&mut explicit)
            .expect("validate explicit library");
        assert_eq!(explicit.library.as_deref(), Some("workspace/library"));
    }

    #[test]
    fn conversation_title_preserves_actual_surface() {
        assert_eq!(conversation_title("mcp", "  Lookup adapters  "), "[MCP] Lookup adapters");
        assert_eq!(conversation_title("ui", ""), "[UI] grounded_answer");
    }

    #[test]
    fn conversation_title_preserves_exact_domain_limit_for_all_runtime_surfaces() {
        use crate::{
            domains::agent_runtime::RuntimeSurfaceKind,
            services::query::service::QUERY_CONVERSATION_TITLE_LIMIT,
        };

        for surface in [
            RuntimeSurfaceKind::Ui,
            RuntimeSurfaceKind::Rest,
            RuntimeSurfaceKind::Mcp,
            RuntimeSurfaceKind::Worker,
            RuntimeSurfaceKind::Internal,
        ] {
            let prefix = format!("[{}] ", surface.as_str().to_ascii_uppercase());
            let query = "x".repeat(QUERY_CONVERSATION_TITLE_LIMIT - prefix.chars().count());
            let title = conversation_title(surface.as_str(), &query);

            assert_eq!(title.chars().count(), QUERY_CONVERSATION_TITLE_LIMIT);
            assert_eq!(title, format!("{prefix}{query}"));
        }
    }

    #[test]
    fn conversation_title_truncates_long_unicode_within_total_domain_limit() {
        use crate::{
            domains::agent_runtime::RuntimeSurfaceKind,
            services::query::service::QUERY_CONVERSATION_TITLE_LIMIT,
        };

        let query = "🧩".repeat(QUERY_CONVERSATION_TITLE_LIMIT * 2);
        for surface in [
            RuntimeSurfaceKind::Ui,
            RuntimeSurfaceKind::Rest,
            RuntimeSurfaceKind::Mcp,
            RuntimeSurfaceKind::Worker,
            RuntimeSurfaceKind::Internal,
        ] {
            let prefix = format!("[{}] ", surface.as_str().to_ascii_uppercase());
            let visible_query =
                "🧩".repeat(QUERY_CONVERSATION_TITLE_LIMIT - prefix.chars().count());
            let title = conversation_title(surface.as_str(), &query);

            assert_eq!(title.chars().count(), QUERY_CONVERSATION_TITLE_LIMIT);
            assert_eq!(title, format!("{prefix}{visible_query}"));
        }
    }

    #[test]
    fn ui_tool_context_keeps_runtime_surface_but_uses_transient_retention_storage() {
        use crate::domains::agent_runtime::RuntimeSurfaceKind;

        for runtime_surface in [RuntimeSurfaceKind::Ui, RuntimeSurfaceKind::Mcp] {
            let routing = grounded_answer_conversation_routing(runtime_surface);

            assert_eq!(routing.runtime_surface, runtime_surface);
            assert_eq!(routing.storage_surface, RuntimeSurfaceKind::Mcp.as_str());
            assert_ne!(routing.storage_surface, RuntimeSurfaceKind::Ui.as_str());
        }
    }

    #[test]
    fn grounded_answer_execution_error_does_not_expose_internal_context() {
        let private_context = "private user question and database failure chain";
        let result = tool_error_result(sanitize_grounded_answer_execution_error(
            ApiError::InternalMessage(private_context.to_string()),
        ));
        let serialized = serde_json::to_string(&result).expect("serialize MCP error result");

        assert!(result.is_error);
        assert_eq!(result.structured_content["errorKind"], json!("query_execution_failed"));
        assert_eq!(result.structured_content["retryable"], json!(false));
        assert!(result.structured_content.get("retryAfterMs").is_none());
        assert!(result.structured_content.get("repairHint").is_none());
        assert!(!serialized.contains(private_context));
        assert!(serialized.contains("grounded answer execution failed"));
    }

    #[test]
    fn grounded_answer_execution_errors_preserve_safe_retryable_failure_kinds() {
        let private_context = "private question, credential label, and upstream chain";
        let cases = [
            (
                ApiError::service_unavailable(
                    format!("library projection converging: {private_context}"),
                    "query_content_projection_converging",
                ),
                "query_content_projection_converging",
                "library content is converging; retry shortly",
            ),
            (
                ApiError::ProviderFailure(format!("provider request failed: {private_context}")),
                "query_provider_unavailable",
                "query provider is temporarily unavailable; retry shortly",
            ),
            (
                ApiError::service_unavailable(private_context, "query_binding_unavailable"),
                "query_binding_unavailable",
                "query AI binding is unavailable; retry after configuration is restored",
            ),
            (
                ApiError::service_unavailable(private_context, "query_retrieval_unavailable"),
                "query_retrieval_unavailable",
                "query retrieval is temporarily unavailable; retry shortly",
            ),
            (
                ApiError::service_unavailable(private_context, "query_dependency_unavailable"),
                "query_retrieval_unavailable",
                "query retrieval is temporarily unavailable; retry shortly",
            ),
        ];

        for (error, expected_kind, expected_message) in cases {
            let result = tool_error_result(sanitize_grounded_answer_execution_error(error));
            let serialized = serde_json::to_string(&result).expect("serialize MCP error result");

            assert!(result.is_error);
            assert_eq!(result.structured_content["errorKind"], json!(expected_kind));
            assert_eq!(result.structured_content["retryable"], json!(true));
            if expected_kind == "query_binding_unavailable" {
                assert_eq!(result.structured_content["repairHint"], json!("restore_query_binding"));
                assert!(result.structured_content.get("retryAfterMs").is_none());
            } else {
                assert_eq!(result.structured_content["repairHint"], json!("retry_same_request"));
                let retry_after_ms = result.structured_content["retryAfterMs"]
                    .as_u64()
                    .expect("retryable transient error must carry bounded retryAfterMs");
                assert!((1..=5_000).contains(&retry_after_ms));
            }
            assert!(serialized.contains(expected_message), "{serialized}");
            assert!(!serialized.contains(private_context), "{serialized}");
        }
    }

    #[test]
    fn grounded_answer_execution_error_ignores_misleading_untyped_text() {
        for error in [
            ApiError::Conflict(
                "active query compile binding is not configured: opaque".to_string(),
            ),
            ApiError::InternalMessage("all chunk retrieval lanes failed: opaque".to_string()),
        ] {
            let result = tool_error_result(sanitize_grounded_answer_execution_error(error));

            assert_eq!(result.structured_content["errorKind"], json!("query_execution_failed"));
            assert_eq!(result.structured_content["retryable"], json!(false));
        }
    }

    #[test]
    fn structured_content_embeds_canonical_assistant_execution_detail() {
        let execution_id = Uuid::from_u128(1);
        let chunk_id = Uuid::from_u128(2);
        let segment_id = Uuid::from_u128(3);
        let revision_id = Uuid::from_u128(4);
        let fact_id = Uuid::from_u128(5);
        let node_id = Uuid::from_u128(6);
        let edge_id = Uuid::from_u128(7);
        let detail = sample_execution_detail(
            execution_id,
            chunk_id,
            segment_id,
            revision_id,
            fact_id,
            node_id,
            edge_id,
        );

        let answer = "Use `/etc/alpha.ini` with `alphaKey = true`.";
        let structured =
            crate::interfaces::http::mcp::grounded_answer_contract_payload(answer, &detail);
        let structured_content = &structured["structuredContent"];
        let execution_detail = &structured_content["executionDetail"];

        assert_eq!(structured["isError"], json!(false));
        assert_eq!(structured_content.get("citations"), None);
        assert_eq!(structured_content["answerBody"], json!(answer));
        assert_eq!(structured_content["finalAnswerReady"], json!(true));
        assert_eq!(structured_content["finalizable"], json!(true));
        assert_eq!(structured_content["repairPolicy"]["required"], json!(false));
        assert_eq!(
            structured_content["repairPolicy"]["maxAdditionalGroundedAnswerCalls"],
            json!(0)
        );
        assert_eq!(
            structured_content["mustPreserveSpans"],
            json!([
                "/etc/alpha.ini",
                "alphaKey = true",
                "Synthetic API calls endpoint",
                "Synthetic API",
                "Synthetic API node",
                "Synthetic contract"
            ])
        );
        assert_eq!(execution_detail["chunkReferences"][0]["executionId"], json!(execution_id));
        assert_eq!(execution_detail["chunkReferences"][0]["chunkId"], json!(chunk_id));
        assert_eq!(
            execution_detail["preparedSegmentReferences"][0]["executionId"],
            json!(execution_id)
        );
        assert_eq!(
            execution_detail["preparedSegmentReferences"][0]["segmentId"],
            json!(segment_id)
        );
        assert_eq!(
            execution_detail["preparedSegmentReferences"][0]["revisionId"],
            json!(revision_id)
        );
        assert_eq!(
            execution_detail["technicalFactReferences"][0]["executionId"],
            json!(execution_id)
        );
        assert_eq!(execution_detail["technicalFactReferences"][0]["factId"], json!(fact_id));
        assert_eq!(execution_detail["entityReferences"][0]["executionId"], json!(execution_id));
        assert_eq!(execution_detail["entityReferences"][0]["nodeId"], json!(node_id));
        assert_eq!(execution_detail["relationReferences"][0]["executionId"], json!(execution_id));
        assert_eq!(execution_detail["relationReferences"][0]["edgeId"], json!(edge_id));
    }

    #[test]
    fn non_finalizable_structured_content_does_not_promote_source_titles_to_preserve_spans() {
        let mut detail = sample_execution_detail(
            Uuid::from_u128(21),
            Uuid::from_u128(22),
            Uuid::from_u128(23),
            Uuid::from_u128(24),
            Uuid::from_u128(25),
            Uuid::from_u128(26),
            Uuid::from_u128(27),
        );
        detail.verification_state = AssistantVerificationState::Conflicting;
        detail.answer_disposition = AssistantAnswerDisposition::NonTerminal;

        let answer = "Use `/etc/alpha.ini` with `alphaKey = true`.";
        let structured =
            crate::interfaces::http::mcp::grounded_answer_contract_payload(answer, &detail);
        let structured_content = &structured["structuredContent"];

        assert_eq!(structured_content["finalAnswerReady"], json!(false));
        assert_eq!(structured_content["finalizable"], json!(false));
        assert_eq!(
            structured_content["mustPreserveSpans"],
            json!(["/etc/alpha.ini", "alphaKey = true"])
        );
    }

    #[test]
    fn clarification_is_terminal_without_being_advertised_as_a_factual_answer() {
        let mut detail = sample_execution_detail(
            Uuid::from_u128(28),
            Uuid::from_u128(29),
            Uuid::from_u128(30),
            Uuid::from_u128(31),
            Uuid::from_u128(32),
            Uuid::from_u128(33),
            Uuid::from_u128(34),
        );
        detail.clarification.required = true;
        detail.clarification.question = Some("Which documented variant do you mean?".to_string());
        detail.answer_disposition = AssistantAnswerDisposition::Clarification;

        for profile in [
            crate::mcp_types::McpGroundedAnswerResponseProfile::Full,
            crate::mcp_types::McpGroundedAnswerResponseProfile::Compact,
        ] {
            let payload =
                crate::interfaces::http::mcp::grounded_answer_contract_payload_with_profile(
                    "Choose one documented variant.",
                    &detail,
                    profile,
                    Some(3),
                );
            let structured = &payload["structuredContent"];

            assert_eq!(structured["finalAnswerReady"], json!(false));
            assert_eq!(structured["finalizable"], json!(false));
            assert_eq!(structured["readiness"]["clarificationRequired"], json!(true));
            assert_eq!(structured["repairPolicy"]["required"], json!(false));
            assert_eq!(structured["repairPolicy"]["reason"], json!(null));
            assert_eq!(structured["repairPolicy"]["maxAdditionalGroundedAnswerCalls"], json!(0));
        }
    }

    #[test]
    fn empty_answer_is_never_ready_in_either_profile() {
        let detail = sample_execution_detail(
            Uuid::from_u128(81),
            Uuid::from_u128(82),
            Uuid::from_u128(83),
            Uuid::from_u128(84),
            Uuid::from_u128(85),
            Uuid::from_u128(86),
            Uuid::from_u128(87),
        );

        for profile in [
            crate::mcp_types::McpGroundedAnswerResponseProfile::Full,
            crate::mcp_types::McpGroundedAnswerResponseProfile::Compact,
        ] {
            let payload =
                crate::interfaces::http::mcp::grounded_answer_contract_payload_with_profile(
                    "   ",
                    &detail,
                    profile,
                    Some(3),
                );
            let structured = &payload["structuredContent"];

            assert_eq!(structured["finalAnswerReady"], json!(false));
            assert_eq!(structured["finalizable"], json!(false));
            assert_eq!(structured["repairPolicy"]["required"], json!(true));
            assert_eq!(structured["repairPolicy"]["reason"], json!("answer_missing"));
        }
    }

    #[test]
    fn blocking_verifier_warning_is_not_advertised_as_a_final_answer() {
        let mut detail = sample_execution_detail(
            Uuid::from_u128(35),
            Uuid::from_u128(36),
            Uuid::from_u128(37),
            Uuid::from_u128(38),
            Uuid::from_u128(39),
            Uuid::from_u128(40),
            Uuid::from_u128(41),
        );
        detail.verification_warnings.push(
            ironrag_contracts::assistant::AssistantVerificationWarning {
                code: "partial_coverage".to_string(),
                message: "Synthetic coverage warning".to_string(),
                related_segment_id: None,
                related_fact_id: None,
            },
        );
        detail.answer_disposition = AssistantAnswerDisposition::NonTerminal;

        let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload(
            "Only part of the requested inventory is supported.",
            &detail,
        );
        let structured = &payload["structuredContent"];

        assert_eq!(structured["finalAnswerReady"], json!(false));
        assert_eq!(structured["finalizable"], json!(false));
    }

    #[test]
    fn unsupported_literal_state_remains_nonterminal() {
        let mut detail = sample_execution_detail(
            Uuid::from_u128(49),
            Uuid::from_u128(50),
            Uuid::from_u128(51),
            Uuid::from_u128(52),
            Uuid::from_u128(53),
            Uuid::from_u128(54),
            Uuid::from_u128(55),
        );
        detail.verification_state = AssistantVerificationState::InsufficientEvidence;
        detail.answer_disposition = AssistantAnswerDisposition::NonTerminal;
        detail.verification_warnings.push(
            ironrag_contracts::assistant::AssistantVerificationWarning {
                code: "unsupported_literal".to_string(),
                message: "An exact literal is unsupported.".to_string(),
                related_segment_id: None,
                related_fact_id: None,
            },
        );

        let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload(
            "Candidate with an unsupported exact literal.",
            &detail,
        );
        let structured = &payload["structuredContent"];

        assert_eq!(structured["finalAnswerReady"], json!(false));
        assert_eq!(structured["finalizable"], json!(false));
        assert_eq!(structured["repairPolicy"]["required"], json!(true));
    }

    #[test]
    fn safe_fallback_is_terminal_without_becoming_factual_ready() {
        let mut detail = sample_execution_detail(
            Uuid::from_u128(56),
            Uuid::from_u128(57),
            Uuid::from_u128(58),
            Uuid::from_u128(59),
            Uuid::from_u128(60),
            Uuid::from_u128(61),
            Uuid::from_u128(62),
        );
        detail.verification_state = AssistantVerificationState::InsufficientEvidence;
        detail.answer_disposition = AssistantAnswerDisposition::SafeFallback;

        let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload(
            "A deterministic safe fallback.",
            &detail,
        );
        let structured = &payload["structuredContent"];

        assert_eq!(structured["finalAnswerReady"], json!(false));
        assert_eq!(structured["repairPolicy"]["required"], json!(false));
        assert_eq!(structured["readiness"]["answerDisposition"], json!("safe_fallback"));
    }

    #[test]
    fn finalizer_owned_factual_disposition_accepts_ordinary_prose_states() {
        for state in
            [AssistantVerificationState::NotRun, AssistantVerificationState::PartiallySupported]
        {
            let mut detail = sample_execution_detail(
                Uuid::from_u128(63),
                Uuid::from_u128(64),
                Uuid::from_u128(65),
                Uuid::from_u128(66),
                Uuid::from_u128(67),
                Uuid::from_u128(68),
                Uuid::from_u128(69),
            );
            detail.verification_state = state;
            detail.answer_disposition = AssistantAnswerDisposition::FactualReady;

            let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload(
                "A grounded prose explanation.",
                &detail,
            );
            let structured = &payload["structuredContent"];

            assert_eq!(structured["finalAnswerReady"], json!(true));
            assert_eq!(structured["repairPolicy"]["required"], json!(false));
            assert_eq!(structured["readiness"]["answerDisposition"], json!("factual_ready"));
        }
    }

    #[test]
    fn compact_profile_exposes_readiness_verifier_warnings_and_bounded_references() {
        let mut detail = sample_execution_detail(
            Uuid::from_u128(31),
            Uuid::from_u128(32),
            Uuid::from_u128(33),
            Uuid::from_u128(34),
            Uuid::from_u128(35),
            Uuid::from_u128(36),
            Uuid::from_u128(37),
        );
        detail.verification_warnings.push(
            ironrag_contracts::assistant::AssistantVerificationWarning {
                code: "synthetic_warning".to_string(),
                message: "Synthetic verifier warning".to_string(),
                related_segment_id: None,
                related_fact_id: None,
            },
        );

        let payload = crate::interfaces::http::mcp::grounded_answer_contract_payload_with_profile(
            "Grounded compact answer.",
            &detail,
            crate::mcp_types::McpGroundedAnswerResponseProfile::Compact,
            Some(3),
        );
        let structured = &payload["structuredContent"];

        assert_eq!(structured["answerBody"], json!("Grounded compact answer."));
        assert_eq!(structured["responseProfile"], json!("compact"));
        assert!(structured["executionDetail"].get("chunkReferences").is_none());
        assert_eq!(structured["readiness"]["finalAnswerReady"], json!(true));
        assert_eq!(structured["readiness"]["finalizable"], json!(true));
        assert_eq!(structured["verifier"]["state"], json!("verified"));
        assert_eq!(structured["verifier"]["warningCount"], json!(1));
        assert_eq!(structured["warnings"][0]["code"], json!("synthetic_warning"));
        assert_eq!(structured["referenceSummary"]["totalCount"], json!(5));
        assert_eq!(structured["referenceSummary"]["returnedCount"], json!(3));
        assert_eq!(structured["referenceSummary"]["truncated"], json!(true));
        assert_eq!(structured["referenceSummary"]["references"].as_array().map(Vec::len), Some(3));
    }

    #[test]
    fn full_and_compact_profiles_fail_closed_on_shared_completion_gap() {
        let detail = sample_execution_detail(
            Uuid::from_u128(71),
            Uuid::from_u128(72),
            Uuid::from_u128(73),
            Uuid::from_u128(74),
            Uuid::from_u128(75),
            Uuid::from_u128(76),
            Uuid::from_u128(77),
        );
        let query_ir = QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::MultiDocument,
            language: QueryLanguage::Auto,
            target_types: vec![crate::domains::query_ir::QueryTargetKind::Release],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: Some(SourceSliceSpec {
                direction: SourceSliceDirection::Tail,
                count: Some(4),
                filter: SourceSliceFilter::ReleaseMarker,
            }),
            retrieval_query: Some("neutral synthetic inventory".to_string()),
            confidence: 1.0,
        };
        let answer = "1. Release 2.0 — improved startup.\n2. Release 1.0 — fixed retries.";

        for profile in [
            crate::mcp_types::McpGroundedAnswerResponseProfile::Full,
            crate::mcp_types::McpGroundedAnswerResponseProfile::Compact,
        ] {
            let payload =
                crate::interfaces::http::mcp::grounded_answer_contract_payload_for_query_ir(
                    answer,
                    &detail,
                    &query_ir,
                    profile,
                    Some(3),
                );
            let structured = &payload["structuredContent"];

            assert_eq!(structured["queryLanguage"], json!("auto"));
            assert_eq!(structured["finalAnswerReady"], json!(false));
            assert_eq!(structured["finalizable"], json!(false));
            assert_eq!(structured["completion"]["complete"], json!(false));
            assert_eq!(structured["completion"]["reason"], json!("ordered_inventory_incomplete"));
            assert_eq!(structured["completion"]["expected"], json!(4));
            assert_eq!(structured["completion"]["observed"], json!(2));
            assert_eq!(structured["readiness"]["finalAnswerReady"], json!(false));
            assert_eq!(structured["readiness"]["finalizable"], json!(false));
            assert_eq!(structured["readiness"]["completionRequired"], json!(true));
            assert_eq!(structured["repairPolicy"]["required"], json!(true));
            assert_eq!(structured["repairPolicy"]["reason"], json!("ordered_inventory_incomplete"));
            assert_eq!(structured["repairPolicy"]["maxAdditionalGroundedAnswerCalls"], json!(1));
        }
    }

    #[test]
    fn compact_reference_limit_validation_fails_loudly() {
        use crate::mcp_types::McpGroundedAnswerResponseProfile::{Compact, Full};

        assert!(resolve_compact_reference_limit(Compact, None).is_ok());
        assert!(resolve_compact_reference_limit(Compact, Some(1)).is_ok());
        assert!(
            resolve_compact_reference_limit(
                Compact,
                Some(crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT),
            )
            .is_ok()
        );
        assert!(
            resolve_compact_reference_limit(
                Compact,
                Some(crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT + 1),
            )
            .is_err()
        );
        assert!(resolve_compact_reference_limit(Full, Some(1)).is_err());
    }

    #[test]
    fn grounded_answer_input_rejects_unknown_fields_and_invalid_bounds() {
        assert!(
            serde_json::from_value::<GroundedAnswerArgs>(json!({
                "library": "workspace/library",
                "query": "question",
                "top_k": 8
            }))
            .is_err()
        );

        let mut empty = serde_json::from_value::<GroundedAnswerArgs>(json!({
            "library": "   ",
            "query": "   "
        }))
        .expect("parse empty strings before semantic validation");
        assert!(validate_and_normalize_grounded_answer_args(&mut empty).is_err());

        for top_k in [0, MAX_TOP_K + 1] {
            let mut args = serde_json::from_value::<GroundedAnswerArgs>(json!({
                "library": "workspace/library",
                "query": "question",
                "topK": top_k
            }))
            .expect("parse topK");
            assert!(validate_and_normalize_grounded_answer_args(&mut args).is_err());
        }

        let turns = (0..=MCP_GROUNDED_MAX_CONVERSATION_TURNS)
            .map(|_| json!({"role": "user", "content": "context"}))
            .collect::<Vec<_>>();
        let mut too_many_turns = serde_json::from_value::<GroundedAnswerArgs>(json!({
            "library": "workspace/library",
            "query": "question",
            "conversationTurns": turns
        }))
        .expect("parse turn array");
        assert!(validate_and_normalize_grounded_answer_args(&mut too_many_turns).is_err());

        let mut oversized_turn = serde_json::from_value::<GroundedAnswerArgs>(json!({
            "library": "workspace/library",
            "query": "question",
            "conversationTurns": [{
                "role": "assistant",
                "content": "x".repeat(MCP_GROUNDED_MAX_CONVERSATION_TURN_CHARS + 1)
            }]
        }))
        .expect("parse oversized turn");
        assert!(validate_and_normalize_grounded_answer_args(&mut oversized_turn).is_err());
    }

    #[test]
    fn include_debug_rejects_explicit_compact_profile() {
        let mut args = serde_json::from_value::<GroundedAnswerArgs>(json!({
            "library": "workspace/library",
            "query": "question",
            "includeDebug": true,
            "responseProfile": "compact"
        }))
        .expect("parse args");

        assert!(validate_and_normalize_grounded_answer_args(&mut args).is_err());
    }

    #[test]
    fn compact_profile_bounds_large_reference_payloads() {
        let mut detail = sample_execution_detail(
            Uuid::from_u128(41),
            Uuid::from_u128(42),
            Uuid::from_u128(43),
            Uuid::from_u128(44),
            Uuid::from_u128(45),
            Uuid::from_u128(46),
            Uuid::from_u128(47),
        );
        detail.chunk_references = (0..200)
            .map(|index| AssistantChunkReference {
                execution_id: detail.execution.id,
                chunk_id: Uuid::from_u128(1_000 + index as u128),
                rank: index + 1,
                score: 1.0 / f64::from(index + 1),
            })
            .collect();

        let full = crate::interfaces::http::mcp::grounded_answer_contract_payload(
            "Grounded compact answer.",
            &detail,
        );
        let compact = crate::interfaces::http::mcp::grounded_answer_contract_payload_with_profile(
            "Grounded compact answer.",
            &detail,
            crate::mcp_types::McpGroundedAnswerResponseProfile::Compact,
            None,
        );
        let compact_structured = &compact["structuredContent"];
        let full_bytes = serde_json::to_vec(&full).expect("serialize full payload");
        let compact_bytes = serde_json::to_vec(&compact).expect("serialize compact payload");

        assert_eq!(compact_structured["referenceSummary"]["totalCount"], json!(204));
        assert_eq!(compact_structured["referenceSummary"]["returnedCount"], json!(8));
        assert_eq!(compact_structured["referenceSummary"]["truncated"], json!(true));
        assert!(compact_bytes.len() * 3 < full_bytes.len());
    }

    fn sample_execution_detail(
        execution_id: Uuid,
        chunk_id: Uuid,
        segment_id: Uuid,
        revision_id: Uuid,
        fact_id: Uuid,
        node_id: Uuid,
        edge_id: Uuid,
    ) -> AssistantExecutionDetail {
        let now = Utc::now();
        let workspace_id = Uuid::from_u128(11);
        let library_id = Uuid::from_u128(12);
        let conversation_id = Uuid::from_u128(13);
        let context_bundle_id = Uuid::from_u128(16);
        let runtime_execution_id = Uuid::from_u128(17);

        AssistantExecutionDetail {
            context_bundle_id,
            execution: AssistantExecution {
                id: execution_id,
                workspace_id,
                library_id,
                conversation_id,
                context_bundle_id,
                request_turn_id: None,
                response_turn_id: None,
                binding_id: None,
                runtime_execution_id: Some(runtime_execution_id),
                lifecycle_state: "completed".to_string(),
                active_stage: None,
                query_text: "Which endpoint is canonical?".to_string(),
                failure_code: None,
                started_at: now,
                completed_at: Some(now),
            },
            runtime_summary: AssistantRuntimeSummary {
                runtime_execution_id,
                lifecycle_state: "completed".to_string(),
                active_stage: None,
                turn_budget: 1,
                turn_count: 1,
                parallel_action_limit: 1,
                failure_code: None,
                failure_summary_redacted: None,
                policy_summary: AssistantPolicySummary {
                    allow_count: 0,
                    reject_count: 0,
                    terminate_count: 0,
                    recent_decisions: Vec::new(),
                },
                accepted_at: now,
                completed_at: Some(now),
            },
            runtime_stage_summaries: Vec::new(),
            request_turn: None,
            response_turn: None,
            chunk_references: vec![AssistantChunkReference {
                execution_id,
                chunk_id,
                rank: 1,
                score: 0.91,
            }],
            prepared_segment_references: vec![AssistantPreparedSegmentReference {
                execution_id,
                segment_id,
                revision_id,
                block_kind: "endpoint_block".to_string(),
                rank: 2,
                score: 0.82,
                heading_trail: vec!["API".to_string()],
                section_path: vec!["contracts".to_string()],
                document_id: Some(Uuid::from_u128(18)),
                document_title: Some("Synthetic contract".to_string()),
                document_hint: Some("Synthetic contract".to_string()),
                source_access: Some(AssistantContentSourceAccess {
                    kind: "stored_document".to_string(),
                    href: "urn:synthetic:contract".to_string(),
                }),
            }],
            technical_fact_references: vec![AssistantTechnicalFactReference {
                execution_id,
                fact_id,
                revision_id,
                fact_kind: "endpoint_path".to_string(),
                canonical_value: "/v1/items".to_string(),
                display_value: "/v1/items".to_string(),
                rank: 3,
                score: 0.73,
            }],
            entity_references: vec![AssistantEntityReference {
                execution_id,
                node_id,
                rank: 4,
                score: 0.64,
                label: "Synthetic API".to_string(),
                entity_type: Some("service".to_string()),
                summary: Some("Synthetic API node".to_string()),
            }],
            relation_references: vec![AssistantRelationReference {
                execution_id,
                edge_id,
                rank: 5,
                score: 0.55,
                predicate: "calls".to_string(),
                normalized_assertion: Some("Synthetic API calls endpoint".to_string()),
            }],
            reference_summary: None,
            verification_state: AssistantVerificationState::Verified,
            verification_warnings: Vec::new(),
            answer_disposition: AssistantAnswerDisposition::FactualReady,
            clarification: ironrag_contracts::assistant::AssistantClarification::default(),
        }
    }
}
