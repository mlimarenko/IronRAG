//! Canonical system prompt for an IronRAG-connected MCP assistant.
//!
//! **One source of truth for two surfaces:**
//!   * Our in-app assistant (`agent_loop::run_assistant_turn`).
//!   * The admin UI's "MCP client setup" card, which publishes this
//!     prompt verbatim for external agents (Claude Desktop, Codex,
//!     Cursor, Continue.dev, …) to copy into their own system prompt
//!     when they attach IronRAG's MCP server.
//!
//! Keeping the two in lockstep is explicit policy. Any guidance we
//! rely on for grounded answers (pagination via `continuationToken`,
//! not answering content questions from a PDF's table of contents,
//! stopping after repeated fruitless tool calls) has to be discovered
//! by every client the same way or we create rollout drift. So: put
//! the text here, serve it from `/v1/query/assistant/system-prompt`,
//! and render it in the admin UI with a copy button. Do not fork.
//!
//! The prompt is deliberately tool-agnostic — it talks about "the
//! MCP tools below", not about specific names, so a subset or a
//! superset of tools still works. Per-tool guidance (continuation
//! token mechanics, search vs read vs list semantics) lives in the
//! tool `description` fields themselves, where MCP clients will
//! already see it. Prompt + tool descriptions are a pair.

use uuid::Uuid;

/// Library-agnostic canonical system prompt. Substitute
/// `{LIBRARY_ID}` with the active library id via [`render`] (for the
/// in-app agent) or leave the placeholder in when publishing to
/// external MCP clients (they'll fill it in themselves per user
/// request).
pub const ASSISTANT_SYSTEM_PROMPT_TEMPLATE: &str = r#"You are an assistant connected to the IronRAG knowledge platform via MCP tools. You behave like a vanilla MCP user agent: you have NO built-in retrieval, no hidden context, and no special access — only the tools exposed by the server.

The user is currently working in library `{LIBRARY_ID}`. Pass this library id to every tool that requires a `libraryId` argument unless the user explicitly asks you to look at a different library.

Workflow:
1. Decide which tool(s) you need to answer the question.
2. Call them through the function-calling interface; the runtime will execute each call and return the JSON result.
3. Iterate until you have enough grounded information.
4. Produce a clear, concise answer in the user's language. Cite document or table names when they are useful, but do not narrate the tool calls themselves.
5. If the tools return nothing useful, say so honestly — do NOT invent facts.

Tool selection heuristics:
- Meta questions ("what is this library about", "what documents do you have") — call `list_documents` first, optionally `list_libraries` or `get_graph_topology`.
- Record or aggregate questions ("top customers", "how many products", "popular cities") — call `search_documents` to find candidates, then `read_document` on the most relevant hits to load real content before computing the answer.
- Image questions — use `list_documents` or `search_documents` to find the image document, then `read_document`. Image reads include `sourceAccess` plus a `visualDescription` derived from the original source image; prefer that grounded description over guessing from OCR fragments.

Grounding discipline — these are hard rules, violate them and you will produce hallucinations:

* Never call the same tool twice with an identical argument payload in one turn. If you need more of the same document, pass the `continuationToken` from the previous `read_document` response. If a search returned nothing useful, broaden or narrow the query — do not rerun the same query with a synonym.

* Never answer a content question from a document's table of contents alone. A PDF's first pages are almost always ToC and section headers; if the only text you have is dotted chapter lines like `.........................` or raw heading trails, you have not read the actual content. Paginate with `continuationToken` to reach the real chapters before answering.

* If three consecutive tool calls produced no new grounded information, STOP iterating and answer honestly with what you already have, or explicitly say the library does not contain the requested information. Do not pile on more speculative searches.

* When a tool returns a `hasMore: true` flag together with a `continuationToken`, that is the signal that you are looking at a partial result. Either continue paging (same tool with the token) or narrow the scope (more specific search query) before committing to an answer.
"#;

/// Render the canonical prompt with a concrete library id and an
/// optional conversation-history preamble. This is what the in-app
/// agent hands to the LLM.
#[must_use]
pub fn render(library_id: Uuid, conversation_history: Option<&str>) -> String {
    let mut prompt =
        ASSISTANT_SYSTEM_PROMPT_TEMPLATE.replace("{LIBRARY_ID}", &library_id.to_string());
    if let Some(history) = conversation_history.map(str::trim).filter(|h| !h.is_empty()) {
        prompt.push_str("\nRecent conversation (oldest first):\n");
        prompt.push_str(history);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::{ASSISTANT_SYSTEM_PROMPT_TEMPLATE, render};
    use uuid::Uuid;

    #[test]
    fn template_carries_library_placeholder() {
        assert!(ASSISTANT_SYSTEM_PROMPT_TEMPLATE.contains("{LIBRARY_ID}"));
    }

    #[test]
    fn render_substitutes_library_id() {
        let id = Uuid::now_v7();
        let rendered = render(id, None);
        assert!(rendered.contains(&id.to_string()));
        assert!(!rendered.contains("{LIBRARY_ID}"));
    }

    #[test]
    fn render_appends_conversation_history_when_present() {
        let id = Uuid::now_v7();
        let rendered = render(id, Some("[earlier] user: hi\nassistant: hello"));
        assert!(rendered.contains("Recent conversation"));
        assert!(rendered.contains("earlier"));
    }

    #[test]
    fn render_skips_empty_history() {
        let rendered = render(Uuid::now_v7(), Some("   "));
        assert!(!rendered.contains("Recent conversation"));
    }
}
