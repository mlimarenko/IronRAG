use super::*;

pub(crate) async fn read_document(
    auth: &AuthContext,
    state: &AppState,
    request: McpReadDocumentRequest,
) -> Result<McpReadDocumentResponse, ApiError> {
    auth.require_any_scope(POLICY_MCP_MEMORY_READ)?;
    let include_references = request.include_references.unwrap_or(false);
    let settings = &state.mcp_memory;
    let normalized = normalize_read_request(
        auth,
        &request,
        ReadWindowBounds {
            default_chars: settings.default_read_window_chars,
            max_chars: settings.max_read_window_chars,
        },
    )?;
    let mut state_view =
        resolve_document_state(auth, state, normalized.document_id, POLICY_MCP_MEMORY_READ).await?;
    if state_view.snapshot.suppressed || !state_view.snapshot.is_active() {
        return Err(ApiError::resource_not_found("document", normalized.document_id));
    }
    let latest_revision_id = state_view.latest_revision_id;
    if normalized.expected_revision_id.is_some()
        && normalized.expected_revision_id != latest_revision_id
    {
        return Err(ApiError::invalid_continuation_token(
            "continuation revision is no longer published",
        ));
    }
    let source_access = state_view.source_access.as_ref().map(map_source_access);
    let visual_description = load_source_visual_description(state, &state_view).await?;
    if !crate::infra::repositories::content_document_head_repository::document_read_snapshot_is_current(
            &state.persistence.postgres,
            &state_view.snapshot,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "document visibility lookup failed"))?
    {
        return Err(ApiError::resource_not_found("document", state_view.document_id));
    }
    let mime_type = state_view.mime_type.clone();
    let source_uri = state_view.source_uri.clone();

    if state_view.readability_state != McpReadabilityState::Readable {
        return Ok(McpReadDocumentResponse {
            document_id: state_view.document_id,
            document_title: state_view.document_title,
            library_id: state_view.library.id,
            workspace_id: state_view.library.workspace_id,
            latest_revision_id,
            read_mode: normalized.read_mode,
            readability_state: state_view.readability_state,
            readiness_kind: state_view.readiness_kind,
            graph_coverage_kind: state_view.graph_coverage_kind,
            status_reason: state_view.status_reason,
            mime_type,
            source_uri,
            source_access,
            visual_description,
            content: None,
            slice_start_offset: normalized.start_offset,
            slice_end_offset: normalized.start_offset,
            total_content_length: None,
            continuation_token: None,
            has_more: false,
            chunk_references: Vec::new(),
            technical_fact_references: Vec::new(),
            entity_references: Vec::new(),
            relation_references: Vec::new(),
            evidence_references: Vec::new(),
        });
    }

    let published_revision_id = latest_revision_id
        .ok_or_else(|| ApiError::resource_not_found("document", state_view.document_id))?;
    // Offsets identify the immutable canonical text. Visual descriptions have
    // their own response field and must not shift or extend that coordinate space.
    let CanonicalReadWindow {
        content: slice,
        start: slice_start_offset,
        end: slice_end_offset,
        total: total_content_length,
        has_more,
    } = canonical_read_window(
        state_view.content.as_deref().unwrap_or_default(),
        &normalized.read_mode,
        normalized.start_offset,
        normalized.window_chars,
    );
    let continuation_token = has_more.then(|| {
        encode_continuation_token(
            auth,
            McpReadCursor {
                document_id: normalized.document_id,
                run_id: published_revision_id,
                latest_revision_id,
                next_offset: slice_end_offset,
                window_chars: normalized.window_chars,
                read_mode: normalized.read_mode.clone(),
            },
        )
    });

    if !include_references {
        state_view.chunk_references.clear();
        state_view.technical_fact_references.clear();
        state_view.entity_references.clear();
        state_view.relation_references.clear();
        state_view.evidence_references.clear();
    }
    Ok(McpReadDocumentResponse {
        document_id: state_view.document_id,
        document_title: state_view.document_title,
        library_id: state_view.library.id,
        workspace_id: state_view.library.workspace_id,
        latest_revision_id,
        read_mode: normalized.read_mode,
        readability_state: state_view.readability_state,
        readiness_kind: state_view.readiness_kind,
        graph_coverage_kind: state_view.graph_coverage_kind,
        status_reason: state_view.status_reason,
        mime_type,
        source_uri,
        source_access,
        visual_description,
        content: Some(slice),
        slice_start_offset: slice_start_offset.min(total_content_length),
        slice_end_offset,
        total_content_length: Some(total_content_length),
        continuation_token,
        has_more,
        chunk_references: state_view.chunk_references,
        technical_fact_references: state_view.technical_fact_references,
        entity_references: state_view.entity_references,
        relation_references: state_view.relation_references,
        evidence_references: state_view.evidence_references,
    })
}

