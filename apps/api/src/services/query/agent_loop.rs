//! LLM turn helpers used by the assistant answer surfaces.
//!
//! The in-app UI assistant runs as the same kind of tool-using MCP
//! client agent an external chat client would run: the model sees the
//! answer-tool registry, chooses tool calls, receives tool
//! results, and then writes the final reply.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use futures::{StreamExt as _, stream};
use serde_json::Value;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::provider_profiles::ProviderModelSelection,
    domains::query::{QueryTurnKind, resolve_contextual_grounded_answer_top_k},
    domains::query_ir::literal_text_is_identifier_shaped,
    domains::{agent_runtime::RuntimeSurfaceKind, ai::AiBindingPurpose},
    integrations::llm::{ChatMessage, ChatToolCall, ChatToolDef, ToolUseRequest},
    interfaces::http::{
        auth::AuthContext,
        mcp::{
            McpToolSurface,
            tools::{
                self, ToolCallContext, ToolVisibilityCapabilities,
                documents::{READ_DOCUMENT_TOOL_NAME, SEARCH_DOCUMENTS_TOOL_NAME},
            },
        },
    },
    services::query::{
        assistant_grounding::AssistantGroundingEvidence,
        error::QueryServiceError,
        llm_context_debug::{
            AgentLoopMetadata, AgentStopReason, LlmIterationDebug, ResponseToolCallDebug,
        },
        service::ExternalConversationTurn,
        text_match::normalized_alnum_token_sequence,
    },
};

const RUNTIME_RETRIEVED_CONTEXT_TOOL: &str = "ironrag_retrieved_context";
const RUNTIME_LITERAL_REVISION_CONTEXT_TOOL: &str = "ironrag_literal_revision_context";
const GROUNDED_ANSWER_TOOL_NAME: &str = "grounded_answer";
const GROUNDED_ANSWER_LIFECYCLE_COMPLETED: &str = "completed";
const GROUNDED_ANSWER_VERIFICATION_NOT_RUN: &str = "not_run";
const GROUNDED_ANSWER_VERIFICATION_VERIFIED: &str = "verified";
const TOOL_MODEL_DEFAULT_CONTENT_CHAR_LIMIT: usize = 3_000;
const TOOL_MODEL_GROUNDED_ANSWER_CONTENT_CHAR_LIMIT: usize = 12_000;
const TOOL_MODEL_READ_DOCUMENT_CONTENT_CHAR_LIMIT: usize = 5_000;
const TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT: usize = 8_000;
const TOOL_VERIFICATION_CONTENT_CHAR_LIMIT: usize = 8_000;
const TOOL_VERIFICATION_STRUCTURED_JSON_CHAR_LIMIT: usize = 16_000;
const TOOL_MODEL_GROUNDED_REFERENCE_LIMIT: usize = 8;
const TOOL_MODEL_FINALIZABLE_GROUNDED_REFERENCE_LIMIT: usize = 0;
const TOOL_VERIFICATION_GROUNDED_REFERENCE_LIMIT: usize = 8;
const TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT: usize = 96_000;
const TOOL_GROUNDING_FRAGMENT_CHAR_LIMIT: usize = 20_000;
const TOOL_GROUNDING_TOTAL_CHAR_LIMIT: usize = 80_000;
const GROUNDED_EVIDENCE_LEDGER_ENTRY_LIMIT: usize = 6;
const GROUNDED_EVIDENCE_LEDGER_ANSWER_CHARS: usize = 1_600;
const GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT: usize = 32;
const GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT: usize = 24;
const GROUNDED_EVIDENCE_LEDGER_TEXT_CHARS: usize = 10_000;
const GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_ANCHORS: usize = 3;
const GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_MISSING_HIGH_SIGNAL_ANCHORS: usize = 2;
const SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS: usize = 4;
const VERBATIM_USER_FRAGMENT_LIMIT: usize = 6;
const VERBATIM_USER_FRAGMENT_MIN_CHARS: usize = 4;
const VERBATIM_USER_FRAGMENT_MAX_CHARS: usize = 400;
const VERBATIM_USER_FRAGMENT_TOTAL_CHARS: usize = 1_200;
const HISTORY_PADDED_QUERY_ANCHOR_LIMIT: usize = 24;
const HISTORY_PADDED_QUERY_MAX_CHARS: usize = 900;
const HISTORY_PADDED_QUERY_ANCHOR_MAX_CHARS: usize = 180;
const CONTEXTUAL_SEARCH_QUERY_ANCHOR_LIMIT: usize = 10;
const CONTEXTUAL_SEARCH_QUERY_MIN_ANCHORS: usize = 2;
const CONTEXTUAL_SEARCH_QUERY_HISTORY_OVERLAP_SELF_SUFFICIENT: usize = 2;
const VERIFIED_GROUNDED_LITERAL_GUARD_MIN_LITERALS: usize = 4;
const PRIOR_ASSISTANT_ANSWER_FALLBACK_MIN_ANCHORS: usize = 3;
const USER_FRAGMENT_QUOTE_PAIRS: [(char, char); 8] = [
    ('«', '»'),
    ('“', '”'),
    ('„', '“'),
    ('「', '」'),
    ('『', '』'),
    ('‹', '›'),
    ('"', '"'),
    ('`', '`'),
];

/// Final result of one assistant turn.
#[derive(Debug, Clone)]
pub struct AgentTurnResult {
    pub answer: String,
    pub provider: ProviderModelSelection,
    pub usage_json: serde_json::Value,
    pub iterations: usize,
    pub assistant_grounding: AssistantGroundingEvidence,
    pub child_query_execution_ids: Vec<Uuid>,
    /// Per-iteration capture of the exact LLM request/response chain,
    /// for the assistant debug panel. Populated unconditionally — the
    /// cost is a few clones and the operator toggles the UI to view.
    pub debug_iterations: Vec<super::llm_context_debug::LlmIterationDebug>,
    /// Present when a turn was driven by the MCP client-style agent
    /// loop instead of a single fixed-context answer stage.
    pub agent_loop: Option<AgentLoopMetadata>,
}

/// Agent-loop failure with the partial provider transcript preserved
/// for the debug panel.
#[derive(Debug)]
pub struct AgentTurnFailure {
    pub error: QueryServiceError,
    pub debug_iterations: Vec<LlmIterationDebug>,
    pub agent_loop: Option<AgentLoopMetadata>,
}

impl AgentTurnFailure {
    fn empty(error: impl Into<QueryServiceError>) -> Self {
        Self { error: error.into(), debug_iterations: Vec::new(), agent_loop: None }
    }

    fn with_loop(
        error: impl Into<QueryServiceError>,
        debug_iterations: Vec<LlmIterationDebug>,
        agent_loop: AgentLoopMetadata,
    ) -> Self {
        Self { error: error.into(), debug_iterations, agent_loop: Some(agent_loop) }
    }
}

impl fmt::Display for AgentTurnFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.error)
    }
}

impl Error for AgentTurnFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

/// Inputs for the UI assistant's MCP-backed tool loop.
#[derive(Clone)]
pub struct McpToolAgentTurnInput<'a> {
    pub state: &'a AppState,
    pub auth: &'a AuthContext,
    pub library_id: Uuid,
    pub library_ref: &'a str,
    pub user_question: &'a str,
    pub contextual_follow_up: bool,
    pub conversation_history: &'a [ChatMessage],
    pub follow_up_context_messages: &'a [ChatMessage],
    pub grounded_answer_tool_history: &'a [ExternalConversationTurn],
    pub request_id: &'a str,
    pub grounded_answer_top_k: usize,
    pub iteration_cap: usize,
    pub max_parallel_actions: usize,
    pub deadline: Duration,
    pub soft_final_answer_deadline: Option<Duration>,
    pub activity_tx: Option<Sender<AgentLoopActivityEvent>>,
}

#[derive(Debug, Clone)]
pub enum AgentLoopActivityEvent {
    ModelRequest {
        iteration: usize,
        provider_kind: String,
        model_name: String,
    },
    ModelResponse {
        iteration: usize,
        provider_kind: String,
        model_name: String,
        tool_call_count: usize,
        has_final_answer: bool,
    },
    ToolCallStarted {
        iteration: usize,
        tool_name: String,
    },
    ToolCallFinished {
        iteration: usize,
        tool_name: String,
        elapsed_ms: u64,
        is_error: bool,
        child_execution_id: Option<Uuid>,
        result_preview: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct ToolExecutionOutcome {
    arguments_json: Option<String>,
    requested_arguments_json: Option<String>,
    message_content: String,
    result_text: Option<String>,
    result_json: Option<Value>,
    grounding_text: Option<String>,
    grounded_answer_ready: bool,
    grounded_answer_completed: bool,
    grounded_answer_needs_follow_up: bool,
    is_error: bool,
    /// Wall-clock the tool ran. Set centrally in `execute_tool_calls`;
    /// constructors default to 0.
    duration_ms: u64,
    child_query_execution_ids: Vec<Uuid>,
    child_runtime_execution_ids: Vec<Uuid>,
}

#[derive(Debug, Default, Clone)]
struct GroundedAnswerEvidenceLedger {
    entries: Vec<GroundedAnswerEvidenceEntry>,
    seen_keys: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct GroundedAnswerEvidenceEntry {
    index: usize,
    execution_id: Option<String>,
    final_answer_ready: bool,
    finalizable: bool,
    verification_state: Option<String>,
    warning_codes: Vec<String>,
    unsupported_literal_spans: BTreeSet<String>,
    answer_body: Option<String>,
    answer_excerpt: Option<String>,
    must_preserve_spans: Vec<String>,
    source_labels: Vec<String>,
}

impl GroundedAnswerEvidenceLedger {
    fn remember(&mut self, tool_name: &str, outcome: &ToolExecutionOutcome) {
        if tool_name != GROUNDED_ANSWER_TOOL_NAME || outcome.is_error {
            return;
        }
        if self.entries.len() >= GROUNDED_EVIDENCE_LEDGER_ENTRY_LIMIT {
            return;
        }
        let Some(result_json) = outcome.result_json.as_ref() else {
            return;
        };
        let Some(entry) = GroundedAnswerEvidenceEntry::from_result(
            self.entries.len() + 1,
            result_json,
            outcome.result_text.as_deref(),
        ) else {
            return;
        };
        let key = entry
            .execution_id
            .clone()
            .unwrap_or_else(|| format!("entry:{}", self.entries.len() + 1));
        if self.seen_keys.insert(key) {
            self.entries.push(entry);
        }
    }

    fn system_message(&self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let mut message = String::from(
            "Same-turn grounded_answer evidence ledger. Treat these entries as accumulated tool evidence, not as a replacement final answer. Later focused repair entries add or clarify coverage; they do not erase earlier grounded facts, source labels, warnings, or action paths unless directly contradicted. Before finalizing, cover the relevant preserved spans and source labels from all entries, or mark unsupported/conflicting branches plainly.",
        );
        for entry in &self.entries {
            entry.push_to_message(&mut message);
            if message.chars().count() >= GROUNDED_EVIDENCE_LEDGER_TEXT_CHARS {
                message = message.chars().take(GROUNDED_EVIDENCE_LEDGER_TEXT_CHARS).collect();
                break;
            }
        }
        Some(message)
    }

    fn guard_candidate_for_answer(&self, answer: &str) -> Option<String> {
        let high_signal_missing_count = self
            .guard_high_signal_anchor_set()
            .iter()
            .filter(|anchor| !answer_contains_guard_anchor(answer, anchor))
            .count();
        if high_signal_missing_count
            >= GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_MISSING_HIGH_SIGNAL_ANCHORS
        {
            return self.answer_guard_text();
        }

        let anchors = self.guard_anchor_set();
        if anchors.len() < GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_ANCHORS {
            return None;
        }
        let missing_count =
            anchors.iter().filter(|anchor| !answer_contains_guard_anchor(answer, anchor)).count();
        let missing_threshold = anchors.len().div_ceil(2).max(2);
        if missing_count < missing_threshold {
            return None;
        }
        self.answer_guard_text()
    }

    fn guard_high_signal_anchor_set(&self) -> BTreeSet<String> {
        let mut anchors = BTreeSet::new();
        for entry in self.guardable_entries() {
            push_grounded_evidence_ledger_high_signal_anchors(
                &mut anchors,
                &entry.must_preserve_spans,
                &entry.unsupported_literal_spans,
            );
            if anchors.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
                break;
            }
        }
        anchors
    }

    fn guard_anchor_set(&self) -> BTreeSet<String> {
        let mut anchors = BTreeSet::new();
        for entry in self.guardable_entries() {
            push_grounded_evidence_ledger_anchors(&mut anchors, &entry.must_preserve_spans);
            if anchors.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
                return anchors;
            }
        }
        if anchors.len() < GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_ANCHORS {
            for entry in self.guardable_entries() {
                push_grounded_evidence_ledger_anchors(&mut anchors, &entry.source_labels);
                if anchors.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
                    return anchors;
                }
            }
        }
        anchors
    }

    fn guardable_entries(&self) -> impl Iterator<Item = &GroundedAnswerEvidenceEntry> {
        self.entries.iter().filter(|entry| entry.can_guard_final_answer())
    }

    fn answer_guard_text(&self) -> Option<String> {
        let mut lines = Vec::new();
        let mut seen = BTreeSet::new();
        for entry in self.guardable_entries() {
            if let Some(answer) = &entry.answer_body {
                push_guard_answer_line(&mut lines, &mut seen, answer);
            }
            if lines.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
                break;
            }
        }
        if lines.is_empty() {
            return None;
        }
        let text = lines.join("\n");
        Some(text.chars().take(GROUNDED_EVIDENCE_LEDGER_TEXT_CHARS).collect())
    }
}

impl GroundedAnswerEvidenceEntry {
    fn from_result(index: usize, result_json: &Value, fallback_text: Option<&str>) -> Option<Self> {
        let structured = result_json.get("structuredContent")?;
        let answer_body = structured
            .get("answerBody")
            .and_then(Value::as_str)
            .or(fallback_text)
            .map(str::trim)
            .map(|text| text.chars().take(GROUNDED_EVIDENCE_LEDGER_TEXT_CHARS).collect::<String>())
            .filter(|text| !text.trim().is_empty());
        let answer_excerpt =
            answer_body.as_deref().map(compact_ledger_text).filter(|text| !text.trim().is_empty());
        let mut must_preserve_spans = Vec::new();
        let mut seen_spans = BTreeSet::new();
        push_json_string_array(
            &mut must_preserve_spans,
            &mut seen_spans,
            structured.get("mustPreserveSpans"),
            GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT,
        );

        let execution_detail = structured.get("executionDetail");
        let mut source_labels = Vec::new();
        let mut seen_labels = BTreeSet::new();
        push_reference_field_values(
            &mut source_labels,
            &mut seen_labels,
            execution_detail,
            "/relationReferences",
            "normalizedAssertion",
            GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT,
        );
        push_reference_field_values(
            &mut source_labels,
            &mut seen_labels,
            execution_detail,
            "/entityReferences",
            "label",
            GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT,
        );
        push_reference_field_values(
            &mut source_labels,
            &mut seen_labels,
            execution_detail,
            "/entityReferences",
            "summary",
            GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT,
        );
        push_reference_field_values(
            &mut source_labels,
            &mut seen_labels,
            execution_detail,
            "/preparedSegmentReferences",
            "documentTitle",
            GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT,
        );

        if answer_excerpt.is_none() && must_preserve_spans.is_empty() && source_labels.is_empty() {
            return None;
        }

        Some(Self {
            index,
            execution_id: structured
                .get("executionId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            final_answer_ready: structured
                .get("finalAnswerReady")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            finalizable: structured.get("finalizable").and_then(Value::as_bool).unwrap_or(false),
            verification_state: execution_detail
                .and_then(|detail| detail.get("verificationState"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            warning_codes: collect_verification_warning_codes(execution_detail),
            unsupported_literal_spans: collect_unsupported_literal_spans(execution_detail),
            answer_body,
            answer_excerpt,
            must_preserve_spans,
            source_labels,
        })
    }

    fn push_to_message(&self, message: &mut String) {
        message.push_str("\n\n[grounded_answer ");
        message.push_str(&self.index.to_string());
        message.push_str("] finalAnswerReady=");
        message.push_str(if self.final_answer_ready { "true" } else { "false" });
        message.push_str(" finalizable=");
        message.push_str(if self.finalizable { "true" } else { "false" });
        if let Some(state) = &self.verification_state {
            message.push_str(" verificationState=");
            message.push_str(state);
        }
        if !self.warning_codes.is_empty() {
            message.push_str(" warnings=");
            message.push_str(&self.warning_codes.join(", "));
        }
        if let Some(excerpt) = &self.answer_excerpt {
            message.push_str("\nanswerExcerpt: ");
            message.push_str(excerpt);
        }
        if !self.must_preserve_spans.is_empty() {
            message.push_str("\nmustPreserveSpans: ");
            message.push_str(&self.must_preserve_spans.join(" | "));
        }
        if !self.source_labels.is_empty() {
            message.push_str("\nsourceLabelsAndEvidenceSummaries: ");
            message.push_str(&self.source_labels.join(" | "));
        }
    }

    fn can_guard_final_answer(&self) -> bool {
        self.final_answer_ready
            || self.finalizable
            || self.verification_state.as_deref() == Some(GROUNDED_ANSWER_VERIFICATION_VERIFIED)
            || self.has_guardable_partial_inventory()
    }

    fn has_guardable_partial_inventory(&self) -> bool {
        if self.answer_body.as_deref().map(str::trim).unwrap_or_default().is_empty() {
            return false;
        }
        if self.verification_state.as_deref() == Some("conflicting") {
            return false;
        }
        if self.warning_codes.iter().any(|code| code != "unsupported_literal") {
            return false;
        }
        self.must_preserve_spans
            .iter()
            .filter(|span| {
                let anchor = single_line_text(span);
                let anchor = anchor.trim();
                !self.unsupported_literal_spans.contains(anchor)
                    && is_high_signal_grounded_answer_anchor(anchor)
            })
            .take(GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_MISSING_HIGH_SIGNAL_ANCHORS + 1)
            .count()
            > GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_MISSING_HIGH_SIGNAL_ANCHORS
    }
}

/// Build the LLM-facing tool definitions from the MCP
/// descriptors. MCP JSON-RPC and in-process UI agent calls therefore
/// share one schema source of truth, and — given the same
/// [`ToolVisibilityCapabilities`] — the same visible tool set. This is
/// what keeps the UI agent at 1:1 tool parity with external MCP clients,
/// including `view_document_image` when the running agent model is
/// vision-capable.
pub(crate) fn answer_surface_tool_defs(
    auth: &AuthContext,
    capabilities: ToolVisibilityCapabilities,
) -> Vec<ChatToolDef> {
    tools::visible_tool_names_with_capabilities(auth, McpToolSurface::Answer, capabilities)
        .into_iter()
        .filter_map(|name| tools::descriptor_for(&name))
        .map(|descriptor| ChatToolDef {
            name: descriptor.name.to_string(),
            description: descriptor.description.to_string(),
            parameters: descriptor.input_schema,
        })
        .collect()
}

/// Run the web UI assistant as a native tool-using agent over the
/// answer MCP surface. The model chooses tools, receives real tool
/// results, can fan out independent calls within one iteration, and
/// can refine the next query from prior results.
pub async fn run_mcp_tool_agent_turn(
    input: McpToolAgentTurnInput<'_>,
) -> Result<AgentTurnResult, AgentTurnFailure> {
    let binding = input
        .state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(input.state, input.library_id, AiBindingPurpose::Agent)
        .await
        .map_err(|e| {
            AgentTurnFailure::empty(anyhow::anyhow!("failed to resolve agent binding: {e}"))
        })?
        .ok_or_else(|| {
            AgentTurnFailure::empty(anyhow::anyhow!(
                "no active agent binding configured for library {}",
                input.library_id
            ))
        })?;

    let provider = ProviderModelSelection {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
    };
    // Tool-surface parity with external MCP clients: expose
    // `view_document_image` exactly when the agent model that runs this
    // turn is vision-capable, mirroring the MCP `tools/list` capability
    // gate. Gate on the turn's resolved binding (not "any visible library
    // has vision") so we never offer the tool to a model that cannot
    // consume image content.
    let agent_vision_available = input
        .state
        .canonical_services
        .ai_catalog
        .get_model_catalog(input.state, binding.model_catalog_id)
        .await
        .map(|model| model.modality_kind == "multimodal")
        .unwrap_or(false);
    let tool_defs =
        answer_surface_tool_defs(input.auth, ToolVisibilityCapabilities { agent_vision_available });
    if tool_defs.is_empty() {
        return Err(AgentTurnFailure::empty(anyhow::anyhow!(
            "no MCP answer tools are visible for the current caller"
        )));
    }

    let iteration_cap = input.iteration_cap.max(1);
    let max_parallel_actions = input.max_parallel_actions.max(1);
    let deadline_started = Instant::now();
    let mut messages = Vec::with_capacity(
        input
            .conversation_history
            .len()
            .saturating_add(input.follow_up_context_messages.len())
            .saturating_add(iteration_cap * 3 + 2),
    );
    messages.push(ChatMessage::system(super::assistant_prompt::render(input.library_ref, None)));
    messages.extend(input.conversation_history.iter().cloned());
    messages.push(ChatMessage::user(input.user_question.to_string()));
    messages.extend(input.follow_up_context_messages.iter().cloned());
    if let Some(reminder) = latest_user_verbatim_fragment_reminder(input.user_question) {
        messages.push(ChatMessage::system(reminder));
    }

    let mut usage_json = serde_json::json!({});
    let mut debug_iterations = Vec::new();
    let mut total_tool_call_count = 0usize;
    let mut successful_tool_call_count = 0usize;
    let mut successful_tool_names = BTreeSet::new();
    let mut seen_effective_tool_payloads = BTreeMap::new();
    let mut assistant_grounding = AssistantGroundingEvidence::default();
    let mut child_query_execution_ids = Vec::new();
    let mut stopped_reason = AgentStopReason::IterationCap;
    let mut last_required_tool_refusal_answer: Option<String> = None;
    let mut verified_grounded_answer_count = 0usize;
    let mut last_verified_grounded_answer: Option<String> = None;
    let mut verified_grounded_answer_guard_text: Option<String> = None;
    let mut last_completed_grounded_answer: Option<String> = None;
    let mut grounded_answer_evidence_ledger = GroundedAnswerEvidenceLedger::default();
    let mut incomplete_grounded_answer_needs_follow_up = false;
    // There is no hidden post-loop synthesis pass: the model must spend
    // one of these iterations on a final answer after seeing tool results.
    // The caller budgets one extra iteration beyond the tool-round cap.
    for iteration in 1..=iteration_cap {
        let Some(deadline_budget) = deadline_remaining(deadline_started, input.deadline) else {
            stopped_reason = AgentStopReason::Deadline;
            break;
        };

        let force_final_answer = force_final_answer_iteration(
            iteration,
            iteration_cap,
            total_tool_call_count,
            successful_tool_call_count,
            verified_grounded_answer_count,
            &successful_tool_names,
            incomplete_grounded_answer_needs_follow_up,
            deadline_started,
            input.soft_final_answer_deadline,
        );
        let require_tool_call = should_require_tool_call_before_final(
            force_final_answer,
            &tool_defs,
            &successful_tool_names,
            incomplete_grounded_answer_needs_follow_up,
        );
        let tools_for_iteration =
            tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, force_final_answer);
        let mut request_messages = final_answer_request_messages(&messages, force_final_answer);
        emit_activity(
            &input.activity_tx,
            AgentLoopActivityEvent::ModelRequest {
                iteration,
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
            },
        );
        let model_call_started = std::time::Instant::now();
        let mut response = match tokio::time::timeout(
            deadline_budget,
            input.state.llm_gateway.generate_with_tools(ToolUseRequest {
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
                api_key_override: binding.api_key.clone(),
                base_url_override: binding.provider_base_url.clone(),
                temperature: binding.temperature,
                top_p: binding.top_p,
                max_output_tokens_override: binding.max_output_tokens_override,
                messages: request_messages.clone(),
                tools: tools_for_iteration,
                extra_parameters_json: binding.extra_parameters_json.clone(),
                require_tool_call,
            }),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                tracing::warn!(
                    provider = %binding.provider_kind,
                    model = %binding.model_name,
                    iteration,
                    max_output_tokens_override = ?binding.max_output_tokens_override,
                    error = %error,
                    "MCP-backed assistant agent provider call failed"
                );
                return Err(AgentTurnFailure::with_loop(
                    error.context("MCP-backed assistant agent LLM call failed"),
                    debug_iterations.clone(),
                    agent_loop_metadata(
                        iteration_cap,
                        input.deadline,
                        AgentStopReason::ProviderError,
                        total_tool_call_count,
                    ),
                ));
            }
            Err(_) => {
                return Err(AgentTurnFailure::with_loop(
                    anyhow::anyhow!(
                        "assistant agent exceeded its turn deadline while waiting for the model"
                    ),
                    debug_iterations.clone(),
                    agent_loop_metadata(
                        iteration_cap,
                        input.deadline,
                        AgentStopReason::Deadline,
                        total_tool_call_count,
                    ),
                ));
            }
        };
        merge_usage_into(&mut usage_json, &response.usage_json);
        let mut model_call_duration_ms =
            model_call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        if force_final_answer && !response.tool_calls.is_empty() {
            tracing::warn!(
                request_id = input.request_id,
                library_id = %input.library_id,
                iteration,
                tool_call_count = response.tool_calls.len(),
                has_output_text = !response.output_text.trim().is_empty(),
                "assistant agent provider returned tool calls during forced-final iteration"
            );
            if response.output_text.trim().is_empty() {
                let Some(retry_deadline_remaining) =
                    deadline_remaining(deadline_started, input.deadline)
                else {
                    stopped_reason = AgentStopReason::Deadline;
                    break;
                };
                let retry_messages = final_answer_retry_messages(
                    &request_messages,
                    response.reasoning_content.clone(),
                    &response.tool_calls,
                );
                emit_activity(
                    &input.activity_tx,
                    AgentLoopActivityEvent::ModelRequest {
                        iteration,
                        provider_kind: binding.provider_kind.clone(),
                        model_name: binding.model_name.clone(),
                    },
                );
                let retry_started = std::time::Instant::now();
                let retry_response = match tokio::time::timeout(
                    retry_deadline_remaining,
                    input.state.llm_gateway.generate_with_tools(ToolUseRequest {
                        provider_kind: binding.provider_kind.clone(),
                        model_name: binding.model_name.clone(),
                        api_key_override: binding.api_key.clone(),
                        base_url_override: binding.provider_base_url.clone(),
                        temperature: binding.temperature,
                        top_p: binding.top_p,
                        max_output_tokens_override: binding.max_output_tokens_override,
                        messages: retry_messages.clone(),
                        tools: Vec::new(),
                        extra_parameters_json: binding.extra_parameters_json.clone(),
                        require_tool_call: false,
                    }),
                )
                .await
                {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => {
                        tracing::warn!(
                            provider = %binding.provider_kind,
                            model = %binding.model_name,
                            iteration,
                            max_output_tokens_override = ?binding.max_output_tokens_override,
                            error = %error,
                            "MCP-backed assistant agent forced-final retry failed"
                        );
                        return Err(AgentTurnFailure::with_loop(
                            error.context("MCP-backed assistant agent forced-final retry failed"),
                            debug_iterations.clone(),
                            agent_loop_metadata(
                                iteration_cap,
                                input.deadline,
                                AgentStopReason::ProviderError,
                                total_tool_call_count,
                            ),
                        ));
                    }
                    Err(_) => {
                        return Err(AgentTurnFailure::with_loop(
                            anyhow::anyhow!(
                                "assistant agent exceeded its turn deadline while waiting for the forced-final retry"
                            ),
                            debug_iterations.clone(),
                            agent_loop_metadata(
                                iteration_cap,
                                input.deadline,
                                AgentStopReason::Deadline,
                                total_tool_call_count,
                            ),
                        ));
                    }
                };
                merge_usage_into(&mut usage_json, &retry_response.usage_json);
                model_call_duration_ms = model_call_duration_ms.saturating_add(
                    retry_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                );
                request_messages = retry_messages;
                response = retry_response;
                if !response.tool_calls.is_empty() {
                    tracing::warn!(
                        request_id = input.request_id,
                        library_id = %input.library_id,
                        iteration,
                        tool_call_count = response.tool_calls.len(),
                        "assistant agent forced-final retry still returned tool calls; discarding them"
                    );
                    response.tool_calls.clear();
                }
            } else {
                response.tool_calls.clear();
            }
        }

        if response.tool_calls.is_empty() {
            let answer = response.output_text.trim().to_string();
            emit_activity(
                &input.activity_tx,
                AgentLoopActivityEvent::ModelResponse {
                    iteration,
                    provider_kind: binding.provider_kind.clone(),
                    model_name: binding.model_name.clone(),
                    tool_call_count: 0,
                    has_final_answer: !answer.is_empty(),
                },
            );
            if answer.is_empty() {
                return Err(AgentTurnFailure::with_loop(
                    anyhow::anyhow!("assistant agent returned an empty final answer"),
                    debug_iterations,
                    agent_loop_metadata(
                        iteration_cap,
                        input.deadline,
                        AgentStopReason::ProviderError,
                        total_tool_call_count,
                    ),
                ));
            }
            debug_iterations.push(LlmIterationDebug {
                iteration,
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
                request_messages,
                response_text: Some(answer.clone()),
                response_tool_calls: Vec::new(),
                usage: response.usage_json,
                duration_ms: Some(model_call_duration_ms),
                child_runtime_execution_ids: Vec::new(),
                child_query_execution_ids: Vec::new(),
            });
            if require_tool_call {
                if last_required_tool_refusal_answer.is_some() || iteration == iteration_cap {
                    stopped_reason = AgentStopReason::FinalAnswer;
                    let prior_answer_guard = prior_assistant_answer_fallback_candidate(
                        input.grounded_answer_tool_history,
                        input.conversation_history,
                    );
                    let ledger_guard =
                        grounded_answer_evidence_ledger.guard_candidate_for_answer(&answer);
                    let grounded_guard = ledger_guard
                        .as_deref()
                        .or_else(|| {
                            verified_grounded_answer_guard_candidate(
                                last_verified_grounded_answer.as_deref(),
                                verified_grounded_answer_guard_text.as_deref(),
                                verified_grounded_answer_count,
                                successful_tool_call_count,
                            )
                        })
                        .or_else(|| {
                            prior_assistant_answer_guard_for_final_answer(
                                &answer,
                                prior_answer_guard.as_deref(),
                            )
                        });
                    let answer = finalize_agent_loop_answer(
                        answer,
                        input.user_question,
                        grounded_guard,
                        input.request_id,
                        input.library_id,
                    );
                    return Ok(AgentTurnResult {
                        answer,
                        provider,
                        usage_json,
                        iterations: debug_iterations.len(),
                        assistant_grounding,
                        child_query_execution_ids,
                        debug_iterations,
                        agent_loop: Some(agent_loop_metadata(
                            iteration_cap,
                            input.deadline,
                            stopped_reason,
                            total_tool_call_count,
                        )),
                    });
                }
                last_required_tool_refusal_answer = Some(answer.clone());
                messages.push(ChatMessage::assistant_text(answer));
                messages.push(ChatMessage::system(tool_requirement_reminder()));
                continue;
            }
            stopped_reason = AgentStopReason::FinalAnswer;
            let prior_answer_guard = prior_assistant_answer_fallback_candidate(
                input.grounded_answer_tool_history,
                input.conversation_history,
            );
            let ledger_guard = grounded_answer_evidence_ledger.guard_candidate_for_answer(&answer);
            let grounded_guard = ledger_guard
                .as_deref()
                .or_else(|| {
                    verified_grounded_answer_guard_candidate(
                        last_verified_grounded_answer.as_deref(),
                        verified_grounded_answer_guard_text.as_deref(),
                        verified_grounded_answer_count,
                        successful_tool_call_count,
                    )
                })
                .or_else(|| {
                    prior_assistant_answer_guard_for_final_answer(
                        &answer,
                        prior_answer_guard.as_deref(),
                    )
                });
            let answer = finalize_agent_loop_answer(
                answer,
                input.user_question,
                grounded_guard,
                input.request_id,
                input.library_id,
            );
            return Ok(AgentTurnResult {
                answer,
                provider,
                usage_json,
                iterations: debug_iterations.len(),
                assistant_grounding,
                child_query_execution_ids,
                debug_iterations,
                agent_loop: Some(agent_loop_metadata(
                    iteration_cap,
                    input.deadline,
                    stopped_reason,
                    total_tool_call_count,
                )),
            });
        }

        let tool_calls = response.tool_calls.clone();
        emit_activity(
            &input.activity_tx,
            AgentLoopActivityEvent::ModelResponse {
                iteration,
                provider_kind: binding.provider_kind.clone(),
                model_name: binding.model_name.clone(),
                tool_call_count: tool_calls.len(),
                has_final_answer: false,
            },
        );
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: (!response.output_text.trim().is_empty())
                .then(|| response.output_text.trim().to_string()),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: tool_calls.clone(),
            tool_call_id: None,
            name: None,
        });

