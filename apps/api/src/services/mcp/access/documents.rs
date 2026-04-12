use serde_json::json;
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{ai::AiBindingPurpose, content::revision_text_state_is_readable},
    infra::repositories::{catalog_repository, content_repository},
    integrations::llm::EmbeddingRequest,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_MCP_MEMORY_READ, authorize_library_discovery, load_library_and_authorize,
        },
        router_support::ApiError,
    },
    mcp_types::{
        McpChunkReference, McpContentSourceAccess, McpDocumentHit, McpEntityReference,
        McpEvidenceReference, McpReadDocumentRequest, McpReadDocumentResponse, McpReadabilityState,
        McpRelationReference, McpSearchDocumentsRequest, McpSearchDocumentsResponse,
        McpTechnicalFactReference,
    },
    services::mcp::support::{
        char_slice, encode_continuation_token, normalize_read_request, saturating_rank,
    },
};

use super::{
    catalog::{describe_libraries, load_visible_library_contexts},
    types::{
        ArangoChunkMentionReferenceRow, ArangoRelationSupportReferenceRow, McpDocumentAccumulator,
        McpRevisionGroundingReferences, McpSearchEmbeddingContext, ResolvedDocumentState,
        VisibleLibraryContext,
    },
};

pub async fn search_documents(
    auth: &AuthContext,
    state: &AppState,
    request: McpSearchDocumentsRequest,
) -> Result<McpSearchDocumentsResponse, ApiError> {
    auth.require_any_scope(POLICY_MCP_MEMORY_READ)?;
    let settings = &state.mcp_memory;
    let include_references = request.include_references.unwrap_or(false);
    let query = request.query.trim();
    if query.is_empty() {
        return Err(ApiError::BadRequest("query must not be empty".into()));
    }

    let limit =
        request.limit.unwrap_or(settings.default_search_limit).clamp(1, settings.max_search_limit);
    let requested_library_ids = request.requested_library_ids();
    let libraries = resolve_search_libraries(auth, state, requested_library_ids.as_deref()).await?;
    let library_ids = libraries.iter().map(|item| item.library.id).collect::<Vec<_>>();
    let mut hits = Vec::new();
    for library in libraries {
        let lexical_limit = limit.saturating_mul(3).max(6);
        let lexical_chunk_hits = state
            .arango_search_store
            .search_chunks(library.library.id, query, lexical_limit)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let embedding_context =
            resolve_search_embedding_context(state, library.library.id, query).await?;
        let vector_chunk_hits = if let Some(context) = embedding_context.as_ref() {
            match state
                .arango_search_store
                .search_chunk_vectors_by_similarity(
                    library.library.id,
                    &context.model_catalog_id.to_string(),
                    context.freshness_generation,
                    &context.query_vector,
                    lexical_limit.saturating_mul(2),
                    Some(16),
                )
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    warn!(
                        library_id = %library.library.id,
                        model_catalog_id = %context.model_catalog_id,
                        freshness_generation = context.freshness_generation,
                        error = ?error,
                        "mcp search vector lookup failed; degrading to lexical-only MCP search",
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let all_chunk_ids = lexical_chunk_hits
            .iter()
            .map(|hit| hit.chunk_id)
            .chain(vector_chunk_hits.iter().map(|hit| hit.chunk_id))
            .collect::<Vec<_>>();
        let chunk_rows = load_knowledge_chunks_by_ids(state, &all_chunk_ids).await?;
        let chunk_map = chunk_rows
            .into_iter()
            .map(|row| (row.chunk_id, row))
            .collect::<std::collections::HashMap<_, _>>();
        let mut document_accumulators =
            std::collections::HashMap::<Uuid, McpDocumentAccumulator>::new();

        for (index, hit) in lexical_chunk_hits.iter().enumerate() {
            let Some(chunk) = chunk_map.get(&hit.chunk_id) else {
                continue;
            };
            let document = state
                .arango_document_store
                .get_document(chunk.document_id)
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("document", chunk.document_id))?;
            let revision = state
                .arango_document_store
                .get_revision(chunk.revision_id)
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("revision", chunk.revision_id))?;
            let accumulator =
                document_accumulators.entry(document.document_id).or_insert_with(|| {
                    McpDocumentAccumulator::from_knowledge(&document, &revision, hit)
                });
            accumulator.bump_score(hit.score);
            accumulator.merge_chunk_reference(
                chunk.chunk_id,
                saturating_rank(index),
                hit.score,
                Some("lexical_chunk".to_string()),
            );
            accumulator.populate_excerpt_from_text(&hit.normalized_text, query);
        }

        for (index, hit) in vector_chunk_hits.iter().enumerate() {
            let Some(chunk) = chunk_map.get(&hit.chunk_id) else {
                continue;
            };
            let document = state
                .arango_document_store
                .get_document(chunk.document_id)
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("document", chunk.document_id))?;
            let revision = state
                .arango_document_store
                .get_revision(chunk.revision_id)
                .await
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("revision", chunk.revision_id))?;
            let accumulator =
                document_accumulators.entry(document.document_id).or_insert_with(|| {
                    McpDocumentAccumulator::from_knowledge(
                        &document,
                        &revision,
                        &crate::infra::arangodb::search_store::KnowledgeChunkSearchRow {
                            chunk_id: chunk.chunk_id,
                            workspace_id: chunk.workspace_id,
                            library_id: chunk.library_id,
                            revision_id: chunk.revision_id,
                            content_text: chunk.content_text.clone(),
                            normalized_text: chunk.normalized_text.clone(),
                            section_path: chunk.section_path.clone(),
                            heading_trail: chunk.heading_trail.clone(),
                            score: hit.score,
                            quality_score: chunk.quality_score,
                        },
                    )
                });
            accumulator.bump_score(hit.score);
            accumulator.merge_chunk_reference(
                chunk.chunk_id,
                saturating_rank(index),
                hit.score,
                Some("vector_chunk".to_string()),
            );
            accumulator.populate_excerpt_from_text(&chunk.normalized_text, query);
        }

        let mut library_hits = document_accumulators.into_values().collect::<Vec<_>>();
        library_hits.sort_by(|left, right| right.score.total_cmp(&left.score));
        library_hits.truncate(limit);
        for accumulator in library_hits {
            let chunk_references = accumulator.clone().into_chunk_references();
            let content_summary = state
                .canonical_services
                .content
                .get_document(state, accumulator.document_id)
                .await?;
            let readiness_summary = content_summary.readiness_summary.ok_or(ApiError::Internal)?;
            let grounding = collect_revision_grounding_references(
                state,
                accumulator.readable_revision_id,
                &accumulator.chunk_reference_ids(),
                8,
            )
            .await?;
            let status_reason = readable_status_reason(&readiness_summary, &grounding);
            hits.push(McpDocumentHit {
                document_id: accumulator.document_id,
                logical_document_id: accumulator.document_id,
                library_id: accumulator.library_id,
                workspace_id: accumulator.workspace_id,
                document_title: accumulator.document_title,
                latest_revision_id: Some(accumulator.readable_revision_id),
                score: accumulator.score,
                excerpt: accumulator.excerpt,
                excerpt_start_offset: accumulator.excerpt_start_offset,
                excerpt_end_offset: accumulator.excerpt_end_offset,
                readability_state: readability_state_from_kind(&readiness_summary.readiness_kind),
                readiness_kind: readiness_summary.readiness_kind.clone(),
                graph_coverage_kind: readiness_summary.graph_coverage_kind.clone(),
                status_reason,
                chunk_references,
                technical_fact_references: grounding.technical_fact_references,
                entity_references: grounding.entity_references,
                relation_references: grounding.relation_references,
                evidence_references: grounding.evidence_references,
            });
        }
    }
    hits.sort_by(|left, right| right.score.total_cmp(&left.score));
    hits.truncate(limit);

    if !include_references {
        for hit in &mut hits {
            hit.chunk_references.clear();
            hit.technical_fact_references.clear();
            hit.entity_references.clear();
            hit.relation_references.clear();
            hit.evidence_references.clear();
        }
    }

    Ok(McpSearchDocumentsResponse { query: query.to_string(), limit, library_ids, hits })
}

