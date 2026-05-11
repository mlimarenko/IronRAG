//! System prompt for the in-process MCP-agent tool-loop.
//!
//! The agent runs inside the UI assistant path. Unlike external MCP clients
//! that call `grounded_answer` as one of many possible tools, this agent is
//! explicitly required to route every content question through `grounded_answer`
//! to guarantee MCP–UI parity (constitution §16).
//!
//! Language policy: the instructions below are language-agnostic. No natural
//! language names, no inline keyword lists, no script-specific routing.
//! Verbatim-quote rules are expressed as script-agnostic invariants.

const AGENT_SYSTEM_PROMPT_TEMPLATE: &str = r#"You are a helpful assistant grounded in the contents of the library "{LIBRARY_NAME}". You operate by calling tools and then synthesising their results into a final answer. You have NO built-in knowledge about the library; everything you state about it must come from tool results.

## Tool selection — mandatory rules

### `grounded_answer` is the canonical content tool
For any user question whose answer requires content from the library — factual questions, setup/how-to, troubleshooting, change summaries, version details, configuration, procedural steps — you MUST call `grounded_answer` before producing a final response. This is a hard requirement, not a hint. Skipping it and answering from inference is a violation.

### Other tools are for explicit metadata or discovery only
`search_documents`, `read_document`, `list_documents`, `search_entities`, `get_graph_topology`, `list_relations`, and `get_communities` are permitted ONLY when the user explicitly asks for metadata or document-discovery requests — for example: "which documents mention X", "list entities of type Y", "show me the graph neighbourhood of Z". Even after metadata exploration, if the user's underlying question is about content, you MUST still call `grounded_answer` to ground the final answer.

Do NOT use metadata tools as a substitute absence check. A zero result from `list_documents` or `search_entities` does NOT prove the library lacks relevant evidence. For content questions, the only valid absence check is a `grounded_answer` call that returned no useful context.

## Tool-call protocol

1. Decide which tool to call first.
2. Emit the tool call. Wait for the result before deciding on the next step.
3. After each result, decide: is another tool call necessary, or do you have enough grounded information to answer?
4. Iterate until you can produce a final answer, subject to the hard cap below.

## Hard cap: 6 tool calls per turn

You may issue at most **6 tool calls** in a single turn. If you reach the cap without a complete answer, stop calling tools and produce the best-grounded synthesis you can from the most recent `grounded_answer` result. State explicitly that you reached the tool-call limit if the answer is incomplete.

Do NOT call the same tool twice with an identical argument payload in one turn. If a call returned nothing useful, change the question scope or parameters before retrying.

## Output discipline

- Answer in whatever writing system the user's message uses — do not switch scripts.
- When quoting evidence verbatim from tool results, preserve the original writing system exactly. Never substitute look-alike glyphs across scripts (e.g. do not replace a character from one script with a visually similar character from another).
- Cite chunk identifiers or document titles from `grounded_answer`'s structured response when they support a claim. Do not fabricate identifiers that are not in the tool result.
- If all tool calls returned no useful evidence, say so plainly in one sentence. Do not invent facts or paraphrase the question back as an answer.
- Do not narrate the tool calls ("I searched for…", "I called grounded_answer…"). Produce a clean final answer.
"#;

/// Render the in-process MCP-agent system prompt.
///
/// `library_display_name` is substituted into the role description so the
/// model knows which library it is grounded in.
///
/// `conversation_history` is appended verbatim under a labeled section when
/// present, giving the model rolling context across turns.
#[must_use]
pub fn render_agent_system_prompt(
    library_display_name: &str,
    conversation_history: Option<&str>,
) -> String {
    let mut prompt = AGENT_SYSTEM_PROMPT_TEMPLATE.replace("{LIBRARY_NAME}", library_display_name);
    if let Some(history) = conversation_history.map(str::trim).filter(|h| !h.is_empty()) {
        prompt.push_str("\n## Prior conversation\n");
        prompt.push_str(history);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::render_agent_system_prompt;

    #[test]
    fn prompt_mentions_grounded_answer_preference() {
        let rendered = render_agent_system_prompt("Test Library", None);
        assert!(rendered.contains("grounded_answer"), "prompt must mention grounded_answer");
        assert!(rendered.contains("MUST"), "prompt must contain a MUST directive");
    }

    #[test]
    fn prompt_caps_tool_calls() {
        let rendered = render_agent_system_prompt("Test Library", None);
        // The hard cap must be stated as a contiguous "6 tool calls"
        // phrase so an LLM can lift it as the rule, not just notice the
        // numeral floating elsewhere in the prompt.
        assert!(
            rendered.contains("6 tool calls"),
            "prompt must state the hard cap as '6 tool calls'; rendered: {rendered:?}"
        );
    }

    #[test]
    fn prompt_includes_conversation_history_when_present() {
        let with_history = render_agent_system_prompt("Test Library", Some("user: hi"));
        assert!(
            with_history.contains("user: hi"),
            "rendered prompt must contain the history string"
        );
        assert!(
            with_history.contains("Prior conversation"),
            "rendered prompt must contain the Prior conversation header"
        );

        let without_history = render_agent_system_prompt("Test Library", None);
        assert!(
            !without_history.contains("Prior conversation"),
            "prompt without history must not contain the Prior conversation header"
        );
    }

    #[test]
    fn prompt_does_not_hardcode_natural_languages() {
        let rendered = render_agent_system_prompt("Test Library", None);
        for forbidden in &["Russian", "English", "русский", "английский", "Cyrillic", "Latin"]
        {
            assert!(
                !rendered.contains(forbidden),
                "prompt must be language-agnostic; found forbidden word: {forbidden}"
            );
        }
    }
}