        let outcomes = execute_tool_calls(
            input.clone(),
            iteration,
            &tool_calls,
            max_parallel_actions,
            deadline_started,
            &mut seen_effective_tool_payloads,
        )
        .await;
        total_tool_call_count = total_tool_call_count.saturating_add(tool_calls.len());

        let mut response_tool_calls = Vec::with_capacity(tool_calls.len());
        let mut child_runtime_execution_ids = Vec::new();
        let mut iteration_child_query_execution_ids = Vec::new();
        let mut iteration_had_incomplete_grounded_answer = false;
        let mut iteration_had_follow_up_after_incomplete_grounded_answer = false;
        for (call, outcome) in tool_calls.iter().zip(outcomes.iter()) {
            child_query_execution_ids.extend(outcome.child_query_execution_ids.iter().copied());
            iteration_child_query_execution_ids
                .extend(outcome.child_query_execution_ids.iter().copied());
            child_runtime_execution_ids.extend(outcome.child_runtime_execution_ids.iter().copied());
            if !outcome.is_error {
                successful_tool_call_count = successful_tool_call_count.saturating_add(1);
                successful_tool_names.insert(call.name.clone());
                if incomplete_grounded_answer_needs_follow_up
                    && tool_outcome_satisfies_incomplete_grounded_follow_up(&call.name, outcome)
                {
                    iteration_had_follow_up_after_incomplete_grounded_answer = true;
                }
                if outcome.grounded_answer_ready {
                    verified_grounded_answer_count =
                        verified_grounded_answer_count.saturating_add(1);
                    last_verified_grounded_answer = remember_verified_grounded_answer(
                        last_verified_grounded_answer,
                        &call.name,
                        &outcome,
                    );
                    verified_grounded_answer_guard_text =
                        remember_verified_grounded_answer_guard_text(
                            verified_grounded_answer_guard_text,
                            &call.name,
                            &outcome,
                        );
                }
                if outcome.grounded_answer_completed {
                    last_completed_grounded_answer = remember_completed_grounded_answer(
                        last_completed_grounded_answer,
                        &call.name,
                        &outcome,
                    );
                }
                if outcome.grounded_answer_needs_follow_up {
                    iteration_had_incomplete_grounded_answer = true;
                }
                grounded_answer_evidence_ledger.remember(&call.name, outcome);
                if let Some(grounding_text) = &outcome.grounding_text {
                    push_tool_grounding_fragment(
                        &mut assistant_grounding,
                        &call.name,
                        grounding_text,
                    );
                }
            }
            response_tool_calls.push(ResponseToolCallDebug {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments_json: outcome
                    .arguments_json
                    .clone()
                    .unwrap_or_else(|| call.arguments_json.clone()),
                requested_arguments_json: outcome.requested_arguments_json.clone(),
                result_text: outcome.result_text.clone(),
                result_json: outcome.result_json.clone(),
                is_error: outcome.is_error,
                duration_ms: Some(outcome.duration_ms),
            });
            messages.push(ChatMessage::tool_result(
                call.id.clone(),
                call.name.clone(),
                outcome.message_content.clone(),
            ));
        }
        if let Some(ledger_message) = grounded_answer_evidence_ledger.system_message() {
            messages.push(ChatMessage::system(ledger_message));
        }
        if let Some(reminder) = latest_user_verbatim_fragment_reminder(input.user_question) {
            messages.push(ChatMessage::system(reminder));
        }

        debug_iterations.push(LlmIterationDebug {
            iteration,
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            request_messages,
            response_text: (!response.output_text.trim().is_empty())
                .then(|| response.output_text.trim().to_string()),
            response_tool_calls,
            usage: response.usage_json,
            duration_ms: Some(model_call_duration_ms),
            child_runtime_execution_ids,
            child_query_execution_ids: iteration_child_query_execution_ids,
        });

        let next_incomplete_grounded_answer_needs_follow_up =
            next_incomplete_grounded_answer_follow_up_required(
                incomplete_grounded_answer_needs_follow_up,
                iteration_had_incomplete_grounded_answer,
                iteration_had_follow_up_after_incomplete_grounded_answer,
            );
        if next_incomplete_grounded_answer_needs_follow_up
            != incomplete_grounded_answer_needs_follow_up
        {
            tracing::debug!(
                request_id = input.request_id,
                library_id = %input.library_id,
                iteration,
                previous = incomplete_grounded_answer_needs_follow_up,
                iteration_had_incomplete_grounded_answer,
                iteration_had_follow_up_after_incomplete_grounded_answer,
                next = next_incomplete_grounded_answer_needs_follow_up,
                "query.agent_loop.grounded_answer_follow_up_state"
            );
        }
        incomplete_grounded_answer_needs_follow_up =
            next_incomplete_grounded_answer_needs_follow_up;
    }

    if matches!(stopped_reason, AgentStopReason::IterationCap)
        && successful_tool_call_count == 0
        && total_tool_call_count > 0
    {
        stopped_reason = AgentStopReason::ToolError;
    }

    if matches!(stopped_reason, AgentStopReason::IterationCap)
        && let Some(answer) = verified_grounded_answer_fallback_candidate(
            last_verified_grounded_answer.as_deref(),
            verified_grounded_answer_count,
            successful_tool_call_count,
        )
    {
        let answer = ensure_user_fragments_visible(answer.to_string(), input.user_question);
        return Ok(AgentTurnResult {
            answer,
            provider,
            usage_json,
            iterations: debug_iterations.len(),
            assistant_grounding,
            child_query_execution_ids,
            debug_iterations,
            agent_loop: Some(agent_loop_metadata(
                iteration_cap,
                input.deadline,
                AgentStopReason::IterationCap,
                total_tool_call_count,
            )),
        });
    }

    if should_return_completed_grounded_answer_on_iteration_cap(
        stopped_reason,
        successful_tool_call_count,
        last_completed_grounded_answer.as_deref(),
    ) {
        let answer = last_completed_grounded_answer.unwrap_or_default();
        let answer = ensure_user_fragments_visible(answer, input.user_question);
        return Ok(AgentTurnResult {
            answer,
            provider,
            usage_json,
            iterations: debug_iterations.len(),
            assistant_grounding,
            child_query_execution_ids,
            debug_iterations,
            agent_loop: Some(agent_loop_metadata(
                iteration_cap,
                input.deadline,
                AgentStopReason::IterationCap,
                total_tool_call_count,
            )),
        });
    }

    if matches!(stopped_reason, AgentStopReason::IterationCap)
        && successful_tool_call_count > 0
        && successful_tool_names.iter().any(|name| tool_result_can_ground_final_answer(name))
        && let Some(answer) = prior_assistant_answer_fallback_candidate(
            input.grounded_answer_tool_history,
            input.conversation_history,
        )
    {
        let answer = ensure_user_fragments_visible(answer, input.user_question);
        return Ok(AgentTurnResult {
            answer,
            provider,
            usage_json,
            iterations: debug_iterations.len(),
            assistant_grounding,
            child_query_execution_ids,
            debug_iterations,
            agent_loop: Some(agent_loop_metadata(
                iteration_cap,
                input.deadline,
                AgentStopReason::IterationCap,
                total_tool_call_count,
            )),
        });
    }

    let mut message = match stopped_reason {
        AgentStopReason::Deadline => {
            "assistant agent exceeded its turn deadline before producing a final answer"
        }
        AgentStopReason::IterationCap => {
            "assistant agent reached its iteration cap before producing a final answer"
        }
        AgentStopReason::FinalAnswer => "assistant agent stopped before producing a final answer",
        AgentStopReason::ToolError => "assistant agent stopped after a tool error",
        AgentStopReason::ProviderError => "assistant agent stopped after a provider error",
    }
    .to_string();
    if successful_tool_call_count == 0 && total_tool_call_count > 0 {
        message.push_str("; no successful MCP tool result was received");
    }
    Err(AgentTurnFailure::with_loop(
        anyhow::anyhow!(message),
        debug_iterations,
        agent_loop_metadata(iteration_cap, input.deadline, stopped_reason, total_tool_call_count),
    ))
}

fn agent_loop_metadata(
    iteration_cap: usize,
    deadline: Duration,
    stopped_reason: AgentStopReason,
    total_tool_call_count: usize,
) -> AgentLoopMetadata {
    AgentLoopMetadata {
        iteration_cap,
        deadline_ms: deadline.as_millis().try_into().unwrap_or(u64::MAX),
        stopped_reason,
        tool_call_count: total_tool_call_count,
    }
}

fn force_final_answer_iteration(
    iteration: usize,
    iteration_cap: usize,
    total_tool_call_count: usize,
    successful_tool_call_count: usize,
    verified_grounded_answer_count: usize,
    successful_tool_names: &BTreeSet<String>,
    incomplete_grounded_answer_needs_follow_up: bool,
    started: Instant,
    soft_final_answer_deadline: Option<Duration>,
) -> bool {
    if iteration == iteration_cap && total_tool_call_count > 0 {
        return true;
    }
    if incomplete_grounded_answer_needs_follow_up {
        return false;
    }
    if verified_grounded_answer_count > 0
        && successful_tool_call_count >= SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS
        && successful_tool_names.contains(READ_DOCUMENT_TOOL_NAME)
        && has_composite_tool_signal(successful_tool_names)
    {
        return true;
    }
    let Some(soft_deadline) = soft_final_answer_deadline else {
        return false;
    };
    if started.elapsed() < soft_deadline {
        return false;
    }

    verified_grounded_answer_count > 0
        && (successful_tool_call_count >= SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS
            || has_composite_tool_signal(successful_tool_names))
}

fn should_require_tool_call_before_final(
    force_final_answer: bool,
    tool_defs: &[ChatToolDef],
    successful_tool_names: &BTreeSet<String>,
    incomplete_grounded_answer_needs_follow_up: bool,
) -> bool {
    if force_final_answer || tool_defs.is_empty() {
        return false;
    }
    if incomplete_grounded_answer_needs_follow_up {
        return true;
    }
    successful_tool_names.is_empty()
}

fn next_incomplete_grounded_answer_follow_up_required(
    previous_required: bool,
    iteration_had_incomplete_grounded_answer: bool,
    iteration_had_follow_up_after_incomplete_grounded_answer: bool,
) -> bool {
    iteration_had_incomplete_grounded_answer
        || (previous_required && !iteration_had_follow_up_after_incomplete_grounded_answer)
}

fn tool_outcome_satisfies_incomplete_grounded_follow_up(
    tool_name: &str,
    outcome: &ToolExecutionOutcome,
) -> bool {
    tool_result_satisfies_incomplete_grounded_follow_up(
        tool_name,
        outcome.is_error,
        outcome.grounded_answer_ready,
        outcome.grounding_text.as_deref(),
    )
}

fn tool_result_satisfies_incomplete_grounded_follow_up(
    tool_name: &str,
    is_error: bool,
    grounded_answer_ready: bool,
    grounding_text: Option<&str>,
) -> bool {
    if is_error {
        return false;
    }
    if grounded_answer_ready {
        return true;
    }
    if tool_name == GROUNDED_ANSWER_TOOL_NAME || !tool_result_can_ground_final_answer(tool_name) {
        return false;
    }
    grounding_text.is_some_and(|text| !text.trim().is_empty())
}

fn tool_defs_for_agent_iteration(
    tool_defs: &[ChatToolDef],
    _successful_tool_names: &BTreeSet<String>,
    force_final_answer: bool,
) -> Vec<ChatToolDef> {
    if force_final_answer || tool_defs.is_empty() {
        return Vec::new();
    }
    tool_defs.to_vec()
}

fn has_composite_tool_signal(successful_tool_names: &BTreeSet<String>) -> bool {
    let categories = [
        successful_tool_names.iter().any(|name| is_document_content_tool(name)),
        successful_tool_names.iter().any(|name| is_graph_content_tool(name)),
        successful_tool_names.iter().any(|name| is_runtime_content_tool(name)),
        successful_tool_names.contains(GROUNDED_ANSWER_TOOL_NAME),
    ];
    categories.into_iter().filter(|present| *present).count() >= 2
}

fn is_document_content_tool(tool_name: &str) -> bool {
    matches!(tool_name, SEARCH_DOCUMENTS_TOOL_NAME | READ_DOCUMENT_TOOL_NAME)
}

fn is_graph_content_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "search_entities" | "get_graph_topology" | "list_relations" | "get_communities"
    )
}

fn is_runtime_content_tool(tool_name: &str) -> bool {
    matches!(tool_name, "get_runtime_execution" | "get_runtime_execution_trace")
}

fn tool_requirement_reminder() -> String {
    "Before writing the final answer, call at least one MCP tool and inspect the result. Use any relevant visible tool, and do not repeat an identical argument payload.".to_string()
}

fn final_answer_request_messages(
    messages: &[ChatMessage],
    force_final_answer: bool,
) -> Vec<ChatMessage> {
    let mut request_messages = messages.to_vec();
    if force_final_answer {
        request_messages.push(ChatMessage::system(final_answer_required_reminder()));
    }
    request_messages
}

fn final_answer_required_reminder() -> String {
    "No more MCP tool calls are available in this turn. Write the final answer now from the tool results already in the transcript. If the evidence is partial, answer only the supported parts and mark missing parts plainly.".to_string()
}

fn final_answer_retry_messages(
    request_messages: &[ChatMessage],
    reasoning_content: Option<String>,
    tool_calls: &[ChatToolCall],
) -> Vec<ChatMessage> {
    let mut retry_messages = request_messages.to_vec();
    retry_messages.push(ChatMessage::assistant_with_reasoning_and_tool_calls(
        reasoning_content,
        tool_calls.to_vec(),
    ));
    for call in tool_calls {
        retry_messages.push(ChatMessage::tool_result(
            call.id.clone(),
            call.name.clone(),
            final_answer_tool_unavailable_result(),
        ));
    }
    retry_messages.push(ChatMessage::system(final_answer_retry_reminder()));
    retry_messages
}

fn final_answer_tool_unavailable_result() -> String {
    "This tool request was not executed because no more MCP tool calls are available in this turn. Produce the final answer from the prior tool results already in the transcript.".to_string()
}

fn final_answer_retry_reminder() -> String {
    "The previous response requested another tool, but this is the final answer step and tools are unavailable. Do not request tools. Produce the answer from the existing tool results now.".to_string()
}

fn latest_user_verbatim_fragment_reminder(user_question: &str) -> Option<String> {
    let fragments = extract_verbatim_user_fragments(user_question);
    if fragments.is_empty() {
        return None;
    }

    let mut reminder = String::from(
        "Latest-user verbatim fragments that may identify a user-visible value, label, code, quoted phrase, requested output slot, or diagnostic message. These fragments come from the user; use them only to identify what the user asked about, not as evidence for external facts. If the final answer explains one of these items, keep the exact fragment visible before explaining it instead of replacing it with a paraphrase. If a fragment names requested answer slots, keep those slot labels visible when you cover them.",
    );
    for (index, fragment) in fragments.iter().enumerate() {
        reminder.push_str("\n\nFragment ");
        reminder.push_str(&(index + 1).to_string());
        reminder.push_str(":\n```text\n");
        reminder.push_str(fragment);
        reminder.push_str("\n```");
    }
    Some(reminder)
}

fn extract_verbatim_user_fragments(user_question: &str) -> Vec<String> {
    let mut fragments = extract_quoted_verbatim_user_fragments(user_question);
    extend_delimited_user_fragments(user_question, &mut fragments);
    extend_identifier_user_fragments(user_question, &mut fragments);
    fragments
}

fn extract_quoted_verbatim_user_fragments(user_question: &str) -> Vec<String> {
    let mut fragments = Vec::new();
    for (open, close) in USER_FRAGMENT_QUOTE_PAIRS {
        extract_quoted_user_fragments(user_question, open, close, &mut fragments);
        if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
            break;
        }
    }
    fragments
}

fn ensure_user_fragments_visible(answer: String, user_question: &str) -> String {
    let missing = extract_required_visible_user_fragments(user_question)
        .into_iter()
        .filter(|fragment| !fragment.is_visible_in(&answer))
        .map(|fragment| fragment.text)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return answer;
    }

    let mut prefixed = missing.join("\n");
    prefixed.push_str("\n\n");
    prefixed.push_str(answer.trim_start());
    prefixed
}

fn finalize_agent_loop_answer(
    answer: String,
    user_question: &str,
    verified_grounded_answer: Option<&str>,
    request_id: &str,
    library_id: Uuid,
) -> String {
    let answer = prefer_verified_grounded_answer_on_ordered_source_loss(
        answer,
        verified_grounded_answer,
        request_id,
        library_id,
    );
    let answer = prefer_verified_grounded_answer_on_literal_drift(
        answer,
        user_question,
        verified_grounded_answer,
        request_id,
        library_id,
    );
    ensure_user_fragments_visible(answer, user_question)
}

fn verified_grounded_answer_fallback_candidate<'a>(
    last_verified_grounded_answer: Option<&'a str>,
    verified_grounded_answer_count: usize,
    successful_tool_call_count: usize,
) -> Option<&'a str> {
    if verified_grounded_answer_count == 1 && successful_tool_call_count == 1 {
        return last_verified_grounded_answer;
    }
    None
}

fn verified_grounded_answer_guard_candidate<'a>(
    last_verified_grounded_answer: Option<&'a str>,
    verified_grounded_answer_guard_text: Option<&'a str>,
    verified_grounded_answer_count: usize,
    successful_tool_call_count: usize,
) -> Option<&'a str> {
    verified_grounded_answer_fallback_candidate(
        last_verified_grounded_answer,
        verified_grounded_answer_count,
        successful_tool_call_count,
    )
    .or_else(|| {
        if verified_grounded_answer_count > 1 { verified_grounded_answer_guard_text } else { None }
    })
}

fn prior_assistant_answer_fallback_candidate(
    grounded_answer_tool_history: &[ExternalConversationTurn],
    conversation_history: &[ChatMessage],
) -> Option<String> {
    let grounded_candidates = grounded_answer_tool_history
        .iter()
        .rev()
        .filter(|turn| matches!(turn.turn_kind, QueryTurnKind::Assistant))
        .filter_map(|turn| normalize_prior_assistant_answer_fallback(&turn.content_text));
    let chat_candidates = conversation_history
        .iter()
        .rev()
        .filter(|message| message.role == "assistant" || message.role == "system")
        .filter_map(|message| message.content.as_deref())
        .filter_map(normalize_prior_assistant_answer_fallback);

    grounded_candidates
        .chain(chat_candidates)
        .max_by(|left, right| {
            left.anchor_count
                .cmp(&right.anchor_count)
                .then_with(|| left.answer.chars().count().cmp(&right.answer.chars().count()))
        })
        .map(|candidate| candidate.answer)
}

#[derive(Debug, Clone)]
struct PriorAssistantAnswerFallbackCandidate {
    answer: String,
    anchor_count: usize,
}

fn normalize_prior_assistant_answer_fallback(
    value: &str,
) -> Option<PriorAssistantAnswerFallbackCandidate> {
    let answer = strip_compact_literal_memory_line(value);
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return None;
    }
    let anchors = verified_grounded_answer_surface_anchor_set(trimmed);
    (anchors.len() >= PRIOR_ASSISTANT_ANSWER_FALLBACK_MIN_ANCHORS).then(|| {
        PriorAssistantAnswerFallbackCandidate {
            answer: trimmed.to_string(),
            anchor_count: anchors.len(),
        }
    })
}

fn strip_compact_literal_memory_line(value: &str) -> String {
    let mut trimmed = value.trim_start();
    loop {
        let Some((first, rest)) = trimmed.split_once('\n') else {
            return trimmed.to_string();
        };
        let first = first.trim_start();
        if first.starts_with("literals:") && first.contains('`') {
            trimmed = rest.trim_start();
            continue;
        }
        return trimmed.to_string();
    }
}

fn prior_assistant_answer_guard_for_final_answer<'a>(
    answer: &str,
    prior_answer: Option<&'a str>,
) -> Option<&'a str> {
    let prior_answer = prior_answer?;
    let answer_anchor_count = verified_grounded_answer_surface_anchor_set(answer).len();
    if answer_anchor_count >= PRIOR_ASSISTANT_ANSWER_FALLBACK_MIN_ANCHORS {
        return None;
    }
    Some(prior_answer)
}

