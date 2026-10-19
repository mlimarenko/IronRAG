use anyhow::Context;

use super::{
    BTreeSet, CONTENT_ANCHOR_CHUNK_CAP, CompanionRetrievalPlan,
    DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT, DOCUMENT_IDENTITY_FOCUSED_CHUNKS_PER_DOCUMENT,
    ENTITY_BIO_CHUNK_CAP, InitialCompanionChunks, InitialCompanionRequest, LatestSelectionKind,
    LatestVersionSemanticRequest, MergedCompanionChunks, QueryAct, QueryIR, RetrievalLane,
    RetrievalLaneContext, RetrievalPlanningContext, RuntimeMatchedChunk, SETUP_VARIANT_CHUNK_CAP,
    Uuid, explicit_content_anchor_requested, latest_version_context_top_k,
    latest_version_scope_terms, latest_version_scoped_document_ids, load_content_anchor_chunks,
    load_document_evidence_anchor_chunks, load_document_identity_chunks_for_targets,
    load_entity_bio_chunks, load_latest_version_document_chunks,
    load_latest_version_semantic_document_chunks, load_query_ir_focus_chunks,
    load_setup_focus_document_chunks, load_setup_variant_document_chunks,
    load_versioned_update_procedure_chunks, merge_chunks, merge_entity_bio_chunks,
    merge_query_ir_focus_chunks_for_query, merge_versioned_update_procedure_chunks_for_query,
    query_ir_focus_context_top_k, query_ir_has_setup_configuration_target,
    query_ir_requests_setup_focus_document_candidates, query_requests_latest_versions,
    question_requests_setup_variant_evidence,
    question_requests_versioned_update_procedure_evidence, resolve_planned_chunk_lane,
    run_planned_chunk_lane, select_latest_version_documents,
};