pub(super) fn effective_read_start_offset(
    read_mode: &McpReadMode,
    requested_start_offset: usize,
    total_content_length: usize,
    window_chars: usize,
) -> usize {
    if matches!(read_mode, McpReadMode::Full) && total_content_length <= window_chars {
        0
    } else {
        requested_start_offset.min(total_content_length)
    }
}

/// Only the captured publication can provide source metadata or readable bytes.
async fn load_published_revision(
    state: &AppState,
    snapshot: &crate::infra::repositories::content_document_head_repository::ContentDocumentReadSnapshot,
) -> Result<Option<crate::infra::knowledge_rows::KnowledgeRevisionRow>, ApiError> {
    let Some(revision_id) = snapshot.head.readable_revision_id else {
        return Ok(None);
    };
    Ok(state
        .document_store
        .get_revision(revision_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .filter(|revision| {
            revision.document_id == snapshot.head.document_id
                && revision.workspace_id == snapshot.workspace_id
                && revision.library_id == snapshot.library_id
        }))
}

pub(crate) async fn resolve_document_state(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
    accepted_permissions: &[&str],
) -> Result<ResolvedDocumentState, ApiError> {
    let knowledge_document = state
        .document_store
        .get_document(document_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::NotFound("document not found".to_string()))?;
    let library = catalog_repository::get_library_by_id(
        &state.persistence.postgres,
        knowledge_document.library_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("library", knowledge_document.library_id))?;
    authorize_document_permission(
        auth,
        library.workspace_id,
        library.id,
        document_id,
        accepted_permissions,
    )?;
    let snapshot =
        crate::infra::repositories::content_document_head_repository::list_document_read_snapshots(
            &state.persistence.postgres,
            &[document_id],
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "document visibility lookup failed"))?
        .pop()
        .filter(|snapshot| {
            snapshot.workspace_id == library.workspace_id
                && snapshot.workspace_id == knowledge_document.workspace_id
                && snapshot.library_id == library.id
        })
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
    let latest_revision_id = snapshot.head.readable_revision_id;
    let content_summary =
        state.application_services.content.get_document(state, document_id).await?;
    // No readiness summary means the knowledge-plane row is deleted. For a
    // single-document read that is a missing document, not an internal fault.
    let readiness_summary = content_summary
        .readiness_summary
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
    let readable_revision = load_published_revision(state, &snapshot).await?;
    let document_title = readable_revision
        .as_ref()
        .and_then(|revision| revision.title.clone())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| knowledge_document.external_key.clone());
    let source_descriptor = readable_revision.as_ref().map(|revision| {
        crate::services::content::source_access::describe_content_source(
            revision.document_id,
            Some(revision.revision_id),
            crate::services::content::source_access::ContentSourceInputs {
                content_source_kind: &revision.revision_kind,
                source_uri: revision.source_uri.as_deref(),
                storage_key: revision.storage_ref.as_deref(),
                title: revision.title.as_deref(),
                external_key: document_title.as_str(),
            },
        )
    });
    let readable_revision_mime_type =
        readable_revision.as_ref().map(|revision| revision.mime_type.clone());
    let readable_revision_source_uri =
        readable_revision.as_ref().and_then(|revision| revision.source_uri.clone());
    let readable_revision_storage_ref =
        readable_revision.as_ref().and_then(|revision| revision.storage_ref.clone());
    let (
        readability_state,
        status_reason,
        content,
        chunk_references,
        technical_fact_references,
        entity_references,
        relation_references,
        evidence_references,
    ) = match readable_revision.as_ref() {
        Some(revision)
            if revision_text_state_is_readable(&revision.text_state)
                && (revision
                    .normalized_text
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty())
                    || revision.mime_type.trim().to_ascii_lowercase().starts_with("image/")) =>
        {
            let chunks = state
                .document_store
                .list_chunks_by_revision(revision.revision_id)
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
            let chunk_references = chunks
                .iter()
                .map(|chunk| McpChunkReference {
                    chunk_id: chunk.chunk_id,
                    rank: chunk.chunk_index.saturating_add(1),
                    score: 1.0,
                    inclusion_reason: Some("revision_chunk".to_string()),
                })
                .collect::<Vec<_>>();
            let grounding = collect_revision_grounding_references(
                state,
                revision.revision_id,
                &chunks.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>(),
                16,
            )
            .await?;
            let status_reason = readable_status_reason(&readiness_summary, &grounding);
            (
                readability_state_from_kind(readiness_summary.readiness_kind),
                status_reason,
                revision.normalized_text.clone(),
                chunk_references,
                grounding.technical_fact_references,
                grounding.entity_references,
                grounding.relation_references,
                grounding.evidence_references,
            )
        }
        Some(revision) if revision.text_state == "failed" => (
            readability_state_from_kind(readiness_summary.readiness_kind),
            Some("latest readable revision extraction failed".to_string()),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
        _ if readiness_summary.readiness_kind == DocumentReadiness::Failed => (
            McpReadabilityState::Failed,
            Some("latest readable revision extraction failed".to_string()),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
        _ if knowledge_document.active_revision_id.is_some() => {
            let reason = if readiness_summary.readiness_kind == DocumentReadiness::Empty {
                // No mutation, no job, no readable text: nothing is being
                // extracted, so the "still being extracted" reason would be a
                // lie (IRG-109).
                "document has no readable revision yet".to_string()
            } else {
                "latest revision is still being extracted".to_string()
            };
            (
                readability_state_from_kind(readiness_summary.readiness_kind),
                Some(reason),
                None,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        }
        _ => (
            readability_state_from_kind(readiness_summary.readiness_kind),
            Some("document has no readable revision yet".to_string()),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
    };
    if !crate::infra::repositories::content_document_head_repository::document_read_snapshot_is_current(
        &state.persistence.postgres,
        &snapshot,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "document visibility lookup failed"))?
    {
        return Err(ApiError::resource_not_found("document", document_id));
    }
    Ok(ResolvedDocumentState {
        snapshot,
        document_id,
        document_title,
        library,
        latest_revision_id,
        readability_state,
        readiness_kind: readiness_summary.readiness_kind.as_str().to_string(),
        graph_coverage_kind: readiness_summary.graph_coverage_kind,
        status_reason,
        mime_type: readable_revision_mime_type,
        source_uri: readable_revision_source_uri,
        source_access: source_descriptor.and_then(|descriptor| descriptor.access),
        storage_ref: readable_revision_storage_ref,
        content,
        chunk_references,
        technical_fact_references,
        entity_references,
        relation_references,
        evidence_references,
    })
}

fn map_source_access(
    access: &crate::domains::content::ContentSourceAccess,
) -> McpContentSourceAccess {
    McpContentSourceAccess {
        kind: match access.kind {
            crate::domains::content::ContentSourceAccessKind::StoredDocument => {
                "stored_document".to_string()
            }
            crate::domains::content::ContentSourceAccessKind::ExternalUrl => {
                "external_url".to_string()
            }
        },
        href: access.href.clone(),
    }
}

pub(super) struct CanonicalReadWindow {
    pub(super) content: String,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) total: usize,
    pub(super) has_more: bool,
}

