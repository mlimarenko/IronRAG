//! MCP tool `view_document_image` — return the original image bytes
//! backing an image-attachment document so a vision-capable agent can
//! see the screenshot/diagram directly instead of relying on the OCR
//! stub text.
//!
//! ## Content storage path
//!
//! Image bytes are stored at the *revision* level, not per chunk. The
//! ingest pipeline writes the original source file (PNG/JPEG/etc.) into
//! the content storage backend (filesystem or S3) keyed by
//! `content_revision.storage_key`; the revision's `mime_type` carries
//! the `image/*` discriminator. There is no per-chunk image blob —
//! every chunk on an image-mime revision conceptually backs onto the
//! same source bytes.
//!
//! Read path: `state.content_storage.read_revision_source_bounded(storage_key, max_bytes)`
//! (same trait used by `interfaces::http::content::source_download` and
//! by the vision describer in `services::mcp::access::documents`).
//!
//! ## Capability gating
//!
//! Visibility and per-call enforcement require an active `Agent` binding whose
//! model is explicitly related to the `extract_text` purpose. This normalized
//! model-purpose relation is the sole eligibility authority; model metadata is
//! observational and never grants or revokes tool access.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    infra::repositories::ai_repository,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_MCP_MEMORY_READ, load_content_document_and_authorize},
        mcp::{
            McpToolDescriptor, McpToolResult,
            audit::{self, record_mcp_audit},
            ok_tool_result, parse_tool_args, tool_error_result,
        },
        router_support::ApiError,
    },
    services::content::storage::BoundedRevisionSource,
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
        description: "Fetch the original image (PNG/JPEG/etc.) backing an image-attachment document so the configured assistant can inspect the screenshot or diagram directly, beyond the OCR text excerpt. Only surfaced when the library has an active assistant Agent binding whose model is explicitly eligible for document extraction. Pass `documentId` from a prior `search_documents`/`grounded_answer` hit; optionally pass `chunkId` as a citation handle (validated to belong to the same document). The response carries `mediaType`, `byteSize`, base64-encoded `data`, and an `imageUrl` data: URL suitable for image-capable chat-completion payloads. Caps payload via `maxBytes` (default 4 MiB, hard ceiling 8 MiB).",
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
            return tool_error_result(error);
        }
    };

    match view_document_image_inner(context, &args).await {
        Ok(payload) => {
            record_mcp_audit(
                context.state,
                context.auth,
                context.request_id,
                audit::McpAuditEvent {
                    action_kind: "agent.memory.view_document_image",
                    result_kind: "succeeded",
                    redacted_message: Some("MCP document image read completed".to_string()),
                    internal_message: Some(format!(
                        "principal {} fetched image bytes for document {} via MCP",
                        context.auth.principal_id, args.document_id
                    )),
                    subjects: Vec::new(),
                },
            )
            .await;
            ok_tool_result("Document image fetched.", payload)
        }
        Err(error) => tool_error_result(error),
    }
}

