//! LLM turn helpers used by the assistant answer surfaces.
//!
//! The in-app UI assistant mirrors the public MCP answer contract with a
//! deterministic runtime-first entry: the runtime dispatches the canonical
//! `grounded_answer` call before model tool selection. If that result is
//! nonterminal, the model sees the answer-tool registry and returned evidence
//! before choosing any lower-level follow-up tools.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    time::{Duration, Instant},
};

use futures::{StreamExt as _, stream};
use serde_json::Value;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use crate::{
    agent_runtime::tasks::query_answer::{
        QueryProviderCall, QueryProviderCallAttribution, QueryProviderCallKind,
    },
    app::state::AppState,
    domains::provider_profiles::ProviderModelSelection,
    domains::query::{
        MAX_TOP_K, QueryAnswerCandidate, QueryAnswerDisposition, QueryClarification, QueryTurnKind,
        resolve_contextual_grounded_answer_top_k,
    },
    domains::query_ir::{QueryLanguage, literal_text_is_identifier_shaped},
    domains::{agent_runtime::RuntimeSurfaceKind, ai::AiBindingPurpose},
    integrations::{
        llm::{ChatMessage, ChatToolCall, ChatToolDef, ToolUseRequest, ToolUseResponse},
        retry::ProviderCallError,
    },
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
    services::ai_catalog_service::ResolvedRuntimeBinding,
    services::query::{
        assistant_grounding::AssistantGroundingEvidence,
        completion_policy::{
            AnswerCompletionAssessment, AnswerCompletionGapReason, GroundedAnswerCompletionEnvelope,
        },
        error::QueryServiceError,
        i18n::grounded_repair_messages,
        llm_context_debug::{
            AgentLoopMetadata, AgentStopReason, LlmIterationDebug, ResponseToolCallDebug,
        },
        provider_billing::{QueryProviderCallReservation, QueryProviderExecutionContext},
        service::ExternalConversationTurn,
    },
    shared::text_tokens::backtick_literal_spans,
};

const RUNTIME_RETRIEVED_CONTEXT_TOOL: &str = "ironrag_retrieved_context";
const RUNTIME_LITERAL_REVISION_CONTEXT_TOOL: &str = "ironrag_literal_revision_context";
const GROUNDED_ANSWER_TOOL_NAME: &str = "grounded_answer";
const RUNTIME_REPAIR_ARGUMENT_FIELD: &str = "_ironragRuntimeRepair";
const GROUNDED_ANSWER_LIFECYCLE_COMPLETED: &str = "completed";
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
/// Consecutive tool-running iterations that may yield zero successful *new*
/// tool results (every call errored, was suppressed as a duplicate, or
/// replayed an earlier result) before the loop forces a final answer instead
/// of spinning the remaining iteration/deadline budget on a stuck model. Two
/// is the smallest window that tolerates a single transient miss while still
/// catching a genuine no-progress loop early.
const NO_PROGRESS_ITERATION_LIMIT: usize = 2;
/// Hard upper bound for a single tool-call wait, independent of how much turn
/// deadline is left. The per-call wait is `min(remaining turn deadline, this)`
/// so one hung tool future can never consume the whole turn budget. Falls back
/// to this when the caller supplies no soft tool-collection target; otherwise
/// the soft target (the canonical per-tool-call SLO threaded into the turn)
/// bounds the wait.
const PER_TOOL_CALL_MAX_WAIT: Duration = Duration::from_secs(35);
const GROUNDED_ANSWER_TOOL_MAX_WAIT: Duration = Duration::from_secs(90);
// A repair is optional recovery after a completed canonical answer. Bound it
// below a full canonical execution so a slow second retrieval cannot consume
// the turn; the completed first result remains available as an explicit
// partial fallback.
const GROUNDED_ANSWER_REPAIR_MAX_WAIT: Duration = Duration::from_secs(45);
const GROUNDED_ANSWER_REPAIR_TOP_K_INCREMENT: usize = 8;
const GROUNDED_ANSWER_REPAIR_TOP_K_HEADROOM: usize = 1;
const STRUCTURAL_IDENTIFIER_MIN_CHARS: usize = 4;
const STRUCTURAL_IDENTIFIER_MAX_CHARS: usize = 400;
const STRUCTURAL_LITERAL_ANCHOR_MAX_CHARS: usize = 180;
const VERIFIED_GROUNDED_LITERAL_GUARD_MIN_LITERALS: usize = 4;
/// Final result of one assistant turn.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum AgentAnswerProvenance {
    Composed,
    CanonicalGroundedAnswerPassthrough,
}

