//! `grounded_answer` — canonical MCP tool that gives every MCP agent
//! exactly the same grounded answer the IronRAG operator UI assistant
//! produces for the same library and question.
//!
//! The implementation is deliberately a thin translator over the
//! canonical query service (`state.canonical_services.query`). The
//! handler creates an ephemeral conversation, delegates to
//! `execute_turn` — the same entry point the UI uses — and reshapes the
//! result into the MCP tool-call payload. No parallel retrieval,
//! ranking, or answer-generation logic lives here — MCP is not a
//! degraded lane; it returns the same answer the UI assistant
//! returns for the same library and question.
//!
//! Phase 1 scope:
//!   - input: `library`, `query`, optional `conversationTurns`,
//!     optional `topK`, optional `includeDebug`
//!   - output: grounded answer text, citation list, verifier verdict,
//!     `runtimeExecutionId`, `conversationId`, `executionId`

use serde_json::{Value, json};

use crate::{
    domains::query::{QueryTurnKind, QueryVerificationState},
    interfaces::http::{authorization::POLICY_QUERY_RUN, router_support::ApiError},
    services::{
        iam::audit::AppendQueryExecutionAuditCommand,
        query::service::{
            CreateConversationCommand, ExecuteConversationTurnCommand, ExternalConversationTurn,
        },
    },
};

