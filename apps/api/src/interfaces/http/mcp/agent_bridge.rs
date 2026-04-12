//! Bridge between the in-app assistant agent loop and the MCP tool surface.
//!
//! The in-app assistant is meant to be a "vanilla" tool-using LLM agent that
//! talks to the same MCP tools that external agents (Codex, Cursor, VS Code,
//! etc.) connect to. To avoid duplicating tool definitions or business logic,
//! the agent loop pulls its tool catalog from the MCP descriptor functions
//! and dispatches every tool call back through the MCP handlers.
//!
//! This module is the only public surface that exposes those internals to
//! the rest of the crate.

use serde_json::Value;

use crate::{
    app::state::AppState, integrations::llm::ChatToolDef, interfaces::http::auth::AuthContext,
};

use super::tools::{
    ToolCallContext,
    catalog::{call_tool as catalog_call, descriptor as catalog_descriptor},
    documents::{call_tool as documents_call, descriptor as documents_descriptor},
    graph::{call_tool as graph_call, descriptor as graph_descriptor},
    runtime::{call_tool as runtime_call, descriptor as runtime_descriptor},
    visible_tool_names,
    web_ingest::{call_tool as web_ingest_call, descriptor as web_ingest_descriptor},
};

/// Build the LLM tool catalog the in-app assistant should hand to the model.
/// The list mirrors what `tools/list` would return for the same auth context,
/// minus tools that are not useful inside an answer-the-user agent loop
/// (write/admin tools are kept; the user wants the assistant to behave like
/// a standard MCP client with the caller's full permissions).
#[must_use]
pub fn list_assistant_tools(auth: &AuthContext) -> Vec<ChatToolDef> {
    let mut out = Vec::new();
    for name in visible_tool_names(auth) {
        let descriptor = catalog_descriptor(&name)
            .or_else(|| documents_descriptor(&name))
            .or_else(|| runtime_descriptor(&name))
            .or_else(|| web_ingest_descriptor(&name))
            .or_else(|| graph_descriptor(&name));
        let Some(descriptor) = descriptor else {
            continue;
        };
        let mut parameters = descriptor.input_schema;
        sanitize_tool_schema(&mut parameters);
        out.push(ChatToolDef {
            name: descriptor.name.to_string(),
            description: descriptor.description.to_string(),
            parameters,
        });
    }
    out
}

/// Strip JSON-schema constructs that OpenAI's strict tool-calling mode does
/// not accept at the top level (`oneOf`, `anyOf`, `allOf`, `not`, top-level
/// `enum`). These constructs are useful for `tools/list` documentation but
/// the LLM only needs the property shape; constraint enforcement happens
/// downstream when the MCP handler parses the arguments.
fn sanitize_tool_schema(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    object.remove("oneOf");
    object.remove("anyOf");
    object.remove("allOf");
    object.remove("not");
    object.remove("if");
    object.remove("then");
    object.remove("else");
    // Recurse into nested property schemas so embedded constructs are
    // cleaned too.
    if let Some(properties) = object.get_mut("properties").and_then(|v| v.as_object_mut()) {
        for value in properties.values_mut() {
            sanitize_tool_schema(value);
        }
    }
    if let Some(items) = object.get_mut("items") {
        sanitize_tool_schema(items);
    }
    if let Some(additional) = object.get_mut("additionalProperties") {
        if additional.is_object() {
            sanitize_tool_schema(additional);
        }
    }
}

/// Result of dispatching one tool call from the agent loop.
pub struct AgentToolDispatch {
    /// JSON-serialized tool result that should be sent back to the LLM as
    /// the `role: tool` message content.
    pub tool_message_text: String,
    /// True when the underlying MCP handler reported an error. The agent
    /// loop still feeds this back to the model so it can recover.
    pub is_error: bool,
}

/// Execute one tool call by name through the canonical MCP dispatchers and
/// return its serialized result. The agent loop wraps this in a
/// `role: tool` message before the next LLM call.
pub async fn dispatch_assistant_tool(
    state: &AppState,
    auth: &AuthContext,
    request_id: &str,
    name: &str,
    arguments: &Value,
) -> AgentToolDispatch {
    let context = ToolCallContext { auth, state, request_id };
    let result = if let Some(result) = catalog_call(name, context, arguments).await {
        result
    } else if let Some(result) = documents_call(name, context, arguments).await {
        result
    } else if let Some(result) = runtime_call(name, context, arguments).await {
        result
    } else if let Some(result) = web_ingest_call(name, context, arguments).await {
        result
    } else if let Some(result) = graph_call(name, context, arguments).await {
        result
    } else {
        return AgentToolDispatch {
            tool_message_text: serde_json::json!({
                "error": format!("unknown tool '{name}'")
            })
            .to_string(),
            is_error: true,
        };
    };

    let payload = serde_json::json!({
        "structuredContent": result.structured_content,
        "content": result.content.iter().map(|block| block.text.clone()).collect::<Vec<_>>(),
        "isError": result.is_error,
    });

    AgentToolDispatch {
        tool_message_text: serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
        is_error: result.is_error,
    }
}