/// Typed finalizer-owned outcome coupled to an exact canonical
/// `grounded_answer` passthrough. Composed answers intentionally carry no
/// child outcome because the parent finalizer owns their disposition.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentCanonicalAnswerOutcome {
    pub disposition: QueryAnswerDisposition,
    pub clarification: QueryClarification,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentTurnResult {
    pub answer: String,
    pub answer_provenance: AgentAnswerProvenance,
    pub canonical_answer_outcome: Option<AgentCanonicalAnswerOutcome>,
    pub usage_json: serde_json::Value,
    /// One immutable attribution record per actual provider response.
    pub provider_calls: Vec<QueryProviderCall>,
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

fn provider_call_attribution(
    binding: &ResolvedRuntimeBinding,
    call_kind: QueryProviderCallKind,
) -> Result<QueryProviderCallAttribution, QueryServiceError> {
    QueryProviderCallAttribution::try_new(
        binding.binding_id,
        binding.binding_purpose,
        ProviderModelSelection {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
        },
        call_kind,
    )
    .map_err(|error| {
        tracing::error!(
            binding_id = %binding.binding_id,
            actual_purpose = binding.binding_purpose.as_str(),
            expected_purpose = call_kind.binding_purpose().as_str(),
            call_kind = call_kind.as_str(),
            "query provider-call attribution purpose mismatch"
        );
        QueryServiceError::Internal(anyhow::Error::new(error))
    })
}

async fn reserve_attributed_provider_call(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    binding: &ResolvedRuntimeBinding,
    call_kind: QueryProviderCallKind,
) -> Result<(QueryProviderCallAttribution, QueryProviderCallReservation), QueryServiceError> {
    // Purpose validation must happen before reservation and before provider
    // I/O; a misrouted binding is never represented as a billable call.
    let attribution = provider_call_attribution(binding, call_kind)?;
    let reservation = QueryProviderCallReservation::reserve(
        state,
        execution_context,
        binding,
        call_kind.binding_purpose(),
        call_kind.as_str(),
    )
    .await
    .map_err(|error| {
        QueryServiceError::Internal(anyhow::anyhow!(
            "failed to reserve {} provider call: {error}",
            call_kind.as_str()
        ))
    })?;
    Ok((attribution, reservation))
}

async fn complete_attributed_provider_call(
    attribution: &QueryProviderCallAttribution,
    reservation: &mut QueryProviderCallReservation,
    usage_json: serde_json::Value,
) -> Result<QueryProviderCall, QueryServiceError> {
    let provider_call_id = reservation.provider_call_id();
    reservation.complete(&usage_json).await.map_err(|error| {
        QueryServiceError::Internal(anyhow::anyhow!(
            "failed to persist provider-call usage {provider_call_id}: {error}"
        ))
    })?;
    Ok(attribution.record(provider_call_id, usage_json))
}

async fn fail_provider_call(reservation: &mut QueryProviderCallReservation) {
    if let Err(error) = reservation.fail().await {
        tracing::error!(
            provider_call_id = %reservation.provider_call_id(),
            %error,
            "failed to terminalize query provider-call reservation"
        );
    }
}

/// Agent-loop failure with the partial provider transcript preserved
/// for the debug panel.
#[derive(Debug)]
pub(crate) struct AgentTurnFailure {
    pub error: QueryServiceError,
    /// Provider responses completed before the loop failed.
    pub provider_calls: Vec<QueryProviderCall>,
    pub debug_iterations: Vec<LlmIterationDebug>,
    pub agent_loop: Option<AgentLoopMetadata>,
}

impl AgentTurnFailure {
    fn empty(error: impl Into<QueryServiceError>) -> Self {
        Self {
            error: error.into(),
            provider_calls: Vec::new(),
            debug_iterations: Vec::new(),
            agent_loop: None,
        }
    }

    fn with_loop(
        error: impl Into<QueryServiceError>,
        provider_calls: Vec<QueryProviderCall>,
        debug_iterations: Vec<LlmIterationDebug>,
        agent_loop: AgentLoopMetadata,
    ) -> Self {
        Self { error: error.into(), provider_calls, debug_iterations, agent_loop: Some(agent_loop) }
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
pub(crate) struct McpToolAgentTurnInput<'a> {
    pub state: &'a AppState,
    pub execution_context: QueryProviderExecutionContext,
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
    /// Exact structured `answerBody` captured before debug/result compaction.
    /// This is the only text eligible for canonical verbatim passthrough.
    grounded_answer_body: Option<String>,
    /// Typed terminal outcome parsed from the unabridged MCP structured
    /// content. Debug compaction must never be used to reconstruct it.
    canonical_answer_outcome: Option<AgentCanonicalAnswerOutcome>,
    grounded_answer_ready: bool,
    grounded_answer_completed: bool,
    grounded_answer_needs_follow_up: bool,
    grounded_answer_repair_reason: Option<GroundedAnswerRepairReason>,
    grounded_answer_language: QueryLanguage,
    grounded_answer_clarification_required: bool,
    is_error: bool,
    /// True when this outcome was replayed from a prior successful call's
    /// cached payload (effective-duplicate) rather than produced by a fresh
    /// execution. A replay is not a *new* successful tool result, so the
    /// no-progress guard treats it the same as an error/duplicate when
    /// deciding whether an iteration made progress.
    is_replay: bool,
    /// Wall-clock the tool ran. Set centrally in `execute_tool_calls`;
    /// constructors default to 0.
    duration_ms: u64,
    child_query_execution_ids: Vec<Uuid>,
    child_runtime_execution_ids: Vec<Uuid>,
}

#[derive(Debug, PartialEq)]
struct TerminalGroundedAnswerPassthrough {
    answer: String,
    stop_reason: AgentStopReason,
    canonical_answer_outcome: AgentCanonicalAnswerOutcome,
}

impl TerminalGroundedAnswerPassthrough {
    fn final_answer(answer: String, canonical_answer_outcome: AgentCanonicalAnswerOutcome) -> Self {
        Self { answer, stop_reason: AgentStopReason::FinalAnswer, canonical_answer_outcome }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum GroundedAnswerRepairReason {
    ProcedureIncomplete,
    TroubleshootingIncomplete,
    AnswerStructureIncomplete,
    OrderedInventory { expected: usize, observed: usize },
    VerificationIncomplete,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeGroundedRepairKind {
    ProcedureIncomplete,
    TroubleshootingIncomplete,
    AnswerStructureIncomplete,
    #[serde(rename = "ordered_inventory_incomplete")]
    OrderedInventory,
    VerificationIncomplete,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeGroundedRepairMetadata {
    reason: RuntimeGroundedRepairKind,
    language: QueryLanguage,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed: Option<usize>,
}

impl RuntimeGroundedRepairMetadata {
    fn from_reason(reason: GroundedAnswerRepairReason, language: QueryLanguage) -> Self {
        match reason {
            GroundedAnswerRepairReason::ProcedureIncomplete => Self {
                reason: RuntimeGroundedRepairKind::ProcedureIncomplete,
                language,
                expected: None,
                observed: None,
            },
            GroundedAnswerRepairReason::TroubleshootingIncomplete => Self {
                reason: RuntimeGroundedRepairKind::TroubleshootingIncomplete,
                language,
                expected: None,
                observed: None,
            },
            GroundedAnswerRepairReason::AnswerStructureIncomplete => Self {
                reason: RuntimeGroundedRepairKind::AnswerStructureIncomplete,
                language,
                expected: None,
                observed: None,
            },
            GroundedAnswerRepairReason::OrderedInventory { expected, observed } => Self {
                reason: RuntimeGroundedRepairKind::OrderedInventory,
                language,
                expected: Some(expected),
                observed: Some(observed),
            },
            GroundedAnswerRepairReason::VerificationIncomplete => Self {
                reason: RuntimeGroundedRepairKind::VerificationIncomplete,
                language,
                expected: None,
                observed: None,
            },
        }
    }

    fn is_consistent(self) -> bool {
        match self.reason {
            RuntimeGroundedRepairKind::OrderedInventory => matches!(
                (self.expected, self.observed),
                (Some(expected), Some(observed)) if expected > 0 && observed < expected
            ),
            RuntimeGroundedRepairKind::ProcedureIncomplete
            | RuntimeGroundedRepairKind::TroubleshootingIncomplete
            | RuntimeGroundedRepairKind::AnswerStructureIncomplete
            | RuntimeGroundedRepairKind::VerificationIncomplete => {
                self.expected.is_none() && self.observed.is_none()
            }
        }
    }
}

/// A grounded answer gets at most one deterministic repair probe per parent
/// turn. Keeping this budget in the runtime (rather than only in the prompt)
/// prevents chooser/retry loops while guaranteeing that the repair query is
/// not an effective duplicate of the original probe.
#[derive(Debug)]
struct FocusedGroundedFollowUpState {
    pending: Option<RuntimeGroundedRepairMetadata>,
    attempted: bool,
    unresolved: bool,
    language: QueryLanguage,
}

impl Default for FocusedGroundedFollowUpState {
    fn default() -> Self {
        Self { pending: None, attempted: false, unresolved: false, language: QueryLanguage::Auto }
    }
}

impl FocusedGroundedFollowUpState {
    fn schedule(&mut self, reason: GroundedAnswerRepairReason, language: QueryLanguage) -> bool {
        if self.attempted || self.pending.is_some() {
            return false;
        }
        self.pending = Some(RuntimeGroundedRepairMetadata::from_reason(reason, language));
        self.unresolved = true;
        self.language = language;
        true
    }

    fn take_call(&mut self, user_question: &str, grounded_top_k: usize) -> Option<ChatToolCall> {
        let metadata = self.pending.take()?;
        self.attempted = true;
        Some(focused_grounded_answer_follow_up_call(user_question, grounded_top_k, metadata))
    }

    fn observe_attempt_outcome(&mut self, outcome: &ToolExecutionOutcome) {
        if !self.attempted || !self.unresolved {
            return;
        }
        if !outcome.is_error
            && !outcome.is_replay
            && outcome.grounded_answer_ready
            && !outcome.grounded_answer_needs_follow_up
            && outcome.grounded_answer_repair_reason.is_none()
        {
            self.unresolved = false;
        }
    }

    fn requires_resolution(&self) -> bool {
        self.unresolved
    }

    fn is_unresolved_after_attempt(&self) -> bool {
        self.attempted && self.pending.is_none() && self.unresolved
    }

    fn was_attempted(&self) -> bool {
        self.attempted
    }

    fn language(&self) -> QueryLanguage {
        self.language
    }
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
    answer_disposition: ironrag_contracts::assistant::AssistantAnswerDisposition,
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

    fn has_guardable_evidence(&self) -> bool {
        self.guardable_entries().next().is_some()
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
        let completion_envelope = parse_grounded_answer_completion_envelope(structured);
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
        for field_name in ["documentTitle", "displayValue", "label", "assertion", "predicate"] {
            push_reference_field_values(
                &mut source_labels,
                &mut seen_labels,
                Some(structured),
                "/referenceSummary/references",
                field_name,
                GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT,
            );
        }
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
            final_answer_ready: completion_envelope
                .as_ref()
                .is_some_and(|envelope| envelope.final_answer_ready),
            finalizable: completion_envelope.as_ref().is_some_and(|envelope| envelope.finalizable),
            answer_disposition: completion_envelope.as_ref().map_or(
                ironrag_contracts::assistant::AssistantAnswerDisposition::NonTerminal,
                |envelope| envelope.readiness.answer_disposition,
            ),
            verification_state: execution_detail
                .and_then(|detail| detail.get("verificationState"))
                .or_else(|| structured.pointer("/verifier/state"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            warning_codes: collect_verification_warning_codes(execution_detail, Some(structured)),
            unsupported_literal_spans: collect_unsupported_literal_spans(
                execution_detail,
                Some(structured),
            ),
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
        matches!(
            self.answer_disposition,
            ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady
        ) && self.answer_body.as_deref().is_some_and(|answer| !answer.trim().is_empty())
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
) -> Result<Vec<ChatToolDef>, tools::McpToolContractError> {
    Ok(tools::visible_tool_contract_with_capabilities(auth, McpToolSurface::Answer, capabilities)?
        .descriptors
        .into_iter()
        .map(|descriptor| ChatToolDef {
            name: descriptor.name.to_string(),
            description: descriptor.description.to_string(),
            parameters: descriptor.input_schema,
        })
        .collect())
}

/// Run the web UI assistant over the answer MCP surface. Every turn starts
/// with the canonical `grounded_answer` tool and may execute one deterministic
/// focused repair. Complete canonical results bypass model synthesis; the
/// model loop remains only for completed results that cannot be returned
/// directly under the answer contract.
pub(crate) async fn run_mcp_tool_agent_turn(
    input: McpToolAgentTurnInput<'_>,
) -> Result<AgentTurnResult, AgentTurnFailure> {
    let context = initialize_mcp_agent_loop(input).await?;
    let mut state = McpAgentLoopState::new(&context);
    for iteration in 1..=context.iteration_cap {
        match run_mcp_agent_iteration(&context, &mut state, iteration)
            .await
            .map_err(|failure| *failure)?
        {
            AgentIterationControl::Continue => {}
            AgentIterationControl::Stop(reason) => {
                state.stopped_reason = reason;
                break;
            }
            AgentIterationControl::Complete(result) => return Ok(*result),
        }
    }
    finish_mcp_agent_loop(&context, state).map_err(|failure| *failure)
}

struct McpAgentLoopContext<'a> {
    input: McpToolAgentTurnInput<'a>,
    binding: ResolvedRuntimeBinding,
    tool_defs: Vec<ChatToolDef>,
    allowed_tool_names: BTreeSet<String>,
    iteration_cap: usize,
    max_parallel_actions: usize,
    deadline_started: Instant,
}

struct McpAgentLoopState {
    messages: Vec<ChatMessage>,
    usage_json: Value,
    provider_calls: Vec<QueryProviderCall>,
    debug_iterations: Vec<LlmIterationDebug>,
    total_tool_call_count: usize,
    successful_tool_call_count: usize,
    successful_tool_names: BTreeSet<String>,
    seen_effective_tool_payloads: BTreeMap<String, EffectiveToolPayloadEntry>,
    assistant_grounding: AssistantGroundingEvidence,
    child_query_execution_ids: Vec<Uuid>,
    stopped_reason: AgentStopReason,
    last_required_tool_refusal_answer: Option<String>,
    verified_grounded_answer_count: usize,
    last_verified_grounded_answer: Option<String>,
    verified_grounded_answer_guard_text: Option<String>,
    last_completed_grounded_answer: Option<String>,
    last_verified_partial_grounded_answer: Option<String>,
    grounded_answer_evidence_ledger: GroundedAnswerEvidenceLedger,
    focused_grounded_follow_up: FocusedGroundedFollowUpState,
    incomplete_grounded_answer_needs_follow_up: bool,
    no_progress_iterations: usize,
    no_progress_force_final: bool,
}

impl McpAgentLoopState {
    fn new(context: &McpAgentLoopContext<'_>) -> Self {
        let input = &context.input;
        let mut messages = Vec::with_capacity(
            input
                .conversation_history
                .len()
                .saturating_add(input.follow_up_context_messages.len())
                .saturating_add(context.iteration_cap * 3 + 2),
        );
        messages
            .push(ChatMessage::system(super::assistant_prompt::render(input.library_ref, None)));
        messages.extend(input.conversation_history.iter().cloned());
        messages.push(ChatMessage::user(input.user_question.to_string()));
        messages.extend(input.follow_up_context_messages.iter().cloned());
        Self {
            messages,
            usage_json: serde_json::json!({}),
            provider_calls: Vec::new(),
            debug_iterations: Vec::new(),
            total_tool_call_count: 0,
            successful_tool_call_count: 0,
            successful_tool_names: BTreeSet::new(),
            seen_effective_tool_payloads: BTreeMap::new(),
            assistant_grounding: AssistantGroundingEvidence::default(),
            child_query_execution_ids: Vec::new(),
            stopped_reason: AgentStopReason::IterationCap,
            last_required_tool_refusal_answer: None,
            verified_grounded_answer_count: 0,
            last_verified_grounded_answer: None,
            verified_grounded_answer_guard_text: None,
            last_completed_grounded_answer: None,
            last_verified_partial_grounded_answer: None,
            grounded_answer_evidence_ledger: GroundedAnswerEvidenceLedger::default(),
            focused_grounded_follow_up: FocusedGroundedFollowUpState::default(),
            incomplete_grounded_answer_needs_follow_up: false,
            no_progress_iterations: 0,
            no_progress_force_final: false,
        }
    }

    fn take_result(
        &mut self,
        context: &McpAgentLoopContext<'_>,
        answer: String,
        answer_provenance: AgentAnswerProvenance,
        canonical_answer_outcome: Option<AgentCanonicalAnswerOutcome>,
        stopped_reason: AgentStopReason,
    ) -> AgentTurnResult {
        let iterations = self.debug_iterations.len();
        AgentTurnResult {
            answer,
            answer_provenance,
            canonical_answer_outcome,
            usage_json: std::mem::replace(&mut self.usage_json, serde_json::json!({})),
            provider_calls: std::mem::take(&mut self.provider_calls),
            iterations,
            assistant_grounding: std::mem::take(&mut self.assistant_grounding),
            child_query_execution_ids: std::mem::take(&mut self.child_query_execution_ids),
            debug_iterations: std::mem::take(&mut self.debug_iterations),
            agent_loop: Some(agent_loop_metadata(
                context.iteration_cap,
                context.input.deadline,
                stopped_reason,
                self.total_tool_call_count,
            )),
        }
    }
}

async fn initialize_mcp_agent_loop(
    input: McpToolAgentTurnInput<'_>,
) -> Result<McpAgentLoopContext<'_>, AgentTurnFailure> {
    let binding = input
        .state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(input.state, input.library_id, AiBindingPurpose::Agent)
        .await
        .map_err(AgentTurnFailure::empty)?
        .ok_or_else(|| missing_agent_binding(input.library_id))?;
    let agent_vision_available = resolve_agent_vision_available(&input, &binding).await;
    let tool_defs =
        answer_surface_tool_defs(input.auth, ToolVisibilityCapabilities { agent_vision_available })
            .map_err(|error| AgentTurnFailure::empty(anyhow::Error::new(error)))?;
    if let Some(failure) = validate_agent_tool_definitions(&tool_defs) {
        return Err(failure);
    }
    let allowed_tool_names = tool_defs.iter().map(|tool| tool.name.clone()).collect();
    Ok(McpAgentLoopContext {
        iteration_cap: input.iteration_cap.max(1),
        max_parallel_actions: input.max_parallel_actions.max(1),
        input,
        binding,
        tool_defs,
        allowed_tool_names,
        deadline_started: Instant::now(),
    })
}

fn missing_agent_binding(library_id: Uuid) -> AgentTurnFailure {
    AgentTurnFailure::empty(QueryServiceError::BindingNotConfigured {
        message: format!("no active agent binding configured for library {library_id}"),
    })
}

async fn resolve_agent_vision_available(
    input: &McpToolAgentTurnInput<'_>,
    binding: &ResolvedRuntimeBinding,
) -> bool {
    input
        .state
        .canonical_services
        .ai_catalog
        .get_model_catalog(input.state, binding.model_catalog_id)
        .await
        .is_ok_and(|model| model.modality_kind == "multimodal")
}

fn validate_agent_tool_definitions(tool_defs: &[ChatToolDef]) -> Option<AgentTurnFailure> {
    if tool_defs.is_empty() {
        return Some(AgentTurnFailure::empty(QueryServiceError::StateConflict {
            message: "no MCP answer tools are visible for the current caller".to_string(),
        }));
    }
    (!tool_defs.iter().any(|tool| tool.name == GROUNDED_ANSWER_TOOL_NAME)).then(|| {
        AgentTurnFailure::empty(QueryServiceError::StateConflict {
            message: format!(
                "required MCP answer tool '{GROUNDED_ANSWER_TOOL_NAME}' is not visible for the current caller"
            ),
        })
    })
}

enum AgentIterationControl {
    Continue,
    Stop(AgentStopReason),
    Complete(Box<AgentTurnResult>),
}

struct AgentIterationPlan {
    deadline_budget: Duration,
    initial_grounded_answer_iteration: bool,
    focused_follow_up_iteration: bool,
    runtime_enforced_call: Option<ChatToolCall>,
    force_final_answer: bool,
    require_tool_call: bool,
    tools_for_iteration: Vec<ChatToolDef>,
    request_messages: Vec<ChatMessage>,
}

struct AgentIterationResponse {
    response: ToolUseResponse,
    request_messages: Vec<ChatMessage>,
    model_call_duration_ms: u64,
    runtime_enforced_iteration: bool,
}

async fn run_mcp_agent_iteration(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
) -> Result<AgentIterationControl, Box<AgentTurnFailure>> {
    let plan = match prepare_agent_iteration(context, state, iteration) {
        Ok(plan) => plan,
        Err(reason) => return Ok(AgentIterationControl::Stop(reason)),
    };
    let mut iteration_response =
        request_agent_iteration(context, state, iteration, &plan).await.map_err(Box::new)?;
    if enforce_forced_final_response(
        context,
        state,
        iteration,
        plan.force_final_answer,
        &mut iteration_response,
    )
    .await
    .map_err(Box::new)?
    {
        return Ok(AgentIterationControl::Stop(AgentStopReason::Deadline));
    }
    inject_required_grounded_answer_call(
        context,
        iteration,
        &plan,
        &mut iteration_response.response,
    );
    if iteration_response.response.tool_calls.is_empty() {
        return handle_agent_final_response(context, state, iteration, &plan, iteration_response);
    }
    handle_agent_tool_response(context, state, iteration, &plan, iteration_response).await
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum UnresolvedFocusedFollowUpAction {
    ReturnSavedFallback,
    SynthesizeFromEvidence,
    FailClosed,
}

fn unresolved_focused_follow_up_action(
    has_saved_fallback: bool,
    has_grounded_evidence: bool,
) -> UnresolvedFocusedFollowUpAction {
    if has_saved_fallback {
        UnresolvedFocusedFollowUpAction::ReturnSavedFallback
    } else if has_grounded_evidence {
        UnresolvedFocusedFollowUpAction::SynthesizeFromEvidence
    } else {
        UnresolvedFocusedFollowUpAction::FailClosed
    }
}

fn prepare_agent_iteration(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
) -> Result<AgentIterationPlan, AgentStopReason> {
    let Some(deadline_budget) =
        deadline_remaining(context.deadline_started, context.input.deadline)
    else {
        return Err(AgentStopReason::Deadline);
    };
    let initial_call = initial_grounded_answer_tool_call(
        iteration,
        state.total_tool_call_count,
        context.input.user_question,
        context.input.grounded_answer_top_k,
    );
    let initial_grounded_answer_iteration = initial_call.is_some();
    let focused_call = initial_call
        .is_none()
        .then(|| {
            state
                .focused_grounded_follow_up
                .take_call(context.input.user_question, context.input.grounded_answer_top_k)
        })
        .flatten();
    let focused_follow_up_iteration = focused_call.is_some();
    if !focused_follow_up_iteration
        && state.focused_grounded_follow_up.is_unresolved_after_attempt()
    {
        log_unresolved_focused_follow_up(context, iteration);
        return match unresolved_focused_follow_up_action(
            failed_focused_follow_up_answer(state).is_some(),
            state.grounded_answer_evidence_ledger.has_guardable_evidence(),
        ) {
            UnresolvedFocusedFollowUpAction::ReturnSavedFallback => Err(AgentStopReason::ToolError),
            UnresolvedFocusedFollowUpAction::SynthesizeFromEvidence => Ok(AgentIterationPlan {
                deadline_budget,
                initial_grounded_answer_iteration,
                focused_follow_up_iteration,
                runtime_enforced_call: None,
                force_final_answer: true,
                require_tool_call: false,
                tools_for_iteration: Vec::new(),
                request_messages: final_answer_request_messages(&state.messages, true),
            }),
            UnresolvedFocusedFollowUpAction::FailClosed => Err(AgentStopReason::ToolError),
        };
    }
    let runtime_enforced_call = initial_call.or(focused_call);
    let force_final_answer =
        should_force_agent_final_answer(context, state, iteration, focused_follow_up_iteration);
    let require_tool_call = focused_follow_up_iteration
        || should_require_tool_call_before_final(
            force_final_answer,
            &context.tool_defs,
            &state.successful_tool_names,
            state.incomplete_grounded_answer_needs_follow_up,
        );
    let tools_for_iteration =
        agent_iteration_tool_defs(context, state, focused_follow_up_iteration, force_final_answer);
    Ok(AgentIterationPlan {
        deadline_budget,
        initial_grounded_answer_iteration,
        focused_follow_up_iteration,
        runtime_enforced_call,
        force_final_answer,
        require_tool_call,
        tools_for_iteration,
        request_messages: final_answer_request_messages(&state.messages, force_final_answer),
    })
}

fn log_unresolved_focused_follow_up(context: &McpAgentLoopContext<'_>, iteration: usize) {
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        "query.agent_loop.focused_grounded_follow_up_unresolved"
    );
}

fn should_force_agent_final_answer(
    context: &McpAgentLoopContext<'_>,
    state: &McpAgentLoopState,
    iteration: usize,
    focused_follow_up_iteration: bool,
) -> bool {
    !focused_follow_up_iteration
        && (state.no_progress_force_final
            || force_final_answer_iteration(
                iteration,
                context.iteration_cap,
                state.total_tool_call_count,
                state.successful_tool_call_count,
                state.verified_grounded_answer_count,
                &state.successful_tool_names,
                state.incomplete_grounded_answer_needs_follow_up,
                context.deadline_started,
                context.input.soft_final_answer_deadline,
            ))
}

fn agent_iteration_tool_defs(
    context: &McpAgentLoopContext<'_>,
    state: &McpAgentLoopState,
    focused_follow_up_iteration: bool,
    force_final_answer: bool,
) -> Vec<ChatToolDef> {
    if focused_follow_up_iteration {
        return context
            .tool_defs
            .iter()
            .filter(|tool| tool.name == GROUNDED_ANSWER_TOOL_NAME)
            .cloned()
            .collect();
    }
    tool_defs_for_agent_iteration(
        &context.tool_defs,
        &state.successful_tool_names,
        force_final_answer,
    )
}

async fn request_agent_iteration(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    plan: &AgentIterationPlan,
) -> Result<AgentIterationResponse, AgentTurnFailure> {
    if let Some(call) = plan.runtime_enforced_call.clone() {
        log_runtime_enforced_agent_call(context, iteration, plan.initial_grounded_answer_iteration);
        return Ok(AgentIterationResponse {
            response: runtime_enforced_tool_response(
                call,
                &context.binding.provider_kind,
                &context.binding.model_name,
            ),
            request_messages: plan.request_messages.clone(),
            model_call_duration_ms: 0,
            runtime_enforced_iteration: true,
        });
    }
    let (response, duration_ms) = request_agent_model_response(
        context,
        state,
        iteration,
        plan.deadline_budget,
        plan.request_messages.clone(),
        plan.tools_for_iteration.clone(),
        plan.require_tool_call,
        AgentModelRequestKind::Iteration,
    )
    .await?;
    Ok(AgentIterationResponse {
        response,
        request_messages: plan.request_messages.clone(),
        model_call_duration_ms: duration_ms,
        runtime_enforced_iteration: false,
    })
}

fn log_runtime_enforced_agent_call(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    initial_grounded_answer_iteration: bool,
) {
    if initial_grounded_answer_iteration {
        tracing::debug!(
            request_id = context.input.request_id,
            library_id = %context.input.library_id,
            iteration,
            "query.agent_loop.runtime_enforced_initial_grounded_answer"
        );
        return;
    }
    tracing::debug!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        "query.agent_loop.runtime_enforced_focused_grounded_follow_up"
    );
}

#[derive(Clone, Copy)]
enum AgentModelRequestKind {
    Iteration,
    ForcedFinalRetry,
}

impl AgentModelRequestKind {
    const fn error_context(self) -> &'static str {
        match self {
            Self::Iteration => "MCP-backed assistant agent LLM call failed",
            Self::ForcedFinalRetry => "MCP-backed assistant agent forced-final retry failed",
        }
    }
}

async fn request_agent_model_response(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    deadline_budget: Duration,
    messages: Vec<ChatMessage>,
    tools: Vec<ChatToolDef>,
    require_tool_call: bool,
    request_kind: AgentModelRequestKind,
) -> Result<(ToolUseResponse, u64), AgentTurnFailure> {
    let (attribution, mut reservation) = reserve_attributed_provider_call(
        context.input.state,
        context.input.execution_context,
        &context.binding,
        QueryProviderCallKind::QueryAgent,
    )
    .await
    .map_err(AgentTurnFailure::empty)?;
    emit_model_request_activity(context, iteration);
    let started = Instant::now();
    let request = ToolUseRequest {
        provider_kind: context.binding.provider_kind.clone(),
        model_name: context.binding.model_name.clone(),
        api_key_override: context.binding.api_key.clone(),
        base_url_override: context.binding.provider_base_url.clone(),
        temperature: context.binding.temperature,
        top_p: context.binding.top_p,
        max_output_tokens_override: context.binding.max_output_tokens_override,
        messages,
        tools,
        extra_parameters_json: context.binding.extra_parameters_json.clone(),
        require_tool_call,
    };
    let response = match tokio::time::timeout(
        deadline_budget,
        context.input.state.llm_gateway.generate_with_tools(request),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            fail_provider_call(&mut reservation).await;
            log_agent_provider_error(context, iteration, &error, request_kind);
            return Err(agent_loop_failure(
                state,
                context,
                error.context(request_kind.error_context()),
                AgentStopReason::ProviderError,
            ));
        }
        Err(_) => {
            return Err(agent_loop_failure(
                state,
                context,
                QueryServiceError::DeadlineExceeded,
                AgentStopReason::Deadline,
            ));
        }
    };
    let provider_call = complete_attributed_provider_call(
        &attribution,
        &mut reservation,
        response.usage_json.clone(),
    )
    .await
    .map_err(|error| agent_loop_failure(state, context, error, AgentStopReason::ProviderError))?;
    state.provider_calls.push(provider_call);
    merge_usage_into(&mut state.usage_json, &response.usage_json);
    let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    Ok((response, duration_ms))
}

fn log_agent_provider_error(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    error: &anyhow::Error,
    request_kind: AgentModelRequestKind,
) {
    match request_kind {
        AgentModelRequestKind::Iteration => tracing::warn!(
            provider = %context.binding.provider_kind,
            model = %context.binding.model_name,
            iteration,
            max_output_tokens_override = ?context.binding.max_output_tokens_override,
            error = %error,
            "MCP-backed assistant agent provider call failed"
        ),
        AgentModelRequestKind::ForcedFinalRetry => tracing::warn!(
            provider = %context.binding.provider_kind,
            model = %context.binding.model_name,
            iteration,
            max_output_tokens_override = ?context.binding.max_output_tokens_override,
            error = %error,
            "MCP-backed assistant agent forced-final retry failed"
        ),
    }
}

fn emit_model_request_activity(context: &McpAgentLoopContext<'_>, iteration: usize) {
    emit_activity(
        &context.input.activity_tx,
        AgentLoopActivityEvent::ModelRequest {
            iteration,
            provider_kind: context.binding.provider_kind.clone(),
            model_name: context.binding.model_name.clone(),
        },
    );
}

fn agent_loop_failure(
    state: &McpAgentLoopState,
    context: &McpAgentLoopContext<'_>,
    error: impl Into<QueryServiceError>,
    stop_reason: AgentStopReason,
) -> AgentTurnFailure {
    AgentTurnFailure::with_loop(
        error,
        state.provider_calls.clone(),
        state.debug_iterations.clone(),
        agent_loop_metadata(
            context.iteration_cap,
            context.input.deadline,
            stop_reason,
            state.total_tool_call_count,
        ),
    )
}

async fn enforce_forced_final_response(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    force_final_answer: bool,
    iteration_response: &mut AgentIterationResponse,
) -> Result<bool, AgentTurnFailure> {
    if !force_final_answer || iteration_response.response.tool_calls.is_empty() {
        return Ok(false);
    }
    log_forced_final_tool_calls(context, iteration, &iteration_response.response);
    if !iteration_response.response.output_text.trim().is_empty() {
        iteration_response.response.tool_calls.clear();
        return Ok(false);
    }
    let Some(deadline_budget) =
        deadline_remaining(context.deadline_started, context.input.deadline)
    else {
        return Ok(true);
    };
    let retry_messages = final_answer_retry_messages(
        &iteration_response.request_messages,
        iteration_response.response.reasoning_content.clone(),
        &iteration_response.response.tool_calls,
    );
    let (retry_response, retry_duration_ms) = request_agent_model_response(
        context,
        state,
        iteration,
        deadline_budget,
        retry_messages.clone(),
        Vec::new(),
        false,
        AgentModelRequestKind::ForcedFinalRetry,
    )
    .await?;
    iteration_response.request_messages = retry_messages;
    iteration_response.model_call_duration_ms =
        iteration_response.model_call_duration_ms.saturating_add(retry_duration_ms);
    iteration_response.response = retry_response;
    discard_forced_final_retry_tool_calls(context, iteration, &mut iteration_response.response);
    Ok(false)
}

fn log_forced_final_tool_calls(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    response: &ToolUseResponse,
) {
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        tool_call_count = response.tool_calls.len(),
        has_output_text = !response.output_text.trim().is_empty(),
        "assistant agent provider returned tool calls during forced-final iteration"
    );
}

fn discard_forced_final_retry_tool_calls(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    response: &mut ToolUseResponse,
) {
    if response.tool_calls.is_empty() {
        return;
    }
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        tool_call_count = response.tool_calls.len(),
        "assistant agent forced-final retry still returned tool calls; discarding them"
    );
    response.tool_calls.clear();
}

fn inject_required_grounded_answer_call(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    plan: &AgentIterationPlan,
    response: &mut ToolUseResponse,
) {
    if !should_inject_required_grounded_answer_tool_call(
        response.tool_calls.is_empty(),
        plan.require_tool_call,
        plan.force_final_answer,
        &context.tool_defs,
    ) {
        return;
    }
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        provider = %context.binding.provider_kind,
        model = %context.binding.model_name,
        has_output_text = !response.output_text.trim().is_empty(),
        "assistant agent omitted a required MCP tool call; executing grounded_answer fallback"
    );
    response.tool_calls.push(required_grounded_answer_tool_call(
        context.input.user_question,
        context.input.grounded_answer_top_k,
    ));
}

fn handle_agent_final_response(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    plan: &AgentIterationPlan,
    iteration_response: AgentIterationResponse,
) -> Result<AgentIterationControl, Box<AgentTurnFailure>> {
    let answer = iteration_response.response.output_text.trim().to_string();
    emit_model_response_activity(context, iteration, 0, !answer.is_empty());
    if answer.is_empty() {
        return Err(Box::new(agent_loop_failure(
            state,
            context,
            ProviderCallError::protocol("assistant agent returned an empty final answer"),
            AgentStopReason::ProviderError,
        )));
    }
    state.debug_iterations.push(LlmIterationDebug {
        iteration,
        provider_kind: context.binding.provider_kind.clone(),
        model_name: context.binding.model_name.clone(),
        request_messages: iteration_response.request_messages,
        response_text: Some(answer.clone()),
        response_tool_calls: Vec::new(),
        usage: iteration_response.response.usage_json,
        duration_ms: Some(iteration_response.model_call_duration_ms),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
    });
    if plan.require_tool_call
        && state.last_required_tool_refusal_answer.is_none()
        && iteration < context.iteration_cap
    {
        state.last_required_tool_refusal_answer = Some(answer.clone());
        state.messages.push(ChatMessage::assistant_text(answer));
        state.messages.push(ChatMessage::system(tool_requirement_reminder()));
        return Ok(AgentIterationControl::Continue);
    }
    let stop_reason = final_answer_stop_reason(state.no_progress_force_final);
    let answer = finalize_composed_agent_answer(context, state, answer);
    let answer = mark_unresolved_repair_synthesis(&state.focused_grounded_follow_up, answer);
    Ok(AgentIterationControl::Complete(Box::new(state.take_result(
        context,
        answer,
        AgentAnswerProvenance::Composed,
        None,
        stop_reason,
    ))))
}

fn emit_model_response_activity(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
    tool_call_count: usize,
    has_final_answer: bool,
) {
    emit_activity(
        &context.input.activity_tx,
        AgentLoopActivityEvent::ModelResponse {
            iteration,
            provider_kind: context.binding.provider_kind.clone(),
            model_name: context.binding.model_name.clone(),
            tool_call_count,
            has_final_answer,
        },
    );
}

fn mark_unresolved_repair_synthesis(
    focused_follow_up: &FocusedGroundedFollowUpState,
    answer: String,
) -> String {
    if focused_follow_up.is_unresolved_after_attempt() {
        explicitly_mark_unverified_completed_grounded_answer(&answer, focused_follow_up.language())
    } else {
        answer
    }
}

fn finalize_composed_agent_answer(
    context: &McpAgentLoopContext<'_>,
    state: &McpAgentLoopState,
    answer: String,
) -> String {
    let ledger_guard = state.grounded_answer_evidence_ledger.guard_candidate_for_answer(&answer);
    let grounded_guard = ledger_guard.as_deref().or_else(|| {
        verified_grounded_answer_guard_candidate(
            state.last_verified_grounded_answer.as_deref(),
            state.verified_grounded_answer_guard_text.as_deref(),
            state.verified_grounded_answer_count,
            state.successful_tool_call_count,
        )
    });
    finalize_agent_loop_answer(
        answer,
        context.input.user_question,
        grounded_guard,
        context.input.request_id,
        context.input.library_id,
    )
}

async fn handle_agent_tool_response(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    plan: &AgentIterationPlan,
    iteration_response: AgentIterationResponse,
) -> Result<AgentIterationControl, Box<AgentTurnFailure>> {
    let tool_calls = iteration_response.response.tool_calls.clone();
    if !iteration_response.runtime_enforced_iteration {
        emit_model_response_activity(context, iteration, tool_calls.len(), false);
    }
    push_agent_tool_request_message(state, &iteration_response.response, &tool_calls);
    let outcomes = execute_tool_calls(
        context.input.clone(),
        iteration,
        &tool_calls,
        context.max_parallel_actions,
        context.deadline_started,
        plan.focused_follow_up_iteration,
        &context.allowed_tool_names,
        &mut state.seen_effective_tool_payloads,
    )
    .await;
    state.total_tool_call_count = state.total_tool_call_count.saturating_add(tool_calls.len());
    let observed = observe_agent_tool_outcomes(
        context,
        state,
        iteration,
        plan.focused_follow_up_iteration,
        &tool_calls,
        &outcomes,
    );
    let canonical_passthrough = canonical_passthrough_candidate(
        plan.focused_follow_up_iteration,
        &tool_calls,
        &outcomes,
        context.input.user_question,
    );
    push_grounded_evidence_ledger_message(state);
    state.debug_iterations.push(LlmIterationDebug {
        iteration,
        provider_kind: context.binding.provider_kind.clone(),
        model_name: context.binding.model_name.clone(),
        request_messages: iteration_response.request_messages,
        response_text: (!iteration_response.response.output_text.trim().is_empty())
            .then(|| iteration_response.response.output_text.trim().to_string()),
        response_tool_calls: observed.response_tool_calls,
        usage: iteration_response.response.usage_json,
        duration_ms: Some(iteration_response.model_call_duration_ms),
        child_runtime_execution_ids: observed.child_runtime_execution_ids,
        child_query_execution_ids: observed.child_query_execution_ids,
    });
    if runtime_initial_grounded_answer_failed(
        plan.initial_grounded_answer_iteration,
        &tool_calls,
        &outcomes,
        state.focused_grounded_follow_up.requires_resolution(),
    ) {
        log_runtime_initial_grounded_answer_failure(context, iteration);
        return Ok(AgentIterationControl::Stop(AgentStopReason::ToolError));
    }
    if let Some(passthrough) = canonical_passthrough {
        return Ok(complete_canonical_passthrough(context, state, iteration, passthrough));
    }
    update_focused_follow_up_requirement(
        context,
        state,
        iteration,
        plan.focused_follow_up_iteration,
    );
    update_agent_no_progress(context, state, iteration, observed.iteration_made_progress);
    Ok(AgentIterationControl::Continue)
}

fn push_agent_tool_request_message(
    state: &mut McpAgentLoopState,
    response: &ToolUseResponse,
    tool_calls: &[ChatToolCall],
) {
    state.messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: (!response.output_text.trim().is_empty())
            .then(|| response.output_text.trim().to_string()),
        reasoning_content: response.reasoning_content.clone(),
        tool_calls: tool_calls.to_vec(),
        tool_call_id: None,
        name: None,
    });
}

struct ObservedAgentToolOutcomes {
    response_tool_calls: Vec<ResponseToolCallDebug>,
    child_runtime_execution_ids: Vec<Uuid>,
    child_query_execution_ids: Vec<Uuid>,
    iteration_made_progress: bool,
}

fn observe_agent_tool_outcomes(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    focused_follow_up_iteration: bool,
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
) -> ObservedAgentToolOutcomes {
    let mut observed = ObservedAgentToolOutcomes {
        response_tool_calls: Vec::with_capacity(tool_calls.len()),
        child_runtime_execution_ids: Vec::new(),
        child_query_execution_ids: Vec::new(),
        iteration_made_progress: false,
    };
    for (call, outcome) in tool_calls.iter().zip(outcomes) {
        observe_agent_tool_outcome(
            context,
            state,
            iteration,
            focused_follow_up_iteration,
            call,
            outcome,
            &mut observed,
        );
    }
    observed
}

fn observe_agent_tool_outcome(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    focused_follow_up_iteration: bool,
    call: &ChatToolCall,
    outcome: &ToolExecutionOutcome,
    observed: &mut ObservedAgentToolOutcomes,
) {
    observe_focused_follow_up_outcome(
        context,
        state,
        iteration,
        focused_follow_up_iteration,
        call,
        outcome,
    );
    state.child_query_execution_ids.extend(outcome.child_query_execution_ids.iter().copied());
    observed.child_query_execution_ids.extend(outcome.child_query_execution_ids.iter().copied());
    observed
        .child_runtime_execution_ids
        .extend(outcome.child_runtime_execution_ids.iter().copied());
    if !outcome.is_error {
        observe_successful_agent_tool_outcome(state, call, outcome, observed);
    }
    observed.response_tool_calls.push(ResponseToolCallDebug {
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
    state.messages.push(ChatMessage::tool_result(
        call.id.clone(),
        call.name.clone(),
        outcome.message_content.clone(),
    ));
}

fn observe_focused_follow_up_outcome(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    focused_follow_up_iteration: bool,
    call: &ChatToolCall,
    outcome: &ToolExecutionOutcome,
) {
    if !focused_follow_up_iteration || call.name != GROUNDED_ANSWER_TOOL_NAME {
        return;
    }
    state.focused_grounded_follow_up.observe_attempt_outcome(outcome);
    tracing::info!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        duration_ms = outcome.duration_ms,
        succeeded = !outcome.is_error && outcome.grounded_answer_ready,
        unresolved = state.focused_grounded_follow_up.requires_resolution(),
        "query.agent_loop.focused_grounded_follow_up_outcome"
    );
}

fn observe_successful_agent_tool_outcome(
    state: &mut McpAgentLoopState,
    call: &ChatToolCall,
    outcome: &ToolExecutionOutcome,
    observed: &mut ObservedAgentToolOutcomes,
) {
    state.successful_tool_call_count = state.successful_tool_call_count.saturating_add(1);
    state.successful_tool_names.insert(call.name.clone());
    observed.iteration_made_progress |= !outcome.is_replay;
    remember_grounded_answer_outcome(state, call, outcome);
    schedule_grounded_answer_repair(state, outcome);
    state.grounded_answer_evidence_ledger.remember(&call.name, outcome);
    if let Some(grounding_text) = &outcome.grounding_text {
        push_tool_grounding_fragment(&mut state.assistant_grounding, &call.name, grounding_text);
    }
}

fn remember_grounded_answer_outcome(
    state: &mut McpAgentLoopState,
    call: &ChatToolCall,
    outcome: &ToolExecutionOutcome,
) {
    if outcome.grounded_answer_ready {
        state.verified_grounded_answer_count =
            state.verified_grounded_answer_count.saturating_add(1);
        state.last_verified_grounded_answer = remember_verified_grounded_answer(
            state.last_verified_grounded_answer.take(),
            &call.name,
            outcome,
        );
        state.verified_grounded_answer_guard_text = remember_verified_grounded_answer_guard_text(
            state.verified_grounded_answer_guard_text.take(),
            &call.name,
            outcome,
        );
    }
    if !outcome.grounded_answer_completed {
        return;
    }
    state.last_completed_grounded_answer = remember_completed_grounded_answer(
        state.last_completed_grounded_answer.take(),
        &call.name,
        outcome,
    );
    state.last_verified_partial_grounded_answer = remember_verified_partial_grounded_answer(
        state.last_verified_partial_grounded_answer.take(),
        &call.name,
        outcome,
    );
}

fn schedule_grounded_answer_repair(state: &mut McpAgentLoopState, outcome: &ToolExecutionOutcome) {
    if outcome.grounded_answer_clarification_required {
        return;
    }
    if let Some(reason) = outcome.grounded_answer_repair_reason {
        state.focused_grounded_follow_up.schedule(reason, outcome.grounded_answer_language);
    }
}

fn canonical_passthrough_candidate(
    focused_follow_up_iteration: bool,
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
    user_question: &str,
) -> Option<TerminalGroundedAnswerPassthrough> {
    terminal_grounded_answer_nonfactual_candidate(tool_calls, outcomes, user_question)
        .or_else(|| {
            focused_grounded_answer_passthrough_candidate(
                focused_follow_up_iteration,
                tool_calls,
                outcomes,
            )
        })
        .or_else(|| {
            canonical_grounded_answer_passthrough_candidate(tool_calls, outcomes, user_question)
        })
}

fn push_grounded_evidence_ledger_message(state: &mut McpAgentLoopState) {
    if let Some(message) = state.grounded_answer_evidence_ledger.system_message() {
        state.messages.push(ChatMessage::system(message));
    }
}

fn log_runtime_initial_grounded_answer_failure(
    context: &McpAgentLoopContext<'_>,
    iteration: usize,
) {
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        "query.agent_loop.runtime_initial_grounded_answer_failed"
    );
}

