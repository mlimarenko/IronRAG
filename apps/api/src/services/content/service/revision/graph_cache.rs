use super::*;

pub(super) struct GraphMaterializationCacheInput<'a> {
    pub(super) state: &'a AppState,
    pub(super) command: &'a MaterializeRevisionGraphCandidatesCommand,
    pub(super) sources: &'a GraphMaterializationSources,
    pub(super) selection: &'a GraphMaterializationSelection,
    pub(super) binding: &'a GraphMaterializationBinding,
    pub(super) cancellation_token: &'a CancellationToken,
}

impl ContentService {
    pub(super) async fn resolve_graph_materialization_cache(
        &self,
        input: GraphMaterializationCacheInput<'_>,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<GraphExtractionCachePlan>> {
        let GraphMaterializationCacheInput {
            state,
            command,
            sources,
            selection,
            binding,
            cancellation_token,
        } = input;
        let classifier_identity = match sources.graph_runtime_context.entity_type_binding.as_ref() {
            Some(binding) => GraphEntityTypeClassifierIdentity::On(binding),
            None => GraphEntityTypeClassifierIdentity::Off,
        };
        self.materialize_cached_graph_extractions(CachedGraphExtractionMaterialization {
            state,
            command,
            document: &sources.document,
            revision: &sources.revision,
            chunks: sources.all_chunks.as_slice(),
            revision_facts: sources.revision_facts.as_slice(),
            library_extraction_prompt: &sources.library_extraction_prompt,
            sub_type_hints: &sources.sub_type_hints,
            table_graph_context: &sources.table_graph_context,
            graph_chunk_policy: &selection.graph_chunk_policy,
            provider_kind: &binding.provider_kind,
            model_name: &binding.model_name,
            runtime_binding: &binding.runtime_binding,
            classifier_identity,
            request_size_soft_limit_bytes: binding.request_size_soft_limit_bytes,
            cancellation_token,
        })
        .await
    }