pub(super) async fn load_initial_companion_chunks(
    request: InitialCompanionRequest<'_>,
) -> anyhow::Result<InitialCompanionChunks> {
    let InitialCompanionRequest {
        state,
        library_id,
        question,
        targeted_document_ids,
        plan_keywords,
        document_index,
        query_ir,
        query_ir_focus_queries,
        temporal_start,
        temporal_end,
        primary_evidence_available,
        ..
    } = request;
    let lane_context = RetrievalLaneContext {
        state,
        question,
        document_index,
        plan_keywords,
        temporal_start,
        temporal_end,
    };
    let latest_version_selection = select_latest_version_documents(query_ir, document_index);
    let latest_version_requested_count = latest_version_selection.requested_count;
    let latest_version_documents = latest_version_selection.documents;
    let latest_selection_kind = if query_ir.is_some_and(query_requests_latest_versions) {
        LatestSelectionKind::Explicit
    } else {
        LatestSelectionKind::None
    };
    let has_explicit_content_anchor =
        explicit_content_anchor_requested(query_ir, !targeted_document_ids.is_empty());
    let versioned_update_intent =
        question_requests_versioned_update_procedure_evidence(question, query_ir);
    let setup_intent = query_ir.is_some_and(|query_ir| {
        (matches!(query_ir.act, QueryAct::ConfigureHow)
            || query_ir_has_setup_configuration_target(query_ir))
            && (query_ir_requests_setup_focus_document_candidates(query_ir)
                || question_requests_setup_variant_evidence(question, Some(query_ir)))
    });
    let retrieval_plan = CompanionRetrievalPlan::compile(RetrievalPlanningContext {
        query_ir,
        has_target_documents: !targeted_document_ids.is_empty(),
        has_focus_queries: !query_ir_focus_queries.is_empty(),
        has_content_anchor: has_explicit_content_anchor,
        has_document_evidence_anchor: !targeted_document_ids.is_empty()
            || query_ir.is_some_and(|query_ir| query_ir.document_focus.is_some()),
        latest_selection: latest_selection_kind,
        versioned_update_intent,
        setup_intent,
    });
    let latest_version_document_ids =
        latest_version_scoped_document_ids(&latest_version_documents, &[]);
    tracing::info!(
        stage = "retrieval.plan",
        planned_lane_count = retrieval_plan.planned_count(),
        skipped_lane_count = retrieval_plan.skipped_count(),
        planned_lanes = %retrieval_plan.planned_names(),
        skipped_lanes = %retrieval_plan.skipped_names(),
        primary_evidence_available,
        "compiled structural companion retrieval plan"
    );
    let latest_version_scope_terms = query_ir.map(latest_version_scope_terms).unwrap_or_default();
    let has_explicit_source_tail =
        query_ir.and_then(|query_ir| query_ir.source_slice.as_ref()).is_some_and(|source_slice| {
            matches!(source_slice.direction, crate::domains::query_ir::SourceSliceDirection::Tail)
        });
    let prefers_source_tail =
        has_explicit_source_tail || query_ir.is_some_and(query_requests_latest_versions);
    let allows_unscoped_density_fallback = prefers_source_tail && !has_explicit_source_tail;
    let (
        document_identity,
        latest_version,
        latest_version_semantic,
        entity_bio,
        query_ir_focus,
        content_anchor,
        document_evidence_anchor,
        versioned_update_procedure,
        setup_focus_document,
        setup_variant_document,
    ) = tokio::join!(
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::DocumentIdentity,
            load_document_identity_chunks_for_targets(
                state,
                document_index,
                targeted_document_ids,
                plan_keywords,
                query_ir,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::LatestVersion,
            load_latest_version_document_chunks(
                state,
                document_index,
                plan_keywords,
                latest_version_requested_count,
                &latest_version_documents,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::LatestVersionSemantic,
            load_latest_version_semantic_document_chunks(
                lane_context,
                LatestVersionSemanticRequest {
                    requested_count: latest_version_requested_count,
                    scope_terms: &latest_version_scope_terms,
                    explicit_latest_requested: query_ir.is_some_and(query_requests_latest_versions),
                    prefer_source_tail: prefers_source_tail,
                    allow_unscoped_density_fallback: allows_unscoped_density_fallback,
                },
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::EntityBio,
            load_entity_bio_chunks(
                state,
                library_id,
                query_ir,
                document_index,
                plan_keywords,
                targeted_document_ids,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::QueryIrFocus,
            load_query_ir_focus_chunks(
                lane_context,
                library_id,
                query_ir_focus_queries,
                targeted_document_ids,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::ContentAnchor,
            load_content_anchor_chunks(lane_context, query_ir, targeted_document_ids),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::DocumentEvidenceAnchor,
            load_document_evidence_anchor_chunks(lane_context, query_ir),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::VersionedUpdateProcedure,
            load_versioned_update_procedure_chunks(lane_context, library_id, query_ir),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::SetupFocus,
            load_setup_focus_document_chunks(
                state,
                query_ir,
                document_index,
                plan_keywords,
                temporal_start,
                temporal_end,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::SetupVariant,
            load_setup_variant_document_chunks(lane_context, query_ir),
        ),
    );
    Ok(InitialCompanionChunks {
        retrieval_plan,
        latest_version_document_ids,
        document_identity: resolve_planned_chunk_lane(
            document_identity,
            primary_evidence_available,
        )?,
        latest_version: resolve_planned_chunk_lane(latest_version, primary_evidence_available)?,
        latest_version_semantic: resolve_planned_chunk_lane(
            latest_version_semantic,
            primary_evidence_available,
        )?,
        entity_bio: resolve_planned_chunk_lane(entity_bio, primary_evidence_available)?,
        query_ir_focus: resolve_planned_chunk_lane(query_ir_focus, primary_evidence_available)?,
        content_anchor: resolve_planned_chunk_lane(content_anchor, primary_evidence_available)?,
        document_evidence_anchor: resolve_planned_chunk_lane(
            document_evidence_anchor,
            primary_evidence_available,
        )?,
        versioned_update_procedure: resolve_planned_chunk_lane(
            versioned_update_procedure,
            primary_evidence_available,
        )?,
        setup_focus_document: resolve_planned_chunk_lane(
            setup_focus_document,
            primary_evidence_available,
        )?,
        setup_variant_document: resolve_planned_chunk_lane(
            setup_variant_document,
            primary_evidence_available,
        )?,
    })
}

pub(super) fn merge_optional_focus_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    companion_chunks: Vec<RuntimeMatchedChunk>,
    merged_limit: usize,
    query_ir: Option<&QueryIR>,
    protected_document_ids: &mut BTreeSet<Uuid>,
) -> Vec<RuntimeMatchedChunk> {
    if companion_chunks.is_empty() {
        return chunks;
    }
    protected_document_ids.extend(companion_chunks.iter().map(|chunk| chunk.document_id));
    merge_query_ir_focus_chunks_for_query(chunks, companion_chunks, merged_limit, query_ir)
}

pub(super) fn merge_initial_companion_chunks(
    mut chunks: Vec<RuntimeMatchedChunk>,
    initial: InitialCompanionChunks,
    targeted_document_ids: &BTreeSet<Uuid>,
    query_ir: Option<&QueryIR>,
    limit: usize,
    initial_table_row_count: Option<usize>,
) -> anyhow::Result<MergedCompanionChunks> {
    let InitialCompanionChunks {
        retrieval_plan,
        mut latest_version_document_ids,
        document_identity,
        mut latest_version,
        latest_version_semantic,
        entity_bio,
        query_ir_focus,
        content_anchor,
        document_evidence_anchor,
        versioned_update_procedure,
        setup_focus_document,
        setup_variant_document,
    } = initial;
    if !document_identity.is_empty() {
        let identity_budget_per_document =
            DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT + DOCUMENT_IDENTITY_FOCUSED_CHUNKS_PER_DOCUMENT;
        chunks = merge_chunks(
            chunks,
            document_identity,
            limit
                .max(initial_table_row_count.unwrap_or(0))
                .saturating_add(targeted_document_ids.len() * identity_budget_per_document),
        );
    }
    latest_version_document_ids
        .extend(latest_version_scoped_document_ids(&[], &latest_version_semantic));
    if !latest_version_semantic.is_empty() {
        let merged_limit = query_ir.map_or(limit, |ir| latest_version_context_top_k(ir, limit));
        latest_version = merge_chunks(latest_version, latest_version_semantic, merged_limit);
    }
    if !latest_version.is_empty() {
        let query_ir = query_ir
            .context("explicit latest-version lane returned chunks without a structured query")?;
        let latest_version_context_limit = limit
            .max(initial_table_row_count.unwrap_or(0))
            .saturating_add(latest_version_context_top_k(query_ir, 0));
        chunks = merge_chunks(chunks, latest_version, latest_version_context_limit);
    }
    if !entity_bio.is_empty() {
        chunks =
            merge_entity_bio_chunks(chunks, entity_bio, limit.saturating_add(ENTITY_BIO_CHUNK_CAP));
    }
    if !query_ir_focus.is_empty() {
        chunks = merge_query_ir_focus_chunks_for_query(
            chunks,
            query_ir_focus,
            query_ir_focus_context_top_k(limit),
            query_ir,
        );
    }
    let mut protected_document_ids = BTreeSet::new();
    chunks = merge_optional_focus_chunks(
        chunks,
        content_anchor,
        query_ir_focus_context_top_k(limit).saturating_add(CONTENT_ANCHOR_CHUNK_CAP),
        query_ir,
        &mut protected_document_ids,
    );
    chunks = merge_optional_focus_chunks(
        chunks,
        document_evidence_anchor,
        query_ir_focus_context_top_k(limit),
        query_ir,
        &mut protected_document_ids,
    );
    if !versioned_update_procedure.is_empty() {
        protected_document_ids
            .extend(versioned_update_procedure.iter().map(|chunk| chunk.document_id));
        chunks = merge_versioned_update_procedure_chunks_for_query(
            chunks,
            versioned_update_procedure,
            query_ir_focus_context_top_k(limit),
            query_ir,
        );
    }
    chunks = merge_optional_focus_chunks(
        chunks,
        setup_focus_document,
        query_ir_focus_context_top_k(limit),
        query_ir,
        &mut protected_document_ids,
    );
    chunks = merge_optional_focus_chunks(
        chunks,
        setup_variant_document,
        query_ir_focus_context_top_k(limit).max(SETUP_VARIANT_CHUNK_CAP),
        query_ir,
        &mut protected_document_ids,
    );
    Ok(MergedCompanionChunks {
        chunks,
        retrieval_plan,
        latest_version_document_ids,
        protected_document_ids,
    })
}