fn complete_canonical_passthrough(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    passthrough: TerminalGroundedAnswerPassthrough,
) -> AgentIterationControl {
    let TerminalGroundedAnswerPassthrough { answer, stop_reason, canonical_answer_outcome } =
        passthrough;
    tracing::info!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        answer_chars = answer.chars().count(),
        "query.agent_loop.canonical_grounded_answer_passthrough"
    );
    AgentIterationControl::Complete(Box::new(state.take_result(
        context,
        answer,
        AgentAnswerProvenance::CanonicalGroundedAnswerPassthrough,
        Some(canonical_answer_outcome),
        stop_reason,
    )))
}

fn update_focused_follow_up_requirement(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    focused_follow_up_iteration: bool,
) {
    let next = state.focused_grounded_follow_up.requires_resolution();
    if next != state.incomplete_grounded_answer_needs_follow_up {
        tracing::debug!(
            request_id = context.input.request_id,
            library_id = %context.input.library_id,
            iteration,
            previous = state.incomplete_grounded_answer_needs_follow_up,
            focused_follow_up_iteration,
            next,
            "query.agent_loop.grounded_answer_follow_up_state"
        );
    }
    state.incomplete_grounded_answer_needs_follow_up = next;
}

fn update_agent_no_progress(
    context: &McpAgentLoopContext<'_>,
    state: &mut McpAgentLoopState,
    iteration: usize,
    iteration_made_progress: bool,
) {
    let force_final;
    (state.no_progress_iterations, force_final) =
        next_no_progress_state(state.no_progress_iterations, iteration_made_progress);
    if !force_final {
        return;
    }
    tracing::warn!(
        request_id = context.input.request_id,
        library_id = %context.input.library_id,
        iteration,
        no_progress_iterations = state.no_progress_iterations,
        "query.agent_loop.no_progress_forced_final"
    );
    state.no_progress_force_final = true;
}

fn finish_mcp_agent_loop(
    context: &McpAgentLoopContext<'_>,
    mut state: McpAgentLoopState,
) -> Result<AgentTurnResult, Box<AgentTurnFailure>> {
    normalize_agent_loop_stop_reason(&mut state);
    if failed_focused_follow_up_answer(&state).is_some() {
        let answer = mark_failed_follow_up_answer(&state);
        return Ok(state.take_result(
            context,
            answer,
            AgentAnswerProvenance::Composed,
            None,
            AgentStopReason::ToolError,
        ));
    }
    if let Some(answer) = iteration_cap_grounded_answer(&state) {
        return Ok(state.take_result(
            context,
            answer,
            AgentAnswerProvenance::Composed,
            None,
            AgentStopReason::IterationCap,
        ));
    }
    Err(Box::new(terminal_agent_loop_failure(context, state)))
}

fn normalize_agent_loop_stop_reason(state: &mut McpAgentLoopState) {
    if matches!(state.stopped_reason, AgentStopReason::IterationCap)
        && state.successful_tool_call_count == 0
        && state.total_tool_call_count > 0
    {
        state.stopped_reason = AgentStopReason::ToolError;
    }
}

fn failed_focused_follow_up_answer(state: &McpAgentLoopState) -> Option<&str> {
    let failed_follow_up = state.incomplete_grounded_answer_needs_follow_up
        && state.focused_grounded_follow_up.is_unresolved_after_attempt();
    let verified_partial = failed_follow_up
        .then_some(state.last_verified_partial_grounded_answer.as_deref())
        .flatten()
        .filter(|answer| !answer.trim().is_empty());
    verified_partial.or_else(|| {
        completed_grounded_answer_after_failed_focused_follow_up(
            state.incomplete_grounded_answer_needs_follow_up,
            state.focused_grounded_follow_up.was_attempted(),
            state.focused_grounded_follow_up.is_unresolved_after_attempt(),
            state.last_completed_grounded_answer.as_deref(),
        )
    })
}

fn mark_failed_follow_up_answer(state: &McpAgentLoopState) -> String {
    if let Some(answer) = state
        .last_verified_partial_grounded_answer
        .as_deref()
        .filter(|answer| !answer.trim().is_empty())
    {
        return explicitly_mark_partial_grounded_answer(
            answer,
            state.focused_grounded_follow_up.language(),
        );
    }
    let answer = state.last_completed_grounded_answer.as_deref().unwrap_or_default();
    explicitly_mark_unverified_completed_grounded_answer(
        answer,
        state.focused_grounded_follow_up.language(),
    )
}

fn iteration_cap_grounded_answer(state: &McpAgentLoopState) -> Option<String> {
    current_turn_grounded_answer_on_iteration_cap(
        state.stopped_reason,
        state.incomplete_grounded_answer_needs_follow_up,
        state.focused_grounded_follow_up.was_attempted(),
        state.successful_tool_call_count,
        state.verified_grounded_answer_count,
        state.last_verified_grounded_answer.as_deref(),
        state.last_completed_grounded_answer.as_deref(),
    )
    .map(ToOwned::to_owned)
}

fn terminal_agent_loop_failure(
    context: &McpAgentLoopContext<'_>,
    state: McpAgentLoopState,
) -> AgentTurnFailure {
    let mut message = agent_loop_stop_message(state.stopped_reason).to_string();
    if state.successful_tool_call_count == 0 && state.total_tool_call_count > 0 {
        message.push_str("; no successful MCP tool result was received");
    }
    let error = match state.stopped_reason {
        AgentStopReason::Deadline => QueryServiceError::DeadlineExceeded,
        AgentStopReason::ProviderError => QueryServiceError::ProviderUnavailable { message },
        _ => QueryServiceError::Internal(anyhow::anyhow!(message)),
    };
    AgentTurnFailure::with_loop(
        error,
        state.provider_calls,
        state.debug_iterations,
        agent_loop_metadata(
            context.iteration_cap,
            context.input.deadline,
            state.stopped_reason,
            state.total_tool_call_count,
        ),
    )
}

fn agent_loop_stop_message(reason: AgentStopReason) -> &'static str {
    match reason {
        AgentStopReason::Deadline => {
            "assistant agent exceeded its turn deadline before producing a final answer"
        }
        AgentStopReason::IterationCap => {
            "assistant agent reached its iteration cap before producing a final answer"
        }
        AgentStopReason::FinalAnswer => "assistant agent stopped before producing a final answer",
        AgentStopReason::NoProgress => {
            "assistant agent stopped after consecutive iterations made no progress"
        }
        AgentStopReason::ToolError => "assistant agent stopped after a tool error",
        AgentStopReason::ProviderError => "assistant agent stopped after a provider error",
    }
}

/// Advance the no-progress streak counter after one tool-running iteration.
///
/// A *new* successful tool result (`iteration_made_progress`) resets the
/// streak; otherwise it grows by one. Returns the updated streak and whether
/// it has reached [`NO_PROGRESS_ITERATION_LIMIT`], at which point the next
/// iteration must be forced into the final-answer path instead of burning the
/// remaining budget on a stuck model.
fn next_no_progress_state(
    no_progress_iterations: usize,
    iteration_made_progress: bool,
) -> (usize, bool) {
    if iteration_made_progress {
        return (0, false);
    }
    let updated = no_progress_iterations.saturating_add(1);
    (updated, updated >= NO_PROGRESS_ITERATION_LIMIT)
}

/// Stop reason for a model-emitted final answer: `NoProgress` when the loop
/// forced this final answer because of a no-progress streak, otherwise the
/// ordinary `FinalAnswer`.
fn final_answer_stop_reason(no_progress_force_final: bool) -> AgentStopReason {
    if no_progress_force_final { AgentStopReason::NoProgress } else { AgentStopReason::FinalAnswer }
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

fn should_inject_required_grounded_answer_tool_call(
    response_tool_calls_empty: bool,
    require_tool_call: bool,
    force_final_answer: bool,
    tool_defs: &[ChatToolDef],
) -> bool {
    response_tool_calls_empty
        && require_tool_call
        && !force_final_answer
        && tool_defs.iter().any(|tool| tool.name == GROUNDED_ANSWER_TOOL_NAME)
}

fn required_grounded_answer_tool_call(user_question: &str, grounded_top_k: usize) -> ChatToolCall {
    ChatToolCall {
        id: format!("call_{GROUNDED_ANSWER_TOOL_NAME}_fallback"),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({
            "query": user_question,
            "topK": initial_grounded_answer_top_k(grounded_top_k),
            "responseProfile": "compact",
            "maxReferences": crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT,
        })
        .to_string(),
    }
}

fn initial_grounded_answer_top_k(grounded_top_k: usize) -> usize {
    let repair_ceiling = MAX_TOP_K.saturating_sub(GROUNDED_ANSWER_REPAIR_TOP_K_HEADROOM).max(1);
    grounded_top_k.max(1).min(repair_ceiling)
}

fn initial_grounded_answer_tool_call(
    iteration: usize,
    total_tool_call_count: usize,
    user_question: &str,
    grounded_top_k: usize,
) -> Option<ChatToolCall> {
    (iteration == 1 && total_tool_call_count == 0)
        .then(|| required_grounded_answer_tool_call(user_question, grounded_top_k))
}

fn runtime_enforced_tool_response(
    call: ChatToolCall,
    provider_kind: &str,
    model_name: &str,
) -> ToolUseResponse {
    ToolUseResponse {
        provider_kind: provider_kind.to_string(),
        model_name: model_name.to_string(),
        output_text: String::new(),
        tool_calls: vec![call],
        finish_reason: Some("runtime_enforced_tool_call".to_string()),
        usage_json: serde_json::json!({ "runtimeEnforced": true }),
        reasoning_content: None,
    }
}

fn focused_grounded_answer_follow_up_call(
    user_question: &str,
    grounded_top_k: usize,
    metadata: RuntimeGroundedRepairMetadata,
) -> ChatToolCall {
    ChatToolCall {
        id: format!("call_{GROUNDED_ANSWER_TOOL_NAME}_focused_follow_up"),
        name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
        arguments_json: serde_json::json!({
            "query": user_question,
            "topK": focused_grounded_answer_repair_top_k(grounded_top_k),
            "responseProfile": "compact",
            "maxReferences": crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT,
            (RUNTIME_REPAIR_ARGUMENT_FIELD): metadata,
        })
        .to_string(),
    }
}

fn focused_grounded_answer_repair_top_k(grounded_top_k: usize) -> usize {
    grounded_top_k.max(1).saturating_add(GROUNDED_ANSWER_REPAIR_TOP_K_INCREMENT).min(MAX_TOP_K)
}

#[cfg(test)]
fn next_incomplete_grounded_answer_follow_up_required(
    previous_required: bool,
    iteration_had_incomplete_grounded_answer: bool,
    iteration_had_follow_up_after_incomplete_grounded_answer: bool,
) -> bool {
    iteration_had_incomplete_grounded_answer
        || (previous_required && !iteration_had_follow_up_after_incomplete_grounded_answer)
}

#[cfg(test)]
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

fn finalize_agent_loop_answer(
    answer: String,
    user_question: &str,
    verified_grounded_answer: Option<&str>,
    request_id: &str,
    library_id: Uuid,
) -> String {
    let answer = prefer_verified_grounded_answer_on_unverified_procedure_rewrite(
        answer,
        verified_grounded_answer,
        request_id,
        library_id,
    );
    let answer = prefer_verified_grounded_answer_on_ordered_source_loss(
        answer,
        verified_grounded_answer,
        request_id,
        library_id,
    );
    prefer_verified_grounded_answer_on_literal_drift(
        answer,
        user_question,
        verified_grounded_answer,
        request_id,
        library_id,
    )
}

fn verified_grounded_answer_fallback_candidate(
    last_verified_grounded_answer: Option<&str>,
    verified_grounded_answer_count: usize,
    successful_tool_call_count: usize,
) -> Option<&str> {
    if verified_grounded_answer_count == 1 && successful_tool_call_count == 1 {
        return last_verified_grounded_answer;
    }
    None
}

fn terminal_grounded_answer_nonfactual_candidate(
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
    user_question: &str,
) -> Option<TerminalGroundedAnswerPassthrough> {
    let ([call], [outcome]) = (tool_calls, outcomes) else {
        return None;
    };
    if call.name != GROUNDED_ANSWER_TOOL_NAME
        || outcome.is_error
        || outcome.is_replay
        || !outcome.grounded_answer_completed
        || !grounded_answer_outcome_is_terminal_nonfactual(outcome)
        || !grounded_answer_executed_query_matches_user_question(outcome, user_question)
    {
        return None;
    }
    let answer =
        outcome.grounded_answer_body.as_ref().filter(|answer| !answer.trim().is_empty())?.clone();
    let canonical_answer_outcome = outcome.canonical_answer_outcome.clone()?;
    Some(TerminalGroundedAnswerPassthrough::final_answer(answer, canonical_answer_outcome))
}

fn grounded_answer_outcome_is_terminal_nonfactual(outcome: &ToolExecutionOutcome) -> bool {
    outcome.canonical_answer_outcome.as_ref().is_some_and(|typed| {
        matches!(
            typed.disposition,
            QueryAnswerDisposition::SafeFallback | QueryAnswerDisposition::Clarification
        )
    })
}

fn canonical_grounded_answer_passthrough_candidate(
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
    user_question: &str,
) -> Option<TerminalGroundedAnswerPassthrough> {
    let ([call], [outcome]) = (tool_calls, outcomes) else {
        return None;
    };
    if call.name != GROUNDED_ANSWER_TOOL_NAME
        || outcome.is_error
        || outcome.is_replay
        || !outcome.grounded_answer_ready
        || !outcome.grounded_answer_completed
        || outcome.grounded_answer_needs_follow_up
        || outcome.grounded_answer_repair_reason.is_some()
        || !grounded_answer_executed_query_matches_user_question(outcome, user_question)
    {
        return None;
    }
    let answer =
        outcome.grounded_answer_body.as_ref().filter(|answer| !answer.trim().is_empty())?.clone();
    let canonical_answer_outcome = outcome
        .canonical_answer_outcome
        .as_ref()
        .filter(|typed| matches!(typed.disposition, QueryAnswerDisposition::FactualReady))?
        .clone();
    Some(TerminalGroundedAnswerPassthrough::final_answer(answer, canonical_answer_outcome))
}

fn focused_grounded_answer_passthrough_candidate(
    focused_grounded_follow_up: bool,
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
) -> Option<TerminalGroundedAnswerPassthrough> {
    if !focused_grounded_follow_up {
        return None;
    }
    let ([call], [outcome]) = (tool_calls, outcomes) else {
        return None;
    };
    if call.name != GROUNDED_ANSWER_TOOL_NAME
        || outcome.is_error
        || outcome.is_replay
        || !outcome.grounded_answer_ready
        || !outcome.grounded_answer_completed
        || outcome.grounded_answer_needs_follow_up
        || outcome.grounded_answer_repair_reason.is_some()
    {
        return None;
    }
    let answer =
        outcome.grounded_answer_body.as_ref().filter(|answer| !answer.trim().is_empty())?.clone();
    let canonical_answer_outcome = outcome
        .canonical_answer_outcome
        .as_ref()
        .filter(|typed| matches!(typed.disposition, QueryAnswerDisposition::FactualReady))?
        .clone();
    Some(TerminalGroundedAnswerPassthrough::final_answer(answer, canonical_answer_outcome))
}

fn runtime_initial_grounded_answer_failed(
    initial_grounded_answer_iteration: bool,
    tool_calls: &[ChatToolCall],
    outcomes: &[ToolExecutionOutcome],
    focused_follow_up_scheduled: bool,
) -> bool {
    if !initial_grounded_answer_iteration || focused_follow_up_scheduled {
        return false;
    }
    let ([call], [outcome]) = (tool_calls, outcomes) else {
        return true;
    };
    call.name != GROUNDED_ANSWER_TOOL_NAME || outcome.is_error || !outcome.grounded_answer_completed
}

fn grounded_answer_executed_query_matches_user_question(
    outcome: &ToolExecutionOutcome,
    user_question: &str,
) -> bool {
    let Some(arguments_json) = outcome.arguments_json.as_deref() else {
        return false;
    };
    let Ok(arguments) = serde_json::from_str::<Value>(arguments_json) else {
        return false;
    };
    let Some(executed_query) = arguments.get("query").and_then(Value::as_str) else {
        return false;
    };

    let canonical_user_question = canonical_passthrough_question(user_question);
    !canonical_user_question.is_empty()
        && canonical_passthrough_question(executed_query) == canonical_user_question
}

fn canonical_passthrough_question(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n").trim().to_string()
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
    .or({
        if verified_grounded_answer_count > 1 { verified_grounded_answer_guard_text } else { None }
    })
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
    let grounded_marker_count = ordered_source_marker_count(verified_grounded_answer);
    if grounded_marker_count < 2 {
        return answer;
    }
    if ordered_source_marker_count(&answer) >= grounded_marker_count
        || answer_ordered_item_count(&answer) >= grounded_marker_count
    {
        return answer;
    }
    tracing::debug!(
        request_id,
        library_id = %library_id,
        "query.agent_loop.verified_grounded_ordered_source_guard"
    );
    verified_grounded_answer.to_string()
}

fn ordered_source_marker_count(answer: &str) -> usize {
    answer
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("source=`") || trimmed.contains(" source=`")
        })
        .count()
}

fn prefer_verified_grounded_answer_on_unverified_procedure_rewrite(
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
    if !answer_rewrites_unverified_procedure(&answer, verified_grounded_answer) {
        return answer;
    }
    tracing::debug!(
        request_id,
        library_id = %library_id,
        "query.agent_loop.verified_grounded_procedure_rewrite_guard"
    );
    verified_grounded_answer.to_string()
}

fn answer_rewrites_unverified_procedure(answer: &str, grounded: &str) -> bool {
    if canonical_passthrough_question(answer) == canonical_passthrough_question(grounded) {
        return false;
    }

    let grounded_step_count = answer_ordered_item_count(grounded);
    grounded_step_count >= 2
        && ordered_source_marker_count(grounded) < 2
        && answer_ordered_item_count(answer) != grounded_step_count
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
        .filter(|anchor| {
            !answer_anchors.contains(*anchor) && !answer_contains_guard_anchor(&answer, anchor)
        })
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
    let missing_count = grounded_anchors
        .iter()
        .filter(|anchor| {
            !answer_anchors.contains(*anchor) && !answer_contains_guard_anchor(&answer, anchor)
        })
        .count();
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
    if is_structural_literal_anchor(candidate) || is_plain_code_literal(candidate) {
        literals.insert(candidate.to_string());
    }
}

fn is_identifier_shaped_fragment(candidate: &str) -> bool {
    let char_count = candidate.chars().count();
    if !(STRUCTURAL_IDENTIFIER_MIN_CHARS..=STRUCTURAL_IDENTIFIER_MAX_CHARS).contains(&char_count) {
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

async fn execute_tool_calls(
    input: McpToolAgentTurnInput<'_>,
    iteration: usize,
    tool_calls: &[ChatToolCall],
    max_parallel_actions: usize,
    deadline_started: Instant,
    focused_grounded_follow_up: bool,
    allowed_tool_names: &BTreeSet<String>,
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadEntry>,
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
                    Some(remaining) => {
                        // Bound the per-tool-call wait by the smaller of the
                        // remaining turn deadline and the canonical per-tool-call
                        // max, so one hung tool future can never run past every
                        // budget. On timeout produce a structured error outcome;
                        // the error path clears the dedup fingerprint so a retry
                        // of the same call is not suppressed.
                        let per_call_wait = per_tool_call_wait_for_tool(
                            &pending.call.name,
                            remaining,
                            input.soft_final_answer_deadline,
                            focused_grounded_follow_up,
                        );
                        Box::pin(run_tool_call_within_budget(
                            execute_one_tool_call(
                                &input,
                                &pending.call,
                                single_tool_iteration,
                                allowed_tool_names,
                            ),
                            per_call_wait,
                            &pending.call.name,
                            iteration,
                        ))
                        .await
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
                let raw_args_key = raw_tool_call_argument_key(&pending.call.arguments_json);
                (pending.index, pending.fingerprint, raw_args_key, outcome)
            }
        })
        .buffer_unordered(max_parallel_actions)
        .collect::<Vec<_>>()
        .await;

    for (pending_index, fingerprint, raw_args_key, outcome) in pending_results {
        if let Some(fingerprint) = fingerprint {
            record_effective_tool_payload_outcome(
                seen_effective_payloads,
                fingerprint,
                raw_args_key,
                &outcome,
            );
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

/// Replay payload cached for a successfully completed tool call so a later
/// effective-duplicate of the same call replays the real data instead of
/// dead-ending the agent with a refusal message.
#[derive(Debug, Clone)]
struct CompletedToolPayload {
    message_content: String,
    result_text: Option<String>,
    result_json: Option<Value>,
    grounding_text: Option<String>,
    grounded_answer_body: Option<String>,
    canonical_answer_outcome: Option<AgentCanonicalAnswerOutcome>,
    grounded_answer_ready: bool,
    grounded_answer_completed: bool,
    grounded_answer_needs_follow_up: bool,
    grounded_answer_repair_reason: Option<GroundedAnswerRepairReason>,
    grounded_answer_language: QueryLanguage,
    grounded_answer_clarification_required: bool,
}

#[derive(Debug, Clone)]
enum EffectiveToolPayloadState {
    InFlight,
    Completed(Box<CompletedToolPayload>),
}

/// One entry in the cross-iteration dedup map keyed by the *effective*
/// (post-normalization) fingerprint. `raw_args_key` is the canonical hash of
/// the *raw* arguments the model actually sent, so two distinct raw queries
/// that normalize to the same effective fingerprint are not confused for each
/// other: only a genuine raw duplicate is suppressed, while a distinct raw call
/// replays a prior successful result (or runs on its own when none exists yet).
#[derive(Debug, Clone)]
struct EffectiveToolPayloadEntry {
    raw_args_key: String,
    state: EffectiveToolPayloadState,
}

/// How a pending call is dispatched after dedup classification.
enum PreparedToolCallDisposition {
    /// Execute the call; record its outcome under `fingerprint` afterwards.
    Execute,
    /// Suppress as a genuine same-raw in-flight duplicate.
    SuppressDuplicate,
    /// Replay a prior successful call's cached result instead of executing.
    Replay(Box<CompletedToolPayload>),
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
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadEntry>,
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
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadEntry>,
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
                let raw_args_key = raw_tool_call_argument_key(&call.arguments_json);
                match classify_effective_tool_call(
                    seen_effective_payloads,
                    fingerprint,
                    &raw_args_key,
                ) {
                    PreparedToolCallDisposition::SuppressDuplicate => {
                        outcomes[pending_index] =
                            Some(tool_execution_error(duplicate_tool_call_message(&call.name)));
                        return None;
                    }
                    PreparedToolCallDisposition::Replay(payload) => {
                        outcomes[pending_index] = Some(replayed_tool_execution_outcome(*payload));
                        return None;
                    }
                    PreparedToolCallDisposition::Execute => {
                        seen_effective_payloads.insert(
                            fingerprint.clone(),
                            EffectiveToolPayloadEntry {
                                raw_args_key,
                                state: EffectiveToolPayloadState::InFlight,
                            },
                        );
                    }
                }
            }
            Some(PendingAgentToolCall { index: pending_index, call, fingerprint })
        })
        .collect::<Vec<_>>()
}