pub(super) fn canonical_read_window(
    content: &str,
    mode: &McpReadMode,
    requested_start: usize,
    window_chars: usize,
) -> CanonicalReadWindow {
    let total = content.chars().count();
    let start = effective_read_start_offset(mode, requested_start, total, window_chars);
    let content = char_slice(content, start, window_chars);
    let end = start.saturating_add(content.chars().count());
    CanonicalReadWindow { content, start, end, total, has_more: end < total }
}

async fn load_source_visual_description(
    state: &AppState,
    state_view: &ResolvedDocumentState,
) -> Result<Option<String>, ApiError> {
    if state_view.readability_state != McpReadabilityState::Readable {
        return Ok(None);
    }
    let Some(mime_type) = state_view.mime_type.as_deref() else {
        return Ok(None);
    };
    if !mime_type.trim().to_ascii_lowercase().starts_with("image/") {
        return Ok(None);
    }
    let Some(storage_ref) = state_view.storage_ref.as_deref() else {
        return Ok(None);
    };
    let Some(latest_revision_id) = state_view.latest_revision_id else {
        return Ok(None);
    };
    let Some(binding) = state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, state_view.library.id, AiBindingPurpose::ExtractText)
        .await?
    else {
        return Ok(None);
    };
    let file_bytes = match state.content_storage.read_revision_source(storage_ref).await {
        Ok(bytes) => bytes,
        Err(error) => {
            warn!(
                document_id = %state_view.document_id,
                revision_id = %latest_revision_id,
                storage_ref = %storage_ref,
                error = %error,
                "failed to read stored source for MCP image description"
            );
            return Ok(None);
        }
    };
    describe_source_image(state, state_view, latest_revision_id, &binding, mime_type, &file_bytes)
        .await
}