    async fn reuse_cached_graph_extraction_for_chunk(
        &self,
        materialization: &CachedGraphExtractionMaterialization<'_>,
        chunk: &KnowledgeChunkRow,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<Option<(usize, usize)>>> {
        let CachedGraphExtractionMaterialization {
            state,
            command,
            document,
            revision,
            revision_facts,
            library_extraction_prompt,
            sub_type_hints,
            table_graph_context,
            graph_chunk_policy,
            provider_kind,
            model_name,
            runtime_binding,
            classifier_identity,
            request_size_soft_limit_bytes,
            cancellation_token,
            ..
        } = materialization;
        ensure_not_cancelled(cancellation_token)?;
        let Some(chunk_content) = build_graph_chunk_content(
            chunk,
            table_graph_context.profile_for_chunk(chunk),
            table_graph_context.requires_row_only_graph(),
            graph_chunk_policy,
        ) else {
            return Ok(IngestAttemptMutationOutcome::Applied(None));
        };
        let chunk_facts = revision_facts
            .iter()
            .filter(|fact| typed_fact_supports_chunk(fact, chunk))
            .cloned()
            .collect::<Vec<_>>();
        let request = build_graph_extraction_request(GraphExtractionRequestInput {
            document,
            revision,
            chunk,
            chunk_content,
            technical_facts: &chunk_facts,
            attempt_id: Some(command.authority.attempt_id),
            library_extraction_prompt: (*library_extraction_prompt).clone(),
            sub_type_hints: (*sub_type_hints).clone(),
        });
        let fingerprint = build_graph_extraction_cache_fingerprint(
            &request,
            runtime_binding,
            *classifier_identity,
            *request_size_soft_limit_bytes,
        );
        let text_checksum = sha256_hex_text(&chunk.normalized_text);
        let Some(record) =
            crate::infra::repositories::runtime_graph_repository::find_ready_runtime_graph_extraction_record_by_semantic_cache_key(
                &state.persistence.postgres,
                crate::infra::repositories::runtime_graph_repository::RuntimeGraphExtractionSemanticCacheKey {
                    library_id: command.authority.library_id,
                    text_checksum: &text_checksum,
                    extraction_version: &fingerprint.extraction_version,
                    provider_kind,
                    model_name,
                    extraction_contract_hash: &fingerprint.extraction_contract_hash,
                },
            )
            .await
            .map_err(|e| {
                ApiError::internal_with_log(
                    e,
                    "graph_cache: find_ready_runtime_graph_extraction_record_by_semantic_cache_key",
                )
            })?
        else {
            crate::observability::metrics::record_graph_extraction_cache(
                crate::observability::metrics::GraphCacheResult::Miss,
            );
            return Ok(IngestAttemptMutationOutcome::Applied(None));
        };
        let original_counts = graph_candidate_counts(&record.normalized_output_json);
        let repaired_candidate_set = serde_json::from_value(record.normalized_output_json.clone())
            .map(repair_graph_extraction_candidate_set)
            .unwrap_or_default();
        if original_counts.0.saturating_add(original_counts.1) > 0
            && repaired_candidate_set.entities.is_empty()
            && repaired_candidate_set.relations.is_empty()
        {
            crate::observability::metrics::record_graph_extraction_cache(
                crate::observability::metrics::GraphCacheResult::Miss,
            );
            return Ok(IngestAttemptMutationOutcome::Applied(None));
        }
        let repaired_json = normalized_graph_extraction_json(repaired_candidate_set);
        if record.chunk_id == chunk.chunk_id {
            let lifecycle = extraction_lifecycle_from_record(&record);
            if lifecycle.revision_id.is_some()
                && lifecycle.revision_id != Some(command.authority.revision_id)
            {
                crate::observability::metrics::record_graph_extraction_cache(
                    crate::observability::metrics::GraphCacheResult::Miss,
                );
                return Ok(IngestAttemptMutationOutcome::Applied(None));
            }
        }
        ensure_not_cancelled(cancellation_token)?;
        let persistence =
            persist_reused_graph_extraction(materialization, chunk, &record, &repaired_json)
                .await?;
        if matches!(persistence, IngestAttemptMutationOutcome::AuthorityLost) {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
        let (entity_count, relation_count) = graph_candidate_counts(&repaired_json);
        crate::observability::metrics::record_graph_extraction_cache(
            crate::observability::metrics::GraphCacheResult::Hit,
        );
        Ok(IngestAttemptMutationOutcome::Applied(Some((entity_count, relation_count))))
    }

    async fn materialize_cached_graph_extractions(
        &self,
        materialization: CachedGraphExtractionMaterialization<'_>,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<GraphExtractionCachePlan>> {
        let mut plan = GraphExtractionCachePlan::default();
        for chunk in materialization.chunks {
            let cached =
                self.reuse_cached_graph_extraction_for_chunk(&materialization, chunk).await?;
            let cached = match cached {
                IngestAttemptMutationOutcome::Applied(cached) => cached,
                IngestAttemptMutationOutcome::AuthorityLost => {
                    return Ok(IngestAttemptMutationOutcome::AuthorityLost);
                }
            };
            let Some((entity_count, relation_count)) = cached else {
                continue;
            };
            plan.reused_entity_count = plan.reused_entity_count.saturating_add(entity_count);
            plan.reused_relation_count = plan.reused_relation_count.saturating_add(relation_count);
            plan.reused_chunk_ids.insert(chunk.chunk_id);
        }
        Ok(IngestAttemptMutationOutcome::Applied(plan))
    }
}

pub(super) fn log_graph_materialization_cache(
    command: &MaterializeRevisionGraphCandidatesCommand,
    selection: &GraphMaterializationSelection,
    cache: &GraphExtractionCachePlan,
) {
    if cache.reused_chunk_ids.is_empty() {
        return;
    }
    tracing::info!(
        revision_id = %command.authority.revision_id,
        total_chunks = selection.chunk_count,
        selected_graph_chunks = selection.selected_graph_chunk_count,
        reused = cache.reused_chunk_ids.len(),
        reused_entities = cache.reused_entity_count,
        reused_relations = cache.reused_relation_count,
        "graph extraction cache: reusing ready extraction output",
    );
}

async fn persist_reused_graph_extraction(
    materialization: &CachedGraphExtractionMaterialization<'_>,
    chunk: &KnowledgeChunkRow,
    record: &crate::infra::repositories::RuntimeGraphExtractionRecordRow,
    repaired_normalized_output_json: &serde_json::Value,
) -> anyhow::Result<IngestAttemptMutationOutcome<()>> {
    let CachedGraphExtractionMaterialization {
        state, command, revision, cancellation_token, ..
    } = materialization;
    let raw_output_json = graph_cache_reuse_raw_output(
        record,
        command.authority.revision_id,
        Some(command.authority.attempt_id),
    );
    let outcome = crate::infra::repositories::runtime_graph_repository::create_runtime_graph_extraction_record_fenced(
        &state.persistence.postgres,
        command.authority,
        &crate::infra::repositories::CreateRuntimeGraphExtractionRecordInput {
            id: Uuid::now_v7(),
            runtime_execution_id: record.runtime_execution_id,
            library_id: command.authority.library_id,
            document_id: revision.document_id,
            chunk_id: chunk.chunk_id,
            provider_kind: record.provider_kind.clone(),
            model_name: record.model_name.clone(),
            extraction_version: record.extraction_version.clone(),
            prompt_hash: record.prompt_hash.clone(),
            status: ASYNC_OP_STATUS_READY.to_string(),
            raw_output_json,
            normalized_output_json: repaired_normalized_output_json.clone(),
            glean_pass_count: 0,
            error_message: None,
        },
    )
    .await
    .map_err(|e| {
        ApiError::internal_with_log(e, "graph_cache: create_runtime_graph_extraction_record")
    })?;
    ensure_not_cancelled(cancellation_token)?;
    Ok(match outcome {
        IngestAttemptMutationOutcome::Applied(_) => IngestAttemptMutationOutcome::Applied(()),
        IngestAttemptMutationOutcome::AuthorityLost => IngestAttemptMutationOutcome::AuthorityLost,
    })
}

struct CachedGraphExtractionMaterialization<'a> {
    state: &'a AppState,
    command: &'a MaterializeRevisionGraphCandidatesCommand,
    document: &'a KnowledgeDocumentRow,
    revision: &'a KnowledgeRevisionRow,
    chunks: &'a [KnowledgeChunkRow],
    revision_facts: &'a [TypedTechnicalFact],
    library_extraction_prompt: &'a Option<String>,
    sub_type_hints: &'a GraphExtractionSubTypeHints,
    table_graph_context: &'a RevisionTableGraphContext,
    graph_chunk_policy: &'a GraphExtractionChunkPolicy,
    provider_kind: &'a str,
    model_name: &'a str,
    runtime_binding: &'a ResolvedRuntimeBinding,
    classifier_identity: GraphEntityTypeClassifierIdentity<'a>,
    request_size_soft_limit_bytes: usize,
    cancellation_token: &'a CancellationToken,
}

#[derive(Debug, Default)]
pub(super) struct GraphExtractionCachePlan {
    pub(super) reused_chunk_ids: BTreeSet<Uuid>,
    pub(super) reused_entity_count: usize,
    pub(super) reused_relation_count: usize,
}

fn graph_cache_reuse_raw_output(
    record: &crate::infra::repositories::RuntimeGraphExtractionRecordRow,
    revision_id: Uuid,
    activated_by_attempt_id: Option<Uuid>,
) -> serde_json::Value {
    let mut raw_output_json = record.raw_output_json.clone();
    if let Some(obj) = raw_output_json.as_object_mut() {
        obj.insert(
            "lifecycle".to_string(),
            serde_json::json!({
                "revision_id": revision_id,
                "activated_by_attempt_id": activated_by_attempt_id,
            }),
        );
        obj.insert(
            "reuse".to_string(),
            serde_json::json!({
                "source": "graph_extraction_cache",
                "source_extraction_id": record.id,
                "source_chunk_id": record.chunk_id,
            }),
        );
    }
    raw_output_json
}

fn graph_candidate_counts(normalized_output_json: &serde_json::Value) -> (usize, usize) {
    let entity_count = normalized_output_json
        .get("entities")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    let relation_count = normalized_output_json
        .get("relations")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    (entity_count, relation_count)
}