/// Decide how a pending tool call is dispatched given the dedup map.
///
/// The dedup map exists to curb genuine same-arguments spam, not to dead-end a
/// distinct raw call that merely *normalizes* to the same effective fingerprint
/// as an earlier call. Therefore:
/// - a genuine same-raw in-flight duplicate is suppressed (true spam);
/// - a same-raw completed duplicate replays the cached successful result;
/// - a distinct-raw call replays a prior *completed* result (effective args are
///   identical, so the answer is the same), or, when the colliding entry is
///   still in-flight (same iteration batch, no result yet), is allowed to run so
///   the model's distinct intent is never dead-ended.
fn classify_effective_tool_call(
    seen_effective_payloads: &BTreeMap<String, EffectiveToolPayloadEntry>,
    fingerprint: &str,
    raw_args_key: &str,
) -> PreparedToolCallDisposition {
    let Some(entry) = seen_effective_payloads.get(fingerprint) else {
        return PreparedToolCallDisposition::Execute;
    };
    match &entry.state {
        EffectiveToolPayloadState::Completed(payload) => {
            PreparedToolCallDisposition::Replay(payload.clone())
        }
        EffectiveToolPayloadState::InFlight => {
            if entry.raw_args_key == raw_args_key {
                PreparedToolCallDisposition::SuppressDuplicate
            } else {
                PreparedToolCallDisposition::Execute
            }
        }
    }
}

fn record_effective_tool_payload_outcome(
    seen_effective_payloads: &mut BTreeMap<String, EffectiveToolPayloadEntry>,
    fingerprint: String,
    raw_args_key: String,
    outcome: &ToolExecutionOutcome,
) {
    if outcome.is_error {
        seen_effective_payloads.remove(&fingerprint);
        return;
    }
    seen_effective_payloads.insert(
        fingerprint,
        EffectiveToolPayloadEntry {
            raw_args_key,
            state: EffectiveToolPayloadState::Completed(Box::new(CompletedToolPayload {
                message_content: outcome.message_content.clone(),
                result_text: outcome.result_text.clone(),
                result_json: outcome.result_json.clone(),
                grounding_text: outcome.grounding_text.clone(),
                grounded_answer_body: outcome.grounded_answer_body.clone(),
                canonical_answer_outcome: outcome.canonical_answer_outcome.clone(),
                grounded_answer_ready: outcome.grounded_answer_ready,
                grounded_answer_completed: outcome.grounded_answer_completed,
                grounded_answer_needs_follow_up: outcome.grounded_answer_needs_follow_up,
                grounded_answer_repair_reason: outcome.grounded_answer_repair_reason,
                grounded_answer_language: outcome.grounded_answer_language,
                grounded_answer_clarification_required: outcome
                    .grounded_answer_clarification_required,
            })),
        },
    );
}

/// Canonical hash of the *raw* (pre-normalization) tool-call arguments, used to
/// tell genuine same-arguments duplicates apart from distinct raw calls that
/// happen to normalize to the same effective fingerprint. Unparseable JSON
/// falls back to the verbatim string so it still keys deterministically.
fn raw_tool_call_argument_key(arguments_json: &str) -> String {
    match serde_json::from_str::<Value>(arguments_json) {
        Ok(value) => serde_json::to_string(&canonical_json_value(&value))
            .unwrap_or_else(|_| arguments_json.to_string()),
        Err(_) => arguments_json.to_string(),
    }
}

/// Rebuild a successful tool-execution outcome from a cached replay payload so a
/// later effective-duplicate call receives the same data the original produced
/// instead of a refusal. Replays carry no child execution ids: the canonical
/// child trace belongs to the original call, not the replay.
fn replayed_tool_execution_outcome(payload: CompletedToolPayload) -> ToolExecutionOutcome {
    ToolExecutionOutcome {
        arguments_json: None,
        requested_arguments_json: None,
        message_content: payload.message_content,
        result_text: payload.result_text,
        result_json: payload.result_json,
        grounding_text: payload.grounding_text,
        grounded_answer_body: payload.grounded_answer_body,
        canonical_answer_outcome: payload.canonical_answer_outcome,
        grounded_answer_ready: payload.grounded_answer_ready,
        grounded_answer_completed: payload.grounded_answer_completed,
        grounded_answer_needs_follow_up: payload.grounded_answer_needs_follow_up,
        grounded_answer_repair_reason: payload.grounded_answer_repair_reason,
        grounded_answer_language: payload.grounded_answer_language,
        grounded_answer_clarification_required: payload.grounded_answer_clarification_required,
        is_error: false,
        is_replay: true,
        duration_ms: 0,
        child_query_execution_ids: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
    }
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
    allowed_tool_names: &BTreeSet<String>,
) -> ToolExecutionOutcome {
    if let Err(message) = validate_ui_agent_tool_allowed(&call.name, allowed_tool_names) {
        return tool_execution_error(message);
    }
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
        surface_kind: RuntimeSurfaceKind::Ui,
    };
    let Some(result) = Box::pin(tools::call_named_tool(&call.name, context, &arguments)).await
    else {
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
    let grounded_answer_body = grounded_answer_verbatim_body(&call.name, &result);
    let canonical_answer_outcome = grounded_answer_canonical_outcome(&call.name, &result);
    let grounded_answer_repair_reason =
        grounded_answer_repair_reason(&call.name, input.user_question, &result);
    let grounded_answer_ready =
        grounded_answer_ready_for_question(&call.name, input.user_question, &result);
    let grounded_answer_completed = grounded_answer_completed(&call.name, &result);
    let grounded_answer_needs_follow_up = grounded_answer_repair_reason.is_some()
        || grounded_answer_needs_follow_up(&call.name, &result);
    let grounded_answer_language = grounded_answer_query_language(&call.name, &result);
    let grounded_answer_clarification_required =
        canonical_answer_outcome.as_ref().is_some_and(|outcome| {
            matches!(outcome.disposition, QueryAnswerDisposition::Clarification)
        });
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
        grounded_answer_body,
        canonical_answer_outcome,
        grounded_answer_ready,
        grounded_answer_completed,
        grounded_answer_needs_follow_up,
        grounded_answer_repair_reason,
        grounded_answer_language,
        grounded_answer_clarification_required,
        is_error,
        is_replay: false,
        duration_ms: 0,
        child_query_execution_ids,
        child_runtime_execution_ids,
    }
}

fn validate_ui_agent_tool_allowed(
    tool_name: &str,
    allowed_tool_names: &BTreeSet<String>,
) -> Result<(), String> {
    allowed_tool_names.contains(tool_name).then_some(()).ok_or_else(|| {
        format!("tool '{tool_name}' is not available on the UI assistant answer surface")
    })
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
    for field in ["limit", "topK", "startOffset", "length", "maxBytes", "maxReferences"] {
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
    _single_tool_iteration: bool,
    grounded_top_k: usize,
    library_ref: &str,
    grounded_answer_tool_history: &[ExternalConversationTurn],
) {
    let Value::Object(object) = arguments else {
        return;
    };
    let runtime_repair = take_runtime_grounded_repair_metadata(object);
    apply_agent_tool_library_defaults(tool_name, object, library_ref);
    let bounded_top_k = grounded_top_k.max(1);
    if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        apply_grounded_answer_argument_defaults(
            object,
            user_question,
            contextual_follow_up,
            bounded_top_k,
            grounded_answer_tool_history,
            runtime_repair.is_some(),
        );
        return;
    }
    apply_agent_tool_limit_default(tool_name, object, bounded_top_k);
}

fn take_runtime_grounded_repair_metadata(
    object: &mut serde_json::Map<String, Value>,
) -> Option<RuntimeGroundedRepairMetadata> {
    object
        .remove(RUNTIME_REPAIR_ARGUMENT_FIELD)
        .and_then(|value| serde_json::from_value::<RuntimeGroundedRepairMetadata>(value).ok())
        .filter(|metadata| metadata.is_consistent())
}

fn apply_agent_tool_library_defaults(
    tool_name: &str,
    object: &mut serde_json::Map<String, Value>,
    library_ref: &str,
) {
    if tool_uses_single_library_scope(tool_name) {
        object.insert("library".to_string(), serde_json::json!(library_ref));
    }
    if tool_name == SEARCH_DOCUMENTS_TOOL_NAME {
        object.insert("libraries".to_string(), serde_json::json!([library_ref]));
    }
}

fn apply_grounded_answer_argument_defaults(
    object: &mut serde_json::Map<String, Value>,
    user_question: &str,
    contextual_follow_up: bool,
    bounded_top_k: usize,
    grounded_answer_tool_history: &[ExternalConversationTurn],
    is_runtime_repair: bool,
) {
    apply_grounded_answer_response_profile_defaults(object);
    let contextual_history = if contextual_follow_up { grounded_answer_tool_history } else { &[] };
    // UI and MCP must cross the same semantic boundary. The current
    // question and server-owned typed history are canonical; a model or
    // runtime repair may not reconstruct intent by rewriting either one.
    object.insert("query".to_string(), serde_json::json!(user_question));
    let requested_top_k =
        object.get("topK").and_then(Value::as_u64).and_then(|value| usize::try_from(value).ok());
    object.insert(
        "conversationTurns".to_string(),
        Value::Array(grounded_answer_conversation_turn_defaults(contextual_history)),
    );
    let effective_top_k = resolve_agent_grounded_answer_top_k(
        requested_top_k,
        !contextual_history.is_empty(),
        if is_runtime_repair { MAX_TOP_K } else { bounded_top_k },
    );
    if requested_top_k != Some(effective_top_k) {
        object.insert("topK".to_string(), serde_json::json!(effective_top_k));
    }
}

fn apply_grounded_answer_response_profile_defaults(object: &mut serde_json::Map<String, Value>) {
    if object.get("includeDebug").and_then(Value::as_bool) == Some(true) {
        object.insert("responseProfile".to_string(), serde_json::json!("full"));
        object.remove("maxReferences");
        return;
    }
    object.insert("responseProfile".to_string(), serde_json::json!("compact"));
    let reference_limit = crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT;
    let requested_references = object
        .get("maxReferences")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    if requested_references.is_none_or(|value| value == 0 || value > reference_limit) {
        object.insert("maxReferences".to_string(), serde_json::json!(reference_limit));
    }
}

fn apply_agent_tool_limit_default(
    tool_name: &str,
    object: &mut serde_json::Map<String, Value>,
    bounded_top_k: usize,
) {
    let Some(limit_cap) = agent_tool_limit_cap(tool_name) else {
        return;
    };
    // Static tool caps are ceilings; the parent turn's top-k budget
    // tightens them further so UI-agent subqueries cannot fan out wider
    // than the turn that spawned them.
    let bounded_limit = limit_cap.min(bounded_top_k.max(8)).max(1);
    let requested_limit =
        object.get("limit").and_then(Value::as_u64).and_then(|value| usize::try_from(value).ok());
    if requested_limit.is_none_or(|value| value > bounded_limit) {
        object.insert("limit".to_string(), serde_json::json!(bounded_limit));
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

fn is_structural_literal_anchor(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return false;
    }
    let char_count = candidate.chars().count();
    if !(2..=STRUCTURAL_LITERAL_ANCHOR_MAX_CHARS).contains(&char_count) {
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

fn is_plain_code_literal(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate.chars().any(char::is_whitespace) {
        return false;
    }
    let char_count = candidate.chars().count();
    if !(2..=STRUCTURAL_LITERAL_ANCHOR_MAX_CHARS).contains(&char_count) {
        return false;
    }
    let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
    alnum_count >= 2
        && candidate.chars().all(|ch| {
            ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\' | ':' | '=')
        })
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

fn collect_unsupported_literal_spans(
    execution_detail: Option<&Value>,
    structured_content: Option<&Value>,
) -> BTreeSet<String> {
    let mut literals = BTreeSet::new();
    let warnings = execution_detail
        .and_then(|detail| detail.get("verificationWarnings"))
        .and_then(Value::as_array)
        .or_else(|| {
            structured_content
                .and_then(|structured| structured.get("warnings"))
                .and_then(Value::as_array)
        });
    let Some(warnings) = warnings else {
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

fn collect_verification_warning_codes(
    execution_detail: Option<&Value>,
    structured_content: Option<&Value>,
) -> Vec<String> {
    let mut codes = Vec::new();
    let mut seen = BTreeSet::new();
    let warnings = execution_detail
        .and_then(|detail| detail.get("verificationWarnings"))
        .or_else(|| structured_content.and_then(|structured| structured.get("warnings")));
    let Some(Value::Array(warnings)) = warnings else {
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
        grounded_answer_body: None,
        canonical_answer_outcome: None,
        grounded_answer_ready: false,
        grounded_answer_completed: false,
        grounded_answer_needs_follow_up: false,
        grounded_answer_repair_reason: None,
        grounded_answer_language: QueryLanguage::Auto,
        grounded_answer_clarification_required: false,
        is_error: true,
        is_replay: false,
        duration_ms: 0,
        child_query_execution_ids: Vec::new(),
        child_runtime_execution_ids: Vec::new(),
    }
}

fn deadline_remaining(started: Instant, deadline: Duration) -> Option<Duration> {
    deadline.checked_sub(started.elapsed()).filter(|remaining| !remaining.is_zero())
}

/// Bound a single tool-call's wait by the smaller of the remaining turn
/// deadline and a per-tool-call max. The max is the caller-supplied soft
/// tool-collection target (the canonical per-tool-call SLO threaded into the
/// turn) when present, otherwise [`PER_TOOL_CALL_MAX_WAIT`]; either way it is
/// itself clamped to `PER_TOOL_CALL_MAX_WAIT` so a misconfigured soft target
/// can never let one call run for the whole turn.
fn per_tool_call_wait(
    remaining_turn_deadline: Duration,
    soft_final_answer_deadline: Option<Duration>,
) -> Duration {
    let per_call_max =
        soft_final_answer_deadline.unwrap_or(PER_TOOL_CALL_MAX_WAIT).min(PER_TOOL_CALL_MAX_WAIT);
    remaining_turn_deadline.min(per_call_max)
}

fn per_tool_call_wait_for_tool(
    tool_name: &str,
    remaining_turn_deadline: Duration,
    soft_final_answer_deadline: Option<Duration>,
    focused_grounded_follow_up: bool,
) -> Duration {
    if tool_name == GROUNDED_ANSWER_TOOL_NAME {
        let max_wait = if focused_grounded_follow_up {
            GROUNDED_ANSWER_REPAIR_MAX_WAIT
        } else {
            GROUNDED_ANSWER_TOOL_MAX_WAIT
        };
        return remaining_turn_deadline.min(max_wait);
    }
    per_tool_call_wait(remaining_turn_deadline, soft_final_answer_deadline)
}

/// Run one tool-execution future under a per-call wait. If the future does not
/// resolve within `wait`, produce a structured error outcome (`is_error =
/// true`) stating the call timed out, so the loop classifies it as a failed
/// call: the dedup fingerprint is cleared on the error path, no `is_replay`
/// flag is set, and the no-progress guard counts it as no progress.
async fn run_tool_call_within_budget(
    fut: impl std::future::Future<Output = ToolExecutionOutcome>,
    wait: Duration,
    tool_name: &str,
    iteration: usize,
) -> ToolExecutionOutcome {
    let call_started = Instant::now();
    match tokio::time::timeout(wait, fut).await {
        Ok(outcome) => outcome,
        Err(_) => {
            let waited_ms = call_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            tracing::warn!(
                tool_name,
                iteration,
                waited_ms,
                "MCP-backed assistant agent tool call timed out before producing a result"
            );
            tool_execution_error(format!(
                "tool '{tool_name}' timed out after {waited_ms} ms before producing a result"
            ))
        }
    }
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
            "clarification": result
                .structured_content
                .get("clarification")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"required": false})),
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
    grounded_answer_completion_envelope(result).is_some_and(|envelope| {
        envelope.completion.complete && grounded_answer_has_final_candidate(tool_name, result)
    })
}

fn grounded_answer_has_final_candidate(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return false;
    }
    let Some(envelope) = grounded_answer_completion_envelope(result) else {
        return false;
    };
    if !envelope.final_answer_ready || !envelope.finalizable {
        return false;
    }
    if result
        .structured_content
        .get("answerBody")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        return false;
    }
    if envelope.readiness.lifecycle_state != GROUNDED_ANSWER_LIFECYCLE_COMPLETED {
        return false;
    }
    true
}

fn grounded_answer_ready_for_question(
    tool_name: &str,
    user_question: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    grounded_answer_ready(tool_name, result)
        && grounded_answer_repair_reason(tool_name, user_question, result).is_none()
}

fn grounded_answer_repair_reason(
    tool_name: &str,
    _user_question: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<GroundedAnswerRepairReason> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return None;
    }
    let Some(envelope) = grounded_answer_completion_envelope(result) else {
        return Some(GroundedAnswerRepairReason::VerificationIncomplete);
    };
    if matches!(
        grounded_answer_canonical_outcome(tool_name, result).map(|outcome| outcome.disposition),
        Some(QueryAnswerDisposition::SafeFallback | QueryAnswerDisposition::Clarification)
    ) {
        return None;
    }
    if let Some(reason) = completion_assessment_repair_reason(&envelope.completion) {
        return Some(reason);
    }
    if grounded_answer_ready(tool_name, result)
        || envelope.readiness.lifecycle_state != GROUNDED_ANSWER_LIFECYCLE_COMPLETED
    {
        return None;
    }
    Some(GroundedAnswerRepairReason::VerificationIncomplete)
}

fn grounded_answer_query_language(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> QueryLanguage {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return QueryLanguage::Auto;
    }
    result
        .structured_content
        .get("queryLanguage")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or(QueryLanguage::Auto)
}

fn grounded_answer_completion_envelope(
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<GroundedAnswerCompletionEnvelope> {
    parse_grounded_answer_completion_envelope(&result.structured_content)
}

fn parse_grounded_answer_completion_envelope(
    structured_content: &Value,
) -> Option<GroundedAnswerCompletionEnvelope> {
    match GroundedAnswerCompletionEnvelope::from_structured_content(structured_content) {
        Ok(envelope) => Some(envelope),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "grounded-answer completion envelope rejected; consumer is failing closed"
            );
            None
        }
    }
}

fn grounded_answer_canonical_outcome(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<AgentCanonicalAnswerOutcome> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return None;
    }
    let envelope = grounded_answer_completion_envelope(result)?;
    if envelope.readiness.lifecycle_state != GROUNDED_ANSWER_LIFECYCLE_COMPLETED
        || result
            .structured_content
            .get("answerBody")
            .and_then(Value::as_str)
            .is_none_or(|answer| answer.trim().is_empty())
    {
        return None;
    }
    let disposition = match envelope.readiness.answer_disposition {
        ironrag_contracts::assistant::AssistantAnswerDisposition::NonTerminal => return None,
        ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady => {
            QueryAnswerDisposition::FactualReady
        }
        ironrag_contracts::assistant::AssistantAnswerDisposition::SafeFallback => {
            QueryAnswerDisposition::SafeFallback
        }
        ironrag_contracts::assistant::AssistantAnswerDisposition::Clarification => {
            QueryAnswerDisposition::Clarification
        }
    };
    let clarification = grounded_answer_typed_clarification(
        &result.structured_content,
        matches!(disposition, QueryAnswerDisposition::Clarification),
    )?;
    Some(AgentCanonicalAnswerOutcome { disposition, clarification })
}

fn grounded_answer_typed_clarification(
    structured_content: &Value,
    clarification_expected: bool,
) -> Option<QueryClarification> {
    let clarification = structured_content.get("clarification")?.as_object()?;
    let required = clarification.get("required")?.as_bool()?;
    if required != clarification_expected {
        return None;
    }
    let question = match clarification.get("question") {
        None | Some(Value::Null) => None,
        Some(Value::String(question)) if !question.trim().is_empty() => Some(question.clone()),
        Some(_) => return None,
    };
    if !required {
        return question.is_none().then(QueryClarification::default);
    }

    let candidates = match structured_content.get("responseProfile").and_then(Value::as_str) {
        Some("compact") => {
            let summary = structured_content.get("answerCandidateSummary")?.as_object()?;
            if summary.get("truncated")?.as_bool()? {
                return None;
            }
            let total_count = summary.get("totalCount")?.as_u64()?;
            let returned_count = summary.get("returnedCount")?.as_u64()?;
            let candidates = summary.get("candidates")?.clone();
            if total_count != returned_count
                || returned_count != candidates.as_array()?.len() as u64
            {
                return None;
            }
            candidates
        }
        Some("full") => structured_content.get("answerCandidates")?.clone(),
        _ => return None,
    };
    let answer_candidates = serde_json::from_value::<Vec<QueryAnswerCandidate>>(candidates).ok()?;
    Some(QueryClarification { required, question, answer_candidates })
}

fn completion_assessment_repair_reason(
    assessment: &AnswerCompletionAssessment,
) -> Option<GroundedAnswerRepairReason> {
    match assessment.reason? {
        AnswerCompletionGapReason::OrderedInventory => {
            Some(GroundedAnswerRepairReason::OrderedInventory {
                expected: assessment.expected.unwrap_or(1),
                observed: assessment.observed.unwrap_or(0),
            })
        }
        AnswerCompletionGapReason::Procedure => {
            Some(GroundedAnswerRepairReason::ProcedureIncomplete)
        }
        AnswerCompletionGapReason::Troubleshooting => {
            Some(GroundedAnswerRepairReason::TroubleshootingIncomplete)
        }
        AnswerCompletionGapReason::AnswerStructure => {
            Some(GroundedAnswerRepairReason::AnswerStructureIncomplete)
        }
    }
}

fn grounded_answer_verbatim_body(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return None;
    }
    result
        .structured_content
        .get("answerBody")
        .and_then(Value::as_str)
        .filter(|answer| !answer.trim().is_empty())
        .map(ToOwned::to_owned)
}

fn answer_ordered_item_count(answer: &str) -> usize {
    answer.lines().filter(|line| ordered_item_body(line).is_some()).count()
}

fn ordered_item_body(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if let Some(body) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("• "))
    {
        return body.chars().any(char::is_alphanumeric).then_some(body);
    }
    let digit_chars = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digit_chars == 0 {
        return None;
    }
    let after_digits = &trimmed[digit_chars..];
    let body = after_digits
        .strip_prefix('.')
        .or_else(|| after_digits.strip_prefix(')'))
        .or_else(|| after_digits.strip_prefix(':'))?
        .trim_start();
    body.chars().any(char::is_alphanumeric).then_some(body)
}

fn grounded_answer_needs_follow_up(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME || result.is_error {
        return false;
    }
    let Some(envelope) = grounded_answer_completion_envelope(result) else {
        return true;
    };
    if matches!(
        envelope.readiness.answer_disposition,
        ironrag_contracts::assistant::AssistantAnswerDisposition::SafeFallback
            | ironrag_contracts::assistant::AssistantAnswerDisposition::Clarification
    ) {
        return grounded_answer_canonical_outcome(tool_name, result).is_none();
    }
    if grounded_answer_ready(tool_name, result) {
        return false;
    }
    if envelope.readiness.lifecycle_state != GROUNDED_ANSWER_LIFECYCLE_COMPLETED {
        return false;
    }
    envelope.repair_policy.required
}

fn grounded_answer_completed(
    tool_name: &str,
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    tool_name == GROUNDED_ANSWER_TOOL_NAME
        && !result.is_error
        && grounded_answer_lifecycle_completed(result)
}