async fn describe_source_image(
    state: &AppState,
    state_view: &ResolvedDocumentState,
    latest_revision_id: Uuid,
    binding: &crate::services::ai_catalog_service::ResolvedRuntimeBinding,
    mime_type: &str,
    file_bytes: &[u8],
) -> Result<Option<String>, ApiError> {
    let mut provider_call = crate::services::ops::billing::provider_reservation::ProviderCallReservation::reserve_document_read(
        state,
        crate::infra::repositories::billing_repository::BillingDocumentReadRow {
            id: Uuid::now_v7(),
            workspace_id: state_view.library.workspace_id,
            library_id: state_view.library.id,
            document_id: state_view.document_id,
            revision_id: latest_revision_id,
        },
        binding,
    ).await?;
    match crate::integrations::retry::with_single_provider_attempt(
        crate::observability::with_provider_call_id(
            provider_call.provider_call_id(),
            crate::shared::extraction::image::describe_image_with_provider(
                state.llm_gateway.as_ref(),
                crate::shared::extraction::image::VisionProviderInput {
                    provider_kind: &binding.provider_kind,
                    model_name: &binding.model_name,
                    api_key: binding.api_key.as_deref().unwrap_or_default(),
                    base_url: binding.provider_base_url.as_deref(),
                    extra_parameters_json: &binding.extra_parameters_json,
                },
                mime_type,
                file_bytes,
            ),
        ),
    )
    .await
    {
        Ok(result) => {
            provider_call.complete(&result.usage_json).await?;
            let text = result.text.trim().to_string();
            Ok((!text.is_empty()).then_some(text))
        }
        Err(error) => {
            // Description is optional, but its attempted spend is not. Only a
            // durably failed call may fall back to the canonical text alone.
            provider_call.fail().await?;
            warn!(
                document_id = %state_view.document_id,
                revision_id = %latest_revision_id,
                mime_type = %mime_type,
                error = %error,
                "failed to derive source image description for MCP read_document"
            );
            Ok(None)
        }
    }
}
