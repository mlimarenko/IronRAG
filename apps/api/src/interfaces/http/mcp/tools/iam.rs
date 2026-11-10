//! MCP diagnostics tools over IAM tokens and effective access.
//!
//! Every tool here requires a system administrator session; the access layer
//! enforces that before touching IAM state.

use serde_json::{Value, json};

use crate::mcp_types::{
    McpGetPrincipalAccessRequest, McpListIamTokensRequest, McpMintIamTokenRequest,
    McpRevokeIamTokenRequest, McpUpdateIamTokenRequest,
};

use super::super::{
    McpToolDescriptor, McpToolResult, ok_tool_result, parse_tool_args, tool_error_result,
};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    match name {
        "list_iam_tokens" => Some(McpToolDescriptor {
            name: "list_iam_tokens",
            description: "List API tokens, optionally filtered by workspace. Requires a system administrator session.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspaceId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Optional workspace filter. Absent lists tokens from every workspace."
                    }
                }
            }),
        }),
        "revoke_iam_token" => Some(McpToolDescriptor {
            name: "revoke_iam_token",
            description: "Revoke one API token by its principal id. Requires a system administrator session.",
            input_schema: json!({
                "type": "object",
                "required": ["tokenId"],
                "properties": {
                    "tokenId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "API token principal identifier."
                    }
                }
            }),
        }),
        "get_principal_access" => Some(McpToolDescriptor {
            name: "get_principal_access",
            description: "Resolve one principal's effective grants and workspace memberships. Requires a system administrator session.",
            input_schema: json!({
                "type": "object",
                "required": ["principalId"],
                "properties": {
                    "principalId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Principal identifier."
                    }
                }
            }),
        }),
        "mint_iam_token" => Some(McpToolDescriptor {
            name: "mint_iam_token",
            description: "Mint one API token. The plaintext secret is returned in this response only and cannot be read again; persist it immediately. Optional permissionKinds are validated against workspaceId or every libraryId; omit them to mint a token without grants. Requires a system administrator session.",
            input_schema: json!({
                "type": "object",
                "required": ["label"],
                "properties": {
                    "label": {
                        "type": "string",
                        "description": "Human-readable token label. Must not be empty."
                    },
                    "workspaceId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Workspace the token and its workspace-scoped grants belong to. Optional when libraryIds name exactly one library; required when permissionKinds are given without libraryIds."
                    },
                    "expiresAt": {
                        "type": "string",
                        "format": "date-time",
                        "description": "Optional expiry timestamp. Absent mints a token that never expires."
                    },
                    "libraryIds": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "format": "uuid"
                        },
                        "description": "Libraries to create permissionKinds grants on."
                    },
                    "permissionKinds": {
                        "type": "array",
                        "items": {
                            "type": "string"
                        },
                        "description": "Permission kinds such as library_read, query_run, or connector_admin, granted on workspaceId or on every libraryId. A kind the resource cannot carry is rejected."
                    }
                }
            }),
        }),
        "update_iam_token" => Some(McpToolDescriptor {
            name: "update_iam_token",
            description: "Update an API token's label or expiry. Omitted expiresAt keeps the current expiry, null clears it, and a timestamp sets it. The permission scope is immutable after mint: change scope by minting a new token. Requires a system administrator session.",
            input_schema: json!({
                "type": "object",
                "required": ["tokenId"],
                "properties": {
                    "tokenId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "API token principal identifier."
                    },
                    "label": {
                        "type": "string",
                        "description": "New label. Omit to keep the current one; must not be empty."
                    },
                    "expiresAt": {
                        "type": ["string", "null"],
                        "format": "date-time",
                        "description": "Omit to keep the current expiry, null to clear it, a timestamp to set it."
                    }
                }
            }),
        }),
        _ => None,
    }
}

pub(crate) async fn call_tool(
    name: &str,
    context: ToolCallContext<'_>,
    arguments: &Value,
) -> Option<McpToolResult> {
    let result = match name {
        "list_iam_tokens" => list_iam_tokens(context, arguments).await,
        "revoke_iam_token" => revoke_iam_token(context, arguments).await,
        "get_principal_access" => get_principal_access(context, arguments).await,
        "mint_iam_token" => mint_iam_token(context, arguments).await,
        "update_iam_token" => update_iam_token(context, arguments).await,
        _ => return None,
    };
    Some(result)
}

async fn list_iam_tokens(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpListIamTokensRequest>(arguments.clone()) {
        Ok(args) => {
            match crate::services::mcp::access::list_iam_tokens(context.auth, context.state, args)
                .await
            {
                Ok(payload) => {
                    let summary = format!("Listed {} API token(s).", payload.items.len());
                    ok_tool_result(&summary, json!(payload))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}

async fn revoke_iam_token(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpRevokeIamTokenRequest>(arguments.clone()) {
        Ok(args) => {
            match crate::services::mcp::access::revoke_iam_token(context.auth, context.state, args)
                .await
            {
                Ok(payload) => {
                    let summary = format!(
                        "API token {} revoked; status {}.",
                        payload.principal_id, payload.status
                    );
                    ok_tool_result(&summary, json!(payload))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}

async fn get_principal_access(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpGetPrincipalAccessRequest>(arguments.clone()) {
        Ok(args) => {
            let principal_id = args.principal_id;
            match crate::services::mcp::access::get_principal_access(
                context.auth,
                context.state,
                args,
            )
            .await
            {
                Ok(payload) => {
                    let summary = format!(
                        "Loaded access for principal {principal_id} with {} grant(s).",
                        payload.grants.len()
                    );
                    ok_tool_result(&summary, json!(payload))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}

async fn mint_iam_token(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpMintIamTokenRequest>(arguments.clone()) {
        Ok(args) => {
            match crate::services::mcp::access::mint_iam_token(context.auth, context.state, args)
                .await
            {
                Ok(payload) => {
                    // The summary repeats the one-shot nature of the plaintext:
                    // it is not recoverable once this response is gone.
                    let summary = format!(
                        "Minted API token {}; copy the plaintext now, it is returned once.",
                        payload.api_token.principal_id
                    );
                    ok_tool_result(&summary, json!(payload))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}

async fn update_iam_token(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpUpdateIamTokenRequest>(arguments.clone()) {
        Ok(args) => {
            match crate::services::mcp::access::update_iam_token(context.auth, context.state, args)
                .await
            {
                Ok(payload) => {
                    let summary = format!(
                        "API token {} updated; status {}.",
                        payload.principal_id, payload.status
                    );
                    ok_tool_result(&summary, json!(payload))
                }
                Err(error) => tool_error_result(error),
            }
        }
        Err(error) => tool_error_result(error),
    }
}
