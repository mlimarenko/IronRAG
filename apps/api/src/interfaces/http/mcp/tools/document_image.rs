//! MCP tool `view_document_image` — return the original image bytes
//! backing an image-attachment document so a vision-capable agent can
//! see the screenshot/diagram directly instead of relying on the OCR
//! stub text.
//!
//! ## Canonical storage path discovered
//!
//! Image bytes are stored at the *revision* level, not per chunk. The
//! ingest pipeline writes the original source file (PNG/JPEG/etc.) into
//! the content storage backend (filesystem or S3) keyed by
//! `content_revision.storage_key`; the revision's `mime_type` carries
//! the `image/*` discriminator. There is no per-chunk image blob —
//! every chunk on an image-mime revision conceptually backs onto the
//! same source bytes.
//!
//! Read path: `state.content_storage.read_revision_source(storage_key)`
//! (same trait used by `interfaces::http::content::source_download` and
//! by the vision describer in `services::mcp::access::documents`).
//!
//! ## Vision-capability gating
//!
//! Visibility (and per-call enforcement) checks the active `Agent`
//! binding's resolved model row: a model is vision-capable iff
//! `ai_model_catalog.modality_kind == "multimodal"` (the canonical
//! `ai_model_modality_kind` enum value, mirrored in Rust as
//! `AiModelCatalogRow.modality_kind: String`). No model-name hardcoding.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_MCP_MEMORY_READ, load_content_document_and_authorize},
        mcp::{
            McpToolDescriptor, McpToolResult,
            audit::{record_canonical_mcp_audit, record_error_audit, record_success_audit},
            ok_tool_result, parse_tool_args, tool_error_result,
        },
        router_support::ApiError,
    },
    mcp_types::{McpAuditActionKind, McpAuditScope},
};

use super::ToolCallContext;

pub(crate) const VIEW_DOCUMENT_IMAGE_TOOL_NAME: &str = "view_document_image";

/// Default and hard upper bound for the returned base64 payload. The
/// underlying decoded image must not exceed these caps so the LLM
/// tool-call response stays inside the provider's response budget.
pub(crate) const DEFAULT_MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const HARD_MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ViewDocumentImageArgs {
    pub document_id: Uuid,
    /// Optional citation handle: if present, the chunk must belong to
    /// the same document. The chunk axis is currently informational —
    /// image bytes live at revision granularity.
    #[serde(default)]
    pub chunk_id: Option<Uuid>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

pub(crate) fn descriptor(name: &str) -> Option<McpToolDescriptor> {
    if name != VIEW_DOCUMENT_IMAGE_TOOL_NAME {
        return None;
    }
    Some(McpToolDescriptor {
        name: VIEW_DOCUMENT_IMAGE_TOOL_NAME,
        description: "Fetch the original image (PNG/JPEG/etc.) backing an image-attachment document so a vision-capable model can see the screenshot or diagram directly, beyond the OCR text excerpt. Only surfaced when the active assistant Agent binding points at a model with multimodal/vision capability. Pass `documentId` from a prior `search_documents`/`grounded_answer` hit; optionally pass `chunkId` as a citation handle (validated to belong to the same document). The response carries `mediaType`, `byteSize`, base64-encoded `data`, and an `imageUrl` data: URL suitable for multimodal chat-completion payloads. Caps payload via `maxBytes` (default 4 MiB, hard ceiling 8 MiB).",
        input_schema: json!({
            "type": "object",
            "required": ["documentId"],
            "properties": {
                "documentId": {
                    "type": "string",
                    "format": "uuid",
                    "description": "Document UUID whose active revision must be an image attachment (mimeType image/*)."
                },
                "chunkId": {
                    "type": "string",
                    "format": "uuid",
                    "description": "Optional chunk UUID used as a citation handle. The chunk must belong to the same document."
                },
                "maxBytes": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional cap on the returned image payload size. Defaults to 4 MiB; clamped to the 8 MiB hard ceiling."
                }
            }
        }),
    })
}

pub(crate) async fn call_tool(
    name: &str,
    context: ToolCallContext<'_>,
    arguments: &Value,
) -> Option<McpToolResult> {
    if name != VIEW_DOCUMENT_IMAGE_TOOL_NAME {
        return None;
    }
    Some(view_document_image(context, arguments).await)
}