fn prefer_verified_grounded_answer_on_ordered_source_loss(
    answer: String,
    verified_grounded_answer: Option<&str>,
    request_id: &str,
    library_id: Uuid,
) -> String {
    let Some(verified_grounded_answer) =
        verified_grounded_answer.map(str::trim).filter(|value| !value.is_empty())
    else {
        return answer;
    };
    if !answer_has_ordered_source_markers(verified_grounded_answer) {
        return answer;
    }
    if answer_has_ordered_source_markers(&answer) {
        return answer;
    }
    tracing::debug!(
        request_id,
        library_id = %library_id,
        "query.agent_loop.verified_grounded_ordered_source_guard"
    );
    verified_grounded_answer.to_string()
}

fn answer_has_ordered_source_markers(answer: &str) -> bool {
    let mut marker_count = 0usize;
    for line in answer.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("source=`") || trimmed.contains(" source=`") {
            marker_count = marker_count.saturating_add(1);
            if marker_count >= 2 {
                return true;
            }
        }
    }
    false
}

fn prefer_verified_grounded_answer_on_literal_drift(
    answer: String,
    user_question: &str,
    verified_grounded_answer: Option<&str>,
    request_id: &str,
    library_id: Uuid,
) -> String {
    let Some(verified_grounded_answer) =
        verified_grounded_answer.map(str::trim).filter(|value| !value.is_empty())
    else {
        return answer;
    };

    let grounded_literals = verified_grounded_answer_literal_set(verified_grounded_answer);
    if grounded_literals.len() < VERIFIED_GROUNDED_LITERAL_GUARD_MIN_LITERALS {
        return prefer_verified_grounded_answer_on_surface_anchor_loss(
            answer,
            verified_grounded_answer,
            request_id,
            library_id,
        );
    }
    let answer_literals = verified_grounded_answer_literal_set(&answer);
    let user_literals = verified_grounded_answer_literal_set(user_question);
    let missing_count = grounded_literals.difference(&answer_literals).count();
    let unsupported_count = answer_literals
        .difference(&grounded_literals)
        .filter(|literal| !user_literals.contains(*literal))
        .count();
    if missing_count == 0 && unsupported_count == 0 {
        return prefer_verified_grounded_answer_on_surface_anchor_loss(
            answer,
            verified_grounded_answer,
            request_id,
            library_id,
        );
    }

    tracing::debug!(
        request_id,
        library_id = %library_id,
        grounded_literal_count = grounded_literals.len(),
        answer_literal_count = answer_literals.len(),
        missing_count,
        unsupported_count,
        "query.agent_loop.verified_grounded_literal_guard"
    );
    verified_grounded_answer.to_string()
}

fn prefer_verified_grounded_answer_on_surface_anchor_loss(
    answer: String,
    verified_grounded_answer: &str,
    request_id: &str,
    library_id: Uuid,
) -> String {
    let grounded_anchors = verified_grounded_answer_surface_anchor_set(verified_grounded_answer);
    if grounded_anchors.len() < VERIFIED_GROUNDED_LITERAL_GUARD_MIN_LITERALS {
        return answer;
    }
    let answer_anchors = verified_grounded_answer_surface_anchor_set(&answer);
    let high_signal_missing_count = grounded_anchors
        .iter()
        .filter(|anchor| is_high_signal_grounded_answer_anchor(anchor))
        .filter(|anchor| !answer_anchors.contains(*anchor))
        .count();
    if high_signal_missing_count >= GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_MISSING_HIGH_SIGNAL_ANCHORS {
        tracing::debug!(
            request_id,
            library_id = %library_id,
            grounded_anchor_count = grounded_anchors.len(),
            answer_anchor_count = answer_anchors.len(),
            high_signal_missing_count,
            "query.agent_loop.verified_grounded_high_signal_anchor_guard"
        );
        return verified_grounded_answer.to_string();
    }
    let missing_count = grounded_anchors.difference(&answer_anchors).count();
    let missing_threshold = grounded_anchors.len().div_ceil(3).max(2);
    if missing_count < missing_threshold {
        return answer;
    }

    tracing::debug!(
        request_id,
        library_id = %library_id,
        grounded_anchor_count = grounded_anchors.len(),
        answer_anchor_count = answer_anchors.len(),
        missing_count,
        missing_threshold,
        "query.agent_loop.verified_grounded_surface_anchor_guard"
    );
    verified_grounded_answer.to_string()
}

fn verified_grounded_answer_literal_set(text: &str) -> BTreeSet<String> {
    let mut literals = BTreeSet::new();
    for span in backtick_literal_spans(text) {
        push_verified_grounded_answer_literal_span(&mut literals, &span);
    }
    literals
}

fn verified_grounded_answer_surface_anchor_set(text: &str) -> BTreeSet<String> {
    let mut anchors = verified_grounded_answer_literal_set(text);
    for candidate in split_literal_anchor_candidates(text) {
        let candidate = trim_literal_anchor_candidate(&candidate);
        if is_structural_literal_anchor(candidate) {
            anchors.insert(candidate.to_string());
        }
    }
    anchors
}

fn push_verified_grounded_answer_literal_span(literals: &mut BTreeSet<String>, span: &str) {
    let span = span.trim();
    if span.is_empty() {
        return;
    }
    if span.contains('\n') {
        let mut lines = span.lines();
        if let Some(first_line) = lines.next()
            && !is_probable_code_fence_info(first_line)
        {
            push_verified_grounded_answer_literal_line(literals, first_line);
        }
        for line in lines {
            push_verified_grounded_answer_literal_line(literals, line);
        }
        return;
    }
    push_verified_grounded_answer_literal_candidate(literals, span);
}

fn is_probable_code_fence_info(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty()
        && line.chars().count() <= 32
        && line
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+' | '.' | '#'))
}

fn push_verified_grounded_answer_literal_line(literals: &mut BTreeSet<String>, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    push_verified_grounded_answer_literal_candidate(literals, line);
    if let Some((left, right)) = line.split_once('=') {
        push_verified_grounded_answer_literal_candidate(literals, left.trim());
        push_verified_grounded_answer_literal_candidate(literals, right.trim());
    }
}

fn push_verified_grounded_answer_literal_candidate(
    literals: &mut BTreeSet<String>,
    candidate: &str,
) {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return;
    }
    if is_structural_literal_anchor(candidate) || is_plain_history_code_literal(candidate) {
        literals.insert(candidate.to_string());
    }
}

fn contains_casefolded(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct RequiredVisibleUserFragment {
    text: String,
    match_kind: RequiredVisibleMatchKind,
}

impl RequiredVisibleUserFragment {
    fn casefolded(text: String) -> Self {
        Self { text, match_kind: RequiredVisibleMatchKind::CaseFolded }
    }

    fn exact(text: String) -> Self {
        Self { text, match_kind: RequiredVisibleMatchKind::Exact }
    }

    fn is_visible_in(&self, answer: &str) -> bool {
        match self.match_kind {
            RequiredVisibleMatchKind::CaseFolded => contains_casefolded(answer, &self.text),
            RequiredVisibleMatchKind::Exact => answer.contains(&self.text),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum RequiredVisibleMatchKind {
    CaseFolded,
    Exact,
}

fn extract_required_visible_user_fragments(
    user_question: &str,
) -> Vec<RequiredVisibleUserFragment> {
    let raw_fragments = extract_quoted_verbatim_user_fragments(user_question);
    let mut required =
        raw_fragments.into_iter().map(RequiredVisibleUserFragment::casefolded).collect::<Vec<_>>();
    let mut identifier_fragments = Vec::new();
    extend_identifier_user_fragments(user_question, &mut identifier_fragments);
    for fragment in identifier_fragments {
        if required.iter().any(|existing| existing.text == fragment) {
            continue;
        }
        required.push(RequiredVisibleUserFragment::exact(fragment));
        if required.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
            break;
        }
    }
    required
}

fn extract_quoted_user_fragments(
    user_question: &str,
    open: char,
    close: char,
    fragments: &mut Vec<String>,
) {
    if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
        return;
    }

    if open == close {
        let mut open_at: Option<usize> = None;
        for (index, ch) in user_question.char_indices() {
            if ch != open {
                continue;
            }
            if let Some(start) = open_at.take() {
                push_verbatim_fragment(&user_question[start + open.len_utf8()..index], fragments);
                if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
                    return;
                }
            } else {
                open_at = Some(index);
            }
        }
        return;
    }

    let mut search_from = 0usize;
    while search_from < user_question.len() && fragments.len() < VERBATIM_USER_FRAGMENT_LIMIT {
        let Some(open_offset) = user_question[search_from..].find(open) else {
            return;
        };
        let content_start = search_from + open_offset + open.len_utf8();
        let Some(close_offset) = user_question[content_start..].find(close) else {
            return;
        };
        let close_index = content_start + close_offset;
        push_verbatim_fragment(&user_question[content_start..close_index], fragments);
        search_from = close_index + close.len_utf8();
    }
}

fn push_verbatim_fragment(candidate: &str, fragments: &mut Vec<String>) {
    if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
        return;
    }

    let candidate = candidate.trim();
    let char_count = candidate.chars().count();
    if !(VERBATIM_USER_FRAGMENT_MIN_CHARS..=VERBATIM_USER_FRAGMENT_MAX_CHARS).contains(&char_count)
    {
        return;
    }
    if fragments.iter().any(|existing| existing == candidate) {
        return;
    }
    let total_chars: usize =
        fragments.iter().map(|fragment: &String| fragment.chars().count()).sum();
    if total_chars.saturating_add(char_count) > VERBATIM_USER_FRAGMENT_TOTAL_CHARS {
        return;
    }
    fragments.push(candidate.to_string());
}

fn extend_delimited_user_fragments(user_question: &str, fragments: &mut Vec<String>) {
    if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
        return;
    }

    let mut segment_start = 0usize;
    let mut saw_delimiter = false;
    let mut active_quote_close: Option<char> = None;
    for (index, ch) in user_question.char_indices() {
        if let Some(close) = active_quote_close {
            if ch == close {
                active_quote_close = None;
            }
            continue;
        }
        if let Some(close) = quote_close_for_open(ch) {
            active_quote_close = Some(close);
            continue;
        }
        if !is_verbatim_fragment_delimiter(ch) {
            continue;
        }
        if saw_delimiter {
            push_verbatim_clause(&user_question[segment_start..index], fragments);
            if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
                return;
            }
        }
        saw_delimiter = true;
        segment_start = index + ch.len_utf8();
    }
    if saw_delimiter && segment_start < user_question.len() {
        push_verbatim_clause(&user_question[segment_start..], fragments);
    }
}

fn extend_identifier_user_fragments(user_question: &str, fragments: &mut Vec<String>) {
    if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
        return;
    }

    let mut token_start: Option<usize> = None;
    for (index, ch) in user_question.char_indices() {
        if is_identifier_fragment_char(ch) {
            token_start.get_or_insert(index);
            continue;
        }
        if let Some(start) = token_start.take() {
            push_identifier_fragment(&user_question[start..index], fragments);
            if fragments.len() >= VERBATIM_USER_FRAGMENT_LIMIT {
                return;
            }
        }
    }
    if let Some(start) = token_start {
        push_identifier_fragment(&user_question[start..], fragments);
    }
}

fn is_identifier_fragment_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/')
}

fn push_identifier_fragment(candidate: &str, fragments: &mut Vec<String>) {
    let candidate = candidate.trim_matches(|ch: char| matches!(ch, '.' | '-' | '_' | '/'));
    if !is_identifier_shaped_fragment(candidate) {
        return;
    }
    if fragments.iter().any(|existing| existing.contains(candidate)) {
        return;
    }
    push_verbatim_fragment(candidate, fragments);
}

fn is_identifier_shaped_fragment(candidate: &str) -> bool {
    let char_count = candidate.chars().count();
    if !(VERBATIM_USER_FRAGMENT_MIN_CHARS..=VERBATIM_USER_FRAGMENT_MAX_CHARS).contains(&char_count)
    {
        return false;
    }
    let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
    if alnum_count < 2 {
        return false;
    }
    let has_underscore = candidate.contains('_');
    let has_hyphen = candidate.contains('-');
    let has_dot = candidate.contains('.');
    let has_slash = candidate.contains('/');
    if !(has_underscore || has_hyphen || has_dot || has_slash) {
        return false;
    }
    if has_underscore {
        return true;
    }
    if has_slash {
        return candidate.starts_with('/')
            || has_dot
            || candidate.chars().any(|ch| ch.is_numeric());
    }
    let has_digit = candidate.chars().any(|ch| ch.is_numeric());
    let has_upper = candidate.chars().any(|ch| ch.is_uppercase());
    let has_lower = candidate.chars().any(|ch| ch.is_lowercase());
    let all_cased_upper = has_upper && !has_lower;
    if has_hyphen {
        return has_digit || all_cased_upper;
    }
    has_dot && (has_digit || all_cased_upper)
}

fn is_verbatim_fragment_delimiter(ch: char) -> bool {
    matches!(ch, ',' | ';' | ':' | '(' | ')' | '[' | ']' | '\n')
}

fn quote_close_for_open(ch: char) -> Option<char> {
    match ch {
        '«' => Some('»'),
        '“' => Some('”'),
        '„' => Some('“'),
        '「' => Some('」'),
        '『' => Some('』'),
        '‹' => Some('›'),
        '"' => Some('"'),
        '`' => Some('`'),
        _ => None,
    }
}

fn push_verbatim_clause(candidate: &str, fragments: &mut Vec<String>) {
    let candidate = candidate.trim();
    if candidate.chars().all(|ch| !ch.is_alphanumeric()) {
        return;
    }
    push_verbatim_fragment(candidate, fragments);
}

async fn execute_tool_calls(
    input: McpToolAgentTurnInput<'_>,
    iteration: usize,
    tool_calls: &[ChatToolCall],
    max_parallel_actions: usize,
    deadline_started: Instant,
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadState>,
) -> Vec<ToolExecutionOutcome> {
    let mut outcomes: Vec<Option<ToolExecutionOutcome>> = vec![None; tool_calls.len()];
    let single_tool_iteration = tool_calls.len() == 1;
    let pending_calls = prepare_agent_tool_calls_with_context(
        tool_calls,
        input.user_question,
        input.contextual_follow_up,
        single_tool_iteration,
        input.grounded_answer_top_k,
        input.library_ref,
        input.grounded_answer_tool_history,
        seen_effective_payloads,
        &mut outcomes,
    );

    let pending_results = stream::iter(pending_calls)
        .map(|pending| {
            let input = input.clone();
            async move {
                let started_at = Instant::now();
                emit_activity(
                    &input.activity_tx,
                    AgentLoopActivityEvent::ToolCallStarted {
                        iteration,
                        tool_name: pending.call.name.clone(),
                    },
                );
                let mut outcome = match deadline_remaining(deadline_started, input.deadline) {
                    Some(_) => {
                        execute_one_tool_call(&input, &pending.call, single_tool_iteration).await
                    }
                    None => tool_execution_error(format!(
                        "tool '{}' was not started because the assistant turn deadline expired",
                        pending.call.name
                    )),
                };
                let elapsed_ms = started_at.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                outcome.duration_ms = elapsed_ms;
                emit_activity(
                    &input.activity_tx,
                    AgentLoopActivityEvent::ToolCallFinished {
                        iteration,
                        tool_name: pending.call.name.clone(),
                        elapsed_ms,
                        is_error: outcome.is_error,
                        child_execution_id: outcome.child_query_execution_ids.first().copied(),
                        result_preview: outcome.result_text.as_deref().map(activity_result_preview),
                    },
                );
                (pending.index, pending.fingerprint, outcome)
            }
        })
        .buffer_unordered(max_parallel_actions)
        .collect::<Vec<_>>()
        .await;

    for (pending_index, fingerprint, outcome) in pending_results {
        if let Some(fingerprint) = fingerprint {
            record_effective_tool_payload_outcome(seen_effective_payloads, fingerprint, &outcome);
        }
        outcomes[pending_index] = Some(outcome);
    }

    outcomes
        .into_iter()
        .map(|outcome| {
            outcome.unwrap_or_else(|| {
                tool_execution_error("internal agent tool dispatcher did not return a result")
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum EffectiveToolPayloadState {
    InFlight,
    Completed,
}

#[derive(Debug, Clone)]
struct PendingAgentToolCall {
    index: usize,
    call: ChatToolCall,
    fingerprint: Option<String>,
}

#[cfg(test)]
fn prepare_agent_tool_calls(
    tool_calls: &[ChatToolCall],
    user_question: &str,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadState>,
    outcomes: &mut [Option<ToolExecutionOutcome>],
) -> Vec<PendingAgentToolCall> {
    prepare_agent_tool_calls_with_context(
        tool_calls,
        user_question,
        false,
        false,
        grounded_top_k,
        library_ref,
        grounded_answer_tool_history,
        seen_effective_payloads,
        outcomes,
    )
}

fn prepare_agent_tool_calls_with_context(
    tool_calls: &[ChatToolCall],
    user_question: &str,
    contextual_follow_up: bool,
    single_tool_iteration: bool,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadState>,
    outcomes: &mut [Option<ToolExecutionOutcome>],
) -> Vec<PendingAgentToolCall> {
    tool_calls
        .iter()
        .cloned()
        .enumerate()
        .filter_map(|(pending_index, call)| {
            let fingerprint = effective_tool_call_fingerprint_with_context(
                &call.name,
                &call.arguments_json,
                user_question,
                contextual_follow_up,
                single_tool_iteration,
                grounded_top_k,
                library_ref,
                grounded_answer_tool_history,
            );
            if let Some(fingerprint) = &fingerprint {
                if seen_effective_payloads.contains_key(fingerprint) {
                    outcomes[pending_index] =
                        Some(tool_execution_error(duplicate_tool_call_message(&call.name)));
                    return None;
                }
                seen_effective_payloads
                    .insert(fingerprint.clone(), EffectiveToolPayloadState::InFlight);
            }
            Some(PendingAgentToolCall { index: pending_index, call, fingerprint })
        })
        .collect::<Vec<_>>()
}

fn record_effective_tool_payload_outcome(
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadState>,
    fingerprint: String,
    outcome: &ToolExecutionOutcome,
) {
    if outcome.is_error {
        seen_effective_payloads.remove(&fingerprint);
        return;
    }
    seen_effective_payloads.insert(fingerprint, EffectiveToolPayloadState::Completed);
}

fn duplicate_tool_call_message(tool_name: &str) -> String {
    format!(
        "duplicate MCP tool call suppressed: `{tool_name}` already used the same effective arguments in this model iteration; vary the sub-question, change arguments, or choose another MCP tool"
    )
}

fn emit_activity(sender: &Option<Sender<AgentLoopActivityEvent>>, event: AgentLoopActivityEvent) {
    if let Some(sender) = sender {
        // Diagnostic activity must not back-pressure tool execution; the
        // authoritative transcript is still persisted in debug_iterations.
        let _ = sender.try_send(event);
    }
}

async fn execute_one_tool_call(
    input: &McpToolAgentTurnInput<'_>,
    call: &ChatToolCall,
    single_tool_iteration: bool,
) -> ToolExecutionOutcome {
    let mut arguments = match serde_json::from_str::<Value>(&call.arguments_json) {
        Ok(arguments) => arguments,
        Err(error) => {
            return tool_execution_error(format!("invalid tool arguments JSON: {error}"));
        }
    };
    let requested_arguments = arguments.clone();
    normalize_agent_tool_argument_types(&call.name, &mut arguments);
    if let Err(message) =
        validate_agent_tool_library_scope(&call.name, &arguments, input.library_ref)
    {
        return tool_execution_error(message);
    }
    apply_agent_tool_argument_defaults_with_context(
        &call.name,
        &mut arguments,
        input.user_question,
        input.contextual_follow_up,
        single_tool_iteration,
        input.grounded_answer_top_k,
        input.library_ref,
        input.grounded_answer_tool_history,
    );

    let context = ToolCallContext {
        auth: input.auth,
        state: input.state,
        request_id: input.request_id,
        surface_kind: RuntimeSurfaceKind::Mcp,
    };
    let Some(result) = tools::call_named_tool(&call.name, context, &arguments).await else {
        return tool_execution_error(format!("unsupported MCP answer tool '{}'", call.name));
    };

    let result_text = tool_result_answer_text(&call.name, &result.content);
    let child_query_execution_ids =
        extract_child_query_execution_ids(&call.name, &result.structured_content);
    let child_runtime_execution_ids =
        extract_child_runtime_execution_ids(&call.name, &result.structured_content);
    let is_error = result.is_error;
    let message_content = tool_result_model_message(&call.name, &result);
    let grounding_text = tool_result_verification_text(&call.name, &result);
    let grounded_answer_ready = grounded_answer_ready(&call.name, &result);
    let grounded_answer_completed = grounded_answer_completed(&call.name, &result);
    let grounded_answer_needs_follow_up = grounded_answer_needs_follow_up(&call.name, &result);
    let result_json = Some(debug_tool_result_json(&result));
    let arguments_json = Some(arguments.to_string());
    let requested_arguments_json =
        (requested_arguments != arguments).then(|| call.arguments_json.clone());

    ToolExecutionOutcome {
        arguments_json,
        requested_arguments_json,
        message_content,
        result_text,
        result_json,
        grounding_text,
        grounded_answer_ready,
        grounded_answer_completed,
        grounded_answer_needs_follow_up,
        is_error,
        duration_ms: 0,
        child_query_execution_ids,
        child_runtime_execution_ids,
    }
}

fn validate_agent_tool_library_scope(
    tool_name: &str,
    arguments: &Value,
    library_ref: &str,
) -> Result<(), String> {
    let Value::Object(object) = arguments else {
        return Ok(());
    };
    if tool_uses_single_library_scope(tool_name)
        && let Some(requested) = object.get("library").and_then(Value::as_str)
        && requested != library_ref
    {
        return Err(format!(
            "tool argument library scope mismatch: {tool_name} requested library `{requested}`, but this UI assistant session is scoped to `{library_ref}`"
        ));
    }
    if tool_name == SEARCH_DOCUMENTS_TOOL_NAME
        && let Some(requested_libraries) = object.get("libraries")
    {
        let Some(items) = requested_libraries.as_array() else {
            return Err(format!(
                "tool argument library scope mismatch: {tool_name} `libraries` must be an array scoped to `{library_ref}`"
            ));
        };
        if !items.is_empty()
            && (items.len() != 1 || items.first().and_then(Value::as_str) != Some(library_ref))
        {
            let requested = items
                .iter()
                .map(|item| item.as_str().unwrap_or("<non-string>"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "tool argument library scope mismatch: {tool_name} requested libraries [{requested}], but this UI assistant session is scoped to `{library_ref}`"
            ));
        }
    }
    Ok(())
}

fn normalize_agent_tool_argument_types(tool_name: &str, arguments: &mut Value) {
    let Value::Object(object) = arguments else {
        return;
    };
    if tool_name == SEARCH_DOCUMENTS_TOOL_NAME {
        normalize_stringified_json_array_field(object, "libraries");
    }
    for field in ["limit", "topK", "startOffset", "length", "maxBytes"] {
        normalize_unsigned_integer_string_field(object, field);
    }
}

fn normalize_stringified_json_array_field(
    object: &mut serde_json::Map<String, Value>,
    field: &str,
) {
    let Some(raw) = object.get(field).and_then(Value::as_str).map(str::trim) else {
        return;
    };
    if !raw.starts_with('[') {
        return;
    }
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    if parsed.is_array() {
        object.insert(field.to_string(), parsed);
    }
}

fn normalize_unsigned_integer_string_field(
    object: &mut serde_json::Map<String, Value>,
    field: &str,
) {
    let Some(raw) = object.get(field).and_then(Value::as_str).map(str::trim) else {
        return;
    };
    let Ok(value) = raw.parse::<u64>() else {
        return;
    };
    object.insert(field.to_string(), serde_json::json!(value));
}

#[cfg(test)]
fn apply_agent_tool_argument_defaults(
    tool_name: &str,
    arguments: &mut Value,
    user_question: &str,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) {
    apply_agent_tool_argument_defaults_with_context(
        tool_name,
        arguments,
        user_question,
        false,
        false,
        grounded_top_k,
        library_ref,
        grounded_answer_tool_history,
    );
}

fn apply_agent_tool_argument_defaults_with_context(
    tool_name: &str,
    arguments: &mut Value,
    user_question: &str,
    contextual_follow_up: bool,
    single_tool_iteration: bool,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) {
    let Value::Object(object) = arguments else {
        return;
    };
    if tool_uses_single_library_scope(tool_name) {
        object.insert("library".to_string(), serde_json::json!(library_ref));
    }
    if tool_name == SEARCH_DOCUMENTS_TOOL_NAME {
        object.insert("libraries".to_string(), serde_json::json!([library_ref]));
        if contextual_follow_up
            && let Some(query) = object.get("query").and_then(Value::as_str)
            && let Some(compact_query) = contextual_search_documents_query(
                query,
                user_question,
                grounded_answer_tool_history,
            )
        {
            object.insert("query".to_string(), serde_json::json!(compact_query));
        }
    }
    let bounded_top_k = grounded_top_k.max(1);
    if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        let model_requested_history = object
            .get("conversationTurns")
            .and_then(Value::as_array)
            .is_some_and(|turns| !turns.is_empty());
        let use_contextual_history = contextual_follow_up || model_requested_history;
        let contextual_history = use_contextual_history.then_some(grounded_answer_tool_history);
        if let Some(query) = object.get("query").and_then(Value::as_str) {
            let compact_query = if use_contextual_history {
                compact_history_padded_grounded_answer_query(
                    query,
                    user_question,
                    grounded_answer_tool_history,
                )
            } else if single_tool_iteration {
                compact_standalone_single_grounded_answer_query(query, user_question).or_else(
                    || {
                        compact_non_contextual_history_scoped_grounded_answer_query(
                            query,
                            user_question,
                            grounded_answer_tool_history,
                        )
                    },
                )
            } else {
                compact_non_contextual_history_scoped_grounded_answer_query(
                    query,
                    user_question,
                    grounded_answer_tool_history,
                )
            };
            if let Some(compact_query) = compact_query {
                object.insert("query".to_string(), serde_json::json!(compact_query));
            }
        }
        let requested_top_k = object
            .get("topK")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok());
        let turns =
            grounded_answer_conversation_turn_defaults(contextual_history.unwrap_or_default());
        object.insert("conversationTurns".to_string(), Value::Array(turns));
        let has_contextual_turns = contextual_history.is_some_and(|history| !history.is_empty());
        let effective_top_k = resolve_agent_grounded_answer_top_k(
            requested_top_k,
            has_contextual_turns,
            bounded_top_k,
        );
        if requested_top_k != Some(effective_top_k) {
            object.insert("topK".to_string(), serde_json::json!(effective_top_k));
        }
        return;
    }

    if agent_tool_limit_cap(tool_name).is_some() {
        // Static tool caps are ceilings; the parent turn's top-k budget
        // tightens them further so UI-agent subqueries cannot fan out wider
        // than the turn that spawned them.
        let bounded_limit = agent_tool_limit_cap(tool_name)
            .unwrap_or(bounded_top_k)
            .min(bounded_top_k.max(8))
            .max(1);
        let requested_limit = object
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok());
        if requested_limit.is_none_or(|value| value > bounded_limit) {
            object.insert("limit".to_string(), serde_json::json!(bounded_limit));
        }
    }
}

fn resolve_agent_grounded_answer_top_k(
    requested_top_k: Option<usize>,
    has_contextual_turns: bool,
    max_top_k: usize,
) -> usize {
    resolve_contextual_grounded_answer_top_k(requested_top_k, has_contextual_turns, max_top_k)
        .max(crate::domains::query::DEFAULT_TOP_K.min(max_top_k.max(1)))
}

#[cfg(test)]
fn effective_tool_call_fingerprint(
    tool_name: &str,
    arguments_json: &str,
    user_question: &str,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> Option<String> {
    effective_tool_call_fingerprint_with_context(
        tool_name,
        arguments_json,
        user_question,
        false,
        false,
        grounded_top_k,
        library_ref,
        grounded_answer_tool_history,
    )
}

fn effective_tool_call_fingerprint_with_context(
    tool_name: &str,
    arguments_json: &str,
    user_question: &str,
    contextual_follow_up: bool,
    single_tool_iteration: bool,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> Option<String> {
    let mut arguments = serde_json::from_str::<Value>(arguments_json).ok()?;
    normalize_agent_tool_argument_types(tool_name, &mut arguments);
    validate_agent_tool_library_scope(tool_name, &arguments, library_ref).ok()?;
    apply_agent_tool_argument_defaults_with_context(
        tool_name,
        &mut arguments,
        user_question,
        contextual_follow_up,
        single_tool_iteration,
        grounded_top_k,
        library_ref,
        grounded_answer_tool_history,
    );
    let canonical = canonical_json_value(&arguments);
    let serialized = serde_json::to_string(&canonical).ok()?;
    Some(format!("{tool_name}:{serialized}"))
}

fn canonical_json_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_json_value).collect()),
        Value::Object(object) => {
            let mut sorted = serde_json::Map::new();
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                if let Some(item) = object.get(key) {
                    sorted.insert(key.clone(), canonical_json_value(item));
                }
            }
            Value::Object(sorted)
        }
        _ => value.clone(),
    }
}

