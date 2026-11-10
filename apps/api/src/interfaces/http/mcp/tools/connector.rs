//! MCP diagnostics tool over the bundled connector catalog.
//!
//! The catalog is static code, so the tool takes no arguments and has no
//! mutation counterpart: the access layer authorizes the read and the domain
//! module owns the list.

use serde_json::{Value, json};

use super::super::{McpToolDescriptor, McpToolResult, ok_tool_result, tool_error_result};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    match name {
        "list_connector_sources" => Some(McpToolDescriptor {
            name: "list_connector_sources",
            description: "List the bundled connector sources: kind, display name, supported run modes, container image, and deployment directory. Read-only; requires connector read access.",
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        }),
        _ => None,
    }
}

pub(crate) fn call_tool(
    name: &str,
    context: ToolCallContext<'_>,
    _arguments: &Value,
) -> Option<McpToolResult> {
    match name {
        "list_connector_sources" => Some(list_connector_sources(context)),
        _ => None,
    }
}

fn list_connector_sources(context: ToolCallContext<'_>) -> McpToolResult {
    match crate::services::mcp::access::list_connector_sources(context.auth) {
        Ok(payload) => {
            let summary = format!("Listed {} bundled connector source(s).", payload.items.len());
            ok_tool_result(&summary, json!(payload))
        }
        Err(error) => tool_error_result(error),
    }
}
