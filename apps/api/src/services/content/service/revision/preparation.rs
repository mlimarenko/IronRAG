use super::*;

/// The exact extracted text whose SHA-256 fences structured preparation.
///
/// Normalized text is authoritative; source text is the fallback for plans the
/// normalizer did not rewrite. Every writer of the revision extract artifact
/// that feeds [`ContentService::prepare_and_persist_revision_structure`] must
/// persist this same string and its checksum — adapters such as `record_jsonl`
/// are not byte-identity, so hashing the raw source instead strands the
/// revision in `preparing` behind the finalization fence.
pub(crate) fn preparation_fence_text(extraction_plan: &FileExtractionPlan) -> String {
    extraction_plan
        .normalized_text
        .clone()
        .unwrap_or_else(|| extraction_plan.source_text.clone().unwrap_or_default())
}

impl ContentService {
    pub async fn prepare_and_persist_revision_structure(
        &self,
        state: &AppState,
        authority: IngestRevisionWriteAuthority,
        extraction_plan: &FileExtractionPlan,
        cancellation_token: &CancellationToken,
    ) -> Result<IngestAttemptMutationOutcome<PreparedRevisionPersistenceSummary>, ContentServiceError>
    {
        let revision_id = authority.revision_id;
        ensure_not_cancelled(cancellation_token)?;
        let revision = content_revision_repository::get_revision_by_id(
            &state.persistence.postgres,
            revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("revision", revision_id))?;
        if revision.workspace_id != authority.workspace_id
            || revision.library_id != authority.library_id
        {
            return Err(ContentServiceError::StateConflict {
                message: "ingest preparation authority does not match revision scope".to_string(),
            });
        }
        // Prepared artifacts intentionally are not reused. The clean-v1
        // schema has no private provenance field that can bind an old artifact
        // to this exact extracted text, and overloading public outline JSON
        // would corrupt its API contract. Deterministic rebuild plus the final
        // checksum/count fence below is fail-closed for legacy, partial, and
        // same-shaped artifacts without changing the applied stage migration.
        let source_text = extraction_plan.source_text.clone().unwrap_or_default();
        let normalized_text = preparation_fence_text(extraction_plan);
        let source_text_checksum = sha256_hex_text(&normalized_text);

        let prepare_structure_start = std::time::Instant::now();
        ensure_not_cancelled(cancellation_token)?;
        // Resolve the library's chunking template so we apply the correct profile.
        let library_row =
            catalog_repository::get_library_by_id(&state.persistence.postgres, revision.library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let chunking_template = library_row.as_ref().map_or(ChunkingTemplate::Naive, |lib| {
            ChunkingTemplate::from_db_str(&lib.chunking_template)
        });
        let preparation_service = StructuredPreparationService::with_template(chunking_template);
        let mut prepared = preparation_service
            .prepare_revision(
                PrepareStructuredRevisionCommand {
                    revision_id,
                    document_id: revision.document_id,
                    workspace_id: revision.workspace_id,
                    library_id: revision.library_id,
                    preparation_state: "prepared".to_string(),
                    normalization_profile: extraction_plan.normalization_profile.clone(),
                    source_format: extraction_plan.source_format_metadata.source_format.clone(),
                    language_code: None,
                    source_text,
                    normalized_text: normalized_text.clone(),
                    structure_hints: extraction_plan.structure_hints.clone(),
                    typed_fact_count: 0,
                    prepared_at: Utc::now(),
                },
                cancellation_token,
            )
            .map_err(|error| {
                ApiError::BadRequest(format!(
                    "structured preparation failed for {revision_id}: {error}"
                ))
            })?;
        let prepare_structure_elapsed_ms = prepare_structure_start.elapsed().as_millis() as i64;

        ensure_not_cancelled(cancellation_token)?;
        let extract_technical_facts_start = std::time::Instant::now();
        let extracted_facts = state.application_services.technical_facts.extract_from_blocks(
            ExtractTechnicalFactsCommand {
                revision_id,
                document_id: revision.document_id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                blocks: prepared.ordered_blocks.clone(),
            },
            cancellation_token,
        )?;
        prepared.prepared_revision.typed_fact_count =
            i32::try_from(extracted_facts.facts.len()).unwrap_or(i32::MAX);
        let extract_technical_facts_elapsed_ms =
            extract_technical_facts_start.elapsed().as_millis() as i64;

        ensure_not_cancelled(cancellation_token)?;
        let chunk_content_start = std::time::Instant::now();
        let now = Utc::now();
        let structured_revision_row = build_structured_revision_row(&revision, &prepared, now);
        let structured_block_rows = build_structured_block_rows(&revision, &prepared, now);
        ensure_not_cancelled(cancellation_token)?;
        let structured_artifact = state
            .document_store
            .replace_structured_revision_artifact_fenced(
                authority,
                &structured_revision_row,
                &structured_block_rows,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if matches!(structured_artifact, IngestAttemptMutationOutcome::AuthorityLost) {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }

        let pending_chunks = build_pending_chunks(&prepared, &normalized_text, cancellation_token)?;
        let projected_chunks = build_projected_chunks(&revision, &pending_chunks, revision_id);
        ensure_not_cancelled(cancellation_token)?;
        // Fence the delete before it becomes visible; chunk replacement bumps
        // the source version again in its own transaction after it succeeds.
        {
            let _vector_guard = state
                .application_services
                .search
                .vector_plane_write_guard(state, revision.library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            let expected_source_truth_version =
                catalog_repository::get_library_source_truth_version(
                    &state.persistence.postgres,
                    revision.library_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            let vector_delete = state
                .search_store
                .delete_attempt_owned_chunk_vectors_by_revision_fenced(
                    revision.library_id,
                    expected_source_truth_version,
                    authority.vector_fence(),
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            if matches!(vector_delete, IngestAttemptMutationOutcome::AuthorityLost) {
                return Ok(IngestAttemptMutationOutcome::AuthorityLost);
            }
            invalidate_library_embedding_profile_inventory(revision.library_id);
        }
        ensure_not_cancelled(cancellation_token)?;
        let created_chunks = content_chunk_repository::replace_chunks_with_projection_fenced(
            &state.persistence.postgres,
            authority,
            &projected_chunks,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let created_chunks = match created_chunks {
            IngestAttemptMutationOutcome::Applied(chunks) => chunks,
            IngestAttemptMutationOutcome::AuthorityLost => {
                return Ok(IngestAttemptMutationOutcome::AuthorityLost);
            }
        };
        ensure_not_cancelled(cancellation_token)?;
        let block_to_chunk_ids =
            map_block_to_chunk_ids(&created_chunks, &pending_chunks, cancellation_token)?;
        let technical_fact_rows = build_technical_fact_rows(
            &extracted_facts,
            &block_to_chunk_ids,
            now,
            cancellation_token,
        )?;
        let facts = state
            .document_store
            .replace_technical_facts_fenced(authority, &technical_fact_rows)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if matches!(facts, IngestAttemptMutationOutcome::AuthorityLost) {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
        ensure_not_cancelled(cancellation_token)?;
        let finalization = state
            .document_store
            .finalize_structured_revision_artifact_fenced(authority, &source_text_checksum)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if matches!(finalization, IngestAttemptMutationOutcome::AuthorityLost) {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }

        let chunk_content_elapsed_ms = chunk_content_start.elapsed().as_millis() as i64;

        Ok(IngestAttemptMutationOutcome::Applied(PreparedRevisionPersistenceSummary {
            prepared_revision: map_structured_revision_data(&prepared.prepared_revision),
            chunk_count: prepared.chunk_windows.len(),
            technical_fact_count: extracted_facts.facts.len(),
            technical_conflict_count: extracted_facts.conflicts.len(),
            normalization_profile: prepared.prepared_revision.normalization_profile.clone(),
            prepare_structure_elapsed_ms,
            chunk_content_elapsed_ms,
            extract_technical_facts_elapsed_ms,
        }))
    }
}

/// Builds the structured-revision row persisted by the preparation fence.
fn build_structured_revision_row(
    revision: &content_revision_repository::ContentRevisionRow,
    prepared: &crate::services::ingest::structured_preparation::PreparedStructuredRevision,
    now: DateTime<Utc>,
) -> KnowledgeStructuredRevisionRow {
    KnowledgeStructuredRevisionRow {
        revision_id: revision.id,
        workspace_id: revision.workspace_id,
        library_id: revision.library_id,
        document_id: revision.document_id,
        preparation_state: "preparing".to_string(),
        normalization_profile: prepared.prepared_revision.normalization_profile.clone(),
        source_format: prepared.prepared_revision.source_format.clone(),
        language_code: prepared.prepared_revision.language_code.clone(),
        block_count: prepared.prepared_revision.block_count,
        chunk_count: prepared.prepared_revision.chunk_count,
        typed_fact_count: prepared.prepared_revision.typed_fact_count,
        outline_json: serde_json::to_value(&prepared.prepared_revision.outline)
            .unwrap_or_else(|_| serde_json::json!([])),
        prepared_at: prepared.prepared_revision.prepared_at,
        updated_at: now,
    }
}

/// Projects the prepared blocks into persistable structured-block rows.
fn build_structured_block_rows(
    revision: &content_revision_repository::ContentRevisionRow,
    prepared: &crate::services::ingest::structured_preparation::PreparedStructuredRevision,
    now: DateTime<Utc>,
) -> Vec<KnowledgeStructuredBlockRow> {
    prepared
        .ordered_blocks
        .iter()
        .map(|block| KnowledgeStructuredBlockRow {
            block_id: block.block_id,
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            document_id: revision.document_id,
            revision_id: revision.id,
            ordinal: block.ordinal,
            block_kind: block.block_kind.as_str().to_string(),
            text: block.text.clone(),
            normalized_text: block.normalized_text.clone(),
            heading_trail: block.heading_trail.clone(),
            section_path: block.section_path.clone(),
            page_number: block.page_number,
            span_start: block.source_span.as_ref().map(|span| span.start_offset),
            span_end: block.source_span.as_ref().map(|span| span.end_offset),
            parent_block_id: block.parent_block_id,
            table_coordinates_json: block.table_coordinates.as_ref().map(|coordinates| {
                serde_json::to_value(coordinates).unwrap_or(serde_json::Value::Null)
            }),
            code_language: block.code_language.clone(),
            created_at: now,
            updated_at: now,
        })
        .collect::<Vec<_>>()
}

/// Locates each prepared chunk window in the normalized text and captures its
/// temporal bounds, producing the ordered rows to project and persist. Pure
/// given `prepared` and `normalized_text`; the only shared state is the
/// monotone `next_search_char` cursor, which keeps the search windows ordered.
fn build_pending_chunks(
    prepared: &crate::services::ingest::structured_preparation::PreparedStructuredRevision,
    normalized_text: &str,
    cancellation_token: &CancellationToken,
) -> Result<Vec<PendingChunkInsert>, ContentServiceError> {
    let mut next_search_char = 0usize;
    let mut pending_chunks = Vec::with_capacity(prepared.chunk_windows.len());
    for chunk in &prepared.chunk_windows {
        ensure_not_cancelled(cancellation_token)?;
        let (start_offset, end_offset) =
            locate_chunk_offsets(normalized_text, &chunk.content_text, next_search_char);
        next_search_char = end_offset;
        // Shared temporal-bounds extraction. JSONL chats stamp every
        // record with `occurred_at=ISO`; the helper aggregates MIN/MAX
        // across all records that landed in this chunk so retrieval
        // can hard-filter by `[t_start, t_end)` without parsing chunk
        // text at query time. Non-temporal sources (PDF/image/markdown)
        // return None and the columns stay NULL.
        let (occurred_at, occurred_until): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
            match crate::shared::extraction::record_jsonl::extract_chunk_temporal_bounds(
                &chunk.normalized_text,
            ) {
                Some((min, max)) => (Some(min), Some(max)),
                None => (None, None),
            };
        pending_chunks.push(PendingChunkInsert {
            chunk_index: chunk.chunk_index,
            start_offset: i32::try_from(start_offset).unwrap_or(i32::MAX),
            end_offset: i32::try_from(end_offset).unwrap_or(i32::MAX),
            token_count: chunk.token_count,
            chunk_kind: Some(chunk.chunk_kind.as_str().to_string()),
            content_text: chunk.content_text.clone(),
            normalized_text: chunk.normalized_text.clone(),
            text_checksum: sha256_hex_text(&chunk.normalized_text),
            support_block_ids: chunk.support_block_ids.clone(),
            section_path: chunk.section_path.clone(),
            heading_trail: chunk.heading_trail.clone(),
            literal_digest: chunk.literal_digest.clone(),
            quality_score: Some(chunk.quality_score),
            window_text: chunk.window_text.clone(),
            occurred_at,
            occurred_until,
        });
    }
    Ok(pending_chunks)
}

/// Projects pending chunk inserts into the content-chunk projection rows.
fn build_projected_chunks(
    revision: &content_revision_repository::ContentRevisionRow,
    pending_chunks: &[PendingChunkInsert],
    revision_id: Uuid,
) -> Vec<content_chunk_repository::NewContentChunkProjection> {
    pending_chunks
        .iter()
        .map(|chunk| content_chunk_repository::NewContentChunkProjection {
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            document_id: revision.document_id,
            revision_id,
            chunk_index: chunk.chunk_index,
            start_offset: chunk.start_offset,
            end_offset: chunk.end_offset,
            token_count: chunk.token_count,
            chunk_kind: chunk.chunk_kind.clone(),
            content_text: chunk.content_text.clone(),
            normalized_text: chunk.normalized_text.clone(),
            text_checksum: chunk.text_checksum.clone(),
            support_block_ids: chunk.support_block_ids.clone(),
            section_path: chunk.section_path.clone(),
            heading_trail: chunk.heading_trail.clone(),
            literal_digest: chunk.literal_digest.clone(),
            chunk_state: "ready".to_string(),
            text_generation: Some(i64::from(revision.revision_number)),
            vector_generation: None,
            quality_score: chunk.quality_score,
            window_text: chunk.window_text.clone(),
            occurred_at: chunk.occurred_at,
            occurred_until: chunk.occurred_until,
        })
        .collect::<Vec<_>>()
}

/// Maps each structured block to the created chunk ids that support it, walking
/// created chunks and pending inserts in parallel (their order is aligned).
fn map_block_to_chunk_ids(
    created_chunks: &[content_chunk_repository::ContentChunkRow],
    pending_chunks: &[PendingChunkInsert],
    cancellation_token: &CancellationToken,
) -> Result<std::collections::BTreeMap<Uuid, Vec<Uuid>>, ContentServiceError> {
    let mut block_to_chunk_ids = std::collections::BTreeMap::<Uuid, Vec<Uuid>>::new();
    for (chunk, pending_chunk) in created_chunks.iter().zip(pending_chunks.iter()) {
        ensure_not_cancelled(cancellation_token)?;
        for block_id in &pending_chunk.support_block_ids {
            block_to_chunk_ids.entry(*block_id).or_default().push(chunk.id);
        }
    }
    Ok(block_to_chunk_ids)
}

/// Builds one technical-fact row per extracted fact, resolving its support
/// chunk ids through the block-to-chunk map.
fn build_technical_fact_rows(
    extracted_facts: &crate::services::ingest::technical_facts::ExtractTechnicalFactsResult,
    block_to_chunk_ids: &std::collections::BTreeMap<Uuid, Vec<Uuid>>,
    now: DateTime<Utc>,
    cancellation_token: &CancellationToken,
) -> Result<Vec<KnowledgeTechnicalFactRow>, ContentServiceError> {
    let mut technical_fact_rows = Vec::with_capacity(extracted_facts.facts.len());
    for fact in &extracted_facts.facts {
        ensure_not_cancelled(cancellation_token)?;
        let support_chunk_ids = fact
            .support_block_ids
            .iter()
            .filter_map(|block_id| block_to_chunk_ids.get(block_id))
            .flatten()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        technical_fact_rows.push(KnowledgeTechnicalFactRow {
            fact_id: fact.fact_id,
            workspace_id: fact.workspace_id,
            library_id: fact.library_id,
            document_id: fact.document_id,
            revision_id: fact.revision_id,
            fact_kind: fact.fact_kind.as_str().to_string(),
            normalized_value_text: fact.normalized_value.normalized_string(),
            normalized_value_exact: fact
                .normalized_value
                .normalized_string()
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect(),
            normalized_value_json: serde_json::to_value(&fact.normalized_value)
                .unwrap_or(serde_json::Value::Null),
            display_value: fact.display_value.clone(),
            qualifiers_json: serde_json::to_value(&fact.qualifiers)
                .unwrap_or_else(|_| serde_json::json!([])),
            support_block_ids: fact.support_block_ids.clone(),
            support_chunk_ids,
            confidence: fact.confidence,
            extraction_kind: fact.extraction_kind.clone(),
            conflict_group_id: fact.conflict_group_id.clone(),
            created_at: fact.created_at,
            updated_at: now,
        });
    }
    Ok(technical_fact_rows)
}