pub async fn read_document(
    auth: &AuthContext,
    state: &AppState,
    request: McpReadDocumentRequest,
) -> Result<McpReadDocumentResponse, ApiError> {
    auth.require_any_scope(POLICY_MCP_MEMORY_READ)?;
    let include_references = request.include_references.unwrap_or(false);
    let settings = &state.mcp_memory;
    let normalized = normalize_read_request(
        auth,
        request.document_id,
        request.mode,
        request.start_offset,
        request.length,
        request.continuation_token.as_deref(),
        settings.default_read_window_chars,
        settings.max_read_window_chars,
    )?;
    let state_view = resolve_document_state(auth, state, normalized.document_id).await?;
    let latest_revision_id = state_view.latest_revision_id;
    let source_access = state_view.source_access.as_ref().map(map_source_access);
    let visual_description = load_source_visual_description(state, &state_view).await?;
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

    let content = merge_visual_description_into_content(
        state_view.content.as_deref(),
        visual_description.as_deref(),
    );
    let total_content_length = content.chars().count();
    let slice = char_slice(&content, normalized.start_offset, normalized.window_chars);
    let slice_len = slice.chars().count();
    let slice_end_offset = normalized.start_offset.saturating_add(slice_len);
    let has_more = slice_end_offset < total_content_length;
    let continuation_token = has_more.then(|| {
        encode_continuation_token(
            auth,
            normalized.document_id,
            latest_revision_id.unwrap_or(normalized.document_id),
            latest_revision_id,
            slice_end_offset,
            normalized.window_chars,
            normalized.read_mode.clone(),
        )
    });

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
        slice_start_offset: normalized.start_offset.min(total_content_length),
        slice_end_offset,
        total_content_length: Some(total_content_length),
        continuation_token,
        has_more,
        chunk_references: if include_references { state_view.chunk_references } else { Vec::new() },
        technical_fact_references: if include_references {
            state_view.technical_fact_references
        } else {
            Vec::new()
        },
        entity_references: if include_references {
            state_view.entity_references
        } else {
            Vec::new()
        },
        relation_references: if include_references {
            state_view.relation_references
        } else {
            Vec::new()
        },
        evidence_references: if include_references {
            state_view.evidence_references
        } else {
            Vec::new()
        },
    })
}

