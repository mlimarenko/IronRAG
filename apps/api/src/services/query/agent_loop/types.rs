use super::*;

pub(super) const RUNTIME_RETRIEVED_CONTEXT_TOOL: &str = "ironrag_retrieved_context";
pub(super) const RUNTIME_LITERAL_REVISION_CONTEXT_TOOL: &str = "ironrag_literal_revision_context";
pub(super) const GROUNDED_ANSWER_TOOL_NAME: &str = "grounded_answer";
pub(super) const RUNTIME_REPAIR_ARGUMENT_FIELD: &str = "_ironragRuntimeRepair";
pub(super) const GROUNDED_ANSWER_LIFECYCLE_COMPLETED: &str = "completed";
pub(super) const TOOL_MODEL_DEFAULT_CONTENT_CHAR_LIMIT: usize = 3_000;
pub(super) const TOOL_MODEL_GROUNDED_ANSWER_CONTENT_CHAR_LIMIT: usize = 12_000;
pub(super) const TOOL_MODEL_READ_DOCUMENT_CONTENT_CHAR_LIMIT: usize = 5_000;
pub(super) const TOOL_MODEL_STRUCTURED_JSON_CHAR_LIMIT: usize = 8_000;
pub(super) const TOOL_VERIFICATION_CONTENT_CHAR_LIMIT: usize = 8_000;
pub(super) const TOOL_VERIFICATION_STRUCTURED_JSON_CHAR_LIMIT: usize = 16_000;
pub(super) const TOOL_MODEL_GROUNDED_REFERENCE_LIMIT: usize = 8;
pub(super) const TOOL_MODEL_FINALIZABLE_GROUNDED_REFERENCE_LIMIT: usize = 0;
pub(super) const TOOL_VERIFICATION_GROUNDED_REFERENCE_LIMIT: usize = 8;
pub(super) const TOOL_DEBUG_RESULT_JSON_CHAR_LIMIT: usize = 96_000;
pub(super) const TOOL_GROUNDING_FRAGMENT_CHAR_LIMIT: usize = 20_000;
pub(super) const TOOL_GROUNDING_TOTAL_CHAR_LIMIT: usize = 80_000;
pub(super) const GROUNDED_EVIDENCE_LEDGER_ENTRY_LIMIT: usize = 6;
pub(super) const GROUNDED_EVIDENCE_LEDGER_ANSWER_CHARS: usize = 1_600;
pub(super) const GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT: usize = 32;
pub(super) const GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT: usize = 24;
pub(super) const GROUNDED_EVIDENCE_LEDGER_TEXT_CHARS: usize = 10_000;
pub(super) const GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_ANCHORS: usize = 3;
pub(super) const GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_MISSING_HIGH_SIGNAL_ANCHORS: usize = 2;
pub(super) const SOFT_FINAL_ANSWER_MIN_SUCCESSFUL_TOOLS: usize = 4;
/// Consecutive tool-running iterations that may yield zero successful *new*
/// tool results (every call errored, was suppressed as a duplicate, or
/// replayed an earlier result) before the loop forces a final answer instead
/// of spinning the remaining iteration/deadline budget on a stuck model. Two
/// is the smallest window that tolerates a single transient miss while still
/// catching a genuine no-progress loop early.
pub(super) const NO_PROGRESS_ITERATION_LIMIT: usize = 2;
/// Hard upper bound for a single tool-call wait, independent of how much turn
/// deadline is left. The per-call wait is `min(remaining turn deadline, this)`
/// so one hung tool future can never consume the whole turn budget. Falls back
/// to this when the caller supplies no soft tool-collection target; otherwise
/// the soft target (the configured per-tool-call SLO threaded into the turn)
/// bounds the wait.
pub(super) const PER_TOOL_CALL_MAX_WAIT: Duration = Duration::from_secs(35);
// Initial `grounded_answer` uses the remaining assistant-turn deadline instead
// of a separate cap: large libraries can spend most of that budget in
// retrieve→source-context alone, and a fixed sub-deadline aborts before the
// pipeline can finish even though the turn still has time left.
// A repair is optional recovery after a completed authoritative answer. Bound it
// below a full grounded-answer execution so a slow second retrieval cannot consume
// the turn; the completed first result remains available as an explicit
// partial fallback.
pub(super) const GROUNDED_ANSWER_REPAIR_MAX_WAIT: Duration = Duration::from_secs(45);
pub(super) const GROUNDED_ANSWER_REPAIR_TOP_K_INCREMENT: usize = 8;
pub(super) const GROUNDED_ANSWER_REPAIR_TOP_K_HEADROOM: usize = 1;
pub(super) const STRUCTURAL_IDENTIFIER_MIN_CHARS: usize = 4;
pub(super) const STRUCTURAL_IDENTIFIER_MAX_CHARS: usize = 400;
pub(super) const STRUCTURAL_LITERAL_ANCHOR_MAX_CHARS: usize = 180;
pub(super) const VERIFIED_GROUNDED_LITERAL_GUARD_MIN_LITERALS: usize = 4;
/// Final result of one assistant turn.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum AgentAnswerProvenance {
    Composed,
    GroundedAnswerPassthrough,
}

