use serde_json::{Value, json};

use crate::mcp_types::{
    McpAuditActionKind, McpAuditScope, McpDeleteDocumentRequest, McpGetMutationStatusRequest,
    McpListDocumentsRequest, McpReadDocumentRequest, McpSearchDocumentsRequest,
    McpUpdateDocumentRequest, McpUploadDocumentsRequest,
};

use super::super::{
    McpToolDescriptor, McpToolResult,
    audit::{
        build_mcp_mutation_subjects, build_mcp_search_subjects, mutation_scope_from_receipts,
        record_canonical_mcp_audit, record_error_audit, record_success_audit,
        search_scope_from_request, search_scope_from_response,
    },
    ok_tool_result, parse_tool_args, tool_error_result,
};
use super::ToolCallContext;

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    match name {
        "search_documents" => Some(McpToolDescriptor {
            name: "search_documents",
            description: "Search authorized library memory and return document-level candidates. Agents should usually follow relevant hits with read_document in full mode before answering.",
            input_schema: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural-language question or keyword query to match against IronRAG memory."
                    },
                    "libraryIds": {
                        "type": "array",
                        "items": { "type": "string", "format": "uuid" },
                        "description": "Optional library UUID filter. Narrowing to the most likely library reduces noise."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional hit limit. Small values such as 3-10 keep the candidate set focused."
                    },
                    "includeReferences": {
                        "type": "boolean",
                        "description": "Include chunk/entity/relation/evidence reference arrays (default: false to reduce response size)."
                    }
                }
            }),
        }),
        "read_document" => Some(McpToolDescriptor {
            name: "read_document",
            description: "Read one document in full or as an excerpt. Use this after search_documents or when you already know the documentId; full mode is the safe default for fact extraction. For image-backed documents the response can include sourceAccess and a visualDescription derived from the original source image, not just extracted OCR text.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "documentId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Document UUID from search_documents, upload_documents, or another trusted source."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["full", "excerpt"],
                        "description": "Prefer full for grounded answers; excerpt is useful for incremental reads."
                    },
                    "startOffset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Start character offset."
                    },
                    "length": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional character count for excerpt reads."
                    },
                    "continuationToken": {
                        "type": "string",
                        "description": "Opaque token returned by a previous read when hasMore is true."
                    },
                    "includeReferences": {
                        "type": "boolean",
                        "description": "Include chunk/entity/relation/evidence reference arrays (default: false to reduce response size)."
                    }
                }
            }),
        }),
        "list_documents" => Some(McpToolDescriptor {
            name: "list_documents",
            description: "List documents in a knowledge library. Optionally filter by processing status.",
            input_schema: json!({
                "type": "object",
                "required": ["libraryId"],
                "properties": {
                    "libraryId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Target library UUID."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 200,
                        "description": "Maximum number of documents to return. Defaults to 50."
                    },
                    "statusFilter": {
                        "type": "string",
                        "enum": ["processing", "readable", "failed", "graph_ready"],
                        "description": "Optional readiness status filter."
                    }
                }
            }),
        }),
        "upload_documents" => Some(McpToolDescriptor {
            name: "upload_documents",
            description: "Create one or more new logical documents in an authorized library. Use body for short agent-authored text and contentBase64 for files; always poll get_mutation_status before treating ingestion as complete.",
            input_schema: json!({
                "type": "object",
                "required": ["libraryId", "documents"],
                "properties": {
                    "libraryId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Target library UUID from list_libraries or create_library."
                    },
                    "idempotencyKey": {
                        "type": "string",
                        "description": "Caller-chosen dedupe key."
                    },
                    "documents": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "anyOf": [
                                { "required": ["contentBase64"] },
                                { "required": ["body"] }
                            ],
                            "properties": {
                                "fileName": {
                                    "type": "string",
                                    "description": "Original file name. Optional for inline body uploads; autogenerated if omitted."
                                },
                                "contentBase64": {
                                    "type": "string",
                                    "description": "Base64-encoded file payload for binary/file uploads."
                                },
                                "body": {
                                    "type": "string",
                                    "description": "Inline UTF-8 text body for agent-authored notes and snippets. Target libraries still need the required active AI bindings for extraction and search."
                                },
                                "sourceType": {
                                    "type": "string",
                                    "description": "Optional hint: use inline for text body uploads or file for base64 payload uploads."
                                },
                                "sourceUri": {
                                    "type": "string",
                                    "description": "Optional logical source URI used to derive a default file name for inline uploads."
                                },
                                "mimeType": {
                                    "type": "string",
                                    "description": "Optional MIME type."
                                },
                                "title": {
                                    "type": "string",
                                    "description": "Optional display title shown in search and read responses."
                                }
                            }
                        }
                    }
                }
            }),
        }),
        "update_document" => Some(McpToolDescriptor {
            name: "update_document",
            description: "Append to or replace one logical document while preserving document identity. The call returns mutation receipts; poll get_mutation_status until a terminal state before depending on the new revision.",
            input_schema: json!({
                "type": "object",
                "required": ["libraryId", "documentId", "operationKind"],
                "allOf": [
                    {
                        "if": { "properties": { "operationKind": { "const": "append" } } },
                        "then": { "required": ["appendedText"] }
                    },
                    {
                        "if": { "properties": { "operationKind": { "const": "replace" } } },
                        "then": { "required": ["replacementFileName", "replacementContentBase64"] }
                    }
                ],
                "properties": {
                    "libraryId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Library UUID that owns the target document."
                    },
                    "documentId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Target document UUID from search_documents, read_document, or a prior mutation receipt."
                    },
                    "operationKind": {
                        "type": "string",
                        "enum": ["append", "replace"],
                        "description": "Mutation kind."
                    },
                    "idempotencyKey": {
                        "type": "string",
                        "description": "Caller-chosen dedupe key."
                    },
                    "appendedText": {
                        "type": "string",
                        "description": "Required for append operations. Good for small incremental notes."
                    },
                    "replacementFileName": {
                        "type": "string",
                        "description": "Required for replace operations."
                    },
                    "replacementContentBase64": {
                        "type": "string",
                        "description": "Required for replace operations."
                    },
                    "replacementMimeType": {
                        "type": "string",
                        "description": "Optional for replace."
                    }
                }
            }),
        }),
        "delete_document" => Some(McpToolDescriptor {
            name: "delete_document",
            description: "Delete a document from its library. This removes the document, its revisions, chunks, and graph contributions.",
            input_schema: json!({
                "type": "object",
                "required": ["documentId"],
                "properties": {
                    "documentId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Document UUID to delete."
                    }
                }
            }),
        }),
        "get_mutation_status" => Some(McpToolDescriptor {
            name: "get_mutation_status",
            description: "Check the lifecycle of a previously accepted upload_documents or update_document receipt. Use this to confirm backend completion; read/search visibility can arrive slightly before or after the terminal receipt state.",
            input_schema: json!({
                "type": "object",
                "required": ["receiptId"],
                "properties": {
                    "receiptId": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Mutation receipt UUID."
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
        "search_documents" => search_documents(context, arguments).await,
        "read_document" => read_document(context, arguments).await,
        "list_documents" => list_documents(context, arguments).await,
        "upload_documents" => upload_documents(context, arguments).await,
        "update_document" => update_document(context, arguments).await,
        "delete_document" => delete_document(context, arguments).await,
        "get_mutation_status" => get_mutation_status(context, arguments).await,
        _ => return None,
    };
    Some(result)
}

async fn search_documents(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpSearchDocumentsRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::access::search_documents(
            context.auth,
            context.state,
            args.clone(),
        )
        .await
        {
            Ok(payload) => {
                record_canonical_mcp_audit(
                    context.state,
                    context.auth,
                    context.request_id,
                    "agent.memory.search",
                    "succeeded",
                    Some(format!(
                        "completed MCP document search with {} hit(s)",
                        payload.hits.len()
                    )),
                    Some(format!(
                        "principal {} completed MCP document search across {} library scope(s)",
                        context.auth.principal_id,
                        payload.library_ids.len()
                    )),
                    build_mcp_search_subjects(context.state, &payload),
                )
                .await;
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::SearchDocuments,
                    search_scope_from_response(context.auth, &payload),
                    json!({
                        "tool": "search_documents",
                        "query": payload.query,
                        "hitCount": payload.hits.len(),
                    }),
                )
                .await;
                ok_tool_result("Document memory search completed.", json!(payload))
            }
            Err(error) => {
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::SearchDocuments,
                    search_scope_from_request(context.auth, args.library_ids.as_deref()),
                    &error,
                    json!({
                        "tool": "search_documents",
                        "query": args.query,
                    }),
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
                McpAuditActionKind::SearchDocuments,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "search_documents" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn read_document(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpReadDocumentRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::access::read_document(
            context.auth,
            context.state,
            args.clone(),
        )
        .await
        {
            Ok(payload) => {
                record_canonical_mcp_audit(
                    context.state,
                    context.auth,
                    context.request_id,
                    "agent.memory.read",
                    "succeeded",
                    Some("MCP document read completed".to_string()),
                    Some(format!(
                        "principal {} read knowledge document {} via MCP",
                        context.auth.principal_id, payload.document_id
                    )),
                    vec![context.state.canonical_services.audit.knowledge_document_subject(
                        payload.document_id,
                        payload.workspace_id,
                        payload.library_id,
                    )],
                )
                .await;
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::ReadDocument,
                    McpAuditScope {
                        workspace_id: Some(payload.workspace_id),
                        library_id: Some(payload.library_id),
                        document_id: Some(payload.document_id),
                    },
                    json!({
                        "tool": "read_document",
                        "readMode": payload.read_mode,
                        "readabilityState": payload.readability_state,
                        "hasMore": payload.has_more,
                    }),
                )
                .await;
                ok_tool_result("Document read completed.", json!(payload))
            }
            Err(error) => {
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::ReadDocument,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: args.document_id,
                    },
                    &error,
                    json!({ "tool": "read_document" }),
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
                McpAuditActionKind::ReadDocument,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "read_document" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn list_documents(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpListDocumentsRequest>(arguments.clone()) {
        Ok(args) => {
            let library_id = args.library_id;
            let limit = args.limit.unwrap_or(50).clamp(1, 200);
            match crate::services::mcp::access::list_documents(
                context.auth,
                context.state,
                library_id,
                limit,
                args.status_filter.as_deref(),
            )
            .await
            {
                Ok(payload) => {
                    record_canonical_mcp_audit(
                        context.state,
                        context.auth,
                        context.request_id,
                        "agent.memory.list_documents",
                        "succeeded",
                        Some("listed library documents".to_string()),
                        Some(format!(
                            "principal {} listed documents for library {}",
                            context.auth.principal_id, library_id
                        )),
                        Vec::new(),
                    )
                    .await;
                    record_success_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::ListDocuments,
                        McpAuditScope {
                            workspace_id: context.auth.workspace_id,
                            library_id: Some(library_id),
                            document_id: None,
                        },
                        json!({ "tool": "list_documents" }),
                    )
                    .await;
                    ok_tool_result("Documents listed.", payload)
                }
                Err(error) => {
                    record_error_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::ListDocuments,
                        McpAuditScope {
                            workspace_id: context.auth.workspace_id,
                            library_id: Some(library_id),
                            document_id: None,
                        },
                        &error,
                        json!({ "tool": "list_documents" }),
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
                McpAuditActionKind::ListDocuments,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "list_documents" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn upload_documents(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpUploadDocumentsRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::mutations::upload_documents(
            context.auth,
            context.state,
            args.clone(),
        )
        .await
        {
            Ok(payload) => {
                let canonical_subjects = build_mcp_mutation_subjects(context.state, &payload).await;
                record_canonical_mcp_audit(
                    context.state,
                    context.auth,
                    context.request_id,
                    "agent.memory.upload",
                    "succeeded",
                    Some(format!("accepted {} MCP upload mutation(s)", payload.len())),
                    Some(format!(
                        "principal {} accepted {} MCP upload mutation(s) in library {}",
                        context.auth.principal_id,
                        payload.len(),
                        args.library_id
                    )),
                    canonical_subjects,
                )
                .await;
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::UploadDocuments,
                    mutation_scope_from_receipts(&payload).unwrap_or(McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: Some(args.library_id),
                        document_id: None,
                    }),
                    json!({
                        "tool": "upload_documents",
                        "receiptCount": payload.len(),
                    }),
                )
                .await;
                ok_tool_result("Document uploads accepted.", json!({ "receipts": payload }))
            }
            Err(error) => {
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::UploadDocuments,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: Some(args.library_id),
                        document_id: None,
                    },
                    &error,
                    json!({ "tool": "upload_documents" }),
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
                McpAuditActionKind::UploadDocuments,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "upload_documents" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn update_document(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpUpdateDocumentRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::mutations::update_document(
            context.auth,
            context.state,
            args.clone(),
        )
        .await
        {
            Ok(payload) => {
                let canonical_subjects =
                    build_mcp_mutation_subjects(context.state, std::slice::from_ref(&payload))
                        .await;
                record_canonical_mcp_audit(
                    context.state,
                    context.auth,
                    context.request_id,
                    "agent.memory.update",
                    "succeeded",
                    Some(format!("accepted MCP document {:?} mutation", payload.operation_kind)),
                    Some(format!(
                        "principal {} accepted MCP mutation {} for document {:?}",
                        context.auth.principal_id, payload.receipt_id, payload.document_id
                    )),
                    canonical_subjects,
                )
                .await;
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::UpdateDocument,
                    McpAuditScope {
                        workspace_id: Some(payload.workspace_id),
                        library_id: Some(payload.library_id),
                        document_id: payload.document_id,
                    },
                    json!({
                        "tool": "update_document",
                        "operationKind": payload.operation_kind,
                    }),
                )
                .await;
                ok_tool_result("Document mutation accepted.", json!(payload))
            }
            Err(error) => {
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::UpdateDocument,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: Some(args.library_id),
                        document_id: Some(args.document_id),
                    },
                    &error,
                    json!({ "tool": "update_document" }),
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
                McpAuditActionKind::UpdateDocument,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "update_document" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn delete_document(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpDeleteDocumentRequest>(arguments.clone()) {
        Ok(args) => {
            let document_id = args.document_id;
            match crate::services::mcp::access::delete_document(
                context.auth,
                context.state,
                document_id,
            )
            .await
            {
                Ok(payload) => {
                    record_canonical_mcp_audit(
                        context.state,
                        context.auth,
                        context.request_id,
                        "agent.memory.delete_document",
                        "succeeded",
                        Some(format!("deleted document {document_id}")),
                        Some(format!(
                            "principal {} deleted document {} via MCP",
                            context.auth.principal_id, document_id
                        )),
                        Vec::new(),
                    )
                    .await;
                    record_success_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::DeleteDocument,
                        McpAuditScope {
                            workspace_id: context.auth.workspace_id,
                            library_id: None,
                            document_id: Some(document_id),
                        },
                        json!({ "tool": "delete_document" }),
                    )
                    .await;
                    ok_tool_result("Document deletion accepted.", payload)
                }
                Err(error) => {
                    record_error_audit(
                        context.auth,
                        context.state,
                        context.request_id,
                        McpAuditActionKind::DeleteDocument,
                        McpAuditScope {
                            workspace_id: context.auth.workspace_id,
                            library_id: None,
                            document_id: Some(document_id),
                        },
                        &error,
                        json!({ "tool": "delete_document" }),
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
                McpAuditActionKind::DeleteDocument,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "delete_document" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn get_mutation_status(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    match parse_tool_args::<McpGetMutationStatusRequest>(arguments.clone()) {
        Ok(args) => match crate::services::mcp::mutations::get_mutation_status(
            context.auth,
            context.state,
            args,
        )
        .await
        {
            Ok(payload) => {
                record_success_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::GetMutationStatus,
                    McpAuditScope {
                        workspace_id: Some(payload.workspace_id),
                        library_id: Some(payload.library_id),
                        document_id: payload.document_id,
                    },
                    json!({
                        "tool": "get_mutation_status",
                        "status": payload.status,
                    }),
                )
                .await;
                ok_tool_result("Mutation status loaded.", json!(payload))
            }
            Err(error) => {
                record_error_audit(
                    context.auth,
                    context.state,
                    context.request_id,
                    McpAuditActionKind::GetMutationStatus,
                    McpAuditScope {
                        workspace_id: context.auth.workspace_id,
                        library_id: None,
                        document_id: None,
                    },
                    &error,
                    json!({ "tool": "get_mutation_status" }),
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
                McpAuditActionKind::GetMutationStatus,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": "get_mutation_status" }),
            )
            .await;
            tool_error_result(error)
        }
    }
}