async fn view_document_image(context: ToolCallContext<'_>, arguments: &Value) -> McpToolResult {
    let args = match parse_tool_args::<ViewDocumentImageArgs>(arguments.clone()) {
        Ok(args) => args,
        Err(error) => {
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::ViewDocumentImage,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: None,
                },
                &error,
                json!({ "tool": VIEW_DOCUMENT_IMAGE_TOOL_NAME }),
            )
            .await;
            return tool_error_result(error);
        }
    };

    match view_document_image_inner(context, &args).await {
        Ok((payload, scope)) => {
            record_canonical_mcp_audit(
                context.state,
                context.auth,
                context.request_id,
                "agent.memory.view_document_image",
                "succeeded",
                Some("MCP document image read completed".to_string()),
                Some(format!(
                    "principal {} fetched image bytes for document {} via MCP",
                    context.auth.principal_id, args.document_id
                )),
                Vec::new(),
            )
            .await;
            record_success_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::ViewDocumentImage,
                scope,
                json!({
                    "tool": VIEW_DOCUMENT_IMAGE_TOOL_NAME,
                    "documentId": args.document_id,
                }),
            )
            .await;
            ok_tool_result("Document image fetched.", payload)
        }
        Err(error) => {
            record_error_audit(
                context.auth,
                context.state,
                context.request_id,
                McpAuditActionKind::ViewDocumentImage,
                McpAuditScope {
                    workspace_id: context.auth.workspace_id,
                    library_id: None,
                    document_id: Some(args.document_id),
                },
                &error,
                json!({ "tool": VIEW_DOCUMENT_IMAGE_TOOL_NAME }),
            )
            .await;
            tool_error_result(error)
        }
    }
}

async fn view_document_image_inner(
    context: ToolCallContext<'_>,
    args: &ViewDocumentImageArgs,
) -> Result<(Value, McpAuditScope), ApiError> {
    let document = load_content_document_and_authorize(
        context.auth,
        context.state,
        args.document_id,
        POLICY_MCP_MEMORY_READ,
    )
    .await?;

    // Hard-enforce vision capability per call. The listing predicate is
    // a coarse capability flag; per-call enforcement is the source of
    // truth because Agent binding modality is per-library.
    if !agent_binding_supports_vision(context.state, document.library_id).await? {
        return Err(ApiError::forbidden(
            "view_document_image requires the active Agent binding for this library to point at a \
             vision-capable (multimodal) model",
        ));
    }

    let summary = context
        .state
        .canonical_services
        .content
        .get_document(context.state, args.document_id)
        .await?;
    let revision = summary
        .active_revision
        .as_ref()
        .ok_or_else(|| ApiError::resource_not_found("revision", args.document_id))?;

    let mime_type = revision.mime_type.clone();
    if !mime_type.trim().to_ascii_lowercase().starts_with("image/") {
        return Err(ApiError::resource_not_found("image_attachment", args.document_id));
    }

    // Optional chunk citation: must belong to this document.
    if let Some(chunk_id) = args.chunk_id {
        let chunk = context
            .state
            .arango_document_store
            .get_chunk(chunk_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("chunk", chunk_id))?;
        if chunk.document_id != args.document_id {
            return Err(ApiError::BadRequest(format!(
                "chunk {chunk_id} does not belong to document {}",
                args.document_id
            )));
        }
    }

    let storage_key =
        revision.storage_key.as_deref().filter(|value| !value.trim().is_empty()).ok_or_else(
            || ApiError::BadRequest("document revision has no stored image source".to_string()),
        )?;

    let bytes = context
        .state
        .content_storage
        .read_revision_source(storage_key)
        .await
        .map_err(ApiError::from)?;

    let byte_size = bytes.len();
    let max_bytes =
        args.max_bytes.unwrap_or(DEFAULT_MAX_IMAGE_BYTES).clamp(1, HARD_MAX_IMAGE_BYTES);
    if byte_size > max_bytes {
        return Err(ApiError::BadRequest(format!(
            "image payload {byte_size} bytes exceeds requested cap {max_bytes} bytes"
        )));
    }

    let encoded = BASE64_STANDARD.encode(&bytes);
    let image_url = format!("data:{mime_type};base64,{encoded}");
    let source_text = if revision.title.as_deref().map(str::trim).unwrap_or("").is_empty() {
        None
    } else {
        revision.title.clone()
    };

    let payload = json!({
        "documentId": args.document_id,
        "revisionId": revision.id,
        "libraryId": document.library_id,
        "workspaceId": document.workspace_id,
        "chunkId": args.chunk_id,
        "mediaType": mime_type,
        "byteSize": byte_size,
        "data": encoded,
        "imageUrl": image_url,
        "caption": source_text,
    });

    Ok((
        payload,
        McpAuditScope {
            workspace_id: Some(document.workspace_id),
            library_id: Some(document.library_id),
            document_id: Some(document.id),
        },
    ))
}

