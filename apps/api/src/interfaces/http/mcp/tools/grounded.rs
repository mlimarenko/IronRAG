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
//!   - output: grounded answer text plus the canonical
//!     `AssistantExecutionDetail`, `runtimeExecutionId`,
//!     `conversationId`, `executionId`

use serde_json::{Value, json};

use crate::{
    domains::agent_runtime::RuntimeSurfaceKind,
    domains::query::{DEFAULT_TOP_K, MAX_TOP_K, QueryTurnKind, resolve_top_k},
    interfaces::http::{authorization::POLICY_QUERY_RUN, router_support::ApiError},
    services::{
        iam::audit::AppendQueryExecutionAuditCommand,
        query::service::{
            CreateConversationCommand, ExecuteConversationTurnCommand, ExternalConversationTurn,
        },
    },
};

use super::super::{
    McpToolDescriptor, McpToolResult, grounded_answer_tool_result, tool_error_result,
};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    if name != "grounded_answer" {
        return None;
    }
    Some(McpToolDescriptor {
        name: "grounded_answer",
        description: "Ask a natural-language question against one library and get a grounded answer — the SAME pipeline the IronRAG UI assistant uses (QueryCompiler → hybrid retrieval → graph-aware context → answer generation → verifier). Prefer this over `search_documents` + `read_document` whenever the user expects an answer, not a hit list. The tool text is the human-readable reply. Structured output includes `executionDetail` with the same chunk, prepared-segment, technical-fact, entity, relation, verifier, runtime, request, and response fields the UI assistant receives, plus top-level `runtimeExecutionId`, `executionId`, and `conversationId` shortcuts for trace lookups.",
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
                    "maximum": MAX_TOP_K,
                    "description": format!("Optional retrieval breadth. Defaults to {DEFAULT_TOP_K}, matching the UI assistant. Larger values are rarely useful; the verifier keeps only cited hits.")
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
                surface_kind: RuntimeSurfaceKind::Mcp,
                content_text: parsed.query.clone(),
                external_prior_turns,
                top_k: resolve_grounded_answer_top_k(parsed.top_k),
                include_debug: parsed.include_debug.unwrap_or(false),
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

    let execution_detail = crate::interfaces::http::query::map_turn_execution_response(outcome);

    grounded_answer_tool_result(&answer_text, &execution_detail)
}

pub(crate) fn resolve_grounded_answer_top_k(requested_top_k: Option<usize>) -> usize {
    resolve_top_k(requested_top_k)
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

#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
struct GroundedAnswerArgs {
    library: String,
    query: String,
    conversation_turns: Option<Vec<GroundedAnswerConversationTurn>>,
    top_k: Option<usize>,
    include_debug: Option<bool>,
}

#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ironrag_contracts::assistant::{
        AssistantChunkReference, AssistantContentSourceAccess, AssistantEntityReference,
        AssistantExecution, AssistantExecutionDetail, AssistantPolicySummary,
        AssistantPreparedSegmentReference, AssistantRelationReference, AssistantRuntimeSummary,
        AssistantTechnicalFactReference, AssistantVerificationState,
    };
    use uuid::Uuid;

    use super::*;

    #[test]
    fn grounded_answer_default_top_k_matches_ui_query_turn_default() {
        let library_ref = "alpha-workspace/adapter-library";
        let query = "Which endpoint does the demo adapter call for inventory sync?";

        let mcp_top_k = resolve_grounded_answer_top_k(None);
        let ui_top_k = crate::interfaces::http::query::resolve_query_turn_top_k(None);

        assert_eq!(mcp_top_k, ui_top_k, "top_k drift for library {library_ref} and query {query}");
        assert_eq!(mcp_top_k, DEFAULT_TOP_K);
        assert!(mcp_top_k >= 24);
    }

    #[test]
    fn grounded_answer_explicit_top_k_matches_ui_query_turn() {
        assert_eq!(
            resolve_grounded_answer_top_k(None),
            crate::interfaces::http::query::resolve_query_turn_top_k(None)
        );
        assert_eq!(
            resolve_grounded_answer_top_k(Some(6)),
            crate::interfaces::http::query::resolve_query_turn_top_k(Some(6))
        );
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

        let structured = crate::interfaces::http::mcp::grounded_answer_contract_payload(
            "Synthetic answer",
            &detail,
        );
        let structured_content = &structured["structuredContent"];
        let execution_detail = &structured_content["executionDetail"];

        assert_eq!(structured["isError"], json!(false));
        assert_eq!(structured_content.get("citations"), None);
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
                source_uri: Some("urn:synthetic:contract".to_string()),
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
            verification_state: AssistantVerificationState::Verified,
            verification_warnings: Vec::new(),
        }
    }
}