fn compact_history_padded_grounded_answer_query(
    query: &str,
    user_question: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> Option<String> {
    if !should_replace_history_padded_grounded_answer_query(
        query,
        user_question,
        grounded_answer_tool_history,
    ) {
        return None;
    }

    let user_question = user_question.trim();
    let mut anchors = ordered_query_literal_anchors(query, user_question);
    extend_history_code_literals_present_in_query(
        &mut anchors,
        query,
        user_question,
        grounded_answer_tool_history,
    );

    if anchors.is_empty() {
        return Some(user_question.to_string());
    }

    let mut compact = user_question.to_string();
    let mut appended_anchor = false;
    for anchor in anchors {
        let prefix = if appended_anchor { " " } else { "\n@: " };
        let candidate = format!("{compact}{prefix}{anchor};");
        if candidate.chars().count() > HISTORY_PADDED_QUERY_MAX_CHARS {
            continue;
        }
        compact = candidate;
        appended_anchor = true;
    }
    if !appended_anchor {
        return Some(user_question.to_string());
    }
    Some(compact)
}

fn compact_non_contextual_history_scoped_grounded_answer_query(
    query: &str,
    user_question: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> Option<String> {
    if grounded_answer_tool_history.is_empty() {
        return None;
    }
    let query = query.trim();
    let user_question = user_question.trim();
    if query.is_empty() || user_question.is_empty() || query == user_question {
        return None;
    }
    if !query_mentions_user_question(query, user_question) {
        return None;
    }
    if query.chars().count() <= user_question.chars().count().saturating_add(24) {
        return None;
    }
    if history_added_query_token_overlap(query, user_question, grounded_answer_tool_history) < 2 {
        return None;
    }
    Some(user_question.to_string())
}

fn compact_standalone_single_grounded_answer_query(
    query: &str,
    user_question: &str,
) -> Option<String> {
    let query = query.trim();
    let user_question = user_question.trim();
    if query.is_empty() || user_question.is_empty() || query == user_question {
        return None;
    }
    let query_chars = query.chars().count();
    let user_chars = user_question.chars().count();
    if !query_mentions_user_question(query, user_question) {
        return None;
    }
    let query_tokens =
        normalized_alnum_token_sequence(query, 3).into_iter().collect::<BTreeSet<_>>();
    let user_tokens =
        normalized_alnum_token_sequence(user_question, 3).into_iter().collect::<BTreeSet<_>>();
    if query_tokens.is_empty() {
        return None;
    }
    let shared_count = query_tokens.iter().filter(|token| user_tokens.contains(*token)).count();
    let query_only_count = query_tokens.len().saturating_sub(shared_count);
    let user_only_count = user_tokens.iter().filter(|token| !query_tokens.contains(*token)).count();
    if user_only_count == 0 {
        return None;
    }
    if query_chars.saturating_mul(100) > user_chars.saturating_mul(115) {
        return None;
    }
    if query_chars.saturating_mul(100) > user_chars.saturating_mul(85) && user_only_count < 2 {
        return None;
    }
    if query_only_count > user_only_count.max(2) {
        return None;
    }
    Some(user_question.to_string())
}

fn contextual_search_documents_query(
    query: &str,
    user_question: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> Option<String> {
    let query = query.trim();
    if query.is_empty() || grounded_answer_tool_history.is_empty() {
        return None;
    }
    if history_added_query_token_overlap(query, user_question, grounded_answer_tool_history)
        >= CONTEXTUAL_SEARCH_QUERY_HISTORY_OVERLAP_SELF_SUFFICIENT
    {
        return None;
    }
    let anchors =
        contextual_search_history_anchors(query, user_question, grounded_answer_tool_history);
    if anchors.len() < CONTEXTUAL_SEARCH_QUERY_MIN_ANCHORS {
        return None;
    }

    let mut compact = query.to_string();
    let mut appended_anchor = false;
    for anchor in anchors {
        let prefix = if appended_anchor { " " } else { "\n@: " };
        let candidate = format!("{compact}{prefix}{anchor};");
        if candidate.chars().count() > HISTORY_PADDED_QUERY_MAX_CHARS {
            continue;
        }
        compact = candidate;
        appended_anchor = true;
    }
    appended_anchor.then_some(compact)
}

fn contextual_search_history_anchors(
    query: &str,
    user_question: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> Vec<String> {
    let mut structural = Vec::new();
    let mut identifier = Vec::new();
    let mut seen = BTreeSet::new();
    for turn in grounded_answer_tool_history.iter().rev() {
        for span in backtick_literal_spans(&turn.content_text) {
            for candidate in split_literal_anchor_candidates(&span) {
                let candidate = candidate.trim();
                if candidate.is_empty()
                    || query_contains_literal(query, candidate)
                    || query_contains_literal(user_question, candidate)
                {
                    continue;
                }
                let key = candidate.to_lowercase();
                if !seen.insert(key) {
                    continue;
                }
                if is_structural_literal_anchor(candidate) {
                    structural.push(candidate.to_string());
                } else if contextual_search_plain_anchor(candidate) {
                    identifier.push(candidate.to_string());
                }
                if structural.len().saturating_add(identifier.len())
                    >= CONTEXTUAL_SEARCH_QUERY_ANCHOR_LIMIT
                {
                    break;
                }
            }
            if structural.len().saturating_add(identifier.len())
                >= CONTEXTUAL_SEARCH_QUERY_ANCHOR_LIMIT
            {
                break;
            }
        }
        if structural.len().saturating_add(identifier.len()) >= CONTEXTUAL_SEARCH_QUERY_ANCHOR_LIMIT
        {
            break;
        }
    }
    structural.extend(identifier);
    structural.truncate(CONTEXTUAL_SEARCH_QUERY_ANCHOR_LIMIT);
    structural
}

fn contextual_search_plain_anchor(candidate: &str) -> bool {
    is_plain_history_code_literal(candidate) && literal_text_is_identifier_shaped(candidate)
}

fn should_replace_history_padded_grounded_answer_query(
    query: &str,
    user_question: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> bool {
    if grounded_answer_tool_history.is_empty() {
        return false;
    }
    let query = query.trim();
    let user_question = user_question.trim();
    if query.is_empty() || user_question.is_empty() || query == user_question {
        return false;
    }
    if !query_mentions_user_question(query, user_question) {
        return false;
    }
    let query_chars = query.chars().count();
    let user_chars = user_question.chars().count();
    if query_chars <= user_chars.saturating_add(80) {
        return false;
    }
    let query_markers = technical_surface_marker_count(query);
    let user_markers = technical_surface_marker_count(user_question);
    if query_markers < user_markers.saturating_add(8) {
        return false;
    }
    history_added_query_token_overlap(query, user_question, grounded_answer_tool_history) >= 3
}

fn query_mentions_user_question(query: &str, user_question: &str) -> bool {
    if query.to_lowercase().contains(&user_question.to_lowercase()) {
        return true;
    }
    let query_tokens = normalized_alnum_token_sequence(query, 3);
    let user_tokens = normalized_alnum_token_sequence(user_question, 3);
    if user_tokens.is_empty() || query_tokens.is_empty() {
        return false;
    }
    let overlap = user_tokens.iter().filter(|token| query_tokens.contains(token)).count();
    overlap >= user_tokens.len().min(3)
}

fn history_added_query_token_overlap(
    query: &str,
    user_question: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) -> usize {
    let user_tokens =
        normalized_alnum_token_sequence(user_question, 3).into_iter().collect::<BTreeSet<_>>();
    let history_tokens = grounded_answer_tool_history
        .iter()
        .flat_map(|turn| normalized_alnum_token_sequence(&turn.content_text, 3))
        .collect::<BTreeSet<_>>();

    normalized_alnum_token_sequence(query, 3)
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|token| !user_tokens.contains(token) && history_tokens.contains(token))
        .count()
}

fn ordered_query_literal_anchors(query: &str, user_question: &str) -> Vec<String> {
    let mut anchors = Vec::new();
    let mut seen = BTreeSet::new();
    for candidate in split_literal_anchor_candidates(query) {
        push_literal_anchor_candidate(&mut anchors, &mut seen, candidate, user_question);
        if anchors.len() >= HISTORY_PADDED_QUERY_ANCHOR_LIMIT {
            break;
        }
    }
    anchors
}

fn extend_history_code_literals_present_in_query(
    anchors: &mut Vec<String>,
    query: &str,
    user_question: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) {
    if anchors.len() >= HISTORY_PADDED_QUERY_ANCHOR_LIMIT {
        return;
    }
    let mut seen = anchors.iter().map(|anchor| anchor.to_lowercase()).collect::<BTreeSet<_>>();
    let query_lower = query.to_lowercase();
    for turn in grounded_answer_tool_history {
        for span in backtick_literal_spans(&turn.content_text) {
            for candidate in split_literal_anchor_candidates(&span) {
                if !query_lower.contains(&candidate.to_lowercase()) {
                    continue;
                }
                push_history_code_literal_candidate(anchors, &mut seen, candidate, user_question);
                if anchors.len() >= HISTORY_PADDED_QUERY_ANCHOR_LIMIT {
                    return;
                }
            }
        }
    }
}

fn split_literal_anchor_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut token_start: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        if is_literal_anchor_boundary(ch) {
            if let Some(start) = token_start.take() {
                push_split_literal_anchor_candidate(&mut candidates, &text[start..index]);
            }
            continue;
        }
        token_start.get_or_insert(index);
    }
    if let Some(start) = token_start {
        push_split_literal_anchor_candidate(&mut candidates, &text[start..]);
    }
    candidates
}

fn is_literal_anchor_boundary(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            ',' | ';'
                | '('
                | ')'
                | '{'
                | '}'
                | '«'
                | '»'
                | '“'
                | '”'
                | '„'
                | '‹'
                | '›'
                | '"'
                | '\''
                | '`'
        )
}

fn push_split_literal_anchor_candidate(candidates: &mut Vec<String>, raw: &str) {
    let candidate = trim_literal_anchor_candidate(raw);
    if !candidate.is_empty() {
        candidates.push(candidate.to_string());
    }
}

fn trim_literal_anchor_candidate(raw: &str) -> &str {
    raw.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                ',' | ';'
                    | '.'
                    | ')'
                    | '('
                    | '{'
                    | '}'
                    | '"'
                    | '\''
                    | '`'
                    | '«'
                    | '»'
                    | '“'
                    | '”'
                    | '„'
                    | '‹'
                    | '›'
            )
    })
}

fn push_literal_anchor_candidate(
    anchors: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    candidate: String,
    user_question: &str,
) {
    if !is_structural_literal_anchor(&candidate) {
        return;
    }
    push_literal_anchor(anchors, seen, candidate, user_question);
}

fn push_history_code_literal_candidate(
    anchors: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    candidate: String,
    user_question: &str,
) {
    if !is_structural_literal_anchor(&candidate) && !is_plain_history_code_literal(&candidate) {
        return;
    }
    push_literal_anchor(anchors, seen, candidate, user_question);
}

fn push_literal_anchor(
    anchors: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    candidate: String,
    user_question: &str,
) {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return;
    }
    let char_count = candidate.chars().count();
    if !(2..=HISTORY_PADDED_QUERY_ANCHOR_MAX_CHARS).contains(&char_count) {
        return;
    }
    if query_contains_literal(user_question, candidate) {
        return;
    }
    let key = candidate.to_lowercase();
    if seen.insert(key) {
        anchors.push(candidate.to_string());
    }
}

fn is_structural_literal_anchor(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return false;
    }
    let char_count = candidate.chars().count();
    if !(2..=HISTORY_PADDED_QUERY_ANCHOR_MAX_CHARS).contains(&char_count) {
        return false;
    }
    let bracket_inner =
        candidate.strip_prefix('[').and_then(|value| value.strip_suffix(']')).map(str::trim);
    if let Some(inner) = bracket_inner
        && !inner.is_empty()
        && inner.chars().any(|ch| ch.is_alphanumeric())
    {
        return true;
    }
    let lower = candidate.to_ascii_lowercase();
    if (lower.starts_with("http://") || lower.starts_with("https://"))
        && candidate.chars().any(|ch| ch.is_alphanumeric())
    {
        return true;
    }
    if (candidate.starts_with('/') || candidate.starts_with('\\'))
        && candidate.chars().any(|ch| ch.is_alphanumeric())
    {
        return true;
    }
    if candidate.contains('/') || candidate.contains('\\') {
        return candidate.chars().any(|ch| ch.is_alphanumeric());
    }
    if candidate.contains('=') {
        return candidate.split_once('=').is_some_and(|(left, right)| {
            !left.trim().is_empty()
                && !right.trim().is_empty()
                && left.trim().chars().any(|ch| ch.is_alphanumeric())
        });
    }
    let unwrapped =
        candidate.trim_matches('[').trim_matches(']').trim_matches('`').trim_matches('"');
    literal_text_is_identifier_shaped(unwrapped) || is_identifier_shaped_fragment(unwrapped)
}

fn is_plain_history_code_literal(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate.chars().any(char::is_whitespace) {
        return false;
    }
    let char_count = candidate.chars().count();
    if !(2..=HISTORY_PADDED_QUERY_ANCHOR_MAX_CHARS).contains(&char_count) {
        return false;
    }
    let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
    alnum_count >= 2
        && candidate.chars().all(|ch| {
            ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\' | ':' | '=')
        })
}

fn query_contains_literal(query: &str, literal: &str) -> bool {
    query.to_lowercase().contains(&literal.to_lowercase())
}

fn backtick_literal_spans(text: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut span_start: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        if ch != '`' {
            continue;
        }
        if let Some(start) = span_start.take() {
            if start < index {
                spans.push(text[start..index].to_string());
            }
        } else {
            span_start = Some(index + ch.len_utf8());
        }
    }
    spans
}

fn technical_surface_marker_count(value: &str) -> usize {
    value
        .chars()
        .filter(|ch| {
            matches!(ch, '`' | '/' | '\\' | '[' | ']' | '_' | ':' | '=' | '.' | '<' | '>' | '"')
        })
        .count()
}

fn grounded_answer_conversation_turn_defaults(
    conversation_turns: &[ExternalConversationTurn],
) -> Vec<Value> {
    conversation_turns
        .iter()
        .filter_map(|turn| {
            let role = match turn.turn_kind {
                QueryTurnKind::User => "user",
                QueryTurnKind::Assistant => "assistant",
                QueryTurnKind::System | QueryTurnKind::Tool => return None,
            };
            let content = turn.content_text.trim();
            if content.is_empty() {
                return None;
            }
            Some(serde_json::json!({
                "role": role,
                "content": content,
            }))
        })
        .collect()
}

fn tool_uses_single_library_scope(tool_name: &str) -> bool {
    matches!(
        tool_name,
        GROUNDED_ANSWER_TOOL_NAME
            | "list_documents"
            | "search_entities"
            | "get_graph_topology"
            | "list_relations"
            | "get_communities"
    )
}

fn activity_result_preview(text: &str) -> String {
    text.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).take(240).collect()
}

fn agent_tool_limit_cap(tool_name: &str) -> Option<usize> {
    match tool_name {
        SEARCH_DOCUMENTS_TOOL_NAME | "search_entities" => Some(12),
        "list_relations" | "get_communities" => Some(16),
        "get_graph_topology" => Some(24),
        _ => None,
    }
}

fn push_tool_grounding_fragment(
    grounding: &mut AssistantGroundingEvidence,
    tool_name: &str,
    message_content: &str,
) {
    let trimmed = message_content.trim();
    if trimmed.is_empty() {
        return;
    }
    let existing_chars = grounding
        .verification_corpus
        .iter()
        .map(|fragment| fragment.chars().count())
        .sum::<usize>();
    if existing_chars >= TOOL_GROUNDING_TOTAL_CHAR_LIMIT {
        return;
    }
    let remaining = TOOL_GROUNDING_TOTAL_CHAR_LIMIT - existing_chars;
    let fragment_limit = TOOL_GROUNDING_FRAGMENT_CHAR_LIMIT.min(remaining);
    let fragment = trimmed.chars().take(fragment_limit).collect::<String>();
    grounding.verification_corpus.push(format!("[MCP tool result: {tool_name}]\n{fragment}"));
}

fn compact_ledger_text(value: &str) -> String {
    single_line_text(value).chars().take(GROUNDED_EVIDENCE_LEDGER_ANSWER_CHARS).collect()
}

fn push_guard_answer_line(lines: &mut Vec<String>, seen: &mut BTreeSet<String>, value: &str) {
    if lines.len() >= GROUNDED_EVIDENCE_LEDGER_ENTRY_LIMIT {
        return;
    }
    let text = value.trim();
    if text.is_empty() || !text.chars().any(char::is_alphanumeric) {
        return;
    }
    if seen.insert(text.to_string()) {
        lines.push(text.to_string());
    }
}

fn push_grounded_evidence_ledger_anchors(anchors: &mut BTreeSet<String>, values: &[String]) {
    for value in values {
        let anchor = single_line_text(value);
        let anchor = anchor.trim();
        if anchor.chars().count() >= 4 && anchor.chars().count() <= 240 {
            anchors.insert(anchor.to_string());
        }
        if anchors.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
            break;
        }
    }
}

fn push_grounded_evidence_ledger_high_signal_anchors(
    anchors: &mut BTreeSet<String>,
    values: &[String],
    unsupported_literals: &BTreeSet<String>,
) {
    for value in values {
        let anchor = single_line_text(value);
        let anchor = anchor.trim();
        if unsupported_literals.contains(anchor) {
            continue;
        }
        if is_high_signal_grounded_answer_anchor(anchor) {
            anchors.insert(anchor.to_string());
        }
        if anchors.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
            break;
        }
    }
}

fn is_high_signal_grounded_answer_anchor(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.chars().count() < 4 || trimmed.chars().count() > 240 {
        return false;
    }
    trimmed.contains('=')
        || trimmed.contains('/')
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || literal_text_is_identifier_shaped(trimmed)
}

fn answer_contains_guard_anchor(answer: &str, anchor: &str) -> bool {
    answer.to_lowercase().contains(&anchor.to_lowercase())
}

fn collect_unsupported_literal_spans(execution_detail: Option<&Value>) -> BTreeSet<String> {
    let mut literals = BTreeSet::new();
    let Some(warnings) = execution_detail
        .and_then(|detail| detail.get("verificationWarnings"))
        .and_then(Value::as_array)
    else {
        return literals;
    };
    for warning in warnings {
        if warning.get("code").and_then(Value::as_str) != Some("unsupported_literal") {
            continue;
        }
        let Some(message) = warning.get("message").and_then(Value::as_str) else {
            continue;
        };
        for span in backtick_literal_spans(message) {
            let literal = single_line_text(&span);
            if !literal.trim().is_empty() {
                literals.insert(literal);
            }
        }
    }
    literals
}

fn single_line_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn push_json_string_array(
    values: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    value: Option<&Value>,
    limit: usize,
) {
    let Some(Value::Array(items)) = value else {
        return;
    };
    for item in items {
        let Some(text) = item.as_str() else {
            continue;
        };
        push_ledger_string(values, seen, text, limit);
        if values.len() >= limit {
            break;
        }
    }
}

fn push_reference_field_values(
    values: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    execution_detail: Option<&Value>,
    array_pointer: &str,
    field_name: &str,
    limit: usize,
) {
    let Some(Value::Array(items)) =
        execution_detail.and_then(|detail| detail.pointer(array_pointer))
    else {
        return;
    };
    for item in items {
        let Some(text) = item.get(field_name).and_then(Value::as_str) else {
            continue;
        };
        push_ledger_string(values, seen, text, limit);
        if values.len() >= limit {
            break;
        }
    }
}

fn push_ledger_string(
    values: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    value: &str,
    limit: usize,
) {
    if values.len() >= limit {
        return;
    }
    let text = single_line_text(value);
    let text = text.trim();
    if text.is_empty() || !text.chars().any(|ch| ch.is_alphanumeric()) {
        return;
    }
    let text = text.chars().take(GROUNDED_EVIDENCE_LEDGER_ANSWER_CHARS).collect::<String>();
    if seen.insert(text.clone()) {
        values.push(text);
    }
}

fn collect_verification_warning_codes(execution_detail: Option<&Value>) -> Vec<String> {
    let mut codes = Vec::new();
    let mut seen = BTreeSet::new();
    let Some(Value::Array(warnings)) =
        execution_detail.and_then(|detail| detail.get("verificationWarnings"))
    else {
        return codes;
    };
    for warning in warnings {
        let Some(code) = warning.get("code").and_then(Value::as_str) else {
            continue;
        };
        push_ledger_string(&mut codes, &mut seen, code, 12);
    }
    codes
}

fn tool_execution_error(message: impl Into<String>) -> ToolExecutionOutcome {
    let message = message.into();
    let result_json = serde_json::json!({
        "content": [{
            "type": "text",
            "text": message.clone()
        }],
        "structuredContent": {
            "errorKind": "agent_tool_call",
            "message": message.clone()
        },
        "isError": true
    });
    ToolExecutionOutcome {
        arguments_json: None,
        requested_arguments_json: None,
        message_content: serde_json::to_string(&result_json).unwrap_or_else(|_| {
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": "failed to serialize agent tool error"
                }],
                "structuredContent": {
                    "errorKind": "serialization"
                },
                "isError": true
            })
            .to_string()
        }),
        result_text: result_json
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        result_json: Some(result_json),
        grounding_text: None,
        grounded_answer_ready: false,
        grounded_answer_completed: false,
        grounded_answer_needs_follow_up: false,
        is_error: true,
        duration_ms: 0,
        child_query_execution_ids: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
    }
}

fn deadline_remaining(started: Instant, deadline: Duration) -> Option<Duration> {
    deadline.checked_sub(started.elapsed()).filter(|remaining| !remaining.is_zero())
}

fn debug_tool_result_json(result: &crate::interfaces::http::mcp::McpToolResult) -> Value {
    let full = serde_json::to_value(result).unwrap_or_else(|error| {
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("failed to serialize MCP tool result: {error}")
            }],
            "structuredContent": {
                "errorKind": "serialization",
                "message": error.to_string()
            },
            "isError": true
        })
    });
    let serialized = match serde_json::to_string(&full) {
        Ok(serialized) => serialized,
        Err(_) => return full,
    };
    if serialized.chars().count() <= TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT {
        return full;
    }
    serde_json::json!({
        "content": serde_json::to_value(&result.content).unwrap_or_else(|_| serde_json::json!([])),
        "structuredContent": {
            "truncated": true,
            "jsonPrefix": compact_json_string(
                &result.structured_content,
                TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT
            ),
            "originalCharCount": serialized.chars().count()
        },
        "isError": result.is_error
    })
}

fn compact_json_string(value: &Value, char_limit: usize) -> String {
    let serialized = match serde_json::to_string(value) {
        Ok(serialized) => serialized,
        Err(error) => {
            return serde_json::json!({
                "truncated": true,
                "errorKind": "serialization",
                "message": error.to_string()
            })
            .to_string();
        }
    };
    if serialized.chars().count() <= char_limit {
        serialized
    } else {
        serde_json::json!({
            "truncated": true,
            "jsonPrefix": serialized.chars().take(char_limit).collect::<String>(),
            "originalCharCount": serialized.chars().count()
        })
        .to_string()
    }
}

fn tool_result_model_message(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> String {
    let content_text =
        tool_result_preview_with_limit(&result.content, tool_model_content_char_limit(tool_name))
            .unwrap_or_default();
    let structured_content = if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        compact_grounded_answer_structured_content_for_model(
            &result.structured_content,
            TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT,
        )
    } else {
        compact_structured_content_for_model(&result.structured_content)
    };
    serde_json::to_string(&serde_json::json!({
        "content": [{
            "type": "text",
            "text": content_text
        }],
        "structuredContent": structured_content,
        "isError": result.is_error
    }))
    .unwrap_or_else(|error| {
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("failed to serialize compact tool result: {error}")
            }],
            "structuredContent": {
                "errorKind": "serialization",
                "message": error.to_string()
            },
            "isError": true
        })
        .to_string()
    })
}