/// Whether the active Agent binding for `library_id` resolves to a
/// model whose `modality_kind` is `multimodal`. Used for both listing
/// visibility and per-call enforcement.
pub(crate) async fn agent_binding_supports_vision(
    state: &AppState,
    library_id: Uuid,
) -> Result<bool, ApiError> {
    let Some(binding) = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::Agent)
        .await?
    else {
        return Ok(false);
    };
    let model = state
        .canonical_services
        .ai_catalog
        .get_model_catalog(state, binding.model_catalog_id)
        .await?;
    Ok(model.modality_kind == "multimodal")
}

/// Whether ANY accessible library has an Agent binding pointing at a
/// vision-capable model. Used by the async list-tools site to set the
/// `agent_vision_available` flag passed into the sync visibility
/// predicate.
pub(crate) async fn any_agent_binding_supports_vision(
    auth: &AuthContext,
    state: &AppState,
) -> bool {
    let Ok((_, libraries)) = crate::services::mcp::access::visible_catalog(auth, state).await
    else {
        return false;
    };
    for library in libraries {
        if agent_binding_supports_vision(state, library.library_id).await.unwrap_or(false) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_advertises_canonical_input_schema() {
        let descriptor = descriptor(VIEW_DOCUMENT_IMAGE_TOOL_NAME).expect("descriptor must exist");
        assert_eq!(descriptor.name, "view_document_image");
        let schema = &descriptor.input_schema;
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().expect("required must be an array");
        assert!(required.iter().any(|value| value == "documentId"));
        let properties = schema["properties"].as_object().expect("properties");
        assert!(properties.contains_key("documentId"));
        assert!(properties.contains_key("chunkId"));
        assert!(properties.contains_key("maxBytes"));
    }

    #[test]
    fn descriptor_only_matches_canonical_name() {
        assert!(descriptor("view_document_image").is_some());
        assert!(descriptor("read_document").is_none());
        assert!(descriptor("").is_none());
    }

    #[test]
    fn args_reject_unknown_fields() {
        let raw = serde_json::json!({
            "documentId": "00000000-0000-0000-0000-000000000000",
            "extraneous": true,
        });
        let parsed: Result<ViewDocumentImageArgs, _> = serde_json::from_value(raw);
        assert!(parsed.is_err(), "unknown fields must be rejected to keep the tool surface tight");
    }

    #[test]
    fn args_accept_optional_chunk_and_max_bytes() {
        let raw = serde_json::json!({
            "documentId": "11111111-1111-1111-1111-111111111111",
            "chunkId": "22222222-2222-2222-2222-222222222222",
            "maxBytes": 65536,
        });
        let parsed: ViewDocumentImageArgs = serde_json::from_value(raw).expect("parse");
        assert_eq!(parsed.max_bytes, Some(65536));
        assert!(parsed.chunk_id.is_some());
    }

    #[test]
    fn max_bytes_clamp_to_hard_ceiling() {
        // The handler clamps to [1, HARD_MAX_IMAGE_BYTES]; verify the
        // upper-bound branch by exercising the same clamp expression.
        let requested = HARD_MAX_IMAGE_BYTES + 1;
        let clamped = requested.clamp(1, HARD_MAX_IMAGE_BYTES);
        assert_eq!(clamped, HARD_MAX_IMAGE_BYTES);
    }
}