/// Typed finalizer-owned outcome coupled to an exact grounded-answer
/// `grounded_answer` passthrough. Composed answers intentionally carry no
/// child outcome because the parent finalizer owns their disposition.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentGroundedAnswerOutcome {
    pub disposition: QueryAnswerDisposition,
    pub clarification: QueryClarification,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentTurnResult {
    pub answer: String,
    pub answer_provenance: AgentAnswerProvenance,
    pub grounded_answer_outcome: Option<AgentGroundedAnswerOutcome>,
    pub usage_json: serde_json::Value,
    /// One immutable attribution record per actual provider response.
    pub provider_calls: Vec<QueryProviderCall>,
    pub iterations: usize,
    pub assistant_grounding: AssistantGroundingEvidence,
    pub child_query_execution_ids: Vec<Uuid>,
    /// Per-iteration capture of the exact LLM request/response chain,
    /// for the assistant debug panel. Populated unconditionally — the
    /// cost is a few clones and the operator toggles the UI to view.
    pub debug_iterations: Vec<super::super::llm_context_debug::LlmIterationDebug>,
    /// Present when a turn was driven by the MCP client-style agent
    /// loop instead of a single fixed-context answer stage.
    pub agent_loop: Option<AgentLoopMetadata>,
}

/// Agent-loop failure with the partial provider transcript preserved
/// for the debug panel.
#[derive(Debug)]
pub(crate) struct AgentTurnFailure {
    pub error: QueryServiceError,
    /// Provider responses completed before the loop failed.
    pub provider_calls: Vec<QueryProviderCall>,
    pub debug_iterations: Vec<LlmIterationDebug>,
    /// Boxed so the `Err` side of every `Result<_, AgentTurnFailure>` stays small
    /// under `clippy::result_large_err`: the metadata would otherwise push the
    /// struct past the 128-byte default threshold.
    pub agent_loop: Option<Box<AgentLoopMetadata>>,
}

impl AgentTurnFailure {
    pub(super) fn empty(error: impl Into<QueryServiceError>) -> Self {
        Self {
            error: error.into(),
            provider_calls: Vec::new(),
            debug_iterations: Vec::new(),
            agent_loop: None,
        }
    }

    pub(super) fn with_loop(
        error: impl Into<QueryServiceError>,
        provider_calls: Vec<QueryProviderCall>,
        debug_iterations: Vec<LlmIterationDebug>,
        agent_loop: AgentLoopMetadata,
    ) -> Self {
        Self {
            error: error.into(),
            provider_calls,
            debug_iterations,
            agent_loop: Some(Box::new(agent_loop)),
        }
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