fn tool_result_verification_text(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<String> {
    if result.is_error || !tool_result_can_ground_final_answer(tool_name) {
        return None;
    }
    let content_text =
        tool_result_preview_with_limit(&result.content, TOOL_VERIFICATION_CONTENT_CHAR_LIMIT)
            .unwrap_or_default();
    let structured_content = if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        compact_grounded_answer_structured_content_for_verification(
            &result.structured_content,
            TOOL_VERIFICATION_STRUCTURED_JSON_CHAR_LIMIT,
        )
    } else {
        compact_structured_content_for_verification(&result.structured_content)
    };
    let structured_text = serde_json::to_string_pretty(&structured_content)
        .unwrap_or_else(|error| format!("failed to serialize compact structured content: {error}"));
    let mut sections = Vec::new();
    if !content_text.trim().is_empty() {
        sections.push(format!("content:\n{}", content_text.trim()));
    }
    if structured_content != Value::Null && structured_content != serde_json::json!({}) {
        sections.push(format!("structuredContent:\n{structured_text}"));
    }
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

fn tool_result_can_ground_final_answer(tool_name: &str) -> bool {
    matches!(tool_name, GROUNDED_ANSWER_TOOL_NAME)
        || is_document_content_tool(tool_name)
        || is_graph_content_tool(tool_name)
        || is_runtime_content_tool(tool_name)
}

fn tool_model_content_char_limit(tool_name: &str) -> usize {
    match tool_name {
        GROUNDED_ANSWER_TOOL_NAME => TOOL_MODEL_GROUNDED_ANSWER_CONTENT_CHAR_LIMIT,
        READ_DOCUMENT_TOOL_NAME => TOOL_MODEL_READ_DOCUMENT_CONTENT_CHAR_LIMIT,
        _ => TOOL_MODEL_DEFAULT_CONTENT_CHAR_LIMIT,
    }
}

fn grounded_answer_ready(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return false;
    }
    if result.structured_content.get("finalAnswerReady").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    if grounded_answer_lifecycle_state(result) != Some(GROUNDED_ANSWER_LIFECYCLE_COMPLETED)
        || grounded_answer_verification_state(result) != Some(GROUNDED_ANSWER_VERIFICATION_VERIFIED)
    {
        return false;
    }
    true
}

fn grounded_answer_needs_follow_up(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return false;
    }
    if grounded_answer_warning_requires_follow_up(result) {
        return true;
    }
    if grounded_answer_ready(tool_name, result) {
        return false;
    }
    if grounded_answer_lifecycle_state(result) != Some(GROUNDED_ANSWER_LIFECYCLE_COMPLETED) {
        return false;
    }
    match grounded_answer_verification_state(result) {
        Some(GROUNDED_ANSWER_VERIFICATION_VERIFIED | GROUNDED_ANSWER_VERIFICATION_NOT_RUN) => false,
        Some(_) | None => true,
    }
}

fn grounded_answer_warning_requires_follow_up(
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    result
        .structured_content
        .pointer("/executionDetail/verificationWarnings")
        .and_then(Value::as_array)
        .is_some_and(|warnings| {
            warnings.iter().any(|warning| {
                warning
                    .get("code")
                    .and_then(Value::as_str)
                    .is_some_and(verification_warning_code_requires_follow_up)
            })
        })
}

fn verification_warning_code_requires_follow_up(code: &str) -> bool {
    matches!(
        code,
        "unsupported_literal"
            | "unsupported_canonical_claim"
            | "no_canonical_evidence"
            | "no_verifiable_tool_evidence"
            | "no_agent_tool_evidence"
            | "conflicting_evidence"
            | "partial_coverage"
            | "empty_answer"
    )
}

fn grounded_answer_completed(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    tool_name == GROUNDED_ANSWER_TOOL_NAME
        && !result.is_error
        && grounded_answer_lifecycle_state(result) == Some(GROUNDED_ANSWER_LIFECYCLE_COMPLETED)
}

fn grounded_answer_lifecycle_state(
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<&str> {
    result.structured_content.get("lifecycleState").and_then(Value::as_str).or_else(|| {
        result
            .structured_content
            .pointer("/executionDetail/execution/lifecycleState")
            .and_then(Value::as_str)
    })
}

fn grounded_answer_verification_state(
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<&str> {
    result.structured_content.pointer("/executionDetail/verificationState").and_then(Value::as_str)
}

fn tool_result_full_text(
    content: &[crate::interfaces::http::mcp::McpContentBlock],
) -> Option<String> {
    let joined = content
        .iter()
        .map(|block| block.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!joined.is_empty()).then_some(joined)
}

fn tool_result_answer_text(
    tool_name: &str,
    content: &[crate::interfaces::http::mcp::McpContentBlock],
) -> Option<String> {
    if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        tool_result_full_text(content)
    } else {
        tool_result_preview(content)
    }
}

fn grounded_answer_body_text(outcome: &ToolExecutionOutcome) -> Option<&str> {
    outcome
        .result_json
        .as_ref()
        .and_then(|json| json.pointer("/structuredContent/answerBody"))
        .and_then(Value::as_str)
        .or(outcome.result_text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn remember_verified_grounded_answer(
    current: Option<String>,
    tool_name: &str,
    outcome: &ToolExecutionOutcome,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || !outcome.grounded_answer_ready {
        return current;
    }
    grounded_answer_body_text(outcome).map(ToOwned::to_owned).or(current)
}

fn remember_verified_grounded_answer_guard_text(
    current: Option<String>,
    tool_name: &str,
    outcome: &ToolExecutionOutcome,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || !outcome.grounded_answer_ready {
        return current;
    }
    let Some(answer) = grounded_answer_body_text(outcome) else {
        return current;
    };
    match current {
        Some(existing) if existing.contains(answer) => Some(existing),
        Some(mut existing) => {
            existing.push_str("\n\n");
            existing.push_str(answer);
            Some(existing)
        }
        None => Some(answer.to_string()),
    }
}

fn remember_completed_grounded_answer(
    current: Option<String>,
    tool_name: &str,
    outcome: &ToolExecutionOutcome,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || !outcome.grounded_answer_completed {
        return current;
    }
    grounded_answer_body_text(outcome).map(ToOwned::to_owned).or(current)
}

fn should_return_completed_grounded_answer_on_iteration_cap(
    stopped_reason: AgentStopReason,
    successful_tool_call_count: usize,
    last_completed_grounded_answer: Option<&str>,
) -> bool {
    matches!(stopped_reason, AgentStopReason::IterationCap)
        && successful_tool_call_count == 1
        && last_completed_grounded_answer.is_some_and(|answer| !answer.trim().is_empty())
}

fn compact_grounded_answer_structured_content_for_model(
    value: &Value,
    fallback_limit: usize,
) -> Value {
    let reference_limit = if value.get("finalizable").and_then(Value::as_bool) == Some(true) {
        TOOL_MODEL_FINALIZABLE_GROUNDED_REFERENCE_LIMIT
    } else {
        TOOL_MODEL_GROUNDED_REFERENCE_LIMIT
    };
    compact_grounded_answer_structured_content(value, fallback_limit, reference_limit)
}

fn compact_grounded_answer_structured_content_for_verification(
    value: &Value,
    fallback_limit: usize,
) -> Value {
    compact_grounded_answer_structured_content(
        value,
        fallback_limit,
        TOOL_VERIFICATION_GROUNDED_REFERENCE_LIMIT,
    )
}

fn compact_grounded_answer_structured_content(
    value: &Value,
    fallback_limit: usize,
    reference_limit: usize,
) -> Value {
    let Some(execution_detail) = value.get("executionDetail") else {
        return compact_structured_content(value, fallback_limit);
    };
    serde_json::json!({
        "executionId": value.get("executionId").cloned().unwrap_or(Value::Null),
        "runtimeExecutionId": value.get("runtimeExecutionId").cloned().unwrap_or(Value::Null),
        "conversationId": value.get("conversationId").cloned().unwrap_or(Value::Null),
        "libraryId": value.get("libraryId").cloned().unwrap_or(Value::Null),
        "workspaceId": value.get("workspaceId").cloned().unwrap_or(Value::Null),
        "lifecycleState": value.get("lifecycleState").cloned().unwrap_or(Value::Null),
        "finalAnswerReady": value.get("finalAnswerReady").cloned().unwrap_or(Value::Bool(false)),
        "finalizable": value.get("finalizable").cloned().unwrap_or(Value::Bool(false)),
        "mustPreserveSpans": value.get("mustPreserveSpans").cloned().unwrap_or_else(|| serde_json::json!([])),
        "verificationState": execution_detail.get("verificationState").cloned().unwrap_or(Value::Null),
        "verificationWarnings": execution_detail.get("verificationWarnings").cloned().unwrap_or_else(|| serde_json::json!([])),
        "referenceCounts": {
            "chunkReferences": reference_array_len(execution_detail.get("chunkReferences")),
            "preparedSegmentReferences": reference_array_len(execution_detail.get("preparedSegmentReferences")),
            "technicalFactReferences": reference_array_len(execution_detail.get("technicalFactReferences")),
            "entityReferences": reference_array_len(execution_detail.get("entityReferences")),
            "relationReferences": reference_array_len(execution_detail.get("relationReferences"))
        },
        "references": {
            "chunkReferences": compact_reference_array(execution_detail.get("chunkReferences"), reference_limit),
            "preparedSegmentReferences": compact_reference_array(execution_detail.get("preparedSegmentReferences"), reference_limit),
            "technicalFactReferences": compact_reference_array(execution_detail.get("technicalFactReferences"), reference_limit),
            "entityReferences": compact_reference_array(execution_detail.get("entityReferences"), reference_limit),
            "relationReferences": compact_reference_array(execution_detail.get("relationReferences"), reference_limit)
        }
    })
}

fn reference_array_len(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Array(items)) => items.len(),
        _ => 0,
    }
}

fn compact_reference_array(value: Option<&Value>, limit: usize) -> Value {
    let Some(Value::Array(items)) = value else {
        return serde_json::json!([]);
    };
    let mut truncated = items.iter().take(limit).cloned().collect::<Vec<_>>();
    if items.len() > limit {
        truncated.push(serde_json::json!({
            "truncated": true,
            "omittedCount": items.len() - limit
        }));
    }
    Value::Array(truncated)
}

fn compact_structured_content_for_model(value: &Value) -> Value {
    compact_structured_content(value, TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT)
}

fn compact_structured_content_for_verification(value: &Value) -> Value {
    compact_structured_content(value, TOOL_VERIFICATION_STRUCTURED_JSON_CHAR_LIMIT)
}

fn compact_structured_content(value: &Value, char_limit: usize) -> Value {
    let serialized = match serde_json::to_string(value) {
        Ok(serialized) => serialized,
        Err(error) => {
            return serde_json::json!({
                "truncated": true,
                "errorKind": "serialization",
                "message": error.to_string()
            });
        }
    };
    if serialized.chars().count() <= char_limit {
        return value.clone();
    }
    serde_json::json!({
        "truncated": true,
        "jsonPrefix": serialized.chars().take(char_limit).collect::<String>(),
        "originalCharCount": serialized.chars().count()
    })
}

fn tool_result_preview(
    content: &[crate::interfaces::http::mcp::McpContentBlock],
) -> Option<String> {
    tool_result_preview_with_limit(content, 2_000)
}

fn tool_result_preview_with_limit(
    content: &[crate::interfaces::http::mcp::McpContentBlock],
    limit: usize,
) -> Option<String> {
    let joined = content
        .iter()
        .map(|block| block.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then(|| joined.chars().take(limit).collect())
}

fn extract_child_query_execution_ids(tool_name: &str, value: &Value) -> Vec<Uuid> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME {
        return Vec::new();
    }
    value
        .get("executionId")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .into_iter()
        .collect()
}

fn extract_child_runtime_execution_ids(tool_name: &str, value: &Value) -> Vec<Uuid> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME {
        return Vec::new();
    }
    value
        .get("runtimeExecutionId")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .into_iter()
        .collect()
}

/// Run one grounded-answer pipeline step as a single fixed-context LLM
/// call, without exposing tools to the model.
///
/// This belongs to the `grounded_answer` implementation, not to the
/// UI parent agent loop. The retrieval stage already assembled enough
/// evidence to answer the question: `prepare_answer_query` builds
/// `answer_context` out of the top retrieved chunks, graph-aware
/// neighbours, recent documents, and the library summary. Handing that
/// context to the model in one or two fixed-evidence round-trips keeps
/// direct MCP calls and UI-agent `grounded_answer` tool calls on the
/// same citation set.
///
/// Verification is the caller's responsibility: if the output is empty
/// or trips the verifier, the caller either revises over the same
/// grounded context or returns the verifier state to the user.
pub async fn run_single_shot_turn(
    state: &AppState,
    library_id: Uuid,
    user_question: &str,
    conversation_history: &[ChatMessage],
    grounded_context: &str,
) -> Result<AgentTurnResult, QueryServiceError> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryAnswer)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve query_answer binding: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("no active query_answer binding configured for library {library_id}")
        })?;

    let provider = ProviderModelSelection {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
    };

    // The runtime has already performed retrieval. Model-visible
    // context is represented as the same chat transcript shape a
    // tool-using agent would see: prior messages, current user, an
    // assistant tool-call record, and the matching tool result.
    let messages = build_runtime_tool_answer_messages(
        super::assistant_prompt::render_single_shot(),
        conversation_history,
        user_question,
        RUNTIME_RETRIEVED_CONTEXT_TOOL,
        serde_json::json!({ "question": user_question }),
        grounded_context,
    );

    let tool_use_request = ToolUseRequest {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        api_key_override: binding.api_key.clone(),
        base_url_override: binding.provider_base_url.clone(),
        temperature: binding.temperature,
        top_p: binding.top_p,
        max_output_tokens_override: binding.max_output_tokens_override,
        messages: messages.clone(),
        tools: Vec::new(),
        extra_parameters_json: binding.extra_parameters_json.clone(),
        require_tool_call: false,
    };

    let llm_call_started = std::time::Instant::now();
    let response = state
        .llm_gateway
        .generate_with_tools(tool_use_request)
        .await
        .with_context(|| "single-shot grounded-answer LLM call failed")?;

    let answer = response.output_text.trim().to_string();
    let debug_iteration = super::llm_context_debug::LlmIterationDebug {
        iteration: 1,
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        request_messages: messages,
        response_text: (!answer.is_empty()).then(|| answer.clone()),
        response_tool_calls: Vec::new(),
        usage: response.usage_json.clone(),
        duration_ms: Some(llm_call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
    };

    Ok(AgentTurnResult {
        answer,
        provider,
        usage_json: response.usage_json,
        iterations: 1,
        // Single-shot did not observe any tool results. The answer
        // pipeline attaches the selected retrieval context as verifier
        // grounding when it records the generation stage.
        assistant_grounding: AssistantGroundingEvidence::default(),
        child_query_execution_ids: Vec::new(),
        debug_iterations: vec![debug_iteration],
        agent_loop: None,
    })
}

pub async fn run_literal_fidelity_revision_turn(
    state: &AppState,
    library_id: Uuid,
    user_question: &str,
    conversation_history: &[ChatMessage],
    original_answer: &str,
    unsupported_literals: &[String],
    grounded_context: &str,
) -> Result<AgentTurnResult, QueryServiceError> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryAnswer)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve query_answer binding: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("no active query_answer binding configured for library {library_id}")
        })?;

    let provider = ProviderModelSelection {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
    };

    let system_prompt = super::assistant_prompt::render_literal_fidelity_revision(
        "Provided in the `ironrag_literal_revision_context` runtime tool result.",
        original_answer,
        unsupported_literals,
        None,
    );
    let messages = build_runtime_tool_answer_messages(
        system_prompt,
        conversation_history,
        user_question,
        RUNTIME_LITERAL_REVISION_CONTEXT_TOOL,
        serde_json::json!({
            "question": user_question,
            "unsupportedLiterals": unsupported_literals,
        }),
        grounded_context,
    );

    let tool_use_request = ToolUseRequest {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        api_key_override: binding.api_key.clone(),
        base_url_override: binding.provider_base_url.clone(),
        temperature: binding.temperature,
        top_p: binding.top_p,
        max_output_tokens_override: binding.max_output_tokens_override,
        messages: messages.clone(),
        tools: Vec::new(),
        extra_parameters_json: binding.extra_parameters_json.clone(),
        require_tool_call: false,
    };

    let llm_call_started = std::time::Instant::now();
    let response = state
        .llm_gateway
        .generate_with_tools(tool_use_request)
        .await
        .with_context(|| "literal-fidelity revision LLM call failed")?;

    let answer = response.output_text.trim().to_string();
    let debug_iteration = super::llm_context_debug::LlmIterationDebug {
        iteration: 1,
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        request_messages: messages,
        response_text: (!answer.is_empty()).then(|| answer.clone()),
        response_tool_calls: Vec::new(),
        usage: response.usage_json.clone(),
        duration_ms: Some(llm_call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
    };

    Ok(AgentTurnResult {
        answer,
        provider,
        usage_json: response.usage_json,
        iterations: 1,
        assistant_grounding: AssistantGroundingEvidence::default(),
        child_query_execution_ids: Vec::new(),
        debug_iterations: vec![debug_iteration],
        agent_loop: None,
    })
}

pub async fn run_literal_inventory_coverage_revision_turn(
    state: &AppState,
    library_id: Uuid,
    user_question: &str,
    conversation_history: &[ChatMessage],
    original_answer: &str,
    required_literals: &[String],
    revision_context: &str,
) -> Result<AgentTurnResult, QueryServiceError> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryAnswer)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve query_answer binding: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("no active query_answer binding configured for library {library_id}")
        })?;

    let provider = ProviderModelSelection {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
    };

    let system_prompt = super::assistant_prompt::render_literal_inventory_coverage_revision(
        original_answer,
        required_literals,
        "Provided in the `ironrag_literal_revision_context` runtime tool result.",
    );
    let messages = build_runtime_tool_answer_messages(
        system_prompt,
        conversation_history,
        user_question,
        RUNTIME_LITERAL_REVISION_CONTEXT_TOOL,
        serde_json::json!({
            "question": user_question,
            "requiredLiterals": required_literals,
        }),
        revision_context,
    );

    let tool_use_request = ToolUseRequest {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        api_key_override: binding.api_key.clone(),
        base_url_override: binding.provider_base_url.clone(),
        temperature: binding.temperature,
        top_p: binding.top_p,
        max_output_tokens_override: binding.max_output_tokens_override,
        messages: messages.clone(),
        tools: Vec::new(),
        extra_parameters_json: binding.extra_parameters_json.clone(),
        require_tool_call: false,
    };

    let llm_call_started = std::time::Instant::now();
    let response = state
        .llm_gateway
        .generate_with_tools(tool_use_request)
        .await
        .with_context(|| "literal-inventory coverage revision LLM call failed")?;

    let answer = response.output_text.trim().to_string();
    let debug_iteration = super::llm_context_debug::LlmIterationDebug {
        iteration: 1,
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        request_messages: messages,
        response_text: (!answer.is_empty()).then(|| answer.clone()),
        response_tool_calls: Vec::new(),
        usage: response.usage_json.clone(),
        duration_ms: Some(llm_call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
    };

    Ok(AgentTurnResult {
        answer,
        provider,
        usage_json: response.usage_json,
        iterations: 1,
        assistant_grounding: AssistantGroundingEvidence::default(),
        child_query_execution_ids: Vec::new(),
        debug_iterations: vec![debug_iteration],
        agent_loop: None,
    })
}

/// Run one clarify-with-fallback turn.
///
/// The post-retrieval router decided (see
/// `answer_pipeline::classify_answer_disposition`) that the topic the
/// user asked about spans several distinct variants in the library with
/// no dominant one, so a single-shot answer would not cleanly cover them.
/// The caller passes the variant labels PLUS the same retrieved
/// `answer_context` the single-shot would answer from; this function asks
/// the answer model to lead with a grounded best-effort answer only when
/// the evidence itself settles which content to give, then enumerate the
/// variants and ask the user to pick one or add a narrowing constraint.
///
/// Uses the same `QueryAnswer` binding as `run_single_shot_turn`
/// so the reply shares model identity, temperature caps and per-turn
/// billing plumbing.
pub async fn run_clarify_turn(
    state: &AppState,
    library_id: Uuid,
    user_question: &str,
    conversation_history: &[ChatMessage],
    variants: &[String],
    answer_context: &str,
) -> Result<AgentTurnResult, QueryServiceError> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryAnswer)
        .await
        .map_err(|e| anyhow::anyhow!("failed to resolve query_answer binding: {e}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("no active query_answer binding configured for library {library_id}")
        })?;

    let provider = ProviderModelSelection {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
    };

    // Clarify-with-fallback: the model receives the SAME retrieved evidence the
    // single-shot answer would (as the `ironrag_retrieved_context` tool result) so
    // it can ground a best-effort answer for the dominant variant, while the
    // candidate variants live in the system prompt for the clarifying menu.
    let system_prompt = super::assistant_prompt::render_clarify(variants, None);
    let messages = build_runtime_tool_answer_messages(
        system_prompt,
        conversation_history,
        user_question,
        RUNTIME_RETRIEVED_CONTEXT_TOOL,
        serde_json::json!({ "question": user_question }),
        answer_context,
    );

    let tool_use_request = ToolUseRequest {
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        api_key_override: binding.api_key.clone(),
        base_url_override: binding.provider_base_url.clone(),
        temperature: binding.temperature,
        top_p: binding.top_p,
        max_output_tokens_override: binding.max_output_tokens_override,
        messages: messages.clone(),
        tools: Vec::new(),
        extra_parameters_json: binding.extra_parameters_json.clone(),
        require_tool_call: false,
    };

    let llm_call_started = std::time::Instant::now();
    let response = state
        .llm_gateway
        .generate_with_tools(tool_use_request)
        .await
        .with_context(|| "clarify-path LLM call failed")?;

    let answer = response.output_text.trim().to_string();
    let debug_iteration = super::llm_context_debug::LlmIterationDebug {
        iteration: 1,
        provider_kind: binding.provider_kind.clone(),
        model_name: binding.model_name.clone(),
        request_messages: messages,
        response_text: (!answer.is_empty()).then(|| answer.clone()),
        response_tool_calls: Vec::new(),
        usage: response.usage_json.clone(),
        duration_ms: Some(llm_call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
    };

    Ok(AgentTurnResult {
        answer,
        provider,
        usage_json: response.usage_json,
        iterations: 1,
        assistant_grounding: AssistantGroundingEvidence::default(),
        child_query_execution_ids: Vec::new(),
        debug_iterations: vec![debug_iteration],
        agent_loop: None,
    })
}

fn build_runtime_tool_answer_messages(
    system_prompt: String,
    conversation_history: &[ChatMessage],
    user_question: &str,
    tool_name: &str,
    tool_arguments: serde_json::Value,
    tool_result: &str,
) -> Vec<ChatMessage> {
    let tool_call_id = format!("call_{tool_name}");
    let mut messages = Vec::with_capacity(conversation_history.len().saturating_add(4));
    messages.push(ChatMessage::system(system_prompt));
    messages.extend(conversation_history.iter().cloned());
    messages.push(ChatMessage::user(user_question.to_string()));
    messages.push(ChatMessage::assistant_with_tool_calls(vec![ChatToolCall {
        id: tool_call_id.clone(),
        name: tool_name.to_string(),
        arguments_json: tool_arguments.to_string(),
    }]));
    messages.push(ChatMessage::tool_result(
        tool_call_id,
        tool_name.to_string(),
        tool_result.trim().to_string(),
    ));
    messages
}