fn grounded_answer_lifecycle_completed(
    result: &crate::interfaces::http::mcp::McpToolResult,
) -> bool {
    grounded_answer_completion_envelope(result).is_some_and(|envelope| {
        envelope.readiness.lifecycle_state == GROUNDED_ANSWER_LIFECYCLE_COMPLETED
    })
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
        .grounded_answer_body
        .as_deref()
        .or_else(|| {
            outcome
                .result_json
                .as_ref()
                .and_then(|json| json.pointer("/structuredContent/answerBody"))
                .and_then(Value::as_str)
        })
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
    let factual_disposition = outcome
        .result_json
        .as_ref()
        .and_then(|json| json.get("structuredContent"))
        .and_then(parse_grounded_answer_completion_envelope)
        .is_some_and(|envelope| {
            matches!(
                envelope.readiness.answer_disposition,
                ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady
            )
        });
    if !factual_disposition {
        return current;
    }
    grounded_answer_body_text(outcome).map(ToOwned::to_owned).or(current)
}

fn remember_verified_partial_grounded_answer(
    current: Option<String>,
    tool_name: &str,
    outcome: &ToolExecutionOutcome,
) -> Option<String> {
    if tool_name != GROUNDED_ANSWER_TOOL_NAME
        || outcome.is_error
        || !outcome.grounded_answer_completed
        || outcome.grounded_answer_ready
        || !matches!(
            outcome.grounded_answer_repair_reason,
            Some(
                GroundedAnswerRepairReason::ProcedureIncomplete
                    | GroundedAnswerRepairReason::TroubleshootingIncomplete
                    | GroundedAnswerRepairReason::AnswerStructureIncomplete
                    | GroundedAnswerRepairReason::OrderedInventory { .. }
            )
        )
    {
        return current;
    }
    let Some(completion_envelope) = outcome
        .result_json
        .as_ref()
        .and_then(|json| json.get("structuredContent"))
        .and_then(parse_grounded_answer_completion_envelope)
    else {
        return current;
    };
    if completion_envelope.completion.complete {
        return current;
    }
    if !matches!(
        completion_envelope.readiness.answer_disposition,
        ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady
    ) {
        return current;
    }
    grounded_answer_body_text(outcome).map(ToOwned::to_owned).or(current)
}

fn explicitly_mark_partial_grounded_answer(answer: &str, language: QueryLanguage) -> String {
    let notice = grounded_repair_messages(language).partial_answer_notice;
    format!("{notice}\n\n{}", answer.trim())
}

fn explicitly_mark_unverified_completed_grounded_answer(
    answer: &str,
    language: QueryLanguage,
) -> String {
    let notice = grounded_repair_messages(language).unverified_answer_notice;
    format!("{notice}\n\n{}", answer.trim())
}

fn completed_grounded_answer_after_failed_focused_follow_up(
    incomplete_grounded_answer_needs_follow_up: bool,
    focused_grounded_follow_up_attempted: bool,
    focused_grounded_follow_up_unresolved: bool,
    last_completed_grounded_answer: Option<&str>,
) -> Option<&str> {
    (incomplete_grounded_answer_needs_follow_up
        && focused_grounded_follow_up_attempted
        && focused_grounded_follow_up_unresolved)
        .then_some(last_completed_grounded_answer)
        .flatten()
        .filter(|answer| !answer.trim().is_empty())
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

/// Select an iteration-cap answer exclusively from grounded results produced
/// during the current turn. Conversation history can shape follow-up query
/// compilation, but a prior assistant/system message is never itself a valid
/// answer for a new turn.
fn current_turn_grounded_answer_on_iteration_cap<'a>(
    stopped_reason: AgentStopReason,
    incomplete_grounded_answer_needs_follow_up: bool,
    focused_grounded_follow_up_attempted: bool,
    successful_tool_call_count: usize,
    verified_grounded_answer_count: usize,
    last_verified_grounded_answer: Option<&'a str>,
    last_completed_grounded_answer: Option<&'a str>,
) -> Option<&'a str> {
    if incomplete_grounded_answer_needs_follow_up
        || !matches!(stopped_reason, AgentStopReason::IterationCap)
    {
        return None;
    }

    if let Some(answer) = verified_grounded_answer_fallback_candidate(
        last_verified_grounded_answer,
        verified_grounded_answer_count,
        successful_tool_call_count,
    )
    .filter(|answer| !answer.trim().is_empty())
    {
        return Some(answer);
    }

    if focused_grounded_follow_up_attempted
        && let Some(answer) =
            last_verified_grounded_answer.filter(|answer| !answer.trim().is_empty())
    {
        return Some(answer);
    }

    should_return_completed_grounded_answer_on_iteration_cap(
        stopped_reason,
        successful_tool_call_count,
        last_completed_grounded_answer,
    )
    .then_some(last_completed_grounded_answer)
    .flatten()
}

fn compact_grounded_answer_structured_content_for_model(
    value: &Value,
    fallback_limit: usize,
) -> Value {
    let reference_limit = if parse_grounded_answer_completion_envelope(value)
        .is_some_and(|envelope| envelope.finalizable)
    {
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
    if value.get("responseProfile").and_then(Value::as_str) == Some("compact") {
        return compact_existing_grounded_answer_profile(value, reference_limit, fallback_limit);
    }
    let Some(execution_detail) = value.get("executionDetail") else {
        return compact_structured_content(value, fallback_limit);
    };
    let completion_envelope = parse_grounded_answer_completion_envelope(value);
    let compact = serde_json::json!({
        "executionId": value.get("executionId").cloned().unwrap_or(Value::Null),
        "runtimeExecutionId": value.get("runtimeExecutionId").cloned().unwrap_or(Value::Null),
        "conversationId": value.get("conversationId").cloned().unwrap_or(Value::Null),
        "libraryId": value.get("libraryId").cloned().unwrap_or(Value::Null),
        "workspaceId": value.get("workspaceId").cloned().unwrap_or(Value::Null),
        "lifecycleState": value.get("lifecycleState").cloned().unwrap_or(Value::Null),
        "finalAnswerReady": completion_envelope.as_ref().is_some_and(|envelope| envelope.final_answer_ready),
        "finalizable": completion_envelope.as_ref().is_some_and(|envelope| envelope.finalizable),
        "completion": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.completion)).unwrap_or(Value::Null),
        "repairPolicy": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.repair_policy)).unwrap_or(Value::Null),
        "readiness": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.readiness)).unwrap_or(Value::Null),
        "mustPreserveSpans": value.get("mustPreserveSpans").cloned().unwrap_or_else(|| serde_json::json!([])),
        "clarification": value.get("clarification").cloned().unwrap_or_else(|| serde_json::json!({"required": false})),
        "answerCandidates": compact_reference_array(value.get("answerCandidates"), TOOL_MODEL_GROUNDED_REFERENCE_LIMIT),
        "verificationState": execution_detail.get("verificationState").cloned().unwrap_or(Value::Null),
        "verificationWarnings": execution_detail.get("verificationWarnings").cloned().unwrap_or_else(|| serde_json::json!([])),
        "referenceCounts": {
            "chunkReferences": reference_array_len(execution_detail.get("chunkReferences")),
            "preparedSegmentReferences": reference_array_len(execution_detail.get("preparedSegmentReferences")),
            "technicalFactReferences": reference_array_len(execution_detail.get("technicalFactReferences")),
            "entityReferences": reference_array_len(execution_detail.get("entityReferences")),
            "relationReferences": reference_array_len(execution_detail.get("relationReferences"))
        },
        "references": compact_grounded_reference_groups(execution_detail, reference_limit)
    });
    enforce_grounded_structured_content_limit(compact, fallback_limit)
}

fn compact_existing_grounded_answer_profile(
    value: &Value,
    reference_limit: usize,
    fallback_limit: usize,
) -> Value {
    let reference_summary = value.get("referenceSummary");
    let references = bounded_json_array(
        reference_summary.and_then(|summary| summary.get("references")),
        reference_limit,
    );
    let total_reference_count = reference_summary
        .and_then(|summary| summary.get("totalCount"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| u64::try_from(references.len()).unwrap_or(u64::MAX));
    let returned_reference_count = u64::try_from(references.len()).unwrap_or(u64::MAX);

    let candidate_summary = value.get("answerCandidateSummary");
    let answer_candidates = bounded_json_array(
        candidate_summary.and_then(|summary| summary.get("candidates")),
        TOOL_MODEL_GROUNDED_REFERENCE_LIMIT,
    );
    let total_candidate_count = candidate_summary
        .and_then(|summary| summary.get("totalCount"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| u64::try_from(answer_candidates.len()).unwrap_or(u64::MAX));
    let returned_candidate_count = u64::try_from(answer_candidates.len()).unwrap_or(u64::MAX);
    let verifier_state = value.pointer("/verifier/state").cloned().unwrap_or(Value::Null);
    let warnings = bounded_json_array(value.get("warnings"), TOOL_MODEL_GROUNDED_REFERENCE_LIMIT);
    let completion_envelope = parse_grounded_answer_completion_envelope(value);

    let compact = serde_json::json!({
        "responseProfile": "compact",
        "executionId": value.get("executionId").cloned().unwrap_or(Value::Null),
        "runtimeExecutionId": value.get("runtimeExecutionId").cloned().unwrap_or(Value::Null),
        "conversationId": value.get("conversationId").cloned().unwrap_or(Value::Null),
        "libraryId": value.get("libraryId").cloned().unwrap_or(Value::Null),
        "workspaceId": value.get("workspaceId").cloned().unwrap_or(Value::Null),
        "lifecycleState": value.get("lifecycleState").cloned().unwrap_or(Value::Null),
        "finalAnswerReady": completion_envelope.as_ref().is_some_and(|envelope| envelope.final_answer_ready),
        "finalizable": completion_envelope.as_ref().is_some_and(|envelope| envelope.finalizable),
        "completion": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.completion)).unwrap_or(Value::Null),
        "repairPolicy": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.repair_policy)).unwrap_or(Value::Null),
        "readiness": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.readiness)).unwrap_or(Value::Null),
        "verifier": value.get("verifier").cloned().unwrap_or_else(|| serde_json::json!({"state": verifier_state.clone()})),
        "verificationState": verifier_state,
        "verificationWarnings": warnings,
        "mustPreserveSpans": value.get("mustPreserveSpans").cloned().unwrap_or_else(|| serde_json::json!([])),
        "clarification": value.get("clarification").cloned().unwrap_or_else(|| serde_json::json!({"required": false})),
        "referenceSummary": {
            "totalCount": total_reference_count,
            "returnedCount": returned_reference_count,
            "truncated": total_reference_count > returned_reference_count,
            "references": references,
        },
        "answerCandidateSummary": {
            "totalCount": total_candidate_count,
            "returnedCount": returned_candidate_count,
            "truncated": total_candidate_count > returned_candidate_count,
            "candidates": answer_candidates,
        }
    });
    enforce_grounded_structured_content_limit(compact, fallback_limit)
}

fn compact_grounded_reference_groups(execution_detail: &Value, total_limit: usize) -> Value {
    let mut remaining = total_limit;
    let mut groups = serde_json::Map::new();
    for field in [
        "chunkReferences",
        "preparedSegmentReferences",
        "technicalFactReferences",
        "entityReferences",
        "relationReferences",
    ] {
        let items = execution_detail.get(field).and_then(Value::as_array);
        let selected = items
            .map(|items| {
                let take = items.len().min(remaining);
                remaining = remaining.saturating_sub(take);
                let mut selected = items.iter().take(take).cloned().collect::<Vec<_>>();
                if items.len() > take {
                    selected.push(serde_json::json!({
                        "truncated": true,
                        "omittedCount": items.len() - take,
                    }));
                }
                selected
            })
            .unwrap_or_default();
        groups.insert(field.to_string(), Value::Array(selected));
    }
    Value::Object(groups)
}

fn enforce_grounded_structured_content_limit(value: Value, char_limit: usize) -> Value {
    let original_char_count = serde_json::to_string(&value)
        .map(|serialized| serialized.chars().count())
        .unwrap_or(usize::MAX);
    if original_char_count <= char_limit {
        return value;
    }
    let warning_codes = value
        .get("verificationWarnings")
        .or_else(|| value.get("warnings"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|warning| warning.get("code").and_then(Value::as_str))
        .take(8)
        .map(|code| code.chars().take(80).collect::<String>())
        .collect::<Vec<_>>();
    let preserve_spans = value
        .get("mustPreserveSpans")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(8)
        .map(|span| span.chars().take(160).collect::<String>())
        .collect::<Vec<_>>();
    let completion_envelope = parse_grounded_answer_completion_envelope(&value);
    serde_json::json!({
        "truncated": true,
        "originalCharCount": original_char_count,
        "executionId": value.get("executionId").cloned().unwrap_or(Value::Null),
        "runtimeExecutionId": value.get("runtimeExecutionId").cloned().unwrap_or(Value::Null),
        "conversationId": value.get("conversationId").cloned().unwrap_or(Value::Null),
        "libraryId": value.get("libraryId").cloned().unwrap_or(Value::Null),
        "workspaceId": value.get("workspaceId").cloned().unwrap_or(Value::Null),
        "lifecycleState": value.get("lifecycleState").cloned().unwrap_or(Value::Null),
        "finalAnswerReady": completion_envelope.as_ref().is_some_and(|envelope| envelope.final_answer_ready),
        "finalizable": completion_envelope.as_ref().is_some_and(|envelope| envelope.finalizable),
        "completion": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.completion)).unwrap_or(Value::Null),
        "repairPolicy": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.repair_policy)).unwrap_or(Value::Null),
        "readiness": completion_envelope.as_ref().map(|envelope| serde_json::json!(&envelope.readiness)).unwrap_or(Value::Null),
        "clarification": value.get("clarification").cloned().unwrap_or_else(|| serde_json::json!({"required": false})),
        "verificationState": value.get("verificationState").cloned().unwrap_or(Value::Null),
        "verificationWarningCodes": warning_codes,
        "mustPreserveSpans": preserve_spans,
        "referenceCounts": value.get("referenceCounts").cloned().unwrap_or(Value::Null),
        "referenceSummary": value.get("referenceSummary").map(|summary| serde_json::json!({
            "totalCount": summary.get("totalCount").cloned().unwrap_or(Value::Null),
            "returnedCount": 0,
            "truncated": true,
            "references": [],
        })).unwrap_or(Value::Null),
    })
}

fn bounded_json_array(value: Option<&Value>, limit: usize) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.iter().take(limit).cloned().collect(),
        _ => Vec::new(),
    }
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
pub(crate) async fn run_single_shot_turn(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    library_id: Uuid,
    user_question: &str,
    conversation_history: &[ChatMessage],
    grounded_context: &str,
) -> Result<AgentTurnResult, QueryServiceError> {
    // The runtime has already performed retrieval. Model-visible
    // context is represented as the same chat transcript shape a
    // tool-using agent would see: prior messages, current user, an
    // assistant tool-call record, and the matching tool result.
    run_fixed_context_turn(
        state,
        execution_context,
        FixedContextTurnRequest {
            library_id,
            user_question,
            conversation_history,
            context: grounded_context,
            system_prompt: super::assistant_prompt::render_single_shot(),
            tool_name: RUNTIME_RETRIEVED_CONTEXT_TOOL,
            tool_arguments: serde_json::json!({ "question": user_question }),
            call_kind: QueryProviderCallKind::QueryAnswer,
            failure_context: "single-shot grounded-answer LLM call failed",
        },
    )
    .await
}

pub(crate) enum LiteralRevisionRequest<'a> {
    Fidelity {
        library_id: Uuid,
        user_question: &'a str,
        conversation_history: &'a [ChatMessage],
        original_answer: &'a str,
        unsupported_literals: &'a [String],
        grounded_context: &'a str,
    },
    InventoryCoverage {
        library_id: Uuid,
        user_question: &'a str,
        conversation_history: &'a [ChatMessage],
        original_answer: &'a str,
        required_literals: &'a [String],
        revision_context: &'a str,
    },
}

pub(crate) async fn run_literal_revision_turn(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    request: LiteralRevisionRequest<'_>,
) -> Result<AgentTurnResult, QueryServiceError> {
    let (
        library_id,
        user_question,
        conversation_history,
        revision_context,
        call_kind,
        system_prompt,
        tool_arguments,
        failure_context,
    ) = match request {
        LiteralRevisionRequest::Fidelity {
            library_id,
            user_question,
            conversation_history,
            original_answer,
            unsupported_literals,
            grounded_context,
        } => (
            library_id,
            user_question,
            conversation_history,
            grounded_context,
            QueryProviderCallKind::QueryAnswerLiteralRevision,
            super::assistant_prompt::render_literal_fidelity_revision(
                "Provided in the `ironrag_literal_revision_context` runtime tool result.",
                original_answer,
                unsupported_literals,
                None,
            ),
            serde_json::json!({
                "question": user_question,
                "unsupportedLiterals": unsupported_literals,
            }),
            "literal-fidelity revision LLM call failed",
        ),
        LiteralRevisionRequest::InventoryCoverage {
            library_id,
            user_question,
            conversation_history,
            original_answer,
            required_literals,
            revision_context,
        } => (
            library_id,
            user_question,
            conversation_history,
            revision_context,
            QueryProviderCallKind::QueryAnswerInventoryRevision,
            super::assistant_prompt::render_literal_inventory_coverage_revision(
                original_answer,
                required_literals,
                "Provided in the `ironrag_literal_revision_context` runtime tool result.",
            ),
            serde_json::json!({
                "question": user_question,
                "requiredLiterals": required_literals,
            }),
            "literal-inventory coverage revision LLM call failed",
        ),
    };

    run_fixed_context_turn(
        state,
        execution_context,
        FixedContextTurnRequest {
            library_id,
            user_question,
            conversation_history,
            context: revision_context,
            system_prompt,
            tool_name: RUNTIME_LITERAL_REVISION_CONTEXT_TOOL,
            tool_arguments,
            call_kind,
            failure_context,
        },
    )
    .await
}

struct FixedContextTurnRequest<'a> {
    library_id: Uuid,
    user_question: &'a str,
    conversation_history: &'a [ChatMessage],
    context: &'a str,
    system_prompt: String,
    tool_name: &'static str,
    tool_arguments: Value,
    call_kind: QueryProviderCallKind,
    failure_context: &'static str,
}