pub async fn authorize_library_for_mcp(
    auth: &AuthContext,
    state: &AppState,
    library_id: Uuid,
) -> Result<(), ApiError> {
    load_library_and_authorize(auth, state, library_id, POLICY_MCP_MEMORY_READ).await?;
    Ok(())
}

pub async fn list_documents(
    auth: &AuthContext,
    state: &AppState,
    library_id: Uuid,
    limit: usize,
    status_filter: Option<&str>,
) -> Result<serde_json::Value, ApiError> {
    auth.require_any_scope(POLICY_MCP_MEMORY_READ)?;
    let _library =
        load_library_and_authorize(auth, state, library_id, POLICY_MCP_MEMORY_READ).await?;

    let summaries = state.canonical_services.content.list_documents(state, library_id).await?;

    let filtered: Vec<_> = summaries
        .into_iter()
        .filter(|summary| summary.document.document_state != "deleted")
        .filter(|summary| match status_filter {
            Some(filter) => {
                summary.readiness_summary.as_ref().is_some_and(|row| row.readiness_kind == filter)
            }
            None => true,
        })
        .take(limit)
        .collect();

    let documents: Vec<serde_json::Value> = filtered
        .iter()
        .map(|summary| {
            let readiness_kind = summary
                .readiness_summary
                .as_ref()
                .map(|row| row.readiness_kind.as_str())
                .unwrap_or("unknown");
            let source_uri =
                summary.active_revision.as_ref().and_then(|row| row.source_uri.as_deref());
            let byte_size = summary.active_revision.as_ref().map(|row| row.byte_size);
            let title = summary
                .active_revision
                .as_ref()
                .and_then(|row| row.title.as_deref())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(&summary.document.external_key);
            json!({
                "documentId": summary.document.id,
                "title": title,
                "readinessKind": readiness_kind,
                "sourceUri": source_uri,
                "byteSize": byte_size,
                "createdAt": summary.document.created_at,
            })
        })
        .collect();

    Ok(json!({
        "libraryId": library_id,
        "documents": documents,
        "count": documents.len(),
        "limit": limit,
    }))
}