/// Accumulate one iteration's `usage_json` into the running total for
/// a turn. The billing pipeline (`services::ops::billing`) reads token
/// counts from any of the provider-specific key aliases (`prompt_tokens`
/// / `input_tokens`, `completion_tokens` / `output_tokens`, plus cached
/// input variants); we normalize to the OpenAI shape on write so a
/// mixed-provider trace still produces one correct billing row.
///
/// Numbers are summed, and per-iteration counters (`iteration_count`,
/// `provider_call_count`) expose the round-trip volume separately from
/// raw tokens so an operator reading the debug snapshot or the billing
/// `usage_json` can tell a single-shot call apart from a 6-iteration
/// escalation without cross-referencing `debug_iterations`.
pub(crate) fn merge_usage_into(accumulator: &mut serde_json::Value, iteration: &serde_json::Value) {
    fn sum_key(
        accumulator: &mut serde_json::Map<String, serde_json::Value>,
        canonical_key: &str,
        source: &serde_json::Value,
        aliases: &[&str],
    ) {
        let value =
            aliases.iter().find_map(|alias| source.get(*alias)).and_then(serde_json::Value::as_i64);
        let Some(delta) = value else {
            return;
        };
        let existing =
            accumulator.get(canonical_key).and_then(serde_json::Value::as_i64).unwrap_or(0);
        accumulator.insert(canonical_key.to_string(), serde_json::json!(existing + delta));
    }

    if !accumulator.is_object() {
        *accumulator = serde_json::json!({});
    }
    // The branch above guarantees `accumulator` is a JSON object, so
    // `as_object_mut()` returns `Some`; the fallback path is unreachable
    // but keeps the type checker happy without introducing a panic.
    let Some(obj) = accumulator.as_object_mut() else {
        return;
    };

    sum_key(obj, "prompt_tokens", iteration, &["prompt_tokens", "input_tokens"]);
    sum_key(obj, "completion_tokens", iteration, &["completion_tokens", "output_tokens"]);
    sum_key(obj, "total_tokens", iteration, &["total_tokens"]);
    sum_key(
        obj,
        "cached_input_tokens",
        iteration,
        &["cached_input_tokens", "cache_read_input_tokens", "input_cached_tokens"],
    );
    // Nested `{"prompt_tokens_details": {"cached_tokens": N}}` shape
    // some providers emit — merge it into the flat key too
    // so billing sees it regardless of which path upstream used.
    let nested_cached = iteration
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .or_else(|| {
            iteration.get("input_tokens_details").and_then(|details| details.get("cached_tokens"))
        })
        .and_then(serde_json::Value::as_i64);
    if let Some(delta) = nested_cached {
        let existing =
            obj.get("cached_input_tokens").and_then(serde_json::Value::as_i64).unwrap_or(0);
        obj.insert("cached_input_tokens".to_string(), serde_json::json!(existing + delta));
    }

    let existing_iterations =
        obj.get("iteration_count").and_then(serde_json::Value::as_i64).unwrap_or(0);
    obj.insert("iteration_count".to_string(), serde_json::json!(existing_iterations + 1));
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::{
        domains::iam::PrincipalKind,
        interfaces::http::{
            auth::{AuthContext, AuthGrant, AuthTokenKind},
            authorization::{
                POLICY_LIBRARY_READ, POLICY_MCP_MEMORY_READ, POLICY_QUERY_RUN, POLICY_RUNTIME_READ,
            },
            mcp::{McpToolSurface, tools},
        },
    };

    use super::*;

    fn auth_with_answer_tool_access() -> AuthContext {
        AuthContext {
            token_id: Uuid::nil(),
            principal_id: Uuid::nil(),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
            scopes: Vec::new(),
            grants: vec![
                AuthGrant {
                    id: Uuid::from_u128(1),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_QUERY_RUN[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
                AuthGrant {
                    id: Uuid::from_u128(2),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_MCP_MEMORY_READ[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
                AuthGrant {
                    id: Uuid::from_u128(3),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_RUNTIME_READ[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
                AuthGrant {
                    id: Uuid::from_u128(4),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_LIBRARY_READ[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
            ],
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: false,
            system_role: None,
        }
    }

    #[test]
    fn ui_agent_tool_defs_match_mcp_answer_surface_descriptors() {
        let auth = auth_with_answer_tool_access();
        // 1:1 tool parity: for the SAME capabilities, the UI agent's visible
        // tool set must equal the external MCP answer surface's set — and the
        // single vision-gated tool (`view_document_image`) must appear on both
        // surfaces under the same condition, never on one alone.
        for agent_vision_available in [false, true] {
            let capabilities = ToolVisibilityCapabilities { agent_vision_available };
            let expected_names = tools::visible_tool_names_with_capabilities(
                &auth,
                McpToolSurface::Answer,
                capabilities,
            );
            let tool_defs = answer_surface_tool_defs(&auth, capabilities);

            assert_eq!(
                tool_defs.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
                expected_names.iter().map(String::as_str).collect::<Vec<_>>(),
                "UI agent tool set diverged from MCP answer surface (vision={agent_vision_available})"
            );
            assert!(tool_defs.iter().any(|tool| tool.name == "grounded_answer"));
            assert!(!tool_defs.iter().any(|tool| tool.name == "upload_documents"));
            assert_eq!(
                tool_defs.iter().any(|tool| tool.name == "view_document_image"),
                agent_vision_available,
                "view_document_image must track the vision capability for UI<->MCP parity"
            );

            for tool in tool_defs {
                let descriptor = tools::descriptor_for(&tool.name).expect("descriptor");
                assert_eq!(tool.description, descriptor.description);
                assert_eq!(tool.parameters, descriptor.input_schema);
            }
        }
    }

    #[test]
    fn extracts_grounded_answer_child_runtime_execution_id_from_top_level_contract() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let value = serde_json::json!({
            "runtimeExecutionId": first,
            "executionDetail": {
                "execution": {
                    "runtimeExecutionId": first
                }
            },
            "items": [
                { "runtimeExecutionId": second },
                { "runtimeExecutionId": "not-a-uuid" }
            ]
        });

        let ids = extract_child_runtime_execution_ids(GROUNDED_ANSWER_TOOL_NAME, &value);

        assert_eq!(ids, vec![first]);
        assert_ne!(ids, vec![second]);
    }

    #[test]
    fn extracts_grounded_answer_child_query_execution_id_from_top_level_contract() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let value = serde_json::json!({
            "executionId": first,
            "executionDetail": {
                "execution": {
                    "id": first
                }
            },
            "items": [
                { "executionId": second },
                { "executionId": "not-a-uuid" }
            ]
        });

        let ids = extract_child_query_execution_ids(GROUNDED_ANSWER_TOOL_NAME, &value);

        assert_eq!(ids, vec![first]);
        assert_ne!(ids, vec![second]);
    }

    #[test]
    fn ignores_execution_ids_from_non_grounded_tool_results() {
        let execution_id = Uuid::now_v7();
        let runtime_execution_id = Uuid::now_v7();
        let value = serde_json::json!({
            "executionId": execution_id,
            "runtimeExecutionId": runtime_execution_id,
            "items": [
                { "executionId": Uuid::now_v7(), "runtimeExecutionId": Uuid::now_v7() }
            ]
        });

        assert!(extract_child_query_execution_ids("list_documents", &value).is_empty());
        assert!(extract_child_runtime_execution_ids("list_documents", &value).is_empty());
    }

    #[test]
    fn final_iteration_disables_tools_after_prior_tool_calls() {
        let started = Instant::now();
        let names = BTreeSet::new();

        assert!(force_final_answer_iteration(5, 5, 1, 1, 0, &names, false, started, None));
        assert!(force_final_answer_iteration(1, 1, 1, 1, 0, &names, false, started, None));
        assert!(!force_final_answer_iteration(4, 5, 1, 1, 0, &names, false, started, None));
        assert!(!force_final_answer_iteration(5, 5, 0, 0, 0, &names, false, started, None));
    }

    #[test]
    fn forced_final_request_messages_add_answer_only_reminder() {
        let messages = vec![ChatMessage::system("policy"), ChatMessage::user("Q?")];

        let normal_request = final_answer_request_messages(&messages, false);
        let forced_request = final_answer_request_messages(&messages, true);

        assert_eq!(normal_request.len(), messages.len());
        assert_eq!(forced_request.len(), messages.len() + 1);
        assert_eq!(forced_request.last().expect("reminder").role, "system");
        assert!(
            forced_request
                .last()
                .and_then(|message| message.content.as_deref())
                .expect("reminder content")
                .contains("No more MCP tool calls")
        );
    }

    #[test]
    fn forced_final_retry_messages_close_illegal_tool_call() {
        let request_messages = vec![ChatMessage::system("policy"), ChatMessage::user("Q?")];
        let tool_calls = vec![ChatToolCall {
            id: "call-1".to_string(),
            name: READ_DOCUMENT_TOOL_NAME.to_string(),
            arguments_json: "{}".to_string(),
        }];

        let retry_messages = final_answer_retry_messages(
            &request_messages,
            Some("reasoning".to_string()),
            &tool_calls,
        );

        assert_eq!(retry_messages.len(), request_messages.len() + 3);
        let assistant = &retry_messages[request_messages.len()];
        assert_eq!(assistant.role, "assistant");
        assert_eq!(assistant.reasoning_content.as_deref(), Some("reasoning"));
        assert_eq!(assistant.tool_calls.len(), 1);
        let tool_result = &retry_messages[request_messages.len() + 1];
        assert_eq!(tool_result.role, "tool");
        assert_eq!(tool_result.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(tool_result.name.as_deref(), Some(READ_DOCUMENT_TOOL_NAME));
        assert!(
            tool_result.content.as_deref().expect("tool result content").contains("not executed")
        );
        assert_eq!(retry_messages.last().expect("reminder").role, "system");
    }

    #[test]
    fn soft_deadline_keeps_tools_without_verified_grounded_answer() {
        let started = Instant::now() - Duration::from_secs(40);
        let names =
            BTreeSet::from([SEARCH_DOCUMENTS_TOOL_NAME.to_string(), "search_entities".to_string()]);

        assert!(!force_final_answer_iteration(
            3,
            5,
            4,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            0,
            &names,
            false,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn soft_deadline_keeps_collecting_when_tool_evidence_has_one_category() {
        let started = Instant::now() - Duration::from_secs(40);
        let names = BTreeSet::from([SEARCH_DOCUMENTS_TOOL_NAME.to_string()]);

        assert!(!force_final_answer_iteration(
            3,
            5,
            4,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            0,
            &names,
            false,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn soft_deadline_waits_for_enough_successful_tools() {
        let started = Instant::now() - Duration::from_secs(40);
        let names = BTreeSet::new();

        assert!(!force_final_answer_iteration(
            3,
            5,
            3,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS - 1,
            0,
            &names,
            false,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn grounded_answer_success_alone_waits_until_soft_deadline() {
        let before_deadline = Instant::now();
        let after_deadline = Instant::now() - Duration::from_secs(40);
        let names = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

        assert!(!force_final_answer_iteration(
            3,
            5,
            5,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            1,
            &names,
            false,
            before_deadline,
            Some(Duration::from_secs(35)),
        ));
        assert!(force_final_answer_iteration(
            3,
            5,
            5,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            1,
            &names,
            false,
            after_deadline,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn soft_deadline_does_not_force_final_when_grounded_follow_up_is_required() {
        let after_deadline = Instant::now() - Duration::from_secs(40);
        let names = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

        assert!(!force_final_answer_iteration(
            3,
            5,
            5,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            1,
            &names,
            true,
            after_deadline,
            Some(Duration::from_secs(35)),
        ));
        assert!(force_final_answer_iteration(
            5,
            5,
            5,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            1,
            &names,
            true,
            after_deadline,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn grounded_answer_success_keeps_tool_surface_available_for_agent_judgment() {
        let tool_defs = [
            GROUNDED_ANSWER_TOOL_NAME,
            SEARCH_DOCUMENTS_TOOL_NAME,
            READ_DOCUMENT_TOOL_NAME,
            "search_entities",
        ]
        .into_iter()
        .map(|name| ChatToolDef {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        })
        .collect::<Vec<_>>();
        let successful = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful, false);

        assert_eq!(
            next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            vec![
                GROUNDED_ANSWER_TOOL_NAME,
                SEARCH_DOCUMENTS_TOOL_NAME,
                READ_DOCUMENT_TOOL_NAME,
                "search_entities"
            ]
        );
    }

    #[test]
    fn composite_doc_graph_evidence_keeps_tools_without_verified_grounded_answer() {
        let started = Instant::now();
        let names = BTreeSet::from([
            SEARCH_DOCUMENTS_TOOL_NAME.to_string(),
            "search_entities".to_string(),
            READ_DOCUMENT_TOOL_NAME.to_string(),
        ]);

        assert!(!force_final_answer_iteration(
            3,
            5,
            4,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            0,
            &names,
            false,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn verified_grounded_composite_evidence_can_disable_tools_before_soft_deadline() {
        let started = Instant::now();
        let names = BTreeSet::from([
            GROUNDED_ANSWER_TOOL_NAME.to_string(),
            SEARCH_DOCUMENTS_TOOL_NAME.to_string(),
            READ_DOCUMENT_TOOL_NAME.to_string(),
        ]);

        assert!(force_final_answer_iteration(
            3,
            5,
            4,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            1,
            &names,
            false,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn soft_deadline_keeps_tools_before_deadline() {
        let started = Instant::now();
        let names = BTreeSet::from([SEARCH_DOCUMENTS_TOOL_NAME.to_string()]);

        assert!(!force_final_answer_iteration(
            3,
            5,
            4,
            SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS,
            0,
            &names,
            false,
            started,
            Some(Duration::from_secs(35)),
        ));
    }

    #[test]
    fn final_answer_requires_at_least_one_tool_result() {
        let tool_defs = [
            "list_workspaces",
            "list_libraries",
            "grounded_answer",
            "search_documents",
            "search_entities",
            "read_document",
        ]
        .into_iter()
        .map(|name| ChatToolDef {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        })
        .collect::<Vec<_>>();

        assert!(should_require_tool_call_before_final(false, &tool_defs, &BTreeSet::new(), false));

        let simple_evidence = BTreeSet::from(["search_documents".to_string()]);
        assert!(
            !should_require_tool_call_before_final(false, &tool_defs, &simple_evidence, false,)
        );

        let composite_evidence =
            BTreeSet::from(["search_documents".to_string(), "search_entities".to_string()]);
        assert!(!should_require_tool_call_before_final(
            false,
            &tool_defs,
            &composite_evidence,
            false,
        ));

        let complete_composite = BTreeSet::from([
            "search_documents".to_string(),
            "search_entities".to_string(),
            "read_document".to_string(),
        ]);
        assert!(!should_require_tool_call_before_final(
            false,
            &tool_defs,
            &complete_composite,
            false,
        ));
        assert!(!should_require_tool_call_before_final(true, &tool_defs, &BTreeSet::new(), true,));
        assert!(!should_require_tool_call_before_final(false, &[], &BTreeSet::new(), true,));

        let answer_only = [ChatToolDef {
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        }];
        assert!(should_require_tool_call_before_final(
            false,
            &answer_only,
            &BTreeSet::new(),
            false
        ));
        assert!(!should_require_tool_call_before_final(
            false,
            &answer_only,
            &BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]),
            false,
        ));
    }

    #[test]
    fn final_answer_requires_follow_up_after_incomplete_grounded_answer() {
        let tool_defs =
            [GROUNDED_ANSWER_TOOL_NAME, SEARCH_DOCUMENTS_TOOL_NAME, READ_DOCUMENT_TOOL_NAME]
                .into_iter()
                .map(|name| ChatToolDef {
                    name: name.to_string(),
                    description: String::new(),
                    parameters: serde_json::json!({}),
                })
                .collect::<Vec<_>>();
        let grounded = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

        assert!(should_require_tool_call_before_final(false, &tool_defs, &grounded, true));
    }

    #[test]
    fn complete_grounded_answer_allows_final_synthesis() {
        let tool_defs = [GROUNDED_ANSWER_TOOL_NAME]
            .into_iter()
            .map(|name| ChatToolDef {
                name: name.to_string(),
                description: String::new(),
                parameters: serde_json::json!({}),
            })
            .collect::<Vec<_>>();
        let grounded = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);

        assert!(!should_require_tool_call_before_final(false, &tool_defs, &grounded, false));
    }

    #[test]
    fn grounded_answer_evidence_ledger_retains_partial_and_repair_entries() {
        let mut ledger = GroundedAnswerEvidenceLedger::default();
        let partial = grounded_answer_ledger_outcome(serde_json::json!({
            "structuredContent": {
                "executionId": "00000000-0000-0000-0000-000000000001",
                "answerBody": "Alpha path is supported. Beta recovery path still needs repair. Beta recovery summary is grounded.",
                "finalAnswerReady": false,
                "finalizable": false,
                "mustPreserveSpans": ["Alpha path", "Beta recovery path", "Beta recovery summary"],
                "executionDetail": {
                    "verificationState": "conflicting",
                    "verificationWarnings": [{"code": "partial_coverage"}],
                    "preparedSegmentReferences": [{"documentTitle": "Alpha operator guide"}],
                    "entityReferences": [{"label": "Beta recovery path", "summary": "Beta recovery summary"}],
                    "relationReferences": [{"normalizedAssertion": "Alpha supports beta recovery"}]
                }
            },
                "content": [{"type": "text", "text": "Alpha path is supported. Beta recovery path still needs repair. Beta recovery summary is grounded."}],
            "isError": false
        }));
        let repair = grounded_answer_ledger_outcome(serde_json::json!({
            "structuredContent": {
                "executionId": "00000000-0000-0000-0000-000000000002",
                "answerBody": "Gamma repair path is supported.",
                "finalAnswerReady": true,
                "finalizable": true,
                "mustPreserveSpans": ["Gamma repair path"],
                "executionDetail": {
                    "verificationState": "verified",
                    "verificationWarnings": [],
                    "preparedSegmentReferences": [{"documentTitle": "Gamma repair guide"}],
                    "entityReferences": [],
                    "relationReferences": []
                }
            },
            "content": [{"type": "text", "text": "Gamma repair path is supported."}],
            "isError": false
        }));

        ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &partial);
        ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &repair);

        let message = ledger.system_message().expect("ledger message");
        assert!(message.contains("Same-turn grounded_answer evidence ledger"));
        assert!(message.contains("finalizable=false"));
        assert!(message.contains("finalizable=true"));
        assert!(message.contains("Alpha path"));
        assert!(message.contains("Beta recovery path"));
        assert!(message.contains("Beta recovery summary"));
        assert!(message.contains("Gamma repair path"));
        assert!(message.contains("partial_coverage"));
    }

    #[test]
    fn grounded_answer_evidence_ledger_guards_answer_that_drops_anchors() {
        let mut ledger = GroundedAnswerEvidenceLedger::default();
        let partial = grounded_answer_ledger_outcome(serde_json::json!({
            "structuredContent": {
                "executionId": "00000000-0000-0000-0000-000000000011",
                "answerBody": "Alpha path is supported. Beta recovery path still needs repair. Beta recovery summary is grounded.",
                "finalAnswerReady": false,
                "finalizable": false,
                "mustPreserveSpans": ["Alpha path", "Beta recovery path", "Beta recovery summary"],
                "executionDetail": {
                    "verificationState": "conflicting",
                    "verificationWarnings": [{"code": "partial_coverage"}],
                    "preparedSegmentReferences": [{"documentTitle": "Alpha operator guide"}],
                    "entityReferences": [{"label": "Beta recovery path", "summary": "Beta recovery summary"}],
                    "relationReferences": [{"normalizedAssertion": "Alpha supports beta recovery"}]
                }
            },
                "content": [{"type": "text", "text": "Alpha path is supported. Beta recovery path still needs repair. Beta recovery summary is grounded."}],
            "isError": false
        }));
        let repair = grounded_answer_ledger_outcome(serde_json::json!({
            "structuredContent": {
                "executionId": "00000000-0000-0000-0000-000000000012",
                "answerBody": "Gamma repair path is supported. Gamma repair guide names Gamma repair action.",
                "finalAnswerReady": true,
                "finalizable": true,
                "mustPreserveSpans": [
                    "Gamma repair path",
                    "Gamma repair guide",
                    "Gamma repair action"
                ],
                "executionDetail": {
                    "verificationState": "verified",
                    "verificationWarnings": [],
                    "preparedSegmentReferences": [{"documentTitle": "Gamma repair guide"}],
                    "entityReferences": [],
                    "relationReferences": []
                }
            },
            "content": [{"type": "text", "text": "Gamma repair path is supported."}],
            "isError": false
        }));

        ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &partial);
        ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &repair);

        let dropped_answer = "No matching operational branch is described.";
        let guard = ledger.guard_candidate_for_answer(dropped_answer).expect("guard candidate");

        assert!(!guard.contains("Alpha path"));
        assert!(!guard.contains("Beta recovery path"));
        assert!(!guard.contains("Beta recovery summary"));
        assert!(guard.contains("Gamma repair path"));
        assert!(guard.contains("Gamma repair guide"));
        assert!(guard.contains("Gamma repair action"));

        assert!(ledger.guard_candidate_for_answer(&guard).is_none());
    }

    #[test]
    fn grounded_answer_evidence_ledger_guards_insufficient_high_signal_inventory() {
        let mut ledger = GroundedAnswerEvidenceLedger::default();
        let answer_body = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`\n\
- `currency = USD`\n\
- `retryWindow = 60`\n\
- `pollInterval = 1`\n\
- `fillDetails = true`\n\
- `printSlip = false`\n\
- `visible = true`";
        let partial = grounded_answer_ledger_outcome(serde_json::json!({
            "structuredContent": {
                "executionId": "00000000-0000-0000-0000-000000000021",
                "answerBody": answer_body,
                "finalAnswerReady": false,
                "finalizable": false,
                "mustPreserveSpans": [
                    "Provider Alpha Guide",
                    "/opt/alpha/alpha.conf",
                    "[Main]",
                    "url = http://localhost",
                    "timeout = 10",
                    "merchantId",
                    "secretKey",
                    "currency = USD",
                    "retryWindow = 60",
                    "pollInterval = 1",
                    "fillDetails = true",
                    "printSlip = false",
                    "visible = true"
                ],
                "executionDetail": {
                    "verificationState": "insufficient_evidence",
                    "verificationWarnings": [{
                        "code": "unsupported_literal",
                        "message": "Literal `currency = USD` is not grounded in selected evidence."
                    }],
                    "preparedSegmentReferences": [{"documentTitle": "Provider Alpha Guide"}],
                    "entityReferences": [],
                    "relationReferences": []
                }
            },
            "content": [{"type": "text", "text": answer_body}],
            "isError": false
        }));

        ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &partial);

        let dropped_answer = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`\n\
- `retryWindow = 60`\n\
- `pollInterval = 1`";
        let guard = ledger.guard_candidate_for_answer(dropped_answer).expect("guard candidate");

        assert!(guard.contains("fillDetails = true"));
        assert!(guard.contains("printSlip = false"));
        assert!(guard.contains("visible = true"));
    }

    #[test]
    fn grounded_answer_evidence_ledger_does_not_guard_from_conflicting_partial_inventory() {
        let mut ledger = GroundedAnswerEvidenceLedger::default();
        let answer_body = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`\n\
- `fillDetails = true`\n\
- `printSlip = false`\n\
- `visible = true`";
        let partial = grounded_answer_ledger_outcome(serde_json::json!({
            "structuredContent": {
                "executionId": "00000000-0000-0000-0000-000000000024",
                "answerBody": answer_body,
                "finalAnswerReady": false,
                "finalizable": false,
                "mustPreserveSpans": [
                    "Provider Alpha Guide",
                    "/opt/alpha/alpha.conf",
                    "[Main]",
                    "url = http://localhost",
                    "timeout = 10",
                    "merchantId",
                    "secretKey",
                    "fillDetails = true",
                    "printSlip = false",
                    "visible = true"
                ],
                "executionDetail": {
                    "verificationState": "conflicting",
                    "verificationWarnings": [{"code": "partial_coverage"}],
                    "preparedSegmentReferences": [{"documentTitle": "Provider Alpha Guide"}],
                    "entityReferences": [],
                    "relationReferences": []
                }
            },
            "content": [{"type": "text", "text": answer_body}],
            "isError": false
        }));

        ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &partial);

        let dropped_answer = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`";
        assert!(ledger.guard_candidate_for_answer(dropped_answer).is_none());
    }

    #[test]
    fn grounded_answer_evidence_ledger_guards_missing_verified_high_signal_spans() {
        let mut ledger = GroundedAnswerEvidenceLedger::default();
        let answer_body = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`\n\
- `currency = USD`\n\
- `retryWindow = 60`\n\
- `pollInterval = 1`\n\
- `fillDetails = true`\n\
- `printSlip = false`\n\
- `visible = true`";
        let verified = grounded_answer_ledger_outcome(serde_json::json!({
            "structuredContent": {
                "executionId": "00000000-0000-0000-0000-000000000022",
                "answerBody": answer_body,
                "finalAnswerReady": true,
                "finalizable": true,
                "mustPreserveSpans": [
                    "Provider Alpha Guide",
                    "/opt/alpha/alpha.conf",
                    "[Main]",
                    "url = http://localhost",
                    "timeout = 10",
                    "merchantId",
                    "secretKey",
                    "currency = USD",
                    "retryWindow = 60",
                    "pollInterval = 1",
                    "fillDetails = true",
                    "printSlip = false",
                    "visible = true"
                ],
                "executionDetail": {
                    "verificationState": "verified",
                    "verificationWarnings": [{
                        "code": "unsupported_literal",
                        "message": "Literal `currency = USD` is not grounded in selected evidence."
                    }],
                    "preparedSegmentReferences": [{"documentTitle": "Provider Alpha Guide"}],
                    "entityReferences": [],
                    "relationReferences": []
                }
            },
            "content": [{"type": "text", "text": answer_body}],
            "isError": false
        }));

        ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &verified);

        let dropped_answer = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`\n\
- `retryWindow = 60`\n\
- `pollInterval = 1`";
        let guard = ledger.guard_candidate_for_answer(dropped_answer).expect("guard candidate");

        assert!(guard.contains("fillDetails = true"));
        assert!(guard.contains("printSlip = false"));
        assert!(guard.contains("visible = true"));
        assert!(guard.contains("\n- `fillDetails = true`"));
        assert!(
            !ledger.guard_high_signal_anchor_set().contains("currency = USD"),
            "unsupported warning literals must not become high-signal guard anchors"
        );
    }

    #[test]
    fn grounded_answer_evidence_ledger_guard_preserves_body_beyond_excerpt_limit() {
        let mut ledger = GroundedAnswerEvidenceLedger::default();
        let mut answer_body = String::from("Provider Alpha Guide\n\n");
        for index in 0..90 {
            answer_body.push_str(&format!(
                "- field{index:02} = value{index:02}; keep this operational detail intact.\n"
            ));
        }
        answer_body.push_str("- finalSetting = enabled\n");
        answer_body.push_str("- lateIdentifier = preserve_me\n");
        assert!(answer_body.chars().count() > GROUNDED_EVIDENCE_LEDGER_ANSWER_CHARS);

        let verified = grounded_answer_ledger_outcome(serde_json::json!({
            "structuredContent": {
                "executionId": "00000000-0000-0000-0000-000000000023",
                "answerBody": answer_body,
                "finalAnswerReady": true,
                "finalizable": true,
                "mustPreserveSpans": [
                    "Provider Alpha Guide",
                    "field00 = value00",
                    "field01 = value01",
                    "finalSetting = enabled",
                    "lateIdentifier = preserve_me"
                ],
                "executionDetail": {
                    "verificationState": "verified",
                    "verificationWarnings": [],
                    "preparedSegmentReferences": [{"documentTitle": "Provider Alpha Guide"}],
                    "entityReferences": [],
                    "relationReferences": []
                }
            },
            "content": [{"type": "text", "text": answer_body}],
            "isError": false
        }));

        ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &verified);

        let dropped_answer = "Provider Alpha Guide\n- field00 = value00\n- field01 = value01";
        let guard = ledger.guard_candidate_for_answer(dropped_answer).expect("guard candidate");

        assert!(guard.chars().count() > GROUNDED_EVIDENCE_LEDGER_ANSWER_CHARS);
        assert!(guard.contains("finalSetting = enabled"));
        assert!(guard.contains("lateIdentifier = preserve_me"));
        assert!(guard.contains("\n- lateIdentifier = preserve_me"));
    }

    fn grounded_answer_ledger_outcome(result_json: Value) -> ToolExecutionOutcome {
        ToolExecutionOutcome {
            arguments_json: None,
            requested_arguments_json: None,
            message_content: String::new(),
            result_text: result_json
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            result_json: Some(result_json),
            grounding_text: None,
            grounded_answer_ready: false,
            grounded_answer_completed: true,
            grounded_answer_needs_follow_up: false,
            is_error: false,
            duration_ms: 0,
            child_query_execution_ids: Vec::new(),
            child_runtime_execution_ids: Vec::new(),
        }
    }

    #[test]
    fn incomplete_grounded_follow_up_state_stays_required_after_repeated_incomplete_result() {
        assert!(next_incomplete_grounded_answer_follow_up_required(false, true, false));
        assert!(next_incomplete_grounded_answer_follow_up_required(true, true, true));
        assert!(!next_incomplete_grounded_answer_follow_up_required(true, false, true));
        assert!(next_incomplete_grounded_answer_follow_up_required(true, false, false));
    }

    #[test]
    fn incomplete_grounded_follow_up_requires_grounding_evidence() {
        assert!(!tool_result_satisfies_incomplete_grounded_follow_up(
            "list_documents",
            false,
            false,
            Some("document names only"),
        ));
        assert!(!tool_result_satisfies_incomplete_grounded_follow_up(
            SEARCH_DOCUMENTS_TOOL_NAME,
            false,
            false,
            Some("   "),
        ));
        assert!(tool_result_satisfies_incomplete_grounded_follow_up(
            SEARCH_DOCUMENTS_TOOL_NAME,
            false,
            false,
            Some("content evidence"),
        ));
        assert!(tool_result_satisfies_incomplete_grounded_follow_up(
            GROUNDED_ANSWER_TOOL_NAME,
            false,
            true,
            None,
        ));
        assert!(!tool_result_satisfies_incomplete_grounded_follow_up(
            GROUNDED_ANSWER_TOOL_NAME,
            false,
            false,
            Some("clarification without final evidence"),
        ));
    }

    #[test]
    fn extracts_latest_user_verbatim_fragments_from_common_quote_pairs() {
        let fragments = extract_verbatim_user_fragments(
            "Explain «Status: 未確認?» and \"ERR-42: retry?\" without changing the labels.",
        );

        assert_eq!(fragments, vec!["Status: 未確認?", "ERR-42: retry?"]);
    }

    #[test]
    fn extracts_latest_user_verbatim_fragments_from_delimited_clauses() {
        let fragments = extract_verbatim_user_fragments(
            "What should happen if the form shows, item MARK-17 was already accepted?",
        );

        assert!(fragments.iter().any(|fragment| fragment == "item MARK-17 was already accepted?"));
    }

    #[test]
    fn extracts_latest_user_verbatim_fragments_from_identifier_tokens() {
        let fragments = extract_verbatim_user_fragments(
            "Describe SYNC-AGENT topology and compare it with API_V2.",
        );

        assert!(fragments.iter().any(|fragment| fragment == "SYNC-AGENT"));
        assert!(fragments.iter().any(|fragment| fragment == "API_V2"));
    }

    #[test]
    fn ignores_natural_language_punctuation_as_identifier_tokens() {
        let fragments = extract_verbatim_user_fragments(
            "Explain set-up flow, ad-hoc about sync-agent and graph.example.com.",
        );

        assert!(!fragments.iter().any(|fragment| fragment == "set-up"));
        assert!(!fragments.iter().any(|fragment| fragment == "ad-hoc"));
        assert!(!fragments.iter().any(|fragment| fragment == "sync-agent"));
        assert!(!fragments.iter().any(|fragment| fragment == "graph.example.com"));
    }

    #[test]
    fn latest_user_verbatim_fragment_reminder_marks_fragments_as_identifiers_not_evidence() {
        let reminder =
            latest_user_verbatim_fragment_reminder("What does «Result: timeout?» mean?").unwrap();

        assert!(reminder.contains("Result: timeout?"));
        assert!(reminder.contains("not as evidence for external facts"));
        assert!(reminder.contains("requested answer slots"));
        assert!(reminder.contains("instead of replacing it with a paraphrase"));
    }

    #[test]
    fn final_answer_keeps_missing_quoted_user_fragment_visible() {
        let answer = ensure_user_fragments_visible(
            "It means the workstation cannot reach the account servers.".to_string(),
            "Explain «Cash register access to account servers: no».",
        );

        assert!(answer.starts_with("Cash register access to account servers: no\n\n"));
    }

    #[test]
    fn final_answer_does_not_duplicate_visible_quoted_user_fragment() {
        let answer = ensure_user_fragments_visible(
            "cash register access to account servers: no means network access is disabled."
                .to_string(),
            "Explain «Cash register access to account servers: no».",
        );

        assert_eq!(
            answer,
            "cash register access to account servers: no means network access is disabled."
        );
    }

    #[test]
    fn final_answer_does_not_prefix_requested_slot_fragment() {
        let answer = ensure_user_fragments_visible(
            "Package: alpha-plugin.\nConfig path: /opt/app/config.ini.".to_string(),
            "Configure Alpha: name package, config path and parameters, then explain defaults.",
        );

        assert_eq!(answer, "Package: alpha-plugin.\nConfig path: /opt/app/config.ini.");
    }

    #[test]
    fn final_answer_keeps_missing_identifier_fragment_with_exact_case_visible() {
        let answer = ensure_user_fragments_visible(
            "sync-agent exchanges data between the register and central server.".to_string(),
            "Describe SYNC-AGENT topology.",
        );

        assert!(answer.starts_with("SYNC-AGENT\n\n"));
    }

    #[test]
    fn prior_assistant_answer_fallback_strips_compact_literal_memory() {
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "Choose a provider before setup can continue.".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text:
                    "literals: `alpha-package`, `/etc/alpha.ini`, `alphaTimeout`\nInstall `alpha-package`, edit `/etc/alpha.ini`, and set `alphaTimeout` from the source table."
                        .to_string(),
            },
        ];

        let answer = prior_assistant_answer_fallback_candidate(&history, &[])
            .expect("anchored prior assistant answer");

        assert!(answer.starts_with("Install `alpha-package`"));
        assert!(!answer.starts_with("literals:"));
    }

    #[test]
    fn prior_assistant_answer_fallback_ignores_unanchored_history() {
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "Choose a provider before setup can continue.".to_string(),
        }];

        assert!(prior_assistant_answer_fallback_candidate(&history, &[]).is_none());
    }

    #[test]
    fn prior_assistant_answer_fallback_uses_chat_history_when_tool_history_is_empty() {
        let conversation_history = vec![ChatMessage::assistant_text(
            "Use `alpha-package`, `/etc/alpha.ini`, and `alphaTimeout`.".to_string(),
        )];

        let answer = prior_assistant_answer_fallback_candidate(&[], &conversation_history)
            .expect("anchored chat history answer");

        assert!(answer.contains("`alpha-package`"));
        assert!(answer.contains("`alphaTimeout`"));
    }

    #[test]
    fn prior_assistant_answer_fallback_prefers_more_anchored_history_candidate() {
        let compact_tool_history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text:
                "literals: `alpha-package`, `/etc/alpha.ini`, `alphaTimeout`, `alphaMode`, `alphaUrl`, `alphaRetry`\nUse `alpha-package`, `/etc/alpha.ini`, and `alphaTimeout`."
                    .to_string(),
        }];
        let conversation_history = vec![ChatMessage::assistant_text(
            "Use `alpha-package`, `/etc/alpha.ini`, `alphaTimeout`, `alphaMode`, `alphaUrl`, and `alphaRetry`."
                .to_string(),
        )];

        let answer =
            prior_assistant_answer_fallback_candidate(&compact_tool_history, &conversation_history)
                .expect("anchored prior assistant answer");

        assert!(answer.contains("`alphaRetry`"));
        assert!(answer.contains("`alphaMode`"));
        assert!(!answer.starts_with("literals:"));
    }

    #[test]
    fn final_answer_prefers_prior_assistant_fallback_when_parent_drops_anchors() {
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "Use `alpha-package`, `/etc/alpha.ini`, `alphaTimeout`, `alphaMode`, and `alphaUrl`."
                .to_string(),
        }];
        let prior = prior_assistant_answer_fallback_candidate(&history, &[])
            .expect("anchored prior assistant answer");
        let answer = "Install the module and configure the connection settings.".to_string();
        let guard = prior_assistant_answer_guard_for_final_answer(&answer, Some(&prior));

        let finalized =
            finalize_agent_loop_answer(answer, "Explain all settings.", guard, "req", Uuid::nil());

        assert_eq!(finalized, prior);
    }

    #[test]
    fn final_answer_keeps_fresh_anchored_answer_over_prior_fallback() {
        let prior = "Choose one option: alphaProvider, betaProvider, gammaProvider, deltaProvider.";
        let answer =
            "Install `alpha-package`, edit `/etc/alpha.ini`, and set `alphaTimeout`.".to_string();
        let guard = prior_assistant_answer_guard_for_final_answer(&answer, Some(prior));

        let finalized = finalize_agent_loop_answer(
            answer.clone(),
            "Explain all settings.",
            guard,
            "req",
            Uuid::nil(),
        );

        assert_eq!(finalized, answer);
    }

    #[test]
    fn final_answer_keeps_parent_answer_when_verified_literals_match() {
        let grounded = "Configure `alphaPackage`, `/etc/alpha.ini`, `partnerId`, and `secretKey`.";
        let answer =
            "Use `/etc/alpha.ini` for `alphaPackage`; set `partnerId` and `secretKey`.".to_string();

        let finalized = finalize_agent_loop_answer(
            answer.clone(),
            "How do I configure it?",
            Some(grounded),
            "req",
            Uuid::nil(),
        );

        assert_eq!(finalized, answer);
    }

    #[test]
    fn final_answer_keeps_user_literal_that_is_not_in_verified_grounding() {
        let grounded = "Configure `alphaPackage`, `/etc/alpha.ini`, `partnerId`, and `secretKey`.";
        let answer =
            "For `USER-42`, configure `alphaPackage`, `/etc/alpha.ini`, `partnerId`, and `secretKey`."
                .to_string();

        let finalized = finalize_agent_loop_answer(
            answer.clone(),
            "Apply this to `USER-42`.",
            Some(grounded),
            "req",
            Uuid::nil(),
        );

        assert_eq!(finalized, answer);
    }

    #[test]
    fn final_answer_prefers_verified_grounding_when_parent_drops_or_adds_literals() {
        let grounded = "Configure `alphaPackage`, `/etc/alpha.ini`, `partnerId`, `secretKey`, and `sendDetails`.";
        let answer = "Configure `alphaPackage`, `/etc/alpha.ini`, `partnerId`, and `madeUpFlag`."
            .to_string();

        let finalized = finalize_agent_loop_answer(
            answer,
            "How do I configure it?",
            Some(grounded),
            "req",
            Uuid::nil(),
        );

        assert_eq!(finalized, grounded);
    }

    #[test]
    fn final_answer_prefers_verified_grounding_when_parent_drops_plain_surface_anchors() {
        let grounded = "Use alphaPackage with /etc/alpha.ini. Set partnerId, secretKey, sendDetails, callbackUrl, and retryCount from the sourced parameter table.";
        let answer = "Use alphaPackage with /etc/alpha.ini, partnerId, and secretKey.".to_string();

        let finalized = finalize_agent_loop_answer(
            answer,
            "How do I configure it?",
            Some(grounded),
            "req",
            Uuid::nil(),
        );

        assert_eq!(finalized, grounded);
    }

    #[test]
    fn final_answer_keeps_parent_answer_when_plain_surface_anchor_loss_is_small() {
        let grounded = "Use alphaPackage with /etc/alpha.ini. Set partnerId, secretKey, sendDetails, callbackUrl, and retryCount from the sourced parameter table.";
        let answer = "Use alphaPackage with /etc/alpha.ini. Set partnerId, secretKey, sendDetails, and callbackUrl."
            .to_string();

        let finalized = finalize_agent_loop_answer(
            answer.clone(),
            "How do I configure it?",
            Some(grounded),
            "req",
            Uuid::nil(),
        );

        assert_eq!(finalized, answer);
    }

    #[test]
    fn verified_grounded_literal_set_extracts_config_block_without_fence_language() {
        let literals = verified_grounded_answer_literal_set(
            "```ini\n[Main]\nalphaPackage = true\n/etc/alpha.ini\n```",
        );

        assert!(literals.contains("[Main]"));
        assert!(literals.contains("alphaPackage"));
        assert!(literals.contains("/etc/alpha.ini"));
        assert!(!literals.contains("ini"));
    }

    #[test]
    fn agent_iteration_keeps_full_mcp_surface_until_final_answer() {
        let tool_defs = [
            "list_workspaces",
            "list_libraries",
            "grounded_answer",
            "search_documents",
            "search_entities",
            "read_document",
        ]
        .into_iter()
        .map(|name| ChatToolDef {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        })
        .collect::<Vec<_>>();
        let successful_tool_names = BTreeSet::new();

        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
        let next_names = next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>();
        assert_eq!(
            next_names,
            vec![
                "list_workspaces",
                "list_libraries",
                "grounded_answer",
                "search_documents",
                "search_entities",
                "read_document"
            ]
        );

        let mut successful_tool_names = BTreeSet::from(["search_documents".to_string()]);

        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
        let next_names = next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>();
        assert_eq!(
            next_names,
            vec![
                "list_workspaces",
                "list_libraries",
                "grounded_answer",
                "search_documents",
                "search_entities",
                "read_document"
            ]
        );

        successful_tool_names.insert("search_entities".to_string());
        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
        assert_eq!(
            next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            vec![
                "list_workspaces",
                "list_libraries",
                "grounded_answer",
                "search_documents",
                "search_entities",
                "read_document"
            ]
        );

        successful_tool_names.insert("read_document".to_string());
        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
        assert_eq!(
            next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            vec![
                "list_workspaces",
                "list_libraries",
                "grounded_answer",
                "search_documents",
                "search_entities",
                "read_document",
            ]
        );

        assert!(tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, true).is_empty());
    }

    #[test]
    fn first_iteration_exposes_available_answer_tools() {
        let tool_defs = ["list_workspaces", "list_libraries", "grounded_answer"]
            .into_iter()
            .map(|name| ChatToolDef {
                name: name.to_string(),
                description: String::new(),
                parameters: serde_json::json!({}),
            })
            .collect::<Vec<_>>();

        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &BTreeSet::new(), false);

        assert_eq!(
            next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            vec!["list_workspaces", "list_libraries", "grounded_answer"]
        );
    }

    #[test]
    fn wildcard_scope_keeps_full_mcp_surface() {
        let tool_defs =
            ["list_documents", "grounded_answer", "search_documents", "search_entities"]
                .into_iter()
                .map(|name| ChatToolDef {
                    name: name.to_string(),
                    description: String::new(),
                    parameters: serde_json::json!({}),
                })
                .collect::<Vec<_>>();

        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &BTreeSet::new(), false);
        assert_eq!(
            next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            vec!["list_documents", "grounded_answer", "search_documents", "search_entities"]
        );

        let listing_only = BTreeSet::from(["list_documents".to_string()]);
        assert!(!should_require_tool_call_before_final(false, &tool_defs, &listing_only, false,));
        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &listing_only, false);
        assert_eq!(
            next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            vec!["list_documents", "grounded_answer", "search_documents", "search_entities"]
        );

        let grounded = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);
        assert!(!should_require_tool_call_before_final(false, &tool_defs, &grounded, false));
    }

    #[test]
    fn atomic_content_question_keeps_full_mcp_surface() {
        let tool_defs =
            ["list_documents", "grounded_answer", "search_documents", "search_entities"]
                .into_iter()
                .map(|name| ChatToolDef {
                    name: name.to_string(),
                    description: String::new(),
                    parameters: serde_json::json!({}),
                })
                .collect::<Vec<_>>();

        let next_tools = tool_defs_for_agent_iteration(&tool_defs, &BTreeSet::new(), false);

        assert_eq!(
            next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            vec!["list_documents", "grounded_answer", "search_documents", "search_entities"]
        );
    }

    #[test]
    fn grounded_answer_tool_result_is_compact_for_model_messages() {
        let execution_id = Uuid::now_v7();
        let runtime_execution_id = Uuid::now_v7();
        let structured_content = serde_json::json!({
            "answerBody": "Use `/etc/alpha.ini`.",
            "executionId": execution_id,
            "runtimeExecutionId": runtime_execution_id,
            "conversationId": Uuid::now_v7(),
            "libraryId": Uuid::now_v7(),
            "workspaceId": Uuid::now_v7(),
            "lifecycleState": "completed",
            "finalizable": true,
            "mustPreserveSpans": ["/etc/alpha.ini"],
            "executionDetail": {
                "execution": {
                    "id": execution_id,
                    "runtimeExecutionId": runtime_execution_id
                },
                "verificationState": "verified",
                "verificationWarnings": [],
                "chunkReferences": (0..12)
                    .map(|index| serde_json::json!({
                        "chunkId": Uuid::now_v7(),
                        "rank": index + 1,
                        "score": 1.0
                    }))
                    .collect::<Vec<_>>(),
                "preparedSegmentReferences": [],
                "technicalFactReferences": [],
                "entityReferences": [],
                "relationReferences": []
            }
        });
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "grounded answer body".to_string(),
            }],
            structured_content,
            is_error: false,
        };

        let message = tool_result_model_message(GROUNDED_ANSWER_TOOL_NAME, &result);

        assert!(message.contains("grounded answer body"));
        assert!(message.contains("mustPreserveSpans"));
        assert!(message.contains("/etc/alpha.ini"));
        assert!(!message.contains("answerBody"));
        assert!(message.contains(&execution_id.to_string()));
        assert!(message.contains(&runtime_execution_id.to_string()));
        assert!(message.contains("\"referenceCounts\""));
        assert!(message.contains("\"chunkReferences\":12"));
        assert!(message.contains("\"omittedCount\":12"));
        assert!(!message.contains("\"rank\":1"));
        assert!(!message.contains("\"executionDetail\""));

        let verification_text =
            tool_result_verification_text(GROUNDED_ANSWER_TOOL_NAME, &result).unwrap();
        assert!(verification_text.contains("\"omittedCount\": 4"));
        assert!(verification_text.contains("\"rank\": 1"));
    }

    #[test]
    fn ui_agent_bounds_grounded_answer_tool_top_k_to_parent_turn() {
        let mut missing = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "focused subquestion"
        });
        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut missing,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
        );
        assert_eq!(missing["topK"], 8);
        assert_eq!(missing["library"], "workspace-a/library-b");

        let mut wider = serde_json::json!({
            "library": "workspace-x/library-y",
            "query": "focused subquestion",
            "topK": 24
        });
        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut wider,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
        );
        assert_eq!(wider["topK"], 8);
        assert_eq!(wider["library"], "workspace-a/library-b");

        let mut narrower = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "focused subquestion",
            "topK": 4
        });
        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut narrower,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
        );
        assert_eq!(narrower["topK"], 8);
    }

    #[test]
    fn ui_agent_defaults_non_contextual_grounded_answer_top_k_to_canonical_default() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "focused subquestion"
        });

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "focused subquestion",
            32,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["topK"], 24);
    }

    #[test]
    fn ui_agent_raises_non_contextual_grounded_answer_tool_top_k_floor() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "focused subquestion",
            "topK": 5
        });

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "focused subquestion",
            32,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["topK"], 24);
    }

    #[test]
    fn effective_tool_call_fingerprint_normalizes_defaulted_arguments() {
        let left = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            r#"{"query":"focused subquestion"}"#,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
        )
        .expect("fingerprint");
        let right = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            r#"{"topK":8,"library":"workspace-a/library-b","query":"focused subquestion"}"#,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
        )
        .expect("fingerprint");

        assert_eq!(left, right);
    }

    #[test]
    fn effective_tool_call_fingerprint_keeps_scope_errors_authoritative() {
        let fingerprint = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            r#"{"query":"focused subquestion","library":"workspace-x/library-y"}"#,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
        );

        assert!(fingerprint.is_none());
    }

    #[test]
    fn effective_tool_call_fingerprint_detects_contextual_history_defaults() {
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "original question".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "original answer".to_string(),
            },
        ];
        let first = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            r#"{"library":"workspace-a/library-b","query":"follow-up"}"#,
            "follow-up",
            32,
            "workspace-a/library-b",
            &history,
        )
        .expect("fingerprint");
        let second = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            r#"{"conversationTurns":[],"topK":24,"query":"follow-up","library":"workspace-a/library-b"}"#,
            "follow-up",
            32,
            "workspace-a/library-b",
            &history,
        )
        .expect("fingerprint");

        assert_eq!(first, second);
    }

    #[test]
    fn duplicate_effective_tool_payload_is_suppressed_before_dispatch() {
        let tool_calls = vec![
            ChatToolCall {
                id: "call-1".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({
                    "library": "workspace-a/library-b",
                    "query": "focused subquestion",
                    "topK": 8
                })
                .to_string(),
            },
            ChatToolCall {
                id: "call-2".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({
                    "query": "focused subquestion"
                })
                .to_string(),
            },
        ];
        let mut outcomes = vec![None; tool_calls.len()];
        let mut seen = BTreeMap::new();

        let pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut outcomes,
        );

        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].index, 0);
        assert!(outcomes[0].is_none());
        let duplicate = outcomes[1].as_ref().expect("duplicate outcome");
        assert!(duplicate.is_error);
        assert!(
            duplicate
                .result_text
                .as_deref()
                .is_some_and(|text| text.contains("duplicate MCP tool call suppressed"))
        );
    }

    #[test]
    fn completed_effective_tool_payload_tracking_is_turn_scoped() {
        let tool_calls = vec![ChatToolCall {
            id: "call-1".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({
                "library": "workspace-a/library-b",
                "query": "focused subquestion",
                "topK": 8
            })
            .to_string(),
        }];
        let mut first_outcomes = vec![None; tool_calls.len()];
        let mut seen = BTreeMap::new();
        let first_pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut first_outcomes,
        );
        let fingerprint = first_pending[0].fingerprint.clone().expect("fingerprint");
        seen.insert(fingerprint, EffectiveToolPayloadState::Completed);
        let mut second_outcomes = vec![None; tool_calls.len()];
        let second_pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut second_outcomes,
        );

        assert_eq!(first_pending.len(), 1);
        assert!(second_pending.is_empty());
        assert!(first_outcomes[0].is_none());
        let duplicate = second_outcomes[0].as_ref().expect("duplicate outcome");
        assert!(duplicate.is_error);
    }

    #[test]
    fn duplicate_effective_tool_payload_can_retry_after_actual_error() {
        let tool_calls = vec![ChatToolCall {
            id: "call-1".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({
                "library": "workspace-a/library-b",
                "query": "focused subquestion",
                "topK": 8
            })
            .to_string(),
        }];
        let mut seen = BTreeMap::new();
        let mut first_outcomes = vec![None; tool_calls.len()];
        let first_pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut first_outcomes,
        );
        let fingerprint = first_pending[0].fingerprint.clone().expect("fingerprint");
        let error = tool_execution_error("upstream timeout");
        record_effective_tool_payload_outcome(&mut seen, fingerprint, &error);
        let mut second_outcomes = vec![None; tool_calls.len()];
        let second_pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut second_outcomes,
        );

        assert_eq!(first_pending.len(), 1);
        assert_eq!(second_pending.len(), 1);
        assert!(first_outcomes[0].is_none());
        assert!(second_outcomes[0].is_none());
    }

    #[test]
    fn distinct_effective_tool_payloads_remain_allowed_in_one_turn() {
        let tool_calls = vec![ChatToolCall {
            id: "call-1".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({
                "library": "workspace-a/library-b",
                "query": "focused subquestion B",
                "topK": 8
            })
            .to_string(),
        }];
        let mut seen = BTreeMap::new();
        let mut first_outcomes = vec![None; tool_calls.len()];
        let first_pending = prepare_agent_tool_calls(
            &[ChatToolCall {
                id: "call-0".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({
                    "library": "workspace-a/library-b",
                    "query": "focused subquestion A",
                    "topK": 8
                })
                .to_string(),
            }],
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut first_outcomes,
        );
        let mut second_outcomes = vec![None; tool_calls.len()];
        let second_pending = prepare_agent_tool_calls(
            &tool_calls,
            "focused subquestion",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut second_outcomes,
        );

        assert_eq!(first_pending.len(), 1);
        assert_eq!(second_pending.len(), 1);
        assert!(first_outcomes[0].is_none());
        assert!(second_outcomes[0].is_none());
    }

    #[test]
    fn ui_agent_raises_contextual_grounded_answer_tool_top_k_floor() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "follow-up subquestion",
            "topK": 4,
            "conversationTurns": [
                {"role": "user", "content": "original question"},
                {"role": "assistant", "content": "original answer"}
            ]
        });

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "follow-up subquestion",
            true,
            false,
            32,
            "workspace-a/library-b",
            &[
                ExternalConversationTurn {
                    turn_kind: QueryTurnKind::User,
                    content_text: "original question".to_string(),
                },
                ExternalConversationTurn {
                    turn_kind: QueryTurnKind::Assistant,
                    content_text: "original answer".to_string(),
                },
            ],
        );

        assert_eq!(arguments["topK"], 24);
    }

    #[test]
    fn ui_agent_raises_injected_contextual_grounded_answer_tool_top_k_floor() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "follow-up subquestion",
            "topK": 4
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "original question".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "original answer".to_string(),
            },
        ];

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "follow-up subquestion",
            true,
            false,
            32,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["topK"], 24);
        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "original question"},
                {"role": "assistant", "content": "original answer"}
            ])
        );
    }

    #[test]
    fn ui_agent_omits_typed_history_for_non_contextual_grounded_answer() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "new standalone topic",
            "topK": 4,
            "conversationTurns": []
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "previous topic".to_string(),
        }];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "new standalone topic",
            32,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["topK"], 24);
        assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
    }

    #[test]
    fn ui_agent_restores_short_single_grounded_answer_query_to_full_user_question() {
        let user_question =
            "AlphaZero BetaOne GammaTwo DeltaThree EpsilonFour ZetaFive EtaSix ThetaSeven";
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "AlphaZero GammaTwo DeltaThree",
            "topK": 5
        });

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            user_question,
            false,
            true,
            8,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["query"], user_question);
    }

    #[test]
    fn ui_agent_keeps_short_multi_tool_grounded_answer_query_focused() {
        let user_question =
            "AlphaZero BetaOne GammaTwo DeltaThree EpsilonFour ZetaFive EtaSix ThetaSeven";
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "AlphaZero GammaTwo DeltaThree",
            "topK": 5
        });

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            user_question,
            false,
            false,
            8,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["query"], "AlphaZero GammaTwo DeltaThree");
    }

    #[test]
    fn ui_agent_restores_single_grounded_answer_query_with_missing_user_terms() {
        let user_question = "AlphaOne BetaTwo GammaThree DeltaFour EpsilonFive ZetaSix";
        let mut single_tool_arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "AlphaOne BetaTwo GammaThree DeltaFour",
            "topK": 5
        });
        let mut multi_tool_arguments = single_tool_arguments.clone();

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut single_tool_arguments,
            user_question,
            false,
            true,
            8,
            "workspace-a/library-b",
            &[],
        );
        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut multi_tool_arguments,
            user_question,
            false,
            false,
            8,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(single_tool_arguments["query"], user_question);
        assert_eq!(multi_tool_arguments["query"], "AlphaOne BetaTwo GammaThree DeltaFour");
    }

    #[test]
    fn ui_agent_restores_single_grounded_answer_query_with_similar_length_paraphrase() {
        let user_question = "AlphaOne BetaTwo GammaThree DeltaFour EpsilonFive ZetaSix EtaSeven";
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "AlphaOne BetaTwo GammaThree DeltaFour EpsilonNine ZetaTen",
            "topK": 5
        });

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            user_question,
            false,
            true,
            8,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["query"], user_question);
    }

    #[test]
    fn ui_agent_defaults_grounded_answer_conversation_turns_from_typed_history() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "follow-up question"
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "original user question".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "original assistant answer".to_string(),
            },
        ];

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "follow-up question",
            true,
            false,
            8,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "original user question"},
                {"role": "assistant", "content": "original assistant answer"}
            ])
        );
    }

    #[test]
    fn ui_agent_drops_explicit_grounded_answer_conversation_turns_without_server_history() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "follow-up question",
            "conversationTurns": [
                {"role": "user", "content": "model supplied context"}
            ]
        });
        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "follow-up question",
            8,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
    }

    #[test]
    fn ui_agent_replaces_stale_explicit_grounded_answer_conversation_turns_with_server_history() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "show full ready config",
            "conversationTurns": [
                {"role": "user", "content": "how do I configure connector alpha"},
                {"role": "assistant", "content": "Connector Alpha uses `alphaFlag`."},
                {"role": "user", "content": "configure it"}
            ]
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "configure it".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text:
                    "Install `pkg-alpha`, edit `/opt/alpha/alpha.conf`, set `alphaSecret`."
                        .to_string(),
            },
        ];

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "show full ready config",
            true,
            false,
            8,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "configure it"},
                {
                    "role": "assistant",
                    "content": "Install `pkg-alpha`, edit `/opt/alpha/alpha.conf`, set `alphaSecret`."
                }
            ])
        );
    }

    #[test]
    fn ui_agent_keeps_model_requested_grounded_answer_history_from_server_context() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "show full ready config",
            "conversationTurns": [
                {"role": "user", "content": "model supplied context"}
            ]
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "configure Connector Alpha".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text:
                    "Install `pkg-alpha`, edit `/opt/alpha/alpha.conf`, set `alphaSecret`."
                        .to_string(),
            },
        ];

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "show full ready config",
            false,
            false,
            32,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "configure Connector Alpha"},
                {
                    "role": "assistant",
                    "content": "Install `pkg-alpha`, edit `/opt/alpha/alpha.conf`, set `alphaSecret`."
                }
            ])
        );
        assert_eq!(arguments["topK"], 24);
    }

    #[test]
    fn ui_agent_ignores_llm_supplied_literal_padding_without_server_history() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "focused subquestion",
            "topK": 4,
            "conversationTurns": [
                {
                    "role": "assistant",
                    "content": "`fake-a` `fake-b` `fake-c` `fake-d` `fake-e` `fake-f` `fake-g` `fake-h`"
                }
            ]
        });

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "focused subquestion",
            24,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["topK"], 24);
        assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
    }

    #[test]
    fn ui_agent_drops_explicit_empty_grounded_answer_conversation_turns_for_standalone() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "new standalone topic",
            "conversationTurns": []
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::User,
            content_text: "previous topic".to_string(),
        }];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "new standalone topic",
            8,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
    }

    #[test]
    fn ui_agent_strips_prior_subject_from_non_contextual_grounded_answer_query() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "inventory latest ten release entries for ConnectorAlpha and alphaSecret changes",
            "conversationTurns": []
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text:
                "ConnectorAlpha setup uses `alphaSecret` and `/opt/alpha/connector.conf`."
                    .to_string(),
        }];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "inventory latest ten release entries",
            24,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["query"], "inventory latest ten release entries");
        assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
    }

    #[test]
    fn ui_agent_overrides_explicit_empty_grounded_answer_conversation_turns_for_follow_up() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "follow-up question",
            "conversationTurns": []
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "original user question".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "original assistant answer".to_string(),
            },
        ];

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "follow-up question",
            true,
            false,
            8,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "original user question"},
                {"role": "assistant", "content": "original assistant answer"}
            ])
        );
    }

    #[test]
    fn ui_agent_compacts_history_padded_grounded_answer_query_without_losing_anchors() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "Q: [S0], k0, https://localhost/api, [X.f0], http://localhost, k1, merchantId0, cred0, p0-module, staticId0, staticPayload0, codeTtl0, /opt/p0/p0.conf, /var/log/p0.log",
            "conversationTurns": []
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "Q0".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "`p0-module` `/opt/p0/p0.conf` `cred0`".to_string(),
            },
        ];

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "Q",
            true,
            false,
            24,
            "workspace-a/library-b",
            &history,
        );

        let compact_query = arguments["query"].as_str().unwrap();
        assert!(compact_query.starts_with("Q\n@:"));
        assert!(compact_query.chars().count() < 900);
        assert!(compact_query.contains("[S0]"));
        assert!(compact_query.contains("https://localhost/api"));
        assert!(compact_query.contains("[X.f0]"));
        assert!(compact_query.contains("merchantId0"));
        assert!(compact_query.contains("cred0"));
        assert!(compact_query.contains("p0-module"));
        assert!(compact_query.contains("/opt/p0/p0.conf"));
        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([
                {"role": "user", "content": "Q0"},
                {
                    "role": "assistant",
                    "content": "`p0-module` `/opt/p0/p0.conf` `cred0`"
                }
            ])
        );
    }

    #[test]
    fn ui_agent_keeps_user_supplied_long_grounded_answer_query() {
        let user_question = "Q: [S0], k0, https://localhost/api, [X.f0], http://localhost, k1, merchantId0, cred0, p0-module, staticId0, staticPayload0, codeTtl0, /opt/p0/p0.conf, /var/log/p0.log";
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": user_question
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "A0".to_string(),
        }];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            user_question,
            24,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["query"], user_question);
    }

    #[test]
    fn ui_agent_keeps_current_turn_rewrite_when_literals_are_not_from_history() {
        let user_question = "Q";
        let rewritten_query = "Q: `/opt/p1/p1.conf`, `[S0]`, `cred0`, `tok0`, `https://p1.local/api`, `k1 = 30`, `p1-module`";
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": rewritten_query
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "A0".to_string(),
        }];

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            user_question,
            24,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["query"], rewritten_query);
    }

    #[test]
    fn ui_agent_preserves_plain_code_literals_from_prior_grounded_answer_when_compacting_query() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "Q: [S0], [X.f0], https://p0.local/api, k0, k1, k2, fn0, cred0, codeTtl0, /opt/p0/p0.conf, staticId0, staticPayload0, qrId0, qrPayload0, qrTtl0, qrPoll0, qrStatusTtl0, /var/log/p0.log",
            "conversationTurns": []
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "`k0` `k1` `k2` `fn0` `cred0` `codeTtl0` `/opt/p0/p0.conf`".to_string(),
        }];

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "Q",
            true,
            false,
            24,
            "workspace-a/library-b",
            &history,
        );

        let compact_query = arguments["query"].as_str().unwrap();
        assert!(compact_query.starts_with("Q\n@:"));
        assert!(compact_query.contains("k0"));
        assert!(compact_query.contains("k1"));
        assert!(compact_query.contains("k2"));
        assert!(compact_query.contains("fn0"));
        assert!(compact_query.contains("cred0"));
        assert!(compact_query.contains("codeTtl0"));
        assert!(compact_query.contains("/opt/p0/p0.conf"));
    }

    #[test]
    fn ui_agent_omits_empty_literal_anchor_tail_when_budget_is_exhausted() {
        let user_question = format!("Q {}", "segment ".repeat(140));
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": format!(
                "{} [S0] [X.f0] https://p0.local/api p0-module /opt/p0/p0.conf cred0 staticId0 staticPayload0",
                user_question
            ),
            "conversationTurns": []
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "`p0-module` `/opt/p0/p0.conf` `cred0` `staticId0`".to_string(),
        }];

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            &user_question,
            true,
            false,
            24,
            "workspace-a/library-b",
            &history,
        );

        let compact_query = arguments["query"].as_str().unwrap();
        assert_eq!(compact_query, user_question.trim());
        assert!(!compact_query.contains("\n@:"));
    }

    #[test]
    fn ui_agent_bounds_high_fanout_tool_limits() {
        let mut missing = serde_json::json!({
            "library": "workspace-x/library-y",
            "query": "focused probe"
        });
        apply_agent_tool_argument_defaults(
            "search_entities",
            &mut missing,
            "focused probe",
            8,
            "workspace-a/library-b",
            &[],
        );
        assert_eq!(missing["limit"], 8);
        assert_eq!(missing["library"], "workspace-a/library-b");

        let mut wider = serde_json::json!({
            "library": "workspace-a/library-b",
            "limit": 200
        });
        apply_agent_tool_argument_defaults(
            "get_graph_topology",
            &mut wider,
            "",
            12,
            "workspace-a/library-b",
            &[],
        );
        assert_eq!(wider["limit"], 12);

        let mut narrower = serde_json::json!({
            "library": "workspace-a/library-b",
            "limit": 4
        });
        apply_agent_tool_argument_defaults(
            "list_relations",
            &mut narrower,
            "",
            12,
            "workspace-a/library-b",
            &[],
        );
        assert_eq!(narrower["limit"], 4);
    }

    #[test]
    fn ui_agent_forces_search_documents_to_session_library_scope() {
        let mut arguments = serde_json::json!({
            "query": "focused probe",
            "limit": 99
        });

        apply_agent_tool_argument_defaults(
            SEARCH_DOCUMENTS_TOOL_NAME,
            &mut arguments,
            "focused probe",
            8,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["libraries"], serde_json::json!(["workspace-a/library-b"]));
        assert_eq!(arguments["limit"], 8);
    }

    #[test]
    fn ui_agent_pads_search_documents_with_contextual_history_anchors() {
        let mut arguments = serde_json::json!({
            "query": "settings parameters",
            "limit": 5
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "literals: `alpha-package`, `/etc/alpha.ini`, `[Main]`, `retryTimeout`, `plainword`\nUse `alpha-package` and `retryTimeout`."
                .to_string(),
        }];

        apply_agent_tool_argument_defaults_with_context(
            SEARCH_DOCUMENTS_TOOL_NAME,
            &mut arguments,
            "explain all parameters",
            true,
            false,
            8,
            "workspace-a/library-b",
            &history,
        );

        let query = arguments["query"].as_str().expect("query string");
        assert!(query.starts_with("settings parameters"));
        assert!(query.contains("alpha-package"));
        assert!(query.contains("/etc/alpha.ini"));
        assert!(query.contains("[Main]"));
        assert!(query.contains("retryTimeout"));
        assert!(!query.contains("plainword"));
        assert_eq!(arguments["libraries"], serde_json::json!(["workspace-a/library-b"]));
    }

    #[test]
    fn ui_agent_does_not_pad_self_sufficient_contextual_search_query() {
        let mut arguments = serde_json::json!({
            "query": "alphaPackage retryTimeout settings parameters",
            "limit": 5
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text:
                "Choose one: `alphaPackage`, `betaImage`, `/etc/alpha.ini`, `[Main]`, `retryTimeout`."
                    .to_string(),
        }];

        apply_agent_tool_argument_defaults_with_context(
            SEARCH_DOCUMENTS_TOOL_NAME,
            &mut arguments,
            "show settings parameters",
            true,
            false,
            8,
            "workspace-a/library-b",
            &history,
        );

        assert_eq!(arguments["query"], "alphaPackage retryTimeout settings parameters");
        assert_eq!(arguments["libraries"], serde_json::json!(["workspace-a/library-b"]));
    }

    #[test]
    fn ui_agent_keeps_search_documents_query_without_contextual_anchors() {
        let mut arguments = serde_json::json!({
            "query": "settings parameters",
            "limit": 5
        });

        apply_agent_tool_argument_defaults(
            SEARCH_DOCUMENTS_TOOL_NAME,
            &mut arguments,
            "settings parameters",
            8,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["query"], "settings parameters");
    }

    #[test]
    fn ui_agent_normalizes_stringified_search_document_scope_arguments() {
        let mut arguments = serde_json::json!({
            "query": "focused probe",
            "libraries": "[\"workspace-a/library-b\"]",
            "limit": "10"
        });

        normalize_agent_tool_argument_types(SEARCH_DOCUMENTS_TOOL_NAME, &mut arguments);

        validate_agent_tool_library_scope(
            SEARCH_DOCUMENTS_TOOL_NAME,
            &arguments,
            "workspace-a/library-b",
        )
        .expect("normalized scope");
        apply_agent_tool_argument_defaults(
            SEARCH_DOCUMENTS_TOOL_NAME,
            &mut arguments,
            "focused probe",
            8,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["libraries"], serde_json::json!(["workspace-a/library-b"]));
        assert_eq!(arguments["limit"], 8);
    }

    #[test]
    fn ui_agent_rejects_cross_library_stringified_search_document_scope() {
        let mut arguments = serde_json::json!({
            "query": "focused probe",
            "libraries": "[\"workspace-x/library-y\"]"
        });

        normalize_agent_tool_argument_types(SEARCH_DOCUMENTS_TOOL_NAME, &mut arguments);
        let error = validate_agent_tool_library_scope(
            SEARCH_DOCUMENTS_TOOL_NAME,
            &arguments,
            "workspace-a/library-b",
        )
        .expect_err("scope mismatch");

        assert!(error.contains("library scope mismatch"));
        assert!(error.contains("workspace-x/library-y"));
        assert!(error.contains("workspace-a/library-b"));
    }

    #[test]
    fn ui_agent_rejects_cross_library_single_scope_arguments() {
        let arguments = serde_json::json!({
            "library": "workspace-x/library-y",
            "query": "focused probe"
        });

        let error = validate_agent_tool_library_scope(
            GROUNDED_ANSWER_TOOL_NAME,
            &arguments,
            "workspace-a/library-b",
        )
        .expect_err("scope mismatch");

        assert!(error.contains("library scope mismatch"));
        assert!(error.contains("workspace-x/library-y"));
        assert!(error.contains("workspace-a/library-b"));
    }

    #[test]
    fn ui_agent_rejects_cross_library_search_scope_arguments() {
        let arguments = serde_json::json!({
            "query": "focused probe",
            "libraries": ["workspace-a/library-b", "workspace-x/library-y"]
        });

        let error = validate_agent_tool_library_scope(
            SEARCH_DOCUMENTS_TOOL_NAME,
            &arguments,
            "workspace-a/library-b",
        )
        .expect_err("scope mismatch");

        assert!(error.contains("library scope mismatch"));
        assert!(error.contains("workspace-x/library-y"));
    }

    #[test]
    fn tool_result_full_text_extracts_all_content_blocks() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![
                crate::interfaces::http::mcp::McpContentBlock {
                    content_type: "text",
                    text: "First supported paragraph.".to_string(),
                },
                crate::interfaces::http::mcp::McpContentBlock {
                    content_type: "text",
                    text: "Second supported paragraph.".to_string(),
                },
            ],
            structured_content: serde_json::json!({
                "finalAnswerReady": true,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "verified"
                }
            }),
            is_error: false,
        };

        let answer = tool_result_full_text(&result.content).expect("answer");

        assert_eq!(answer, "First supported paragraph.\n\nSecond supported paragraph.");
    }

    #[test]
    fn verified_grounded_answer_fallback_keeps_full_text() {
        let long_answer = "A".repeat(2_500);
        let content = vec![crate::interfaces::http::mcp::McpContentBlock {
            content_type: "text",
            text: long_answer.clone(),
        }];
        let fallback_text =
            tool_result_answer_text(GROUNDED_ANSWER_TOOL_NAME, &content).expect("fallback text");
        let preview_text = tool_result_preview(&content).expect("preview text");
        let outcome = ToolExecutionOutcome {
            arguments_json: None,
            requested_arguments_json: None,
            message_content: String::new(),
            result_text: Some(fallback_text),
            result_json: None,
            grounding_text: None,
            grounded_answer_ready: true,
            grounded_answer_completed: true,
            grounded_answer_needs_follow_up: false,
            is_error: false,
            duration_ms: 0,
            child_query_execution_ids: Vec::new(),
            child_runtime_execution_ids: Vec::new(),
        };

        let remembered =
            remember_verified_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome)
                .expect("remembered answer");

        assert_eq!(preview_text.chars().count(), 2_000);
        assert_eq!(remembered, long_answer);
        assert_eq!(remembered.chars().count(), 2_500);
    }

    #[test]
    fn verified_grounded_answer_memory_prefers_structured_answer_body() {
        let outcome = ToolExecutionOutcome {
            arguments_json: None,
            requested_arguments_json: None,
            message_content: String::new(),
            result_text: Some(
                "Clean Alpha answer.\n\nSources:\nAlpha source title\nBeta source title"
                    .to_string(),
            ),
            result_json: Some(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": "Clean Alpha answer.\n\nSources:\nAlpha source title\nBeta source title"
                }],
                "structuredContent": {
                    "answerBody": "Clean Alpha answer.",
                    "finalAnswerReady": true,
                    "lifecycleState": "completed",
                    "executionDetail": {
                        "verificationState": "verified"
                    }
                },
                "isError": false
            })),
            grounding_text: None,
            grounded_answer_ready: true,
            grounded_answer_completed: true,
            grounded_answer_needs_follow_up: false,
            is_error: false,
            duration_ms: 0,
            child_query_execution_ids: Vec::new(),
            child_runtime_execution_ids: Vec::new(),
        };

        let remembered =
            remember_verified_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome)
                .expect("remembered answer");
        let guard =
            remember_verified_grounded_answer_guard_text(None, GROUNDED_ANSWER_TOOL_NAME, &outcome)
                .expect("guard text");
        let completed =
            remember_completed_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome)
                .expect("completed answer");

        assert_eq!(remembered, "Clean Alpha answer.");
        assert_eq!(guard, "Clean Alpha answer.");
        assert_eq!(completed, "Clean Alpha answer.");
    }

    #[test]
    fn finalization_keeps_verified_ordered_source_inventory() {
        let verified = "3/3\n\n\
1. source=`Release 3.0.0`\n   Added Gamma support.\n\
2. source=`Release 2.0.0`\n   Added Beta support.\n\
3. source=`Release 1.0.0`\n   Added Alpha support.";
        let model_rewrite = "Latest records:\n\n\
1. Release 3.0.0 - Added Gamma support.\n\
2. Release 2.0.0 - Added Beta support.\n\
3. Release 1.0.0 - Added Alpha support.";

        let answer = finalize_agent_loop_answer(
            model_rewrite.to_string(),
            "list latest release records",
            Some(verified),
            "request-1",
            Uuid::now_v7(),
        );

        assert_eq!(answer, verified);
    }

    #[test]
    fn finalization_keeps_verified_grounded_identifier_inventory() {
        let verified = "\
Use module `alpha-connector`.
Config file: `/opt/alpha/modules/connector.conf`, section `[Main]`.
Parameters: `url`, `staticQrPayload`, `fillPaymentDetails`, `paymentDetails`.";
        let model_rewrite = "\
Use module `alpha-connector`.
Config file: `/opt/alpha/modules/connector.conf`, section `[Main]`.
Parameters: `url`, `staticQrPayload`.";

        let answer = finalize_agent_loop_answer(
            model_rewrite.to_string(),
            "configure Provider Alpha",
            Some(verified),
            "request-1",
            Uuid::now_v7(),
        );

        assert_eq!(answer, verified);
    }

    #[test]
    fn final_grounded_answer_requires_ready_flag() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Verified but not explicitly final.".to_string(),
            }],
            structured_content: serde_json::json!({
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "verified",
                    "execution": {
                        "lifecycleState": "completed"
                    }
                }
            }),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn unverified_grounded_answer_is_not_final_evidence() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "unsupported answer".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": false,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "insufficient_evidence"
                }
            }),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn clarification_grounded_answer_does_not_force_follow_up() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Several matching documents were found. Please choose one.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": false,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "not_run"
                }
            }),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(!grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn completed_grounded_answer_without_verification_state_requires_follow_up() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Completed answer with no verifier state.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": false,
                "lifecycleState": "completed"
            }),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn final_grounded_answer_readiness_rejects_unverified_completed_answer() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Completed answer with warnings.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": false,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "lenient"
                }
            }),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_completed(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn final_grounded_answer_ready_flag_controls_final_readiness() {
        let ready_without_execution_detail = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Flag-only answer.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": true,
            }),
            is_error: false,
        };
        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &ready_without_execution_detail));

        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Ready answer.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": true,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "verified"
                }
            }),
            is_error: false,
        };

        assert!(grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn final_grounded_answer_with_verifier_warnings_is_ready_when_verified() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Warning-bearing but verified answer.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": true,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "verified",
                    "verificationWarnings": [
                        {
                            "code": "partial_coverage",
                            "warning": "Only part of the requested evidence was grounded."
                        }
                    ]
                }
            }),
            is_error: false,
        };

        assert!(grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn final_grounded_answer_with_non_evidence_warning_does_not_force_follow_up() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Warning-bearing verified answer.".to_string(),
            }],
            structured_content: serde_json::json!({
                "finalAnswerReady": true,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "verified",
                    "verificationWarnings": [
                        {
                            "code": "operator_note",
                            "warning": "Metadata-only note."
                        }
                    ]
                }
            }),
            is_error: false,
        };

        assert!(grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(!grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn completed_grounded_answer_fallback_keeps_non_verified_text() {
        let outcome = ToolExecutionOutcome {
            arguments_json: None,
            requested_arguments_json: None,
            message_content: String::new(),
            result_text: Some("  Completed tool answer.  ".to_string()),
            result_json: None,
            grounding_text: None,
            grounded_answer_ready: false,
            grounded_answer_completed: true,
            grounded_answer_needs_follow_up: true,
            is_error: false,
            duration_ms: 0,
            child_query_execution_ids: Vec::new(),
            child_runtime_execution_ids: Vec::new(),
        };

        let remembered =
            remember_completed_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome)
                .expect("remembered answer");

        assert_eq!(remembered, "Completed tool answer.");
    }

    #[test]
    fn completed_grounded_answer_fallback_requires_iteration_cap() {
        assert!(should_return_completed_grounded_answer_on_iteration_cap(
            AgentStopReason::IterationCap,
            1,
            Some("Completed tool answer.")
        ));
        assert!(!should_return_completed_grounded_answer_on_iteration_cap(
            AgentStopReason::Deadline,
            1,
            Some("Completed tool answer.")
        ));
        assert!(!should_return_completed_grounded_answer_on_iteration_cap(
            AgentStopReason::IterationCap,
            0,
            Some("Completed tool answer.")
        ));
        assert!(!should_return_completed_grounded_answer_on_iteration_cap(
            AgentStopReason::IterationCap,
            2,
            Some("Completed tool answer.")
        ));
        assert!(!should_return_completed_grounded_answer_on_iteration_cap(
            AgentStopReason::IterationCap,
            1,
            Some("   ")
        ));
    }

    #[test]
    fn verified_grounded_answer_fallback_waits_after_multi_tool_synthesis() {
        assert_eq!(verified_grounded_answer_fallback_candidate(Some("A0"), 1, 1), Some("A0"));
        assert_eq!(verified_grounded_answer_fallback_candidate(Some("A0"), 2, 2), None);
        assert_eq!(verified_grounded_answer_fallback_candidate(Some("A0"), 1, 2), None);
    }

    #[test]
    fn final_answer_prefers_verified_union_when_parent_drops_multi_tool_anchors() {
        let first = "A: `A1`, `A2`, `A3`, and `A4`.";
        let second = "B: `B1`, `B2`, `B3`, and `B4`.";
        let union = format!("{first}\n\n{second}");
        let answer = "A: `A1` and `A2`. B: `B1`.".to_string();

        let guard = verified_grounded_answer_guard_candidate(Some(second), Some(&union), 2, 2)
            .expect("multi-tool verified guard");
        let finalized = finalize_agent_loop_answer(answer, "Q?", Some(guard), "req", Uuid::nil());

        assert_eq!(guard, union);
        assert_eq!(finalized, union);
    }

    #[test]
    fn successful_tool_result_becomes_verifier_grounding() {
        let mut grounding = AssistantGroundingEvidence::default();
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Document read completed.".to_string(),
            }],
            structured_content: serde_json::json!({
                "documentTitle": "Alpha overview",
                "content": "The release channel is stable."
            }),
            is_error: false,
        };
        let evidence =
            tool_result_verification_text("read_document", &result).expect("verification text");

        push_tool_grounding_fragment(&mut grounding, "read_document", &evidence);

        assert_eq!(grounding.verification_corpus.len(), 1);
        assert!(grounding.verification_corpus[0].contains("read_document"));
        assert!(grounding.verification_corpus[0].contains("Document read completed."));
        assert!(grounding.verification_corpus[0].contains("The release channel is stable."));
        assert!(!grounding.verification_corpus[0].contains("\\\"content\\\""));
    }

    #[test]
    fn inventory_tool_result_is_not_verifier_grounding() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Documents listed.".to_string(),
            }],
            structured_content: serde_json::json!({
                "items": [{
                    "documentId": Uuid::now_v7(),
                    "title": "Alpha overview",
                    "readabilityState": "readable"
                }]
            }),
            is_error: false,
        };

        assert!(tool_result_verification_text("list_documents", &result).is_none());
    }

    #[test]
    fn search_and_graph_results_can_be_verifier_grounding() {
        let search_result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Found Alpha overview.".to_string(),
            }],
            structured_content: serde_json::json!({
                "items": [{
                    "documentTitle": "Alpha overview",
                    "snippet": "The supported mode is standby."
                }]
            }),
            is_error: false,
        };
        let graph_result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Found related entities.".to_string(),
            }],
            structured_content: serde_json::json!({
                "entities": [{
                    "label": "Alpha controller",
                    "summary": "Controls standby mode."
                }]
            }),
            is_error: false,
        };

        let search_evidence = tool_result_verification_text("search_documents", &search_result)
            .expect("search evidence");
        let graph_evidence = tool_result_verification_text("search_entities", &graph_result)
            .expect("graph evidence");

        assert!(search_evidence.contains("The supported mode is standby."));
        assert!(graph_evidence.contains("Controls standby mode."));
    }

    #[test]
    fn tool_error_result_is_not_verifier_grounding() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "read failed".to_string(),
            }],
            structured_content: serde_json::json!({
                "errorKind": "not_found",
                "message": "document not found"
            }),
            is_error: true,
        };

        assert!(tool_result_verification_text("read_document", &result).is_none());
    }

    #[test]
    fn debug_tool_result_json_is_bounded() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Large result completed.".to_string(),
            }],
            structured_content: serde_json::json!({
                "payload": "x".repeat(TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT + 128)
            }),
            is_error: false,
        };

        let debug_json = debug_tool_result_json(&result);

        assert_eq!(debug_json["isError"], false);
        assert_eq!(debug_json["content"][0]["text"], "Large result completed.");
        assert_eq!(debug_json["structuredContent"]["truncated"], true);
        assert!(
            debug_json["structuredContent"]["originalCharCount"].as_u64().unwrap()
                > TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT as u64
        );
    }

    #[test]
    fn runtime_tool_answer_messages_preserve_chat_history_and_tool_result() {
        let history =
            vec![ChatMessage::user("first question"), ChatMessage::assistant_text("first answer")];

        let messages = build_runtime_tool_answer_messages(
            "system prompt".to_string(),
            &history,
            "continue",
            RUNTIME_RETRIEVED_CONTEXT_TOOL,
            serde_json::json!({ "question": "continue" }),
            "grounded context",
        );

        assert_eq!(
            messages.iter().map(|message| message.role.as_str()).collect::<Vec<_>>(),
            vec!["system", "user", "assistant", "user", "assistant", "tool"]
        );
        assert_eq!(messages[1].content.as_deref(), Some("first question"));
        assert_eq!(messages[2].content.as_deref(), Some("first answer"));
        assert_eq!(messages[4].tool_calls.len(), 1);
        assert_eq!(messages[4].tool_calls[0].name, RUNTIME_RETRIEVED_CONTEXT_TOOL);
        assert_eq!(
            messages[5].tool_call_id,
            Some(format!("call_{RUNTIME_RETRIEVED_CONTEXT_TOOL}"))
        );
        assert_eq!(messages[5].content.as_deref(), Some("grounded context"));
    }
}