async fn run_fixed_context_turn(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    request: FixedContextTurnRequest<'_>,
) -> Result<AgentTurnResult, QueryServiceError> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, request.library_id, AiBindingPurpose::QueryAnswer)
        .await
        .map_err(|error| anyhow::anyhow!("failed to resolve query_answer binding: {error}"))?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no active query_answer binding configured for library {}",
                request.library_id
            )
        })?;

    let (provider_call_attribution, mut provider_call_reservation) =
        reserve_attributed_provider_call(state, execution_context, &binding, request.call_kind)
            .await?;

    let messages = build_runtime_tool_answer_messages(
        request.system_prompt,
        request.conversation_history,
        request.user_question,
        request.tool_name,
        request.tool_arguments,
        request.context,
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
    let response = match state.llm_gateway.generate_with_tools(tool_use_request).await {
        Ok(response) => response,
        Err(error) => {
            fail_provider_call(&mut provider_call_reservation).await;
            return Err(error.context(request.failure_context).into());
        }
    };
    let provider_call = complete_attributed_provider_call(
        &provider_call_attribution,
        &mut provider_call_reservation,
        response.usage_json.clone(),
    )
    .await?;

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
    let provider_calls = vec![provider_call];

    Ok(AgentTurnResult {
        answer,
        answer_provenance: AgentAnswerProvenance::Composed,
        canonical_answer_outcome: None,
        usage_json: response.usage_json,
        provider_calls,
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

/// Accumulate one iteration's `usage_json` into the diagnostic turn summary.
/// Billing never consumes this aggregate: it persists the typed
/// `provider_calls` ledger one response at a time. We still normalize common
/// provider protocol fields here so the debug summary remains useful.
///
/// Numbers are summed, and per-iteration counters (`iteration_count`,
/// `provider_call_count`) expose the round-trip volume separately from
/// raw tokens so an operator reading the debug snapshot can tell a single-shot
/// call apart from a multi-iteration escalation without cross-referencing
/// `debug_iterations`.
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
    // some providers emit — merge it into the flat diagnostic key too.
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

    #[test]
    fn provider_call_record_captures_exact_resolved_binding_and_unmodified_usage() {
        let binding_id = Uuid::now_v7();
        let usage_json = serde_json::json!({});
        let binding = ResolvedRuntimeBinding {
            binding_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            binding_purpose: AiBindingPurpose::Agent,
            provider_catalog_id: Uuid::now_v7(),
            provider_kind: "provider-a".to_string(),
            provider_base_url: None,
            provider_api_style: "chat".to_string(),
            account_id: Uuid::now_v7(),
            api_key: None,
            model_catalog_id: Uuid::now_v7(),
            model_name: "model-a".to_string(),
            effective_embedding_dimensions: None,
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        };

        let attribution = provider_call_attribution(&binding, QueryProviderCallKind::QueryAgent)
            .expect("valid agent attribution");
        let provider_call_id = Uuid::now_v7();
        let call = attribution.record(provider_call_id, usage_json.clone());

        assert_eq!(call.binding_id(), binding_id);
        assert_eq!(call.binding_purpose(), AiBindingPurpose::Agent);
        assert_eq!(call.provider().provider_kind, "provider-a");
        assert_eq!(call.provider().model_name, "model-a");
        assert_eq!(call.call_kind(), QueryProviderCallKind::QueryAgent);
        assert_eq!(call.usage_json(), &usage_json);
    }

    #[test]
    fn provider_call_attribution_mismatch_fails_before_response_recording() {
        let binding = ResolvedRuntimeBinding {
            binding_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            binding_purpose: AiBindingPurpose::QueryAnswer,
            provider_catalog_id: Uuid::now_v7(),
            provider_kind: "provider-a".to_string(),
            provider_base_url: None,
            provider_api_style: "chat".to_string(),
            account_id: Uuid::now_v7(),
            api_key: None,
            model_catalog_id: Uuid::now_v7(),
            model_name: "model-a".to_string(),
            effective_embedding_dimensions: None,
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        };

        let result = provider_call_attribution(&binding, QueryProviderCallKind::QueryAgent);

        assert!(matches!(result, Err(QueryServiceError::Internal(_))));
    }

    #[test]
    fn paid_response_ledger_survives_a_later_loop_deadline() {
        let attribution = QueryProviderCallAttribution::try_new(
            Uuid::now_v7(),
            AiBindingPurpose::Agent,
            ProviderModelSelection {
                provider_kind: "provider-a".to_string(),
                model_name: "model-a".to_string(),
            },
            QueryProviderCallKind::QueryAgent,
        )
        .expect("valid agent attribution");
        let provider_call =
            attribution.record(Uuid::now_v7(), serde_json::json!({"output_tokens": 5}));

        let failure = AgentTurnFailure::with_loop(
            QueryServiceError::DeadlineExceeded,
            vec![provider_call.clone()],
            Vec::new(),
            agent_loop_metadata(2, Duration::from_secs(1), AgentStopReason::Deadline, 0),
        );

        assert_eq!(failure.provider_calls, vec![provider_call]);
    }

    fn with_test_completion_envelope(mut structured: Value) -> Value {
        let candidate_ready =
            structured.get("finalAnswerReady").and_then(Value::as_bool).unwrap_or(false);
        let answer_text = structured.get("answerBody").and_then(Value::as_str).unwrap_or_default();
        let lifecycle_state =
            structured.get("lifecycleState").and_then(Value::as_str).unwrap_or("completed");
        let clarification_required =
            structured.pointer("/clarification/required").and_then(Value::as_bool).unwrap_or(false);
        let answer_disposition = structured
            .pointer("/executionDetail/answerDisposition")
            .cloned()
            .map(|value| {
                serde_json::from_value(value)
                    .expect("test answer disposition must use the contract vocabulary")
            })
            .unwrap_or_else(|| {
                if clarification_required {
                    ironrag_contracts::assistant::AssistantAnswerDisposition::Clarification
                } else if candidate_ready {
                    ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady
                } else {
                    ironrag_contracts::assistant::AssistantAnswerDisposition::NonTerminal
                }
            });
        let completion = structured
            .get("completion")
            .cloned()
            .map(|value| {
                serde_json::from_value(value)
                    .expect("test completion assessment must match the typed contract")
            })
            .filter(AnswerCompletionAssessment::is_consistent)
            .unwrap_or_else(AnswerCompletionAssessment::complete);
        let envelope = GroundedAnswerCompletionEnvelope::new(
            answer_disposition,
            answer_text,
            completion,
            lifecycle_state,
            None,
        );
        let Value::Object(envelope_fields) = serde_json::json!(envelope) else {
            return structured;
        };
        if let Some(fields) = structured.as_object_mut() {
            fields.extend(envelope_fields);
            fields
                .entry("responseProfile".to_string())
                .or_insert_with(|| Value::String("full".to_string()));
            fields.entry("clarification".to_string()).or_insert_with(|| {
                serde_json::json!({
                    "required": clarification_required,
                    "question": Value::Null,
                })
            });
            fields
                .entry("answerCandidates".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
        }
        structured
    }

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
            let tool_defs = answer_surface_tool_defs(&auth, capabilities)
                .expect("complete answer tool contract");

            assert_eq!(
                tool_defs.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
                expected_names.iter().map(String::as_str).collect::<Vec<_>>(),
                "UI agent tool set diverged from MCP answer surface (vision={agent_vision_available})"
            );
            assert!(tool_defs.iter().any(|tool| tool.name == "grounded_answer"));
            assert!(!tool_defs.iter().any(|tool| tool.name == "create_documents"));
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
    fn required_grounded_answer_fallback_only_runs_before_forced_final() {
        let tool_defs = [ChatToolDef {
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        }];

        assert!(should_inject_required_grounded_answer_tool_call(true, true, false, &tool_defs,));
        assert!(!should_inject_required_grounded_answer_tool_call(false, true, false, &tool_defs,));
        assert!(!should_inject_required_grounded_answer_tool_call(true, false, false, &tool_defs,));
        assert!(!should_inject_required_grounded_answer_tool_call(true, true, true, &tool_defs,));
        assert!(!should_inject_required_grounded_answer_tool_call(true, true, false, &[]));
    }

    #[test]
    fn required_grounded_answer_fallback_preserves_query_and_top_k() {
        let call = required_grounded_answer_tool_call("Q?", 0);
        let arguments =
            serde_json::from_str::<serde_json::Value>(&call.arguments_json).expect("json");

        assert_eq!(call.name, GROUNDED_ANSWER_TOOL_NAME);
        assert_eq!(arguments["query"], "Q?");
        assert_eq!(arguments["topK"], 1);
        assert_eq!(arguments["responseProfile"], "compact");
        assert_eq!(
            arguments["maxReferences"],
            crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT
        );
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
    fn typed_clarification_is_terminal_exact_passthrough_without_retry() {
        let question = "How do I configure the connector?";
        let answer_body = "Choose one of the matching variants.";
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: answer_body.to_string(),
            }],
            structured_content: with_test_completion_envelope(serde_json::json!({
                "queryLanguage": "en",
                "answerBody": answer_body,
                "finalAnswerReady": false,
                "finalizable": false,
                "lifecycleState": "completed",
                "clarification": {
                    "required": true,
                    "question": "Which variant?"
                },
                "answerCandidates": [
                    {"label": "Variant Alpha", "kind": "document", "provenance": {}},
                    {"label": "Variant Beta", "kind": "document", "provenance": {}}
                ],
                "executionDetail": {
                    "verificationState": "not_run",
                    "verificationWarnings": [{
                        "code": "clarification_not_answer",
                        "message": "This is a clarification, not an answer."
                    }]
                }
            })),
            is_error: false,
        };
        let call = ChatToolCall {
            id: "call-grounded".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({"query": question}).to_string(),
        };
        let repair_reason =
            grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result);
        let mut state = FocusedGroundedFollowUpState::default();
        if let Some(reason) = repair_reason {
            state.schedule(
                reason,
                grounded_answer_query_language(GROUNDED_ANSWER_TOOL_NAME, &result),
            );
        }
        let canonical_answer_outcome =
            grounded_answer_canonical_outcome(GROUNDED_ANSWER_TOOL_NAME, &result);
        let grounded_answer_clarification_required =
            canonical_answer_outcome.as_ref().is_some_and(|outcome| {
                matches!(outcome.disposition, QueryAnswerDisposition::Clarification)
            });
        let outcome = ToolExecutionOutcome {
            arguments_json: Some(serde_json::json!({"query": question}).to_string()),
            requested_arguments_json: None,
            message_content: String::new(),
            result_text: Some(answer_body.to_string()),
            result_json: Some(debug_tool_result_json(&result)),
            grounding_text: None,
            grounded_answer_body: grounded_answer_verbatim_body(GROUNDED_ANSWER_TOOL_NAME, &result),
            canonical_answer_outcome,
            grounded_answer_ready: false,
            grounded_answer_completed: grounded_answer_completed(
                GROUNDED_ANSWER_TOOL_NAME,
                &result,
            ),
            grounded_answer_needs_follow_up: grounded_answer_needs_follow_up(
                GROUNDED_ANSWER_TOOL_NAME,
                &result,
            ),
            grounded_answer_repair_reason: repair_reason,
            grounded_answer_language: grounded_answer_query_language(
                GROUNDED_ANSWER_TOOL_NAME,
                &result,
            ),
            grounded_answer_clarification_required,
            is_error: false,
            is_replay: false,
            duration_ms: 5,
            child_query_execution_ids: Vec::new(),
            child_runtime_execution_ids: Vec::new(),
        };

        assert!(repair_reason.is_none());
        assert!(!state.requires_resolution());
        assert!(state.take_call(question, 24).is_none());
        assert!(!outcome.grounded_answer_needs_follow_up);
        let terminal = terminal_grounded_answer_nonfactual_candidate(
            std::slice::from_ref(&call),
            std::slice::from_ref(&outcome),
            question,
        )
        .expect("typed clarification should terminate the initial canonical call");
        let metadata = agent_loop_metadata(6, Duration::from_secs(60), terminal.stop_reason, 1);

        assert_eq!(terminal.answer, answer_body);
        assert_eq!(
            terminal.canonical_answer_outcome.disposition,
            QueryAnswerDisposition::Clarification,
        );
        assert_eq!(terminal.canonical_answer_outcome.clarification.answer_candidates.len(), 2);
        assert_eq!(metadata.stopped_reason, AgentStopReason::FinalAnswer);
        assert_eq!(metadata.tool_call_count, 1);
        assert_eq!(
            outcome
                .result_json
                .as_ref()
                .and_then(|json| json.pointer("/structuredContent/clarification")),
            Some(&serde_json::json!({
                "required": true,
                "question": "Which variant?",
            })),
        );
    }

    #[test]
    fn focused_follow_up_stays_effectively_non_identical_after_ui_normalization() {
        let question = "How do I configure the connector?";
        let initial = required_grounded_answer_tool_call(question, 24);
        let focused = focused_grounded_answer_follow_up_call(
            question,
            24,
            RuntimeGroundedRepairMetadata::from_reason(
                GroundedAnswerRepairReason::ProcedureIncomplete,
                QueryLanguage::En,
            ),
        );
        let initial_fingerprint = effective_tool_call_fingerprint_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &initial.arguments_json,
            question,
            false,
            true,
            24,
            "workspace-a/library-b",
            &[],
        )
        .expect("initial fingerprint");
        let focused_fingerprint = effective_tool_call_fingerprint_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &focused.arguments_json,
            question,
            false,
            true,
            24,
            "workspace-a/library-b",
            &[],
        )
        .expect("focused fingerprint");
        let mut normalized =
            serde_json::from_str::<Value>(&focused.arguments_json).expect("focused arguments");
        normalize_agent_tool_argument_types(GROUNDED_ANSWER_TOOL_NAME, &mut normalized);
        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut normalized,
            question,
            false,
            true,
            24,
            "workspace-a/library-b",
            &[],
        );

        assert_ne!(initial_fingerprint, focused_fingerprint);
        assert_eq!(normalized["query"].as_str(), Some(question));
        assert_eq!(normalized["topK"], serde_json::json!(32));
        assert!(normalized.get(RUNTIME_REPAIR_ARGUMENT_FIELD).is_none());
    }

    #[test]
    fn maximum_initial_breadth_reserves_one_candidate_for_a_fresh_repair_probe() {
        let question = "How do I configure the connector?";
        let initial = required_grounded_answer_tool_call(question, MAX_TOP_K);
        let focused = focused_grounded_answer_follow_up_call(
            question,
            MAX_TOP_K,
            RuntimeGroundedRepairMetadata::from_reason(
                GroundedAnswerRepairReason::ProcedureIncomplete,
                QueryLanguage::En,
            ),
        );
        let initial_arguments =
            serde_json::from_str::<Value>(&initial.arguments_json).expect("initial arguments");
        let focused_arguments =
            serde_json::from_str::<Value>(&focused.arguments_json).expect("focused arguments");

        assert_eq!(initial_arguments["query"].as_str(), Some(question));
        assert_eq!(focused_arguments["query"].as_str(), Some(question));
        assert_eq!(initial_arguments["topK"], serde_json::json!(MAX_TOP_K - 1));
        assert_eq!(focused_arguments["topK"], serde_json::json!(MAX_TOP_K));
    }

    #[test]
    fn focused_follow_up_uses_runtime_dispatch_instead_of_the_model() {
        let call = focused_grounded_answer_follow_up_call(
            "How do I configure the connector?",
            24,
            RuntimeGroundedRepairMetadata::from_reason(
                GroundedAnswerRepairReason::ProcedureIncomplete,
                QueryLanguage::En,
            ),
        );

        let response =
            runtime_enforced_tool_response(call.clone(), "synthetic-provider", "synthetic-model");

        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, GROUNDED_ANSWER_TOOL_NAME);
        assert_eq!(response.tool_calls[0].arguments_json, call.arguments_json);
        assert_eq!(response.usage_json["runtimeEnforced"], serde_json::json!(true));
    }

    #[test]
    fn initial_grounded_answer_uses_runtime_dispatch_before_the_model() {
        let call =
            initial_grounded_answer_tool_call(1, 0, "How do I configure the sample connector?", 24)
                .expect("first iteration must use the canonical answer tool");
        let arguments = serde_json::from_str::<Value>(&call.arguments_json).expect("arguments");

        assert_eq!(call.name, GROUNDED_ANSWER_TOOL_NAME);
        assert_eq!(arguments["query"], "How do I configure the sample connector?");
        assert_eq!(arguments["responseProfile"], "compact");
        assert_eq!(arguments["maxReferences"], serde_json::json!(8));
        assert!(initial_grounded_answer_tool_call(2, 1, "Q?", 24).is_none());
    }

    #[test]
    fn failed_runtime_initial_grounded_answer_stops_before_a_model_retry() {
        let call = required_grounded_answer_tool_call("How do I configure it?", 24);
        let outcome = tool_execution_error("canonical answer timed out");

        assert!(runtime_initial_grounded_answer_failed(
            true,
            std::slice::from_ref(&call),
            std::slice::from_ref(&outcome),
            false,
        ));
        assert!(!runtime_initial_grounded_answer_failed(
            false,
            std::slice::from_ref(&call),
            std::slice::from_ref(&outcome),
            false,
        ));
        assert!(!runtime_initial_grounded_answer_failed(true, &[call], &[outcome], true,));
    }

    #[test]
    fn focused_follow_up_carries_typed_language_and_inventory_counts_without_query_prose() {
        let question = "Вопрос с отступами ";
        let call = focused_grounded_answer_follow_up_call(
            question,
            24,
            RuntimeGroundedRepairMetadata::from_reason(
                GroundedAnswerRepairReason::OrderedInventory { expected: 5, observed: 2 },
                QueryLanguage::Ru,
            ),
        );
        let arguments = serde_json::from_str::<Value>(&call.arguments_json).expect("arguments");

        assert_eq!(arguments["query"].as_str(), Some(question));
        assert_eq!(
            arguments[RUNTIME_REPAIR_ARGUMENT_FIELD],
            serde_json::json!({
                "reason": "ordered_inventory_incomplete",
                "language": "ru",
                "expected": 5,
                "observed": 2,
            })
        );
        assert_eq!(arguments["responseProfile"], "compact");
        assert_eq!(
            arguments["maxReferences"],
            crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT
        );
    }

    #[test]
    fn unresolved_focused_repair_prefers_saved_grounded_fallback() {
        assert_eq!(
            unresolved_focused_follow_up_action(true, true),
            UnresolvedFocusedFollowUpAction::ReturnSavedFallback
        );
    }

    #[test]
    fn unresolved_focused_repair_synthesizes_only_from_grounded_evidence() {
        assert_eq!(
            unresolved_focused_follow_up_action(false, true),
            UnresolvedFocusedFollowUpAction::SynthesizeFromEvidence
        );
        assert_eq!(
            unresolved_focused_follow_up_action(false, false),
            UnresolvedFocusedFollowUpAction::FailClosed
        );
    }

    #[test]
    fn failed_focused_follow_up_remains_unresolved() {
        let mut state = FocusedGroundedFollowUpState::default();
        assert!(
            state.schedule(GroundedAnswerRepairReason::ProcedureIncomplete, QueryLanguage::Auto,)
        );
        assert!(state.take_call("How do I configure it?", 24).is_some());

        state.observe_attempt_outcome(&tool_execution_error("repair timed out"));

        assert!(state.requires_resolution());
        assert!(state.is_unresolved_after_attempt());
    }

    #[test]
    fn repeated_incomplete_focused_follow_up_remains_unresolved() {
        let mut state = FocusedGroundedFollowUpState::default();
        assert!(
            state.schedule(GroundedAnswerRepairReason::ProcedureIncomplete, QueryLanguage::Auto,)
        );
        assert!(state.take_call("How do I configure it?", 24).is_some());
        let mut outcome = synthetic_success_outcome();
        outcome.grounded_answer_completed = true;
        outcome.grounded_answer_needs_follow_up = true;
        outcome.grounded_answer_repair_reason =
            Some(GroundedAnswerRepairReason::ProcedureIncomplete);

        state.observe_attempt_outcome(&outcome);

        assert!(state.requires_resolution());
        assert!(state.is_unresolved_after_attempt());
        assert!(
            !state.schedule(GroundedAnswerRepairReason::ProcedureIncomplete, QueryLanguage::Auto,)
        );
    }

    #[test]
    fn unresolved_repair_synthesis_is_marked_unverified() {
        let mut state = FocusedGroundedFollowUpState::default();
        assert!(state.schedule(GroundedAnswerRepairReason::ProcedureIncomplete, QueryLanguage::En));
        assert!(state.take_call("How do I configure it?", 24).is_some());
        state.observe_attempt_outcome(&tool_execution_error("repair timed out"));

        let answer =
            mark_unresolved_repair_synthesis(&state, "Evidence-backed synthesis.".to_string());

        assert!(answer.starts_with("Below is the last completed system answer."));
        assert!(answer.ends_with("Evidence-backed synthesis."));
    }

    #[test]
    fn failed_focused_repair_returns_the_completed_initial_result_as_partial() {
        assert_eq!(
            completed_grounded_answer_after_failed_focused_follow_up(
                true,
                true,
                true,
                Some("Completed initial evidence."),
            ),
            Some("Completed initial evidence.")
        );
        assert!(
            completed_grounded_answer_after_failed_focused_follow_up(
                true,
                true,
                false,
                Some("Completed initial evidence."),
            )
            .is_none()
        );
    }

    #[test]
    fn ready_runtime_focused_repair_passthrough_skips_model_synthesis() {
        let call = focused_grounded_answer_follow_up_call(
            "How do I configure it?",
            24,
            RuntimeGroundedRepairMetadata::from_reason(
                GroundedAnswerRepairReason::ProcedureIncomplete,
                QueryLanguage::En,
            ),
        );
        let outcome = ready_grounded_answer_passthrough_outcome(
            "Complete repaired answer.",
            "How do I configure it?",
        );

        assert_eq!(
            focused_grounded_answer_passthrough_candidate(
                true,
                std::slice::from_ref(&call),
                std::slice::from_ref(&outcome),
            )
            .as_ref()
            .map(|passthrough| passthrough.answer.as_str()),
            Some("Complete repaired answer."),
        );
        assert!(
            focused_grounded_answer_passthrough_candidate(false, &[call], &[outcome]).is_none()
        );
    }

    #[test]
    fn ready_focused_follow_up_resolves_the_gap() {
        let mut state = FocusedGroundedFollowUpState::default();
        assert!(
            state.schedule(GroundedAnswerRepairReason::ProcedureIncomplete, QueryLanguage::En,)
        );
        assert!(state.take_call("How do I configure it?", 24).is_some());
        let mut outcome = synthetic_success_outcome();
        outcome.grounded_answer_ready = true;
        outcome.grounded_answer_completed = true;

        state.observe_attempt_outcome(&outcome);

        assert!(!state.requires_resolution());
        assert!(!state.is_unresolved_after_attempt());
    }

    #[test]
    fn ui_consumes_typed_completion_gap_from_grounded_answer_result() {
        let question = "How do I configure the connector?";
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "The connector is a configurable integration component.".to_string(),
            }],
            structured_content: with_test_completion_envelope(serde_json::json!({
                "answerBody": "The connector is a configurable integration component.",
                "finalAnswerReady": false,
                "finalizable": false,
                "lifecycleState": "completed",
                "completion": {
                    "complete": false,
                    "reason": "procedure_incomplete",
                    "expected": 2,
                    "observed": 0
                },
                "executionDetail": {
                    "verificationState": "verified",
                    "verificationWarnings": []
                }
            })),
            is_error: false,
        };

        assert_eq!(
            grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
            Some(GroundedAnswerRepairReason::ProcedureIncomplete),
        );
        assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result,));
    }

    #[test]
    fn ui_completion_envelope_fails_closed_when_missing_malformed_or_inconsistent() {
        let question = "Describe the sample component.";
        let missing = serde_json::json!({
            "answerBody": "Grounded text.",
            "executionDetail": {"verificationState": "verified"}
        });
        let mut malformed = with_test_completion_envelope(serde_json::json!({
            "answerBody": "Grounded text.",
            "finalAnswerReady": true,
            "finalizable": true,
            "lifecycleState": "completed",
            "executionDetail": {"verificationState": "verified"}
        }));
        malformed["repairPolicy"]["maxAdditionalGroundedAnswerCalls"] = serde_json::json!(2);
        let mut inconsistent = with_test_completion_envelope(serde_json::json!({
            "answerBody": "Grounded text.",
            "finalAnswerReady": true,
            "finalizable": true,
            "lifecycleState": "completed",
            "executionDetail": {"verificationState": "verified"}
        }));
        inconsistent["readiness"]["finalizable"] = serde_json::json!(false);

        for structured_content in [missing, malformed, inconsistent] {
            let result = crate::interfaces::http::mcp::McpToolResult {
                content: vec![crate::interfaces::http::mcp::McpContentBlock {
                    content_type: "text",
                    text: "Grounded text.".to_string(),
                }],
                structured_content,
                is_error: false,
            };

            assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
            assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
            assert_eq!(
                grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
                Some(GroundedAnswerRepairReason::VerificationIncomplete),
            );
        }
    }

    #[test]
    fn typed_canonical_outcome_fails_closed_on_mismatch_or_truncated_candidates() {
        let answer_body = "Choose a neutral variant.";
        let mismatched = crate::interfaces::http::mcp::McpToolResult {
            content: Vec::new(),
            structured_content: with_test_completion_envelope(serde_json::json!({
                "answerBody": answer_body,
                "lifecycleState": "completed",
                "executionDetail": {"answerDisposition": "clarification"},
                "clarification": {"required": false, "question": null},
                "answerCandidates": []
            })),
            is_error: false,
        };
        let mut truncated_content = with_test_completion_envelope(serde_json::json!({
            "answerBody": answer_body,
            "lifecycleState": "completed",
            "clarification": {"required": true, "question": "Which neutral variant?"},
            "answerCandidates": []
        }));
        truncated_content["responseProfile"] = serde_json::json!("compact");
        truncated_content["answerCandidateSummary"] = serde_json::json!({
            "totalCount": 2,
            "returnedCount": 1,
            "truncated": true,
            "candidates": [{
                "label": "Neutral variant",
                "kind": "document",
                "provenance": {}
            }]
        });
        let truncated = crate::interfaces::http::mcp::McpToolResult {
            content: Vec::new(),
            structured_content: truncated_content,
            is_error: false,
        };

        for result in [mismatched, truncated] {
            assert!(
                grounded_answer_canonical_outcome(GROUNDED_ANSWER_TOOL_NAME, &result).is_none()
            );
            assert_eq!(
                grounded_answer_repair_reason(
                    GROUNDED_ANSWER_TOOL_NAME,
                    "Neutral question",
                    &result
                ),
                Some(GroundedAnswerRepairReason::VerificationIncomplete),
            );
            assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
        }
    }

    #[test]
    fn verified_but_incomplete_procedure_cannot_finalize() {
        let question = "How do I configure the connector?";
        let result = verified_grounded_result(
            "The connector is an integration component with several configuration variants.",
            AnswerCompletionAssessment::incomplete(AnswerCompletionGapReason::Procedure, 2, 0),
        );

        assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result,));
        assert_eq!(
            grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
            Some(GroundedAnswerRepairReason::ProcedureIncomplete),
        );
    }

    #[test]
    fn verified_but_incomplete_troubleshooting_cannot_finalize() {
        let question = "What should I do when the operation fails with `E-17`?";
        let result = verified_grounded_result(
            "`E-17` means that the operation encountered duplicate state.",
            AnswerCompletionAssessment::incomplete(
                AnswerCompletionGapReason::Troubleshooting,
                1,
                0,
            ),
        );

        assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result,));
        assert_eq!(
            grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
            Some(GroundedAnswerRepairReason::TroubleshootingIncomplete),
        );
    }

    #[test]
    fn verified_but_structurally_incomplete_answer_cannot_finalize() {
        let question = "Configure Subject Alpha.";
        let result = verified_grounded_result(
            "1. Open the configuration.\n1. Open the configuration.",
            AnswerCompletionAssessment::incomplete(
                AnswerCompletionGapReason::AnswerStructure,
                1,
                0,
            ),
        );

        assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result));
        assert_eq!(
            grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
            Some(GroundedAnswerRepairReason::AnswerStructureIncomplete),
        );
    }

    #[test]
    fn verified_but_short_latest_n_inventory_cannot_finalize() {
        let question = "What changed in the latest 4 revisions?";
        let result = verified_grounded_result(
            "1. Revision 4: improved startup.\n2. Revision 3: improved diagnostics.",
            AnswerCompletionAssessment::incomplete(
                AnswerCompletionGapReason::OrderedInventory,
                4,
                2,
            ),
        );

        assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result,));
        assert_eq!(
            grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
            Some(GroundedAnswerRepairReason::OrderedInventory { expected: 4, observed: 2 }),
        );
    }

    fn verified_grounded_result(
        answer_body: &str,
        completion: AnswerCompletionAssessment,
    ) -> crate::interfaces::http::mcp::McpToolResult {
        crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: answer_body.to_string(),
            }],
            structured_content: with_test_completion_envelope(serde_json::json!({
                "answerBody": answer_body,
                "finalAnswerReady": true,
                "finalizable": true,
                "lifecycleState": "completed",
                "completion": completion,
                "executionDetail": {
                    "verificationState": "verified",
                    "verificationWarnings": []
                }
            })),
            is_error: false,
        }
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
    fn grounded_answer_evidence_ledger_rejects_insufficient_high_signal_inventory() {
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
        assert!(
            ledger.guard_candidate_for_answer(dropped_answer).is_none(),
            "a non-terminal typed answer must not be promoted back to factual evidence"
        );
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

    fn grounded_answer_ledger_outcome(mut result_json: Value) -> ToolExecutionOutcome {
        if let Some(structured_content) = result_json.get_mut("structuredContent") {
            *structured_content = with_test_completion_envelope(std::mem::take(structured_content));
        }
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
            grounded_answer_body: None,
            canonical_answer_outcome: None,
            grounded_answer_ready: false,
            grounded_answer_completed: true,
            grounded_answer_needs_follow_up: false,
            grounded_answer_repair_reason: None,
            grounded_answer_language: QueryLanguage::Auto,
            grounded_answer_clarification_required: false,
            is_error: false,
            is_replay: false,
            duration_ms: 0,
            child_query_execution_ids: Vec::new(),
            child_runtime_execution_ids: Vec::new(),
        }
    }

    fn ready_grounded_answer_passthrough_outcome(
        answer_body: &str,
        executed_query: &str,
    ) -> ToolExecutionOutcome {
        let structured_content = with_test_completion_envelope(serde_json::json!({
            "answerBody": answer_body,
            "finalAnswerReady": true,
            "finalizable": true,
            "completion": {"complete": true},
            "lifecycleState": "completed",
        }));
        ToolExecutionOutcome {
            arguments_json: Some(serde_json::json!({"query": executed_query}).to_string()),
            requested_arguments_json: None,
            message_content: String::new(),
            result_text: Some("human text must not replace structured answerBody".to_string()),
            result_json: Some(serde_json::json!({
                "structuredContent": structured_content,
                "isError": false
            })),
            grounding_text: Some("verifier-grade evidence".to_string()),
            grounded_answer_body: Some(answer_body.to_string()),
            canonical_answer_outcome: Some(AgentCanonicalAnswerOutcome {
                disposition: QueryAnswerDisposition::FactualReady,
                clarification: QueryClarification::default(),
            }),
            grounded_answer_ready: true,
            grounded_answer_completed: true,
            grounded_answer_needs_follow_up: false,
            grounded_answer_repair_reason: None,
            grounded_answer_language: QueryLanguage::Auto,
            grounded_answer_clarification_required: false,
            is_error: false,
            is_replay: false,
            duration_ms: 5,
            child_query_execution_ids: vec![Uuid::from_u128(101)],
            child_runtime_execution_ids: vec![Uuid::from_u128(102)],
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
    fn final_answer_never_splices_raw_user_fragments_into_grounded_text() {
        let answer = "The verified evidence does not support that claim.".to_string();

        let finalized = finalize_agent_loop_answer(
            answer.clone(),
            "Explain «UNTRUSTED-USER-FRAGMENT».",
            None,
            "req",
            Uuid::nil(),
        );

        assert_eq!(finalized, answer);
        assert!(!finalized.contains("UNTRUSTED-USER-FRAGMENT"));
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
    fn final_answer_keeps_a_narrative_rewording_with_the_same_formal_literals() {
        let grounded = "The `mode` field documents the `stable` value.";
        let answer = "The documented value for `mode` is `stable`.".to_string();

        let finalized = finalize_agent_loop_answer(
            answer.clone(),
            "Describe the mode field.",
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
        let structured_content = with_test_completion_envelope(serde_json::json!({
            "answerBody": "Use `/etc/alpha.ini`.",
            "executionId": execution_id,
            "runtimeExecutionId": runtime_execution_id,
            "conversationId": Uuid::now_v7(),
            "libraryId": Uuid::now_v7(),
            "workspaceId": Uuid::now_v7(),
            "lifecycleState": "completed",
            "finalAnswerReady": true,
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
        }));
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
    fn grounded_answer_model_projection_never_defaults_missing_completion_to_complete() {
        let structured_content = serde_json::json!({
            "answerBody": "Grounded text with missing completion metadata.",
            "executionId": Uuid::now_v7(),
            "lifecycleState": "completed",
            "executionDetail": {
                "verificationState": "verified",
                "verificationWarnings": []
            }
        });

        let compacted = compact_grounded_answer_structured_content_for_model(
            &structured_content,
            TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT,
        );

        assert_eq!(compacted["completion"], Value::Null);
        assert_eq!(compacted["finalAnswerReady"], serde_json::json!(false));
        assert_eq!(compacted["finalizable"], serde_json::json!(false));
    }

    #[test]
    fn already_compact_grounded_answer_keeps_references_and_candidates_for_ui_agent() {
        let structured_content = with_test_completion_envelope(serde_json::json!({
            "responseProfile": "compact",
            "executionId": Uuid::now_v7(),
            "runtimeExecutionId": Uuid::now_v7(),
            "conversationId": Uuid::now_v7(),
            "libraryId": Uuid::now_v7(),
            "workspaceId": Uuid::now_v7(),
            "lifecycleState": "completed",
            "finalAnswerReady": false,
            "finalizable": false,
            "completion": {
                "complete": false,
                "reason": "procedure_incomplete",
                "expected": 2,
                "observed": 0
            },
            "verifier": {"state": "verified", "warningCount": 1},
            "warnings": [{"code": "procedure_incomplete", "message": "More steps required."}],
            "mustPreserveSpans": ["alphaMode"],
            "clarification": {"required": true, "question": "Which variant?"},
            "referenceSummary": {
                "totalCount": 2,
                "returnedCount": 2,
                "truncated": false,
                "references": [
                    {"kind": "prepared_segment", "documentTitle": "Guide Alpha", "rank": 1},
                    {"kind": "technical_fact", "displayValue": "alphaMode", "rank": 2}
                ]
            },
            "answerCandidateSummary": {
                "totalCount": 1,
                "returnedCount": 1,
                "truncated": false,
                "candidates": [{"label": "Variant Alpha", "kind": "document"}]
            }
        }));

        let compacted = compact_grounded_answer_structured_content_for_model(
            &structured_content,
            TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT,
        );

        assert_eq!(compacted["referenceSummary"]["totalCount"], serde_json::json!(2));
        assert_eq!(compacted["referenceSummary"]["returnedCount"], serde_json::json!(2));
        assert_eq!(compacted["referenceSummary"]["references"][0]["documentTitle"], "Guide Alpha");
        assert_eq!(compacted["answerCandidateSummary"]["returnedCount"], serde_json::json!(1));
        assert_eq!(compacted["answerCandidateSummary"]["candidates"][0]["label"], "Variant Alpha");
        assert_eq!(compacted["verificationState"], "verified");
    }

    #[test]
    fn grounded_answer_model_projection_obeys_its_character_budget() {
        let huge = "x".repeat(5_000);
        let references = (0..20)
            .map(|rank| serde_json::json!({"rank": rank, "payload": huge.clone()}))
            .collect::<Vec<_>>();
        let preserve_spans = vec![huge.clone(); 16];
        let answer_candidates =
            (0..8).map(|_| serde_json::json!({"label": huge.clone()})).collect::<Vec<_>>();
        let warnings = (0..16)
            .map(|_| serde_json::json!({"code": "partial_coverage", "message": huge.clone()}))
            .collect::<Vec<_>>();
        let structured_content = with_test_completion_envelope(serde_json::json!({
            "answerBody": "Grounded partial answer.",
            "executionId": Uuid::now_v7(),
            "runtimeExecutionId": Uuid::now_v7(),
            "conversationId": Uuid::now_v7(),
            "libraryId": Uuid::now_v7(),
            "workspaceId": Uuid::now_v7(),
            "lifecycleState": "completed",
            "finalAnswerReady": false,
            "finalizable": false,
            "completion": {
                "complete": false,
                "reason": "procedure_incomplete",
                "expected": 2,
                "observed": 0
            },
            "mustPreserveSpans": preserve_spans,
            "answerCandidates": answer_candidates,
            "clarification": {"required": false},
            "executionDetail": {
                "verificationState": "verified",
                "verificationWarnings": warnings,
                "chunkReferences": references.clone(),
                "preparedSegmentReferences": references.clone(),
                "technicalFactReferences": references.clone(),
                "entityReferences": references.clone(),
                "relationReferences": references
            }
        }));

        let compacted = compact_grounded_answer_structured_content_for_model(
            &structured_content,
            TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT,
        );
        let serialized = serde_json::to_string(&compacted).expect("serialize compact projection");

        assert!(serialized.chars().count() <= TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT);
        assert_eq!(compacted["finalAnswerReady"], serde_json::json!(false));
        assert_eq!(compacted["repairPolicy"]["required"], serde_json::json!(true));
        assert_eq!(compacted["verificationState"], "verified");
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
    fn ui_agent_enforces_shared_compact_profile_and_reference_ceiling() {
        let mut arguments = serde_json::json!({
            "query": "focused subquestion",
            "responseProfile": "full",
            "maxReferences": 64
        });

        normalize_agent_tool_argument_types(GROUNDED_ANSWER_TOOL_NAME, &mut arguments);
        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "focused subquestion",
            24,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["responseProfile"], "compact");
        assert_eq!(
            arguments["maxReferences"],
            crate::services::mcp::agent_policy::AGENT_COMPACT_REFERENCE_LIMIT
        );
    }

    #[test]
    fn ui_agent_explicit_debug_request_uses_full_profile_without_compact_limit() {
        let mut arguments = serde_json::json!({
            "query": "focused subquestion",
            "includeDebug": true,
            "responseProfile": "compact",
            "maxReferences": 8
        });

        apply_agent_tool_argument_defaults(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "focused subquestion",
            24,
            "workspace-a/library-b",
            &[],
        );

        assert_eq!(arguments["responseProfile"], "full");
        assert!(arguments.get("maxReferences").is_none());
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
        // Two calls with *identical raw arguments* in the same in-flight batch
        // are genuine same-args spam and must still be curbed.
        let raw_arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "focused subquestion",
            "topK": 8
        })
        .to_string();
        let tool_calls = vec![
            ChatToolCall {
                id: "call-1".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: raw_arguments.clone(),
            },
            ChatToolCall {
                id: "call-2".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: raw_arguments,
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
    fn distinct_raw_call_is_not_suppressed_by_in_flight_effective_collision() {
        // The stage bug: a history-bleed call and the clean call normalize to
        // the same effective fingerprint. While the first is still in-flight in
        // the same batch, the distinct-raw clean call must NOT be dead-ended.
        let tool_calls = vec![
            ChatToolCall {
                id: "call-1".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({
                    "library": "workspace-a/library-b",
                    "query": "how to update the edge service component? Release 9.7.1 added Alpha Suite cache; Release 9.7.0 shipped Provider Beta connector; Release 9.6.3 patched Gamma module logging",
                    "topK": 8
                })
                .to_string(),
            },
            ChatToolCall {
                id: "call-2".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({
                    "library": "workspace-a/library-b",
                    "query": "how to update the edge service component?",
                    "topK": 8
                })
                .to_string(),
            },
        ];
        let user_question = "how to update the edge service component?";
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "list the latest ten releases".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "Release 9.7.1 added Alpha Suite cache; Release 9.7.0 shipped Provider Beta connector; Release 9.6.3 patched Gamma module logging".to_string(),
            },
        ];
        let mut outcomes = vec![None; tool_calls.len()];
        let mut seen = BTreeMap::new();

        let pending = prepare_agent_tool_calls(
            &tool_calls,
            user_question,
            8,
            "workspace-a/library-b",
            &history,
            &mut seen,
            &mut outcomes,
        );

        // Sanity: today both calls collapse to the SAME effective fingerprint.
        let fingerprint_a = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            &tool_calls[0].arguments_json,
            user_question,
            8,
            "workspace-a/library-b",
            &history,
        )
        .expect("fingerprint a");
        let fingerprint_b = effective_tool_call_fingerprint(
            GROUNDED_ANSWER_TOOL_NAME,
            &tool_calls[1].arguments_json,
            user_question,
            8,
            "workspace-a/library-b",
            &history,
        )
        .expect("fingerprint b");
        assert_eq!(
            fingerprint_a, fingerprint_b,
            "the collision repro requires both calls to normalize identically"
        );
        // But their RAW arguments differ, so the clean call is NOT suppressed.
        assert_ne!(tool_calls[0].arguments_json, tool_calls[1].arguments_json);

        assert_eq!(pending.len(), 2);
        assert!(outcomes[0].is_none());
        assert!(outcomes[1].is_none());
    }

    #[test]
    fn clean_call_runs_after_junk_errored_call_clears_fingerprint() {
        // Cross-iteration shape: the junk history-bleed call errored (its
        // fingerprint is removed), so the later clean call runs unimpeded even
        // though it normalizes to the same effective fingerprint.
        let user_question = "how to update the edge service component?";
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "list the latest ten releases".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "Release 9.7.1 added Alpha Suite cache; Release 9.7.0 shipped Provider Beta connector; Release 9.6.3 patched Gamma module logging".to_string(),
            },
        ];
        let junk_call = ChatToolCall {
            id: "call-junk".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({
                "library": "workspace-a/library-b",
                "query": "how to update the edge service component? Release 9.7.1 added Alpha Suite cache; Release 9.7.0 shipped Provider Beta connector; Release 9.6.3 patched Gamma module logging",
                "topK": 8
            })
            .to_string(),
        };
        let clean_call = ChatToolCall {
            id: "call-clean".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({
                "library": "workspace-a/library-b",
                "query": "how to update the edge service component?",
                "topK": 8
            })
            .to_string(),
        };
        let mut seen = BTreeMap::new();

        let mut first_outcomes = vec![None; 1];
        let first_pending = prepare_agent_tool_calls(
            std::slice::from_ref(&junk_call),
            user_question,
            8,
            "workspace-a/library-b",
            &history,
            &mut seen,
            &mut first_outcomes,
        );
        let fingerprint = first_pending[0].fingerprint.clone().expect("fingerprint");
        let raw_args_key = raw_tool_call_argument_key(&junk_call.arguments_json);
        let error = tool_execution_error("upstream timeout");
        record_effective_tool_payload_outcome(&mut seen, fingerprint, raw_args_key, &error);

        let mut second_outcomes = vec![None; 1];
        let second_pending = prepare_agent_tool_calls(
            std::slice::from_ref(&clean_call),
            user_question,
            8,
            "workspace-a/library-b",
            &history,
            &mut seen,
            &mut second_outcomes,
        );

        assert_eq!(second_pending.len(), 1);
        assert!(second_outcomes[0].is_none());
    }

    fn synthetic_success_outcome() -> ToolExecutionOutcome {
        ToolExecutionOutcome {
            arguments_json: Some("{}".to_string()),
            requested_arguments_json: None,
            message_content: "{}".to_string(),
            result_text: Some("ok".to_string()),
            result_json: None,
            grounding_text: None,
            grounded_answer_body: None,
            canonical_answer_outcome: None,
            grounded_answer_ready: false,
            grounded_answer_completed: false,
            grounded_answer_needs_follow_up: false,
            grounded_answer_repair_reason: None,
            grounded_answer_language: QueryLanguage::Auto,
            grounded_answer_clarification_required: false,
            is_error: false,
            is_replay: false,
            duration_ms: 0,
            child_query_execution_ids: Vec::new(),
            child_runtime_execution_ids: Vec::new(),
        }
    }

    // GUARD 1 — `per_tool_call_wait` is bounded by the smaller of the remaining
    // turn deadline and the canonical per-tool-call max.
    #[test]
    fn per_tool_call_wait_is_bounded_by_remaining_and_soft_target() {
        // Synthetic soft target below the hard max, standing in for the caller's
        // tool-collection target threaded into the turn.
        let soft = Duration::from_secs(20);
        assert!(soft < PER_TOOL_CALL_MAX_WAIT);
        // Plenty of turn deadline left: the soft tool-collection target caps it.
        assert_eq!(per_tool_call_wait(Duration::from_secs(120), Some(soft)), soft);
        // Little turn deadline left: the remaining deadline caps it.
        assert_eq!(per_tool_call_wait(Duration::from_secs(2), Some(soft)), Duration::from_secs(2));
        // No soft target supplied: the hard per-call max applies.
        assert_eq!(per_tool_call_wait(Duration::from_secs(120), None), PER_TOOL_CALL_MAX_WAIT);
        // A misconfigured oversized soft target can never exceed the hard max.
        assert_eq!(
            per_tool_call_wait(Duration::from_secs(600), Some(Duration::from_secs(300))),
            PER_TOOL_CALL_MAX_WAIT
        );
    }

    #[test]
    fn grounded_answer_wait_uses_its_canonical_pipeline_budget() {
        let remaining = Duration::from_secs(150);
        let soft = Duration::from_secs(35);

        let wait =
            per_tool_call_wait_for_tool(GROUNDED_ANSWER_TOOL_NAME, remaining, Some(soft), false);

        assert_eq!(wait, GROUNDED_ANSWER_TOOL_MAX_WAIT);
        assert_eq!(
            per_tool_call_wait_for_tool(
                GROUNDED_ANSWER_TOOL_NAME,
                Duration::from_secs(45),
                Some(soft),
                false,
            ),
            Duration::from_secs(45),
        );
        assert_eq!(
            per_tool_call_wait_for_tool("search_documents", remaining, Some(soft), false),
            soft,
        );
        assert_eq!(
            per_tool_call_wait_for_tool(GROUNDED_ANSWER_TOOL_NAME, remaining, Some(soft), true,),
            GROUNDED_ANSWER_REPAIR_MAX_WAIT,
        );
        assert_eq!(
            per_tool_call_wait_for_tool(
                GROUNDED_ANSWER_TOOL_NAME,
                Duration::from_secs(45),
                Some(soft),
                true,
            ),
            Duration::from_secs(45),
        );
        assert_eq!(
            per_tool_call_wait_for_tool(
                GROUNDED_ANSWER_TOOL_NAME,
                Duration::from_secs(20),
                Some(soft),
                true,
            ),
            Duration::from_secs(20),
        );
    }

    // GUARD 1 — per-tool-call timeout.
    #[tokio::test]
    async fn hanging_tool_future_times_out_within_budget_with_structured_error() {
        // A future that never resolves must yield a structured timeout error
        // within the (small, real-time) budget rather than hang the loop.
        let wait = Duration::from_millis(20);
        let hanging = async {
            std::future::pending::<()>().await;
            synthetic_success_outcome()
        };
        let outcome = run_tool_call_within_budget(hanging, wait, "search_documents", 3).await;
        assert!(outcome.is_error);
        assert!(!outcome.is_replay);
        let message = outcome.result_text.unwrap_or_default();
        assert!(
            message.contains("timed out") && message.contains("search_documents"),
            "expected a structured timeout error, got: {message}"
        );
    }

    #[tokio::test]
    async fn tool_call_within_budget_returns_real_outcome() {
        // A future that resolves before the budget passes its outcome through
        // untouched.
        let wait = Duration::from_secs(5);
        let quick = async { synthetic_success_outcome() };
        let outcome = run_tool_call_within_budget(quick, wait, "search_documents", 1).await;
        assert!(!outcome.is_error);
        assert_eq!(outcome.result_text.as_deref(), Some("ok"));
    }

    // GUARD 1 — a timed-out call's error outcome clears the dedup fingerprint so
    // a retry of the same call is not suppressed.
    #[tokio::test]
    async fn timed_out_tool_call_clears_dedup_fingerprint() {
        let call = ChatToolCall {
            id: "call-1".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({
                "library": "workspace-a/library-b",
                "query": "how to configure the gateway?",
                "topK": 8
            })
            .to_string(),
        };
        let mut seen = BTreeMap::new();
        let mut outcomes = vec![None; 1];
        let pending = prepare_agent_tool_calls(
            std::slice::from_ref(&call),
            "how to configure the gateway?",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut outcomes,
        );
        let fingerprint = pending[0].fingerprint.clone().expect("fingerprint");
        let raw_args_key = raw_tool_call_argument_key(&call.arguments_json);

        // Simulate the hung call: the timeout wrapper produces an error outcome.
        let wait = Duration::from_millis(20);
        let hanging = async {
            std::future::pending::<()>().await;
            synthetic_success_outcome()
        };
        let outcome = run_tool_call_within_budget(hanging, wait, &call.name, 1).await;
        assert!(outcome.is_error);
        record_effective_tool_payload_outcome(&mut seen, fingerprint, raw_args_key, &outcome);

        // The identical call must run again, not be suppressed as a duplicate.
        let mut retry_outcomes = vec![None; 1];
        let retry_pending = prepare_agent_tool_calls(
            std::slice::from_ref(&call),
            "how to configure the gateway?",
            8,
            "workspace-a/library-b",
            &[],
            &mut seen,
            &mut retry_outcomes,
        );
        assert_eq!(retry_pending.len(), 1);
        assert!(retry_outcomes[0].is_none(), "retry must not be suppressed after a timeout");
    }

    // GUARD 2 — no-progress early-stop.
    #[test]
    fn no_progress_iterations_force_final_after_limit() {
        let mut streak = 0usize;
        let mut force_final = false;
        // `NO_PROGRESS_ITERATION_LIMIT` consecutive error/replay-only iterations
        // route into the forced-final path.
        for _ in 0..NO_PROGRESS_ITERATION_LIMIT {
            let (next, force) = next_no_progress_state(streak, false);
            streak = next;
            force_final = force_final || force;
        }
        assert!(force_final);
        assert_eq!(streak, NO_PROGRESS_ITERATION_LIMIT);
        // The forced final answer carries the observable `NoProgress` stop
        // reason, distinct from deadline/iteration-cap.
        assert_eq!(final_answer_stop_reason(true), AgentStopReason::NoProgress);
    }

    #[test]
    fn replayed_outcome_is_flagged_as_replay() {
        // A replay is a cache hit, not a fresh successful tool result; the
        // no-progress guard relies on `is_replay` to avoid counting it as
        // progress.
        let payload = CompletedToolPayload {
            message_content: "{}".to_string(),
            result_text: Some("cached".to_string()),
            result_json: None,
            grounding_text: None,
            grounded_answer_body: None,
            canonical_answer_outcome: None,
            grounded_answer_ready: false,
            grounded_answer_completed: false,
            grounded_answer_needs_follow_up: false,
            grounded_answer_repair_reason: None,
            grounded_answer_language: QueryLanguage::Auto,
            grounded_answer_clarification_required: false,
        };
        let outcome = replayed_tool_execution_outcome(payload);
        assert!(!outcome.is_error);
        assert!(outcome.is_replay);
    }

    #[test]
    fn one_no_progress_iteration_below_limit_does_not_force_final() {
        // A single miss must not trip the guard when the limit is above one.
        let (streak, force_final) = next_no_progress_state(0, false);
        assert_eq!(streak, 1);
        assert_eq!(force_final, NO_PROGRESS_ITERATION_LIMIT <= 1);
    }

    // GUARD 2 — a successful NEW tool result resets the no-progress counter.
    #[test]
    fn successful_new_tool_result_resets_no_progress_counter() {
        // Build a streak just under the limit, then a progress iteration resets
        // it so the loop is given a fresh window.
        let mut streak = NO_PROGRESS_ITERATION_LIMIT.saturating_sub(1);
        let (reset_streak, force_after_progress) = next_no_progress_state(streak, true);
        assert_eq!(reset_streak, 0);
        assert!(!force_after_progress);
        streak = reset_streak;
        // After the reset, a single fresh miss must not immediately force final
        // (proving the counter restarted rather than carrying over).
        let (after_miss, force_after_miss) = next_no_progress_state(streak, false);
        assert_eq!(after_miss, 1);
        assert_eq!(force_after_miss, NO_PROGRESS_ITERATION_LIMIT <= 1);
    }

    #[test]
    fn completed_effective_tool_payload_replays_cached_result() {
        // A duplicate of a COMPLETED call replays the stored data instead of a
        // refusal, so the agent gets the answer it asked for.
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
        let raw_args_key = raw_tool_call_argument_key(&tool_calls[0].arguments_json);

        // The original call completed successfully with a real answer body.
        let mut success = tool_execution_error("placeholder");
        success.is_error = false;
        success.message_content = "{\"answer\":\"edge service upgrade steps\"}".to_string();
        success.result_text = Some("edge service upgrade steps".to_string());
        success.grounded_answer_ready = true;
        success.grounded_answer_completed = true;
        record_effective_tool_payload_outcome(&mut seen, fingerprint, raw_args_key, &success);

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

        assert!(second_pending.is_empty());
        let replay = second_outcomes[0].as_ref().expect("replayed outcome");
        assert!(!replay.is_error);
        assert!(replay.grounded_answer_ready);
        assert!(replay.grounded_answer_completed);
        assert_eq!(replay.result_text.as_deref(), Some("edge service upgrade steps"));
        assert!(
            replay
                .result_text
                .as_deref()
                .is_some_and(|text| !text.contains("duplicate MCP tool call suppressed"))
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
        let raw_args_key = raw_tool_call_argument_key(&tool_calls[0].arguments_json);
        seen.insert(
            fingerprint,
            EffectiveToolPayloadEntry {
                raw_args_key,
                state: EffectiveToolPayloadState::Completed(Box::new(CompletedToolPayload {
                    message_content: "{\"answer\":\"cached body\"}".to_string(),
                    result_text: Some("cached body".to_string()),
                    result_json: None,
                    grounding_text: None,
                    grounded_answer_body: Some("cached body".to_string()),
                    canonical_answer_outcome: Some(AgentCanonicalAnswerOutcome {
                        disposition: QueryAnswerDisposition::FactualReady,
                        clarification: QueryClarification::default(),
                    }),
                    grounded_answer_ready: true,
                    grounded_answer_completed: true,
                    grounded_answer_needs_follow_up: false,
                    grounded_answer_repair_reason: None,
                    grounded_answer_language: QueryLanguage::Auto,
                    grounded_answer_clarification_required: false,
                })),
            },
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

        // The dedup entry is turn-scoped and persists across prepare rounds, so
        // the second identical call is not re-dispatched; it replays the cached
        // successful result instead of dead-ending.
        assert_eq!(first_pending.len(), 1);
        assert!(second_pending.is_empty());
        assert!(first_outcomes[0].is_none());
        let replay = second_outcomes[0].as_ref().expect("replayed outcome");
        assert!(!replay.is_error);
        assert_eq!(replay.result_text.as_deref(), Some("cached body"));
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
        let raw_args_key = raw_tool_call_argument_key(&tool_calls[0].arguments_json);
        let error = tool_execution_error("upstream timeout");
        record_effective_tool_payload_outcome(&mut seen, fingerprint, raw_args_key, &error);
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
    fn ui_agent_keeps_grounded_answer_query_at_the_canonical_current_question() {
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

        assert_eq!(arguments["query"], user_question);
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
        assert_eq!(multi_tool_arguments["query"], user_question);
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
    fn ui_agent_drops_model_requested_grounded_answer_history_without_follow_up_gate() {
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

        assert_eq!(arguments["conversationTurns"], serde_json::json!([]));
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
        assert_eq!(arguments["query"], "focused subquestion");
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
    fn ui_agent_drops_model_requested_history_for_non_contextual_grounded_answer() {
        let mut arguments = serde_json::json!({
            "library": "workspace-a/library-b",
            "query": "Which Alpha service workers connect to the message bus and what URL format is used?",
            "conversationTurns": [
                {"role": "user", "content": "previous model supplied topic"},
                {"role": "assistant", "content": "previous model supplied answer"}
            ]
        });
        let history = vec![
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::User,
                content_text: "Alpha service setup inventory".to_string(),
            },
            ExternalConversationTurn {
                turn_kind: QueryTurnKind::Assistant,
                content_text: "`alpha-api` and `alpha-worker` expose local runtime settings."
                    .to_string(),
            },
        ];

        apply_agent_tool_argument_defaults_with_context(
            GROUNDED_ANSWER_TOOL_NAME,
            &mut arguments,
            "Which Alpha service workers connect to the message bus and what URL format is used?",
            false,
            false,
            24,
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
    fn ui_agent_does_not_reclassify_or_reconstruct_grounded_query_from_history() {
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

        assert_eq!(arguments["query"], "Q");
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
    fn ui_agent_rejects_model_rewrite_even_when_it_contains_new_literals() {
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

        assert_eq!(arguments["query"], user_question);
    }

    #[test]
    fn ui_agent_passes_prior_code_literals_only_through_typed_conversation_history() {
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

        assert_eq!(arguments["query"], "Q");
        assert_eq!(
            arguments["conversationTurns"],
            serde_json::json!([{
                "role": "assistant",
                "content": "`k0` `k1` `k2` `fn0` `cred0` `codeTtl0` `/opt/p0/p0.conf`"
            }])
        );
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
        assert_eq!(compact_query, user_question);
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
    fn ui_agent_does_not_rewrite_search_query_from_raw_history() {
        let mut arguments = serde_json::json!({
            "query": "settings parameters",
            "limit": 5
        });
        let history = vec![ExternalConversationTurn {
            turn_kind: QueryTurnKind::Assistant,
            content_text: "ir.memory.literals.v1: `alpha-package`, `/etc/alpha.ini`, `[Main]`, `retryTimeout`, `plainword`\nUse `alpha-package` and `retryTimeout`."
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

        assert_eq!(arguments["query"], "settings parameters");
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
    fn ui_dispatcher_rejects_tools_hidden_from_the_answer_contract() {
        let allowed = BTreeSet::from([
            GROUNDED_ANSWER_TOOL_NAME.to_string(),
            SEARCH_DOCUMENTS_TOOL_NAME.to_string(),
        ]);

        assert!(validate_ui_agent_tool_allowed(GROUNDED_ANSWER_TOOL_NAME, &allowed).is_ok());
        for hidden in ["delete_document", "create_documents", "view_document_image"] {
            let error = validate_ui_agent_tool_allowed(hidden, &allowed)
                .expect_err("hidden tool must be rejected");
            assert!(error.contains("not available"));
            assert!(error.contains(hidden));
        }
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
                "answerBody": "Ready answer.",
                "finalAnswerReady": true,
                "finalizable": true,
                "completion": {"complete": true},
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
            grounded_answer_body: None,
            canonical_answer_outcome: None,
            grounded_answer_ready: true,
            grounded_answer_completed: true,
            grounded_answer_needs_follow_up: false,
            grounded_answer_repair_reason: None,
            grounded_answer_language: QueryLanguage::Auto,
            grounded_answer_clarification_required: false,
            is_error: false,
            is_replay: false,
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
        let structured_content = with_test_completion_envelope(serde_json::json!({
            "answerBody": "Clean Alpha answer.",
            "finalAnswerReady": true,
            "finalizable": true,
            "lifecycleState": "completed",
            "executionDetail": {
                "verificationState": "verified"
            }
        }));
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
                "structuredContent": structured_content,
                "isError": false
            })),
            grounding_text: None,
            grounded_answer_body: Some("Clean Alpha answer.".to_string()),
            canonical_answer_outcome: Some(AgentCanonicalAnswerOutcome {
                disposition: QueryAnswerDisposition::FactualReady,
                clarification: QueryClarification::default(),
            }),
            grounded_answer_ready: true,
            grounded_answer_completed: true,
            grounded_answer_needs_follow_up: false,
            grounded_answer_repair_reason: None,
            grounded_answer_language: QueryLanguage::Auto,
            grounded_answer_clarification_required: false,
            is_error: false,
            is_replay: false,
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
    fn source_marker_reformatting_keeps_good_synthesis_instead_of_raw_tool_body() {
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

        assert_eq!(answer, model_rewrite);
    }

    #[test]
    fn finalization_fails_closed_when_synthesis_adds_an_unverified_structured_step() {
        let verified = "1. Δelta phase.\n2. Ωmega phase.";
        let model_rewrite = "1. Δelta phase.\n\
2. Ωmega phase.\n\
3. Sigma phase.";

        let answer = finalize_agent_loop_answer(
            model_rewrite.to_string(),
            "How do I configure the connector?",
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
    fn completed_grounded_answer_without_verification_requires_follow_up() {
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
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
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
            structured_content: with_test_completion_envelope(serde_json::json!({
                "finalAnswerReady": false,
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "lenient"
                }
            })),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_completed(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn typed_grounded_answer_envelope_controls_final_readiness() {
        let ready_without_legacy_execution_detail = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Flag-only answer.".to_string(),
            }],
            structured_content: with_test_completion_envelope(serde_json::json!({
                "answerBody": "Flag-only answer.",
                "finalAnswerReady": true,
            })),
            is_error: false,
        };
        assert!(grounded_answer_ready(
            GROUNDED_ANSWER_TOOL_NAME,
            &ready_without_legacy_execution_detail,
        ));

        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Ready answer.".to_string(),
            }],
            structured_content: with_test_completion_envelope(serde_json::json!({
                "answerBody": "Ready answer.",
                "finalAnswerReady": true,
                "finalizable": true,
                "completion": {"complete": true},
                "lifecycleState": "completed",
                "executionDetail": {
                    "verificationState": "verified"
                }
            })),
            is_error: false,
        };

        assert!(grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));

        let empty = crate::interfaces::http::mcp::McpToolResult {
            content: vec![],
            structured_content: serde_json::json!({
                "answerBody": "   ",
                "finalAnswerReady": true,
                "finalizable": true,
                "completion": {"complete": true},
                "lifecycleState": "completed",
                "executionDetail": {"verificationState": "verified"}
            }),
            is_error: false,
        };
        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &empty));
    }

    #[test]
    fn typed_safe_fallback_is_terminal_without_becoming_factual_ready() {
        let question = "Explain the documented behavior.";
        let answer_body = "Safe fallback selected by the typed producer.";
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: answer_body.to_string(),
            }],
            structured_content: with_test_completion_envelope(serde_json::json!({
                "answerBody": answer_body,
                "finalAnswerReady": false,
                "finalizable": false,
                "completion": {"complete": true},
                "lifecycleState": "completed",
                "executionDetail": {
                    "answerDisposition": "safe_fallback",
                    "verificationState": "not_run",
                    "verificationWarnings": [{
                        "code": "semantic_verification_not_run",
                        "message": "The typed producer selected a terminal-safe fallback."
                    }]
                }
            })),
            is_error: false,
        };

        assert!(!grounded_answer_ready_for_question(GROUNDED_ANSWER_TOOL_NAME, question, &result,));
        assert!(!grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert_eq!(
            grounded_answer_repair_reason(GROUNDED_ANSWER_TOOL_NAME, question, &result),
            None,
            "a validated terminal safe fallback must not schedule a focused retry",
        );

        let call = ChatToolCall {
            id: "safe-fallback".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({"query": question}).to_string(),
        };
        let mut outcome = grounded_answer_ledger_outcome(debug_tool_result_json(&result));
        outcome.arguments_json = Some(call.arguments_json.clone());
        outcome.grounded_answer_body = Some(answer_body.to_string());
        outcome.canonical_answer_outcome =
            grounded_answer_canonical_outcome(GROUNDED_ANSWER_TOOL_NAME, &result);
        let terminal = terminal_grounded_answer_nonfactual_candidate(
            std::slice::from_ref(&call),
            std::slice::from_ref(&outcome),
            question,
        )
        .expect("a fresh terminal result must not depend on previous tool-call counters");

        assert_eq!(terminal.answer, answer_body);
        assert_eq!(terminal.stop_reason, AgentStopReason::FinalAnswer);
    }

    #[test]
    fn typed_final_readiness_still_blocks_unsupported_exact_literals() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Candidate with an unsupported exact literal.".to_string(),
            }],
            structured_content: with_test_completion_envelope(serde_json::json!({
                "answerBody": "Candidate with an unsupported exact literal.",
                "finalAnswerReady": false,
                "finalizable": false,
                "completion": {"complete": true},
                "lifecycleState": "completed",
                "executionDetail": {
                    "answerDisposition": "non_terminal",
                    "verificationState": "insufficient_evidence",
                    "verificationWarnings": [{
                        "code": "unsupported_literal",
                        "message": "An exact literal is unsupported."
                    }]
                }
            })),
            is_error: false,
        };

        assert!(!grounded_answer_ready_for_question(
            GROUNDED_ANSWER_TOOL_NAME,
            "Explain the documented behavior.",
            &result,
        ));
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn final_grounded_answer_readiness_requires_complete_assessment() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Incomplete answer.".to_string(),
            }],
            structured_content: with_test_completion_envelope(serde_json::json!({
                "answerBody": "Incomplete answer.",
                "finalAnswerReady": true,
                "finalizable": true,
                "completion": {
                    "complete": false,
                    "reason": "procedure_incomplete",
                    "expected": 2,
                    "observed": 0
                },
                "lifecycleState": "completed",
                "executionDetail": {"verificationState": "verified"}
            })),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn ready_grounded_answer_passthrough_is_independent_of_prior_global_tool_counters() {
        let user_question = "refresh Alpha Tool";
        let answer = "1. Run `alpha-tool --refresh`.\n\n```ini\n[Main]\nmode=true\n```";
        let call = ChatToolCall {
            id: "call-grounded".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({"query": "refresh Alpha Tool"}).to_string(),
        };
        let outcome = ready_grounded_answer_passthrough_outcome(answer, "refresh Alpha Tool");

        let passthrough = canonical_grounded_answer_passthrough_candidate(
            std::slice::from_ref(&call),
            std::slice::from_ref(&outcome),
            user_question,
        )
        .expect("one current fresh, complete, verified result should skip parent finalization");

        assert_eq!(passthrough.answer, answer);
        assert_eq!(
            passthrough.canonical_answer_outcome.disposition,
            QueryAnswerDisposition::FactualReady,
        );
    }

    #[test]
    fn canonical_grounded_answer_passthrough_allows_only_outer_whitespace_and_line_endings() {
        let answer = "Canonical answer.";
        let call = ChatToolCall {
            id: "call-grounded".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json:
                serde_json::json!({"query": "Refresh Alpha Tool\nusing the documented mode."})
                    .to_string(),
        };
        let outcome = ready_grounded_answer_passthrough_outcome(
            answer,
            "  Refresh Alpha Tool\r\nusing the documented mode.  ",
        );

        assert_eq!(
            canonical_grounded_answer_passthrough_candidate(
                std::slice::from_ref(&call),
                std::slice::from_ref(&outcome),
                "Refresh Alpha Tool\nusing the documented mode.",
            )
            .as_ref()
            .map(|passthrough| passthrough.answer.as_str()),
            Some(answer),
        );
    }

    #[test]
    fn canonical_grounded_answer_passthrough_preserves_literal_case_and_spacing() {
        let call = ChatToolCall {
            id: "call-grounded".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json:
                serde_json::json!({"query": "Inspect `/opt/app.ini` after error \"A B\"."})
                    .to_string(),
        };
        let case_changed = ready_grounded_answer_passthrough_outcome(
            "Candidate answer.",
            "Inspect `/opt/app.ini` after error \"A B\".",
        );
        assert!(
            canonical_grounded_answer_passthrough_candidate(
                std::slice::from_ref(&call),
                std::slice::from_ref(&case_changed),
                "Inspect `/Opt/App.ini` after error \"A B\".",
            )
            .is_none(),
            "case-sensitive path literals must compare exactly",
        );

        let spacing_changed = ready_grounded_answer_passthrough_outcome(
            "Candidate answer.",
            "Inspect `/opt/app.ini` after error \"A B\".",
        );
        assert!(
            canonical_grounded_answer_passthrough_candidate(
                std::slice::from_ref(&call),
                std::slice::from_ref(&spacing_changed),
                "Inspect `/opt/app.ini` after error \"A  B\".",
            )
            .is_none(),
            "spacing inside quoted literals must compare exactly",
        );
    }

    #[test]
    fn canonical_grounded_answer_passthrough_rejects_narrowed_composite_query() {
        let user_question = "Compare Alpha and Beta across deployment behavior, then list every Gamma constraint and exception.";
        let executed_query =
            "Compare Alpha and Beta in detail across deployment behavior and runtime architecture.";
        let call = ChatToolCall {
            id: "call-grounded".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({"query": executed_query}).to_string(),
        };
        let outcome = ready_grounded_answer_passthrough_outcome(
            "Alpha and Beta differ in their deployment behavior.",
            executed_query,
        );

        assert!(
            canonical_grounded_answer_passthrough_candidate(
                std::slice::from_ref(&call),
                std::slice::from_ref(&outcome),
                user_question,
            )
            .is_none(),
            "a narrowed A/B answer must return to parent synthesis when the full request also asks for Gamma",
        );
    }

    #[test]
    fn canonical_grounded_answer_passthrough_rejects_incomplete_or_repair_result() {
        let call = ChatToolCall {
            id: "call-grounded".to_string(),
            name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
            arguments_json: serde_json::json!({"query": "configure Alpha Tool"}).to_string(),
        };
        let mut incomplete =
            ready_grounded_answer_passthrough_outcome("Partial answer.", "configure Alpha Tool");
        incomplete.grounded_answer_ready = false;
        assert!(
            canonical_grounded_answer_passthrough_candidate(
                std::slice::from_ref(&call),
                std::slice::from_ref(&incomplete),
                "configure Alpha Tool",
            )
            .is_none()
        );

        let mut repair =
            ready_grounded_answer_passthrough_outcome("Repair candidate.", "configure Alpha Tool");
        repair.grounded_answer_needs_follow_up = true;
        repair.grounded_answer_repair_reason =
            Some(GroundedAnswerRepairReason::ProcedureIncomplete);
        assert!(
            canonical_grounded_answer_passthrough_candidate(
                std::slice::from_ref(&call),
                std::slice::from_ref(&repair),
                "configure Alpha Tool",
            )
            .is_none()
        );

        let mut replay =
            ready_grounded_answer_passthrough_outcome("Cached answer.", "configure Alpha Tool");
        replay.is_replay = true;
        assert!(
            canonical_grounded_answer_passthrough_candidate(
                std::slice::from_ref(&call),
                std::slice::from_ref(&replay),
                "configure Alpha Tool",
            )
            .is_none()
        );
    }

    #[test]
    fn canonical_grounded_answer_passthrough_rejects_multiple_tool_calls() {
        let calls = vec![
            ChatToolCall {
                id: "call-grounded".to_string(),
                name: GROUNDED_ANSWER_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({"query": "describe Alpha Tool"}).to_string(),
            },
            ChatToolCall {
                id: "call-document".to_string(),
                name: READ_DOCUMENT_TOOL_NAME.to_string(),
                arguments_json: serde_json::json!({"documentId": Uuid::nil()}).to_string(),
            },
        ];
        let outcomes = vec![
            ready_grounded_answer_passthrough_outcome("Ready answer.", "describe Alpha Tool"),
            synthetic_success_outcome(),
        ];

        assert!(
            canonical_grounded_answer_passthrough_candidate(
                &calls,
                &outcomes,
                "describe Alpha Tool",
            )
            .is_none()
        );
    }

    #[test]
    fn typed_nonterminal_disposition_blocks_even_when_verifier_says_verified() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Warning-bearing but verified answer.".to_string(),
            }],
            structured_content: with_test_completion_envelope(serde_json::json!({
                "answerBody": "Warning-bearing but verified answer.",
                "finalAnswerReady": false,
                "finalizable": false,
                "completion": {"complete": true},
                "lifecycleState": "completed",
                "executionDetail": {
                    "answerDisposition": "non_terminal",
                    "verificationState": "verified",
                    "verificationWarnings": [
                        {
                            "code": "partial_coverage",
                            "warning": "Only part of the requested evidence was grounded."
                        }
                    ]
                }
            })),
            is_error: false,
        };

        assert!(!grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn final_grounded_answer_with_non_evidence_warning_does_not_force_follow_up() {
        let result = crate::interfaces::http::mcp::McpToolResult {
            content: vec![crate::interfaces::http::mcp::McpContentBlock {
                content_type: "text",
                text: "Warning-bearing verified answer.".to_string(),
            }],
            structured_content: with_test_completion_envelope(serde_json::json!({
                "answerBody": "Warning-bearing verified answer.",
                "finalAnswerReady": true,
                "finalizable": true,
                "completion": {"complete": true},
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
            })),
            is_error: false,
        };

        assert!(grounded_answer_ready(GROUNDED_ANSWER_TOOL_NAME, &result));
        assert!(!grounded_answer_needs_follow_up(GROUNDED_ANSWER_TOOL_NAME, &result));
    }

    #[test]
    fn completed_nonterminal_answer_is_not_remembered_as_a_fallback() {
        let structured_content = with_test_completion_envelope(serde_json::json!({
            "answerBody": "Completed tool answer.",
            "finalAnswerReady": false,
            "finalizable": false,
            "lifecycleState": "completed",
            "executionDetail": {
                "answerDisposition": "non_terminal",
                "verificationState": "insufficient_evidence"
            }
        }));
        let outcome = ToolExecutionOutcome {
            arguments_json: None,
            requested_arguments_json: None,
            message_content: String::new(),
            result_text: Some("  Completed tool answer.  ".to_string()),
            result_json: Some(serde_json::json!({
                "content": [{"type": "text", "text": "Completed tool answer."}],
                "structuredContent": structured_content,
                "isError": false
            })),
            grounding_text: None,
            grounded_answer_body: Some("Completed tool answer.".to_string()),
            canonical_answer_outcome: None,
            grounded_answer_ready: false,
            grounded_answer_completed: true,
            grounded_answer_needs_follow_up: true,
            grounded_answer_repair_reason: Some(GroundedAnswerRepairReason::VerificationIncomplete),
            grounded_answer_language: QueryLanguage::Auto,
            grounded_answer_clarification_required: false,
            is_error: false,
            is_replay: false,
            duration_ms: 0,
            child_query_execution_ids: Vec::new(),
            child_runtime_execution_ids: Vec::new(),
        };

        assert!(
            remember_completed_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome).is_none()
        );
    }

    #[test]
    fn verified_completion_partial_is_returnable_only_with_an_explicit_notice() {
        let structured_content = with_test_completion_envelope(serde_json::json!({
            "answerBody": "Подтверждённая часть ответа.",
            "finalAnswerReady": false,
            "finalizable": false,
            "lifecycleState": "completed",
            "completion": {
                "complete": false,
                "reason": "procedure_incomplete",
                "expected": 2,
                "observed": 0
            },
            "executionDetail": {
                "answerDisposition": "factual_ready",
                "verificationState": "verified",
                "verificationWarnings": []
            }
        }));
        let mut outcome = grounded_answer_ledger_outcome(serde_json::json!({
            "content": [{"type": "text", "text": "Подтверждённая часть ответа."}],
            "structuredContent": structured_content,
            "isError": false
        }));
        outcome.grounded_answer_repair_reason =
            Some(GroundedAnswerRepairReason::ProcedureIncomplete);

        let partial =
            remember_verified_partial_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome)
                .expect("verified completion partial");
        let visible = explicitly_mark_partial_grounded_answer(&partial, QueryLanguage::Ru);

        assert!(visible.starts_with("Ниже приведена только подтверждённая источниками часть"));
        assert!(visible.ends_with("Подтверждённая часть ответа."));

        let nonterminal_structured_content = with_test_completion_envelope(serde_json::json!({
            "answerBody": "Подтверждённая часть ответа.",
            "finalAnswerReady": false,
            "finalizable": false,
            "lifecycleState": "completed",
            "completion": {
                "complete": false,
                "reason": "procedure_incomplete",
                "expected": 2,
                "observed": 0
            },
            "executionDetail": {
                "answerDisposition": "non_terminal",
                "verificationState": "insufficient_evidence",
                "verificationWarnings": [{
                    "code": "unsupported_literal",
                    "message": "Unsupported literal"
                }]
            }
        }));
        outcome.result_json.as_mut().expect("result json")["structuredContent"] =
            nonterminal_structured_content;
        assert!(
            remember_verified_partial_grounded_answer(None, GROUNDED_ANSWER_TOOL_NAME, &outcome,)
                .is_none()
        );
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
    fn current_turn_iteration_cap_rejects_unrelated_prior_turn_without_a_current_answer() {
        let unrelated_prior_turn = ChatMessage::assistant_text(
            "Use `legacy-package`, `/etc/legacy.ini`, and `legacyTimeout`.".to_string(),
        );
        assert_eq!(unrelated_prior_turn.role, "assistant");

        let answer = current_turn_grounded_answer_on_iteration_cap(
            AgentStopReason::IterationCap,
            false,
            false,
            1,
            0,
            None,
            None,
        );

        assert!(answer.is_none(), "an unrelated prior turn is not a current-turn answer source");
    }

    #[test]
    fn current_turn_iteration_cap_uses_explicit_follow_up_verified_result() {
        let answer = current_turn_grounded_answer_on_iteration_cap(
            AgentStopReason::IterationCap,
            false,
            true,
            2,
            1,
            Some("Current verified follow-up answer."),
            None,
        );

        assert_eq!(answer, Some("Current verified follow-up answer."));
    }

    #[test]
    fn current_turn_iteration_cap_keeps_completed_result() {
        let answer = current_turn_grounded_answer_on_iteration_cap(
            AgentStopReason::IterationCap,
            false,
            false,
            1,
            0,
            None,
            Some("Current completed answer."),
        );

        assert_eq!(answer, Some("Current completed answer."));
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
                "clarification": {
                    "required": true,
                    "question": "Which documented variant?"
                },
                "payload": "x".repeat(TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT + 128)
            }),
            is_error: false,
        };

        let debug_json = debug_tool_result_json(&result);

        assert_eq!(debug_json["isError"], false);
        assert_eq!(debug_json["content"][0]["text"], "Large result completed.");
        assert_eq!(debug_json["structuredContent"]["truncated"], true);
        assert_eq!(
            debug_json["structuredContent"]["clarification"],
            serde_json::json!({
                "required": true,
                "question": "Which documented variant?"
            })
        );
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