pub async fn delete_document(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
) -> Result<serde_json::Value, ApiError> {
    let document = content_repository::get_document_by_id(&state.persistence.postgres, document_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;

    authorize_library_discovery(auth, document.workspace_id, document.library_id)?;
    auth.require_any_scope(crate::interfaces::http::authorization::POLICY_MCP_MEMORY_WRITE)?;

    let admission = state
        .canonical_services
        .content
        .admit_mutation(
            state,
            crate::services::content::service::AdmitMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                operation_kind: "delete".to_string(),
                idempotency_key: None,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "mcp".to_string(),
                source_identity: None,
                revision: None,
            },
        )
        .await?;

    Ok(json!({
        "documentId": document_id,
        "libraryId": document.library_id,
        "workspaceId": document.workspace_id,
        "mutationId": admission.mutation.id,
        "status": "accepted",
    }))
}

pub(crate) async fn resolve_document_state(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
) -> Result<ResolvedDocumentState, ApiError> {
    let knowledge_document = state
        .arango_document_store
        .get_document(document_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
    let library = catalog_repository::get_library_by_id(
        &state.persistence.postgres,
        knowledge_document.library_id,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("library", knowledge_document.library_id))?;
    authorize_library_discovery(auth, library.workspace_id, library.id)?;
    let latest_revision_id = knowledge_document.readable_revision_id;
    let content_summary = state.canonical_services.content.get_document(state, document_id).await?;
    let readiness_summary = content_summary.readiness_summary.ok_or(ApiError::Internal)?;
    let readable_revision = match latest_revision_id {
        Some(revision_id) => state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?,
        None => None,
    };
    let document_title = readable_revision
        .as_ref()
        .and_then(|revision| revision.title.clone())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| knowledge_document.external_key.clone());
    let source_descriptor = readable_revision.as_ref().map(|revision| {
        crate::services::content::source_access::describe_content_source(
            revision.document_id,
            Some(revision.revision_id),
            &revision.revision_kind,
            revision.source_uri.as_deref(),
            revision.storage_ref.as_deref(),
            revision.title.as_deref(),
            document_title.as_str(),
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
                .arango_document_store
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
                readability_state_from_kind(&readiness_summary.readiness_kind),
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
            readability_state_from_kind(&readiness_summary.readiness_kind),
            Some("latest readable revision extraction failed".to_string()),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
        _ if knowledge_document.active_revision_id.is_some() => (
            readability_state_from_kind(&readiness_summary.readiness_kind),
            Some("latest revision is still being extracted".to_string()),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
        _ => (
            readability_state_from_kind(&readiness_summary.readiness_kind),
            Some("document has no readable revision yet".to_string()),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
    };
    Ok(ResolvedDocumentState {
        document_id,
        document_title,
        library,
        latest_revision_id,
        readability_state,
        readiness_kind: readiness_summary.readiness_kind,
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

fn merge_visual_description_into_content(
    content: Option<&str>,
    visual_description: Option<&str>,
) -> String {
    let content = content.unwrap_or("").trim();
    let visual_description = visual_description.unwrap_or("").trim();
    if visual_description.is_empty() {
        return content.to_string();
    }
    if content.is_empty() {
        return format!("## Source Image Description\n{visual_description}");
    }
    if content.contains(visual_description) {
        return content.to_string();
    }
    format!("{content}\n\n## Source Image Description\n{visual_description}")
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
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, state_view.library.id, AiBindingPurpose::Vision)
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
    match crate::shared::extraction::image::describe_image_with_provider(
        state.llm_gateway.as_ref(),
        &binding.provider_kind,
        &binding.model_name,
        binding.api_key.as_deref().unwrap_or_default(),
        binding.provider_base_url.as_deref(),
        mime_type,
        &file_bytes,
    )
    .await
    {
        Ok(result) => {
            let text = result.text.trim().to_string();
            Ok((!text.is_empty()).then_some(text))
        }
        Err(error) => {
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

pub(crate) async fn resolve_search_libraries(
    auth: &AuthContext,
    state: &AppState,
    requested_library_ids: Option<&[Uuid]>,
) -> Result<Vec<VisibleLibraryContext>, ApiError> {
    if let Some(library_ids) = requested_library_ids {
        if library_ids.is_empty() {
            return Err(ApiError::invalid_mcp_tool_call(
                "libraryIds must not be empty when provided",
            ));
        }
        let mut rows = Vec::with_capacity(library_ids.len());
        for library_id in library_ids {
            let library = crate::interfaces::http::authorization::load_library_and_authorize(
                auth,
                state,
                *library_id,
                POLICY_MCP_MEMORY_READ,
            )
            .await?;
            rows.push(library);
        }
        return describe_libraries(auth, state, rows).await;
    }

    let libraries = load_visible_library_contexts(auth, state, None).await?;
    Ok(libraries
        .into_iter()
        .filter(|item| {
            auth.has_library_permission(
                item.library.workspace_id,
                item.library.id,
                POLICY_MCP_MEMORY_READ,
            )
        })
        .collect())
}

pub(crate) async fn resolve_search_embedding_context(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
) -> Result<Option<McpSearchEmbeddingContext>, ApiError> {
    let Some(binding) = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
    else {
        return Ok(None);
    };

    let generations = state
        .canonical_services
        .knowledge
        .derive_library_generation_rows(state, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let Some(generation) = generations.first() else {
        return Ok(None);
    };
    if generation.active_vector_generation <= 0 {
        return Ok(None);
    }

    let embedding = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            input: query_text.to_string(),
            api_key_override: binding.api_key,
            base_url_override: binding.provider_base_url,
        })
        .await
        .map_err(|error| {
            ApiError::ProviderFailure(format!("failed to embed MCP memory search query: {error}"))
        })?;

    Ok(Some(McpSearchEmbeddingContext {
        model_catalog_id: binding.model_catalog_id,
        freshness_generation: generation.active_vector_generation,
        query_vector: embedding.embedding,
    }))
}

pub(crate) async fn load_knowledge_chunks_by_ids(
    state: &AppState,
    chunk_ids: &[Uuid],
) -> Result<Vec<crate::infra::arangodb::document_store::KnowledgeChunkRow>, ApiError> {
    if chunk_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::with_capacity(chunk_ids.len());
    for chunk_id in chunk_ids {
        let cursor = state
            .arango_document_store
            .client()
            .query_json(
                "FOR chunk IN @@collection
                 FILTER chunk.chunk_id == @chunk_id
                 LIMIT 1
                 RETURN chunk",
                serde_json::json!({
                    "@collection": crate::infra::arangodb::collections::KNOWLEDGE_CHUNK_COLLECTION,
                    "chunk_id": chunk_id,
                }),
            )
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let result = cursor.get("result").cloned().ok_or(ApiError::Internal)?;
        let mut decoded: Vec<crate::infra::arangodb::document_store::KnowledgeChunkRow> =
            serde_json::from_value(result)
                .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        if let Some(row) = decoded.pop() {
            rows.push(row);
        }
    }
    Ok(rows)
}

pub(crate) async fn collect_revision_grounding_references(
    state: &AppState,
    revision_id: Uuid,
    chunk_ids: &[Uuid],
    limit: usize,
) -> Result<McpRevisionGroundingReferences, ApiError> {
    let technical_facts = state
        .arango_document_store
        .list_technical_facts_by_revision(revision_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let mut technical_fact_rows = technical_facts;
    technical_fact_rows.sort_by(|left, right| {
        technical_fact_support_score(right, chunk_ids)
            .cmp(&technical_fact_support_score(left, chunk_ids))
            .then_with(|| {
                right.confidence.unwrap_or(0.0).total_cmp(&left.confidence.unwrap_or(0.0))
            })
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    technical_fact_rows.truncate(limit);
    let technical_fact_references = technical_fact_rows
        .into_iter()
        .enumerate()
        .map(|(index, fact)| McpTechnicalFactReference {
            fact_id: fact.fact_id,
            fact_kind: fact.fact_kind,
            canonical_value: fact.canonical_value_text,
            display_value: fact.display_value,
            rank: saturating_rank(index),
            score: fact.confidence.unwrap_or(1.0),
            inclusion_reason: Some(
                if fact_supports_requested_chunks(&fact.support_chunk_ids, chunk_ids) {
                    "chunk_supported_fact"
                } else {
                    "revision_fact"
                }
                .to_string(),
            ),
        })
        .collect::<Vec<_>>();
    let evidence_rows = state
        .arango_graph_store
        .list_evidence_by_revision(revision_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let mut evidence_rows = evidence_rows;
    evidence_rows.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.evidence_id.cmp(&right.evidence_id))
    });
    evidence_rows.truncate(limit);
    let evidence_references = evidence_rows
        .iter()
        .enumerate()
        .map(|(index, evidence)| McpEvidenceReference {
            evidence_id: evidence.evidence_id,
            rank: saturating_rank(index),
            score: evidence.confidence.unwrap_or(1.0),
            inclusion_reason: Some("revision_evidence".to_string()),
        })
        .collect::<Vec<_>>();
    let evidence_ids = evidence_rows.iter().map(|row| row.evidence_id).collect::<Vec<_>>();

    let entity_references = if chunk_ids.is_empty() {
        Vec::new()
    } else {
        let cursor = state
            .arango_graph_store
            .client()
            .query_json(
                "FOR edge IN @@collection
                 FILTER edge.chunk_id IN @chunk_ids
                 COLLECT entity_id = edge.entity_id
                 AGGREGATE rank = MIN(edge.rank), score = MAX(edge.score)
                 LET reason = FIRST(
                    FOR item IN @@collection
                    FILTER item.entity_id == entity_id AND item.chunk_id IN @chunk_ids
                    SORT item.rank ASC, item.created_at ASC, item._key ASC
                    LIMIT 1
                    RETURN item.inclusionReason
                 )
                 SORT rank ASC, score DESC, entity_id ASC
                 LIMIT @limit
                 RETURN {
                    entity_id,
                    rank,
                    score,
                    inclusion_reason: reason
                 }",
                serde_json::json!({
                    "@collection": crate::infra::arangodb::collections::KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
                    "chunk_ids": chunk_ids,
                    "limit": limit.max(1),
                }),
            )
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let result = cursor.get("result").cloned().ok_or(ApiError::Internal)?;
        let rows: Vec<ArangoChunkMentionReferenceRow> = serde_json::from_value(result)
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        rows.into_iter()
            .map(|row| McpEntityReference {
                entity_id: row.entity_id,
                rank: row.rank,
                score: row.score,
                inclusion_reason: row.inclusion_reason,
            })
            .collect()
    };

    let relation_references = if evidence_ids.is_empty() {
        Vec::new()
    } else {
        let cursor = state
            .arango_graph_store
            .client()
            .query_json(
                "FOR edge IN @@collection
                 FILTER edge.evidence_id IN @evidence_ids
                 COLLECT relation_id = edge.relation_id
                 AGGREGATE rank = MIN(edge.rank), score = MAX(edge.score)
                 LET reason = FIRST(
                    FOR item IN @@collection
                    FILTER item.relation_id == relation_id AND item.evidence_id IN @evidence_ids
                    SORT item.rank ASC, item.created_at ASC, item._key ASC
                    LIMIT 1
                    RETURN item.inclusionReason
                 )
                 SORT rank ASC, score DESC, relation_id ASC
                 LIMIT @limit
                 RETURN {
                    relation_id,
                    rank,
                    score,
                    inclusion_reason: reason
                 }",
                serde_json::json!({
                    "@collection": crate::infra::arangodb::collections::KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
                    "evidence_ids": evidence_ids,
                    "limit": limit.max(1),
                }),
            )
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let result = cursor.get("result").cloned().ok_or(ApiError::Internal)?;
        let rows: Vec<ArangoRelationSupportReferenceRow> = serde_json::from_value(result)
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        rows.into_iter()
            .map(|row| McpRelationReference {
                relation_id: row.relation_id,
                rank: row.rank,
                score: row.score,
                inclusion_reason: row.inclusion_reason,
            })
            .collect()
    };

    Ok(McpRevisionGroundingReferences {
        technical_fact_references,
        entity_references,
        relation_references,
        evidence_references,
    })
}

pub(crate) fn readable_status_reason(
    readiness_summary: &crate::domains::content::DocumentReadinessSummary,
    grounding: &McpRevisionGroundingReferences,
) -> Option<String> {
    if readiness_summary.readiness_kind == "readable" {
        return Some(
            "document text is readable, but canonical preparation and graph extraction are still processing"
                .to_string(),
        );
    }
    if readiness_summary.graph_coverage_kind == "graph_sparse"
        && grounding.technical_fact_references.is_empty()
        && grounding.entity_references.is_empty()
        && grounding.relation_references.is_empty()
        && grounding.evidence_references.is_empty()
    {
        return Some(
            "document text is readable, but graph coverage is still sparse for this revision"
                .to_string(),
        );
    }
    (grounding.technical_fact_references.is_empty()
        && grounding.entity_references.is_empty()
        && grounding.relation_references.is_empty()
        && grounding.evidence_references.is_empty())
        .then_some(
            "document text is readable, but canonical technical facts and graph evidence are not available yet"
                .to_string(),
        )
}

pub(crate) fn readability_state_from_kind(readiness_kind: &str) -> McpReadabilityState {
    match readiness_kind {
        "failed" => McpReadabilityState::Failed,
        "processing" => McpReadabilityState::Processing,
        "readable" | "graph_sparse" | "graph_ready" => McpReadabilityState::Readable,
        _ => McpReadabilityState::Unavailable,
    }
}

fn fact_supports_requested_chunks(support_chunk_ids: &[Uuid], chunk_ids: &[Uuid]) -> bool {
    !support_chunk_ids.is_empty()
        && support_chunk_ids.iter().any(|support_chunk_id| chunk_ids.contains(support_chunk_id))
}

fn technical_fact_support_score(
    fact: &crate::infra::arangodb::document_store::KnowledgeTechnicalFactRow,
    chunk_ids: &[Uuid],
) -> (bool, usize, usize) {
    (
        fact_supports_requested_chunks(&fact.support_chunk_ids, chunk_ids),
        fact.support_chunk_ids.len(),
        fact.support_block_ids.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::merge_visual_description_into_content;

    #[test]
    fn image_visual_description_appends_to_existing_text_once() {
        let merged = merge_visual_description_into_content(
            Some("Visible text from OCR"),
            Some("A restaurant sign with menu items."),
        );

        assert!(merged.contains("Visible text from OCR"));
        assert!(merged.contains("## Source Image Description"));
        assert!(merged.contains("A restaurant sign with menu items."));
    }

    #[test]
    fn image_visual_description_is_not_duplicated_when_already_present() {
        let merged = merge_visual_description_into_content(
            Some("Visible text\n\n## Source Image Description\nA restaurant sign with menu items."),
            Some("A restaurant sign with menu items."),
        );

        assert_eq!(
            merged,
            "Visible text\n\n## Source Image Description\nA restaurant sign with menu items."
        );
    }
}
