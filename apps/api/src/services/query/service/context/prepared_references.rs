use super::*;

use super::bundle_references::{
    augment_fact_rank_refs_from_chunk_support, derive_block_rank_refs, derive_fact_rank_refs,
    selected_fact_ids_for_detail,
};

fn log_technical_fact_lookup_failed<E: std::fmt::Debug>(error: &E, technical_fact_ids: usize) {
    tracing::warn!(
        ?error,
        technical_fact_ids,
        "post-answer technical fact lookup failed; returning empty reference list"
    );
}

fn log_structured_block_lookup_failed<E: std::fmt::Debug>(error: &E, structured_block_ids: usize) {
    tracing::warn!(
        ?error,
        structured_block_ids,
        "post-answer structured block lookup failed; returning empty reference list"
    );
}

pub(crate) async fn load_execution_prepared_reference_context(
    state: &AppState,
    execution_id: Uuid,
) -> Result<ExecutionPreparedReferenceContext, ApiError> {
    let bundle_refs = state
        .context_store
        .get_bundle_reference_set_by_query_execution(execution_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let Some(bundle) = bundle_refs.as_ref() else {
        return Ok(ExecutionPreparedReferenceContext::default());
    };

    let chunk_rows = state
        .document_store
        .list_chunks_by_ids(
            &bundle.chunk_references.iter().map(|reference| reference.chunk_id).collect::<Vec<_>>(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let evidence_rows = state
        .graph_store
        .list_evidence_by_ids(
            &bundle
                .evidence_references
                .iter()
                .map(|reference| reference.evidence_id)
                .collect::<Vec<_>>(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let mut fact_rank_refs = derive_fact_rank_refs(bundle, &evidence_rows);
    let chunk_supported_fact_rows = if chunk_rows.is_empty() {
        Vec::new()
    } else {
        state
            .document_store
            .list_technical_facts_by_chunk_ids(
                &chunk_rows.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    };
    augment_fact_rank_refs_from_chunk_support(
        Some(bundle),
        &chunk_supported_fact_rows,
        &mut fact_rank_refs,
    );
    // Post-answer reference hydration: the generated answer body is
    // already accepted and streamed before we reach this block, so an
    // intermittent knowledge-store error must not turn a succeeded turn into a
    // 500. Fail-soft to an empty vec and log so references degrade but the
    // answer still reaches the user.
    let technical_fact_rows =
        if fact_rank_refs.is_empty() && bundle.bundle.selected_fact_ids.is_empty() {
            Vec::new()
        } else {
            let ids = selected_fact_ids_for_detail(bundle, &fact_rank_refs);
            match state.document_store.list_technical_facts_by_ids(&ids).await {
                Ok(rows) => rows,
                Err(error) => {
                    log_technical_fact_lookup_failed(&error, ids.len());
                    Vec::new()
                }
            }
        };
    let block_rank_refs =
        derive_block_rank_refs(bundle, &evidence_rows, &technical_fact_rows, &chunk_rows);
    let structured_block_rows = if block_rank_refs.is_empty() {
        Vec::new()
    } else {
        let ids: Vec<Uuid> = block_rank_refs.keys().copied().collect();
        match state.document_store.list_structured_blocks_by_ids(&ids).await {
            Ok(rows) => rows,
            Err(error) => {
                log_structured_block_lookup_failed(&error, ids.len());
                Vec::new()
            }
        }
    };
    let segment_revision_info =
        load_prepared_segment_revision_info(state, &structured_block_rows).await;
    let assistant_document_references =
        assistant_document_references_from_diagnostics(&bundle.bundle.assembly_diagnostics);

    Ok(ExecutionPreparedReferenceContext {
        bundle_refs,
        chunk_rows,
        fact_rank_refs,
        technical_fact_rows,
        block_rank_refs,
        structured_block_rows,
        segment_revision_info,
        assistant_document_references,
    })
}

fn assistant_document_references_from_diagnostics(
    assembly_diagnostics: &serde_json::Value,
) -> Vec<AssistantGroundingDocumentReference> {
    assembly_diagnostics
        .get("assistantGrounding")
        .and_then(|value| value.get("documentReferences"))
        .cloned()
        .and_then(|value| {
            serde_json::from_value::<Vec<AssistantGroundingDocumentReference>>(value).ok()
        })
        .unwrap_or_default()
}

pub(crate) fn query_ir_from_bundle_diagnostics(
    assembly_diagnostics: &serde_json::Value,
) -> Option<crate::domains::query_ir::QueryIR> {
    assembly_diagnostics
        .get("queryIr")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

async fn load_prepared_segment_revision_info(
    state: &AppState,
    blocks: &[crate::infra::knowledge_rows::KnowledgeStructuredBlockRow],
) -> HashMap<Uuid, PreparedSegmentRevisionInfo> {
    if blocks.is_empty() {
        return HashMap::new();
    }

    let document_ids = blocks
        .iter()
        .map(|block| block.document_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let revision_ids = blocks
        .iter()
        .map(|block| block.revision_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let doc_titles: HashMap<Uuid, Option<String>> = state
        .document_store
        .list_documents_by_ids(&document_ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|document| (document.document_id, document.title))
        .collect();

    let library_ids = blocks
        .iter()
        .map(|block| block.library_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut library_settings = HashMap::<Uuid, bool>::new();
    for library_id in library_ids {
        let enabled =
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                .await
                .ok()
                .flatten()
                .map(|library| library.include_document_hint_in_mcp_answers)
                .unwrap_or(true);
        library_settings.insert(library_id, enabled);
    }

    state
        .document_store
        .list_revisions_by_ids(&revision_ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|revision| {
            let source_descriptor =
                crate::services::content::source_access::describe_content_source(
                    revision.document_id,
                    Some(revision.revision_id),
                    crate::services::content::source_access::ContentSourceInputs {
                        content_source_kind: &revision.revision_kind,
                        source_uri: revision.source_uri.as_deref(),
                        storage_key: revision.storage_ref.as_deref(),
                        title: revision.title.as_deref(),
                        external_key: doc_titles
                            .get(&revision.document_id)
                            .and_then(|title| title.as_deref())
                            .unwrap_or("document"),
                    },
                );
            let document_title = doc_titles
                .get(&revision.document_id)
                .cloned()
                .flatten()
                .or_else(|| revision.title.clone())
                .or_else(|| Some(source_descriptor.file_name.clone()));
            let library_setting =
                library_settings.get(&revision.library_id).copied().unwrap_or(true);
            let document_hint = resolve_document_hint(
                &revision.revision_kind,
                revision.source_uri.as_deref(),
                revision.document_hint.as_deref(),
                document_title.as_deref(),
                library_setting,
            );
            (
                revision.revision_id,
                PreparedSegmentRevisionInfo {
                    document_title,
                    source_uri: revision.source_uri.clone(),
                    document_hint,
                    source_access: source_descriptor.access,
                },
            )
        })
        .collect()
}