async fn view_document_image_inner(
    context: ToolCallContext<'_>,
    args: &ViewDocumentImageArgs,
) -> Result<Value, ApiError> {
    let document = load_content_document_and_authorize(
        context.auth,
        context.state,
        args.document_id,
        POLICY_MCP_MEMORY_READ,
    )
    .await?;

    // Enforce the same typed Agent-purpose eligibility used by tool listing.
    if !agent_binding_supports_vision(context.state, document.library_id).await? {
        return Err(ApiError::forbidden(
            "view_document_image requires an active Agent model eligible for extract_text",
        ));
    }

    let revision_id =
        crate::infra::repositories::content_fingerprint_repository::get_current_revision_id(
            &context.state.persistence.postgres,
            args.document_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "image visibility lookup failed"))?
        .ok_or_else(|| ApiError::resource_not_found("image_attachment", args.document_id))?;
    let revision = context
        .state
        .application_services
        .content
        .list_revisions(context.state, args.document_id)
        .await?
        .into_iter()
        .find(|revision| revision.id == revision_id)
        .ok_or_else(|| ApiError::resource_not_found("revision", args.document_id))?;

    let mime_type = revision.mime_type.clone();
    if !mime_type.trim().to_ascii_lowercase().starts_with("image/") {
        return Err(ApiError::resource_not_found("image_attachment", args.document_id));
    }

    // Optional chunk citation: must belong to this document.
    if let Some(chunk_id) = args.chunk_id {
        let chunk = context
            .state
            .document_store
            .get_chunk(chunk_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("chunk", chunk_id))?;
        if chunk.document_id != args.document_id || chunk.revision_id != revision_id {
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

    let max_bytes =
        args.max_bytes.unwrap_or(DEFAULT_MAX_IMAGE_BYTES).clamp(1, HARD_MAX_IMAGE_BYTES);
    let bytes = image_bytes_within_limit(
        context
            .state
            .content_storage
            .read_revision_source_bounded(storage_key, max_bytes)
            .await
            .map_err(ApiError::from)?,
        max_bytes,
    )?;
    let byte_size = bytes.len();

    let encoded = BASE64_STANDARD.encode(&bytes);
    let image_url = format!("data:{mime_type};base64,{encoded}");
    let source_text = if revision.title.as_deref().map_or("", str::trim).is_empty() {
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

    if !crate::infra::repositories::content_fingerprint_repository::revision_is_current(
        &context.state.persistence.postgres,
        args.document_id,
        revision_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "image visibility lookup failed"))?
    {
        return Err(ApiError::resource_not_found("image_attachment", args.document_id));
    }

    Ok(payload)
}

fn image_bytes_within_limit(
    source: BoundedRevisionSource,
    max_bytes: usize,
) -> Result<Vec<u8>, ApiError> {
    match source {
        BoundedRevisionSource::Complete(bytes) => Ok(bytes),
        BoundedRevisionSource::LimitExceeded { byte_size } => Err(ApiError::BadRequest(format!(
            "image payload {byte_size} bytes exceeds requested cap {max_bytes} bytes"
        ))),
    }
}

/// Whether the active Agent model is explicitly eligible for document image
/// extraction. The normalized binding-purpose relation is the sole authority;
/// observed model metadata must not enable or disable capabilities.
pub(crate) async fn agent_binding_supports_vision(
    state: &AppState,
    library_id: Uuid,
) -> Result<bool, ApiError> {
    let Some(binding) = state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::Agent)
        .await?
    else {
        return Ok(false);
    };
    let model = state
        .application_services
        .ai_catalog
        .get_model_catalog(state, binding.model_catalog_id)
        .await?;
    Ok(model.allowed_binding_purposes.contains(&AiBindingPurpose::ExtractText))
}

/// Whether ANY accessible library has an Agent binding pointing at a
/// vision-capable model. Used by the async list-tools site to set the
/// `agent_vision_available` flag passed into the sync visibility
/// predicate.
pub(crate) async fn any_agent_binding_supports_vision(
    auth: &AuthContext,
    state: &AppState,
) -> Result<bool, ApiError> {
    let catalog = crate::services::mcp::access::visible_catalog_summary(auth, state).await?;
    any_library_agent_binding_supports_vision(state, &catalog.library_ids).await
}

/// Variant for callers that already loaded the authorization-scoped catalog.
/// Avoids repeating the same workspace/library queries during capability
/// preflight while preserving the exact visibility predicate used by
/// `tools/list`.
pub(crate) async fn any_library_agent_binding_supports_vision(
    state: &AppState,
    library_ids: &[Uuid],
) -> Result<bool, ApiError> {
    ai_repository::any_effective_binding_model_supports_purpose(
        &state.persistence.postgres,
        library_ids,
        AiBindingPurpose::Agent.as_str(),
        AiBindingPurpose::ExtractText.as_str(),
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_advertises_declared_input_schema() {
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
    fn descriptor_only_matches_declared_name() {
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
    fn args_default_optional_limits_and_citation_handle() {
        let raw = serde_json::json!({
            "documentId": "11111111-1111-1111-1111-111111111111",
        });

        let parsed: ViewDocumentImageArgs = serde_json::from_value(raw).expect("parse");

        assert_eq!(parsed.chunk_id, None);
        assert_eq!(parsed.max_bytes, None);
    }

    #[test]
    fn bounded_image_bytes_preserve_normal_payload() {
        let bytes = b"image bytes".to_vec();

        let result = image_bytes_within_limit(
            BoundedRevisionSource::Complete(bytes.clone()),
            DEFAULT_MAX_IMAGE_BYTES,
        )
        .expect("image within limit");

        assert_eq!(result, bytes);
    }

    #[test]
    fn bounded_image_bytes_preserve_over_limit_error_contract() {
        let result =
            image_bytes_within_limit(BoundedRevisionSource::LimitExceeded { byte_size: 12 }, 5);

        assert!(matches!(
            result,
            Err(ApiError::BadRequest(message))
                if message == "image payload 12 bytes exceeds requested cap 5 bytes"
        ));
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
