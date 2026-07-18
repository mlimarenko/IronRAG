//! Transport-neutral policy shared by MCP clients and the in-process UI agent.
//!
//! Keep this policy short: it is sent in every MCP `initialize` response and
//! prefixed to the UI system prompt. Tool schemas describe mechanics; this
//! module owns the small set of cross-client answer-loop invariants.

use crate::mcp_types::MCP_COMPACT_DEFAULT_REFERENCES;

#[cfg(test)]
pub(crate) const AGENT_POLICY_VERSION: &str = "ironrag-agent-policy/1";
#[cfg(test)]
pub(crate) const AGENT_POLICY_MAX_BYTES: usize = 1_200;
#[cfg(test)]
pub(crate) const GROUNDED_ANSWER_DESCRIPTION_MAX_BYTES: usize = 768;
pub(crate) const AGENT_COMPACT_REFERENCE_LIMIT: usize = MCP_COMPACT_DEFAULT_REFERENCES;

pub(crate) const AGENT_INSTRUCTIONS: &str = r"Policy: ironrag-agent-policy/1
- The canonical entry is `grounded_answer` with the exact current user question and typed conversation turns. Request `responseProfile=compact` and `maxReferences<=8`.
- Built-in UI dispatches this canonical call before model tool selection, and the UI runtime owns at most one exact-query repair when `repairPolicy.required=true` and clarification is not required. Do not duplicate or rewrite either call. External MCP clients should mirror the same bounded sequence.
- Treat `clarification.required=true` as terminal: return its exact `answerBody` and choices without a repair call.
- Build a coverage checklist from the request and tool evidence. Finalize only when `finalAnswerReady=true` and the requested coverage is present; otherwise state the evidence limit.
- Preserve every applicable value in `mustPreserveSpans` verbatim unless later tool evidence directly contradicts it.
- Answer only from tool-returned evidence, in the user's language, without narrating tool calls or inventing missing facts.";

pub(crate) const GROUNDED_ANSWER_TOOL_DESCRIPTION: &str = "Canonical grounded answer for the exact current user question and typed conversation turns. The built-in UI dispatches it before model tool selection and owns at most one exact-query repair; do not duplicate or rewrite those calls. External MCP clients should mirror the same bounded sequence. For non-terminal results, obey repairPolicy.required and cap additional exact-query calls at repairPolicy.maxAdditionalGroundedAnswerCalls. Treat clarification.required=true as terminal and return its exact answerBody and choices. Request responseProfile=compact with maxReferences<=8. Finalize factual answers only when finalAnswerReady=true and requested coverage is present. Preserve applicable mustPreserveSpans verbatim. Shared policy: ironrag-agent-policy/1.";

#[must_use]
pub(crate) const fn instructions() -> &'static str {
    AGENT_INSTRUCTIONS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_agent_policy_is_small_versioned_and_operationally_complete() {
        assert!(AGENT_INSTRUCTIONS.len() <= AGENT_POLICY_MAX_BYTES);
        assert!(AGENT_INSTRUCTIONS.contains(AGENT_POLICY_VERSION));
        for signal in [
            "exact current user question",
            "Built-in UI dispatches this canonical call",
            "responseProfile=compact",
            "maxReferences<=8",
            "finalAnswerReady=true",
            "UI runtime owns at most one exact-query repair",
            "clarification.required=true",
            "mustPreserveSpans",
        ] {
            assert!(AGENT_INSTRUCTIONS.contains(signal), "missing policy signal: {signal}");
        }
        assert_eq!(AGENT_COMPACT_REFERENCE_LIMIT, 8);
    }

    #[test]
    fn grounded_answer_description_stays_bounded_and_points_to_the_same_policy() {
        assert!(GROUNDED_ANSWER_TOOL_DESCRIPTION.len() <= GROUNDED_ANSWER_DESCRIPTION_MAX_BYTES);
        assert!(GROUNDED_ANSWER_TOOL_DESCRIPTION.contains(AGENT_POLICY_VERSION));
        for signal in [
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
            assert!(
                GROUNDED_ANSWER_TOOL_DESCRIPTION.contains(signal),
                "missing descriptor signal: {signal}"
            );
        }
    }
}
