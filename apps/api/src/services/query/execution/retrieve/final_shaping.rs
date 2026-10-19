use anyhow::Context;

use super::{
    AppState, DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT, DIRECT_TABLE_AGGREGATION_ROW_LIMIT,
    DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT, DateTime, FinalChunkShapingRequest, HashSet,
    MergedCompanionChunks, PostMergeCompanionRequest, RetrievalLane, RetrievalLaneContext,
    RuntimeMatchedChunk, TABLE_SECTION_SIBLING_CHUNK_LIMIT,
    TABLE_SECTION_SIBLING_LIMIT_PER_SECTION, Utc, Uuid, load_artifact_sibling_source_chunks,
    load_initial_table_rows_for_documents, load_linked_anchor_context_chunks,
    load_table_rows_for_documents, load_table_section_sibling_chunks,
    load_table_summary_chunks_for_documents,
    load_versioned_update_procedure_source_local_runbook_chunks, merge_chunks,
    merge_query_ir_focus_chunks_for_query, merge_table_aggregation_chunks,
    merge_versioned_update_procedure_chunks_for_query, query_ir_focus_context_top_k,
    query_ir_requests_table_section_siblings, resolve_planned_chunk_lane, run_planned_chunk_lane,
    timed_stage, versioned_update_procedure_focus_terms,
};

pub(super) async fn load_post_merge_companion_chunks(
    mut merged: MergedCompanionChunks,
    request: PostMergeCompanionRequest<'_>,
) -> anyhow::Result<MergedCompanionChunks> {
    let PostMergeCompanionRequest {
        state,
        library_id,
        question,
        document_index,
        plan_keywords,
        query_ir,
        temporal_start,
        temporal_end,
        limit,
        primary_evidence_available,
    } = request;
    let focus_terms = versioned_update_procedure_focus_terms(question, query_ir, plan_keywords);
    let source_local_result = run_planned_chunk_lane(
        &merged.retrieval_plan,
        RetrievalLane::VersionedUpdateSourceLocal,
        load_versioned_update_procedure_source_local_runbook_chunks(
            state,
            &merged.chunks,
            document_index,
            &focus_terms,
            question,
            query_ir,
        ),
    )
    .await;
    let source_local_chunks =
        resolve_planned_chunk_lane(source_local_result, primary_evidence_available)?;
    if !source_local_chunks.is_empty() {
        merged
            .protected_document_ids
            .extend(source_local_chunks.iter().map(|chunk| chunk.document_id));
        merged.chunks = merge_versioned_update_procedure_chunks_for_query(
            merged.chunks,
            source_local_chunks,
            query_ir_focus_context_top_k(limit),
            query_ir,
        );
    }
    let lane_context = RetrievalLaneContext {
        state,
        question,
        document_index,
        plan_keywords,
        temporal_start,
        temporal_end,
    };
    let linked_anchor_result = run_planned_chunk_lane(
        &merged.retrieval_plan,
        RetrievalLane::LinkedAnchorContext,
        load_linked_anchor_context_chunks(lane_context, library_id, query_ir, &merged.chunks),
    )
    .await;
    let linked_anchor_chunks =
        resolve_planned_chunk_lane(linked_anchor_result, primary_evidence_available)?;
    if !linked_anchor_chunks.is_empty() {
        merged.chunks = merge_query_ir_focus_chunks_for_query(
            merged.chunks,
            linked_anchor_chunks,
            query_ir_focus_context_top_k(limit),
            query_ir,
        );
    }
    let artifact_sibling_result = run_planned_chunk_lane(
        &merged.retrieval_plan,
        RetrievalLane::ArtifactSiblingSource,
        load_artifact_sibling_source_chunks(state, &merged.chunks, document_index, plan_keywords),
    )
    .await;
    let artifact_sibling_chunks =
        resolve_planned_chunk_lane(artifact_sibling_result, primary_evidence_available)?;
    if !artifact_sibling_chunks.is_empty() {
        merged.chunks = merge_query_ir_focus_chunks_for_query(
            merged.chunks,
            artifact_sibling_chunks,
            query_ir_focus_context_top_k(limit),
            query_ir,
        );
    }
    Ok(merged)
}

pub(super) async fn apply_temporal_post_filter(
    state: &AppState,
    mut chunks: Vec<RuntimeMatchedChunk>,
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if temporal_start.is_none() || temporal_end.is_none() || chunks.is_empty() {
        return Ok(chunks);
    }
    let chunk_ids: Vec<Uuid> = chunks.iter().map(|chunk| chunk.chunk_id).collect();
    let rows = state
        .document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to look up chunks for temporal post-filter")?;
    let allowed: HashSet<Uuid> = rows
        .into_iter()
        .filter(|row| {
            let Some(at) = row.occurred_at else {
                return false;
            };
            if let Some(start) = temporal_start
                && row.occurred_until.unwrap_or(at) < start
            {
                return false;
            }
            if let Some(end) = temporal_end
                && at >= end
            {
                return false;
            }
            true
        })
        .map(|row| row.chunk_id)
        .collect();
    let before = chunks.len();
    chunks.retain(|chunk| allowed.contains(&chunk.chunk_id));
    tracing::info!(
        stage = "retrieval.temporal_post_filter",
        before,
        after = chunks.len(),
        "applied temporal hard-filter to companion-path chunks"
    );
    Ok(chunks)
}

pub(super) async fn apply_final_chunk_shaping(
    mut chunks: Vec<RuntimeMatchedChunk>,
    request: FinalChunkShapingRequest<'_>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let FinalChunkShapingRequest {
        state,
        targeted_document_ids,
        document_index,
        plan_keywords,
        query_ir,
        temporal_start,
        temporal_end,
        limit,
        initial_table_row_count,
        targeted_table_aggregation,
    } = request;
    chunks = timed_stage(
        "retrieve.temporal_filter",
        apply_temporal_post_filter(state, chunks, temporal_start, temporal_end),
    )
    .await?;
    if let Some(row_count) = initial_table_row_count {
        let initial_rows = load_initial_table_rows_for_documents(
            state,
            document_index,
            targeted_document_ids,
            row_count,
            plan_keywords,
        )
        .await?;
        chunks = merge_chunks(chunks, initial_rows, limit.max(row_count));
    }
    if targeted_table_aggregation {
        let direct_summary_chunks = timed_stage(
            "retrieve.table_summary",
            load_table_summary_chunks_for_documents(
                state,
                document_index,
                targeted_document_ids,
                DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT,
                plan_keywords,
            ),
        )
        .await?;
        let direct_row_chunks = timed_stage(
            "retrieve.table_rows",
            load_table_rows_for_documents(
                state,
                document_index,
                targeted_document_ids,
                DIRECT_TABLE_AGGREGATION_ROW_LIMIT,
                plan_keywords,
            ),
        )
        .await?;
        chunks = merge_table_aggregation_chunks(
            chunks,
            direct_summary_chunks,
            direct_row_chunks,
            limit.max(DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT),
        );
    }
    if query_ir.is_some_and(query_ir_requests_table_section_siblings) {
        let table_section_chunks = timed_stage(
            "retrieve.table_section_siblings",
            load_table_section_sibling_chunks(
                state,
                document_index,
                &chunks,
                TABLE_SECTION_SIBLING_LIMIT_PER_SECTION,
                plan_keywords,
            ),
        )
        .await?;
        if !table_section_chunks.is_empty() {
            chunks = merge_chunks(
                chunks,
                table_section_chunks,
                limit.max(TABLE_SECTION_SIBLING_CHUNK_LIMIT),
            );
        }
    }
    Ok(chunks)
}