use super::super::{McpToolDescriptor, McpToolResult, ok_tool_result, tool_error_result};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    if name != "grounded_answer" {
        return None;
    }
    Some(McpToolDescriptor {
        name: "grounded_answer",
        description: "Ask a natural-language question against one library and get a grounded answer with citations — the SAME pipeline the IronRAG UI assistant uses (QueryCompiler → hybrid retrieval → graph-aware context → answer generation → verifier). Prefer this over `search_documents` + `read_document` whenever the user expects an answer, not a hit list. Output includes `answer` (the human-readable reply), `citations` (document ids, titles, URIs, excerpts), `verifier` (strict/moderate/lenient + warnings), and `runtimeExecutionId` which can be passed to `get_runtime_execution_trace` to inspect the full execution graph, provider/model identities, and per-stage evidence.",
        input_schema: json!({
            "type": "object",
            "required": ["library", "query"],
            "properties": {
                "library": {
                    "type": "string",
                    "description": "Target fully-qualified library ref. The token MUST have query_run on this library."
                },
                "query": {
                    "type": "string",
                    "description": "Natural-language question in the user's language. The internal QueryCompiler turns it into a typed QueryIR (act, scope, target_types) before retrieval — no keyword pre-processing is required on the client side."
                },
                "conversationTurns": {
                    "type": "array",
                    "maxItems": 20,
                    "description": "Optional rolling prior chat turns for short follow-ups and coreference resolution. Pass the actual earlier user/assistant turns in chronological order when the client's tool runtime has them. If the client cannot pass history, rewrite the latest follow-up into one self-contained question before calling the tool.",
                    "items": {
                        "type": "object",
                        "required": ["role", "content"],
                        "properties": {
                            "role": {
                                "type": "string",
                                "enum": ["user", "assistant"]
                            },
                            "content": {
                                "type": "string"
                            }
                        }
                    }
                },
                "topK": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 30,
                    "description": "Optional retrieval breadth. Defaults to 8, matching the UI assistant. Larger values are rarely useful; the verifier keeps only cited hits."
                },
                "includeDebug": {
                    "type": "boolean",
                    "description": "Optional flag. When true, the response carries the same debug metadata the UI debug panel shows (runtime stage summaries, graph expansion, verifier trace)."
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
    Some(grounded_answer(context, arguments).await)
}

async fn grounded_answer(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    let parsed: GroundedAnswerArgs = match serde_json::from_value(arguments.clone()) {
        Ok(parsed) => parsed,
        Err(error) => {
            return tool_error_result(ApiError::invalid_mcp_tool_call(format!(
                "invalid grounded_answer arguments: {error}"
            )));
        }
    };
    let external_prior_turns = match normalize_external_prior_turns(parsed.conversation_turns) {
        Ok(turns) => turns,
        Err(error) => return tool_error_result(error),
    };

    // Scope check: the same POLICY_QUERY_RUN the UI handler uses for
    // `create_session` / `create_session_turn`. An MCP token without
    // query_run on the library gets a clean 401-equivalent tool error
    // instead of silently degrading to a stub answer.
    let library = match crate::services::mcp::access::load_library_by_catalog_ref(
        context.auth,
        context.state,
        &parsed.library,
        POLICY_QUERY_RUN,
    )
    .await
    {
        Ok(library) => library,
        Err(error) => return tool_error_result(error),
    };

    // Result-cache lookup. Canonical key factors in the library's
    // graph projection_version and the active query_answer binding
    // id, so fresh graphs and fresh bindings bump the key
    // automatically — no manual flush dance on rebuild or model
    // swap. Cache misses fall through to the full pipeline; cache
    // hits bypass conversation creation, retrieval, rerank, answer
    // generation, verifier, AND the ephemeral `[MCP]` conversation
    // row, so the operator's audit history does not bloat with
    // identical re-runs.
    //
    // Debug-flagged calls skip the cache on both the lookup and the
    // store, because the debug payload carries trace/context data
    // that's useless once the answer is frozen.
    let cache_enabled = !parsed.include_debug.unwrap_or(false);
    let (cache_key, cache_projection_version) = if cache_enabled {
        let projection_version = crate::infra::repositories::get_runtime_graph_snapshot(
            &context.state.persistence.postgres,
            library.id,
        )
        .await
        .ok()
        .flatten()
        .map(|snapshot| snapshot.projection_version)
        .unwrap_or(0);
        let binding_id =
            crate::infra::repositories::ai_repository::get_effective_binding_assignment_by_purpose(
                &context.state.persistence.postgres,
                library.id,
                "query_answer",
            )
            .await
            .ok()
            .flatten()
            .map(|binding| binding.id);
        let key = crate::services::mcp::grounded_answer_cache::cache_key(
            library.id,
            projection_version,
            binding_id,
            &parsed.query,
            normalized_conversation_cache_input(&external_prior_turns).as_deref(),
        );
        (Some(key), projection_version)
    } else {
        (None, 0)
    };
    if let Some(cache_key_ref) = cache_key.as_deref() {
        match crate::services::mcp::grounded_answer_cache::get_cached(
            &context.state.persistence.redis,
            cache_key_ref,
        )
        .await
        {
            Ok(Some(cached)) => {
                tracing::info!(
                    stage = "grounded_answer.cache_hit",
                    library_id = %library.id,
                    projection_version = cache_projection_version,
                    cache_key = cache_key_ref,
                    "grounded_answer result served from cache"
                );
                return ok_tool_result(&cached.human_text, cached.structured_json);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    stage = "grounded_answer.cache_get_error",
                    library_id = %library.id,
                    %error,
                    "grounded_answer cache lookup failed — falling through to pipeline",
                );
            }
        }
    }

    // Ephemeral conversation: the canonical `execute_turn` API is
    // conversation-scoped because the UI tracks history and the LLM
    // loop consumes the last N turns for coreference resolution. For a
    // stateless MCP tool call we create a single conversation, run one
    // turn on it, and return. The conversation row is left in place so
    // operators can audit the turn alongside UI-originated turns —
    // phase 2 may add a retention policy if this becomes noisy.
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
                title: Some(conversation_title(&parsed.query)),
                request_surface: "mcp".to_string(),
            },
        )
        .await
    {
        Ok(conversation) => conversation,
        Err(error) => return tool_error_result(error),
    };

    let outcome = match context
        .state
        .canonical_services
        .query
        .execute_turn(
            context.state,
            ExecuteConversationTurnCommand {
                conversation_id: conversation.id,
                author_principal_id: Some(context.auth.principal_id),
                content_text: parsed.query.clone(),
                external_prior_turns,
                top_k: parsed.top_k.unwrap_or(8),
                include_debug: parsed.include_debug.unwrap_or(false),
                auth: context.auth.clone(),
            },
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => return tool_error_result(error),
    };

    if let Err(error) = context
        .state
        .canonical_services
        .audit
        .append_query_execution_event(
            context.state,
            AppendQueryExecutionAuditCommand {
                actor_principal_id: context.auth.principal_id,
                surface_kind: "mcp".to_string(),
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

    let answer_text =
        outcome.response_turn.as_ref().map(|turn| turn.content_text.clone()).unwrap_or_default();

    let citations = build_citations(&outcome);
    let verifier_level = verification_level_label(outcome.verification_state);
    let verifier_warnings = outcome
        .verification_warnings
        .iter()
        .map(|warning| {
            json!({
                "code": warning.code,
                "message": warning.message,
                "relatedSegmentId": warning.related_segment_id,
                "relatedFactId": warning.related_fact_id,
            })
        })
        .collect::<Vec<_>>();

    let structured = json!({
        "answer": answer_text,
        "citations": citations,
        "verifier": {
            "level": verifier_level,
            "warnings": verifier_warnings,
        },
        "runtimeExecutionId": outcome.execution.runtime_execution_id,
        "executionId": outcome.execution.id,
        "conversationId": outcome.execution.conversation_id,
        "libraryId": outcome.execution.library_id,
        "workspaceId": outcome.execution.workspace_id,
        "lifecycleState": format!("{:?}", outcome.execution.lifecycle_state),
    });

    let human_text = if answer_text.is_empty() {
        "The grounded-answer pipeline returned no answer text (execution may have failed or degraded). Inspect runtimeExecutionId via get_runtime_execution_trace for details.".to_string()
    } else {
        answer_text
    };

    // Persist into the result cache before constructing the final
    // `McpToolResult`. Only ran when `include_debug=false` (see
    // cache-enabled check above) AND when the verifier actually
    // produced a real answer — we don't want to pin a "no answer
    // text" stub for 5 min, since its backing execution was likely
    // failure-adjacent (provider outage / timeout / degraded
    // pipeline). `ok_tool_result` takes the `structured` JSON value
    // by-move, so the cache entry is cloned from a stable snapshot
    // before we hand it off.
    if let Some(cache_key_ref) = cache_key.as_deref() {
        if !human_text.is_empty()
            && human_text
                != "The grounded-answer pipeline returned no answer text (execution may have failed or degraded). Inspect runtimeExecutionId via get_runtime_execution_trace for details."
        {
            let entry = crate::services::mcp::grounded_answer_cache::CachedGroundedAnswer {
                human_text: human_text.clone(),
                structured_json: structured.clone(),
            };
            if let Err(error) = crate::services::mcp::grounded_answer_cache::put_cached(
                &context.state.persistence.redis,
                cache_key_ref,
                &entry,
            )
            .await
            {
                tracing::warn!(
                    stage = "grounded_answer.cache_set_error",
                    cache_key = cache_key_ref,
                    %error,
                    "grounded_answer cache write failed — answer still returned to caller",
                );
            }
        }
    }

    ok_tool_result(&human_text, structured)
}

fn conversation_title(query: &str) -> String {
    // Keep ephemeral MCP conversations visually distinct from
    // UI-originated ones so operators auditing query history can tell
    // them apart at a glance. The UI derives its title from the first
    // user message; we prepend a transport marker so mixed sessions
    // don't look suspicious.
    const MAX_LEN: usize = 96;
    let trimmed: String = query.trim().chars().take(MAX_LEN).collect();
    if trimmed.is_empty() {
        "[MCP] grounded_answer".to_string()
    } else {
        format!("[MCP] {trimmed}")
    }
}

fn verification_level_label(state: QueryVerificationState) -> &'static str {
    // Snake-case labels match the on-wire representation that the UI
    // already consumes from `QueryExecutionDetail.verification_state`,
    // so parity tests can compare the two transports byte-for-byte.
    match state {
        QueryVerificationState::NotRun => "not_run",
        QueryVerificationState::Verified => "verified",
        QueryVerificationState::PartiallySupported => "partially_supported",
        QueryVerificationState::Conflicting => "conflicting",
        QueryVerificationState::InsufficientEvidence => "insufficient_evidence",
        QueryVerificationState::Failed => "failed",
    }
}

fn build_citations(
    outcome: &crate::services::query::service::QueryTurnExecutionResult,
) -> Vec<Value> {
    // Phase 1 surfaces the same citation handles the UI consumes but
    // avoids re-fetching per-document metadata — the agent can hit
    // `read_document` for a full body and `get_runtime_execution_trace`
    // for the provenance graph. Only stable fields go out: document
    // id, chunk id, score, rank. The UI builds its richer citation
    // card from exactly these handles plus a post-fetch, so parity
    // holds: same handles in, same UX achievable.
    outcome
        .chunk_references
        .iter()
        .map(|reference| {
            json!({
                "kind": "chunk",
                "chunkId": reference.chunk_id,
                "rank": reference.rank,
                "score": reference.score,
            })
        })
        .chain(outcome.graph_node_references.iter().map(|reference| {
            json!({
                "kind": "entity",
                "nodeId": reference.node_id,
                "label": reference.label,
                "entityType": reference.entity_type,
                "summary": reference.summary,
                "rank": reference.rank,
                "score": reference.score,
            })
        }))
        .collect()
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroundedAnswerArgs {
    library: String,
    query: String,
    conversation_turns: Option<Vec<GroundedAnswerConversationTurn>>,
    top_k: Option<usize>,
    include_debug: Option<bool>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroundedAnswerConversationTurn {
    role: GroundedAnswerConversationTurnRole,
    content: String,
}

#[derive(serde::Deserialize)]
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

fn normalized_conversation_cache_input(turns: &[ExternalConversationTurn]) -> Option<String> {
    if turns.is_empty() {
        return None;
    }
    Some(
        turns
            .iter()
            .map(|turn| {
                let normalized = turn
                    .content_text
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
                format!("{}:{normalized}", turn.turn_kind.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}
