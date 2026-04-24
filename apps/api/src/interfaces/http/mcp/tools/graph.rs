use serde_json::{Value, json};

use crate::{
    interfaces::http::router_support::ApiError,
    mcp_types::{
        McpAuditActionKind, McpAuditScope, McpGetCommunitiesRequest, McpGetGraphTopologyRequest,
        McpListRelationsRequest, McpSearchEntitiesRequest,
    },
};

use super::super::{
    McpToolDescriptor, McpToolResult,
    audit::{record_canonical_mcp_audit, record_error_audit, record_success_audit},
    ok_tool_result, parse_tool_args, tool_error_result,
};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    match name {
        "search_entities" => Some(McpToolDescriptor {
            name: "search_entities",
            description: "Search knowledge graph entities by name or label within one library. Returns scored entity matches ordered by relevance.",
            input_schema: json!({
                "type": "object",
                "required": ["library", "query"],
                "properties": {
                    "library": {
                        "type": "string",
                        "description": "Target fully-qualified library ref."
                    },
                    "query": {
                        "type": "string",
                        "description": "Natural-language or keyword query to match against entity labels and summaries."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional hit limit. Defaults to 20."
                    }
                }
            }),
        }),
        "get_graph_topology" => Some(McpToolDescriptor {
            name: "get_graph_topology",
            description: "Get the knowledge graph topology for one library, including documents, entities, relations, and document-entity links. Results are truncated by default (200 entities, 500 relations); use limit to control the entity cap.",
            input_schema: json!({
                "type": "object",
                "required": ["library"],
                "properties": {
                    "library": {
                        "type": "string",
                        "description": "Target fully-qualified library ref."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of entities to return. Relations are capped at 2.5x the entity limit. Defaults to 200."
                    }
                }
            }),
        }),
        "list_relations" => Some(McpToolDescriptor {
            name: "list_relations",
            description: "Returns relations from the knowledge graph, ordered by support count.",
            input_schema: json!({
                "type": "object",
                "required": ["library"],
                "properties": {
                    "library": {
                        "type": "string",
                        "description": "Target fully-qualified library ref."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional limit on number of relations returned. Defaults to 100."
                    }
                }
            }),
        }),
        "get_communities" => Some(McpToolDescriptor {
            name: "get_communities",
            description: "Lists detected communities in the knowledge graph with their summaries, top entities, and sizes.",
            input_schema: json!({
                "type": "object",
                "required": ["library"],
                "properties": {
                    "library": {
                        "type": "string",
                        "description": "Target fully-qualified library ref."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional limit on number of communities returned. Defaults to 50."
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
        "search_entities" => search_entities(context, arguments).await,
        "get_graph_topology" => get_graph_topology(context, arguments).await,
        "list_relations" => list_relations(context, arguments).await,
        "get_communities" => get_communities(context, arguments).await,
        _ => return None,
    };
    Some(result)
}

async fn search_entities(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpSearchEntitiesRequest>(arguments.clone()) {
        Ok(args) => {
            let limit = args.limit.unwrap_or(20).clamp(1, 200);
            match crate::services::mcp::access::authorize_library_for_mcp(
                context.auth,
                context.state,
                &args.library,
            )
            .await
            {
                Ok(library) => match crate::services::mcp::access::search_entities(
                    context.state,
                    library.id,
                    &args.query,
                    limit,
                )
                .await
                {
                    Ok(entities) => {
                        record_canonical_mcp_audit(
                            context.state,
                            context.auth,
                            context.request_id,
                            "agent.graph.search_entities",
                            "succeeded",
                            Some(format!("entity search returned {} hit(s)", entities.len())),
                            Some(format!(
                                "principal {} searched entities in library {}",
                                context.auth.principal_id, library.id
                            )),
                            Vec::new(),
                        )
                        .await;
                        record_success_audit(
                            context.auth,
                            context.state,
                            context.request_id,
                            McpAuditActionKind::SearchEntities,
                            McpAuditScope {
                                workspace_id: Some(library.workspace_id),
                                library_id: Some(library.id),
                                document_id: None,
                            },
                            json!({
                                "tool": "search_entities",
                                "hitCount": entities.len(),
                            }),
                        )
                        .await;
                        ok_tool_result("Entity search completed.", json!({ "entities": entities }))
                    }
                    Err(_) => tool_error_result(ApiError::Internal),
                },
                Err(error) => {
                    record_error_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::SearchEntities,
                        McpAuditScope {
                            workspace_id: context.auth.workspace_id,
                            library_id: None,
                            document_id: None,
                        },
                        &error,
                        json!({ "tool": "search_entities" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            }
        }
        Err(error) => {
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::SearchEntities,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "search_entities" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn get_graph_topology(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpGetGraphTopologyRequest>(arguments.clone()) {
        Ok(args) => {
            match crate::services::mcp::access::authorize_library_for_mcp(
                context.auth,
                context.state,
                &args.library,
            )
            .await
            {
                Ok(library) => match crate::services::mcp::access::get_graph_topology(
                    context.state,
                    library.id,
                    args.limit,
                )
                .await
                {
                    Ok(payload) => {
                        record_canonical_mcp_audit(
                            context.state,
                            context.auth,
                            context.request_id,
                            "agent.graph.topology",
                            "succeeded",
                            Some("graph topology loaded".to_string()),
                            Some(format!(
                                "principal {} loaded graph topology for library {}",
                                context.auth.principal_id, library.id
                            )),
                            Vec::new(),
                        )
                        .await;
                        record_success_audit(
                            context.auth,
                            context.state,
                            context.request_id,
                            McpAuditActionKind::GetGraphTopology,
                            McpAuditScope {
                                workspace_id: Some(library.workspace_id),
                                library_id: Some(library.id),
                                document_id: None,
                            },
                            json!({ "tool": "get_graph_topology" }),
                        )
                        .await;
                        ok_tool_result("Graph topology loaded.", payload)
                    }
                    Err(error) => {
                        record_error_audit(
                            context.auth,
                            context.state,
                            context.request_id,
                            McpAuditActionKind::GetGraphTopology,
                            McpAuditScope {
                                workspace_id: Some(library.workspace_id),
                                library_id: Some(library.id),
                                document_id: None,
                            },
                            &error,
                            json!({ "tool": "get_graph_topology" }),
                        )
                        .await;
                        tool_error_result(error)
                    }
                },
                Err(error) => {
                    record_error_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::GetGraphTopology,
                        McpAuditScope {
                            workspace_id: context.auth.workspace_id,
                            library_id: None,
                            document_id: None,
                        },
                        &error,
                        json!({ "tool": "get_graph_topology" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            }
        }
        Err(error) => {
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::GetGraphTopology,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "get_graph_topology" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn list_relations(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpListRelationsRequest>(arguments.clone()) {
        Ok(args) => {
            let limit = args.limit.unwrap_or(100).clamp(1, 500);
            match crate::services::mcp::access::authorize_library_for_mcp(
                context.auth,
                context.state,
                &args.library,
            )
            .await
            {
                Ok(library) => match crate::services::mcp::access::list_relations(
                    context.state,
                    library.id,
                    limit,
                )
                .await
                {
                    Ok(payload) => {
                        record_canonical_mcp_audit(
                            context.state,
                            context.auth,
                            context.request_id,
                            "agent.graph.list_relations",
                            "succeeded",
                            Some(format!("listed {} relation(s)", payload.len())),
                            Some(format!(
                                "principal {} listed relations for library {}",
                                context.auth.principal_id, library.id
                            )),
                            Vec::new(),
                        )
                        .await;
                        record_success_audit(
                            context.auth,
                            context.state,
                            context.request_id,
                            McpAuditActionKind::ListRelations,
                            McpAuditScope {
                                workspace_id: Some(library.workspace_id),
                                library_id: Some(library.id),
                                document_id: None,
                            },
                            json!({
                                "tool": "list_relations",
                                "relationCount": payload.len(),
                            }),
                        )
                        .await;
                        ok_tool_result("Relations loaded.", json!({ "relations": payload }))
                    }
                    Err(error) => {
                        record_error_audit(
                            context.auth,
                            context.state,
                            context.request_id,
                            McpAuditActionKind::ListRelations,
                            McpAuditScope {
                                workspace_id: Some(library.workspace_id),
                                library_id: Some(library.id),
                                document_id: None,
                            },
                            &error,
                            json!({ "tool": "list_relations" }),
                        )
                        .await;
                        tool_error_result(error)
                    }
                },
                Err(error) => {
                    record_error_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::ListRelations,
                        McpAuditScope {
                            workspace_id: context.auth.workspace_id,
                            library_id: None,
                            document_id: None,
                        },
                        &error,
                        json!({ "tool": "list_relations" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            }
        }
        Err(error) => {
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::ListRelations,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "list_relations" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn get_communities(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpGetCommunitiesRequest>(arguments.clone()) {
        Ok(args) => {
            let limit = args.limit.unwrap_or(50).clamp(1, 500);
            match crate::services::mcp::access::authorize_library_for_mcp(
                context.auth,
                context.state,
                &args.library,
            )
            .await
            {
                Ok(library) => match crate::services::mcp::access::get_communities(
                    context.state,
                    library.id,
                    limit,
                )
                .await
                {
                    Ok(payload) => {
                        record_success_audit(
                            context.auth,
                            context.state,
                            context.request_id,
                            McpAuditActionKind::GetCommunities,
                            McpAuditScope {
                                workspace_id: Some(library.workspace_id),
                                library_id: Some(library.id),
                                document_id: None,
                            },
                            json!({
                                "tool": "get_communities",
                                "communityCount": payload.len(),
                            }),
                        )
                        .await;
                        ok_tool_result("Communities loaded.", json!({ "communities": payload }))
                    }
                    Err(error) => {
                        record_error_audit(
                            context.auth,
                            context.state,
                            context.request_id,
                            McpAuditActionKind::GetCommunities,
                            McpAuditScope {
                                workspace_id: Some(library.workspace_id),
                                library_id: Some(library.id),
                                document_id: None,
                            },
                            &error,
                            json!({ "tool": "get_communities" }),
                        )
                        .await;
                        tool_error_result(error)
                    }
                },
                Err(error) => {
                    record_error_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::GetCommunities,
                        McpAuditScope {
                            workspace_id: context.auth.workspace_id,
                            library_id: None,
                            document_id: None,
                        },
                        &error,
                        json!({ "tool": "get_communities" }),
                    )
                    .await;
                    tool_error_result(error)
                }
            }
        }
        Err(error) => {
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::GetCommunities,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "get_communities" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}
