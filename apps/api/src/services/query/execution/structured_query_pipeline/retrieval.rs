use uuid::Uuid;

use crate::{app::state::AppState, domains::query::RuntimeQueryMode};

use super::super::{
    document_target::resolve_scoped_target_document_ids,
    graph_retrieval::{
        GraphRetrievalRequest, graph_target_entity_profiles, retrieve_global_bundle,
        retrieve_local_bundle, retrieve_mixed_graph_bundle,
    },
    retrieve::{
        DocumentChunkRetrievalRequest, GraphEvidenceScope, graph_evidence_context_top_k,
        load_graph_evidence_chunks_for_bundle, merge_graph_evidence_chunks,
        query_ir_promotes_graph_evidence, retain_current_document_head_chunks,
        retrieve_document_chunks,
    },
    technical_literals::technical_literal_candidate_limit,
    types::{RetrievalBundle, StructuredQueryPlanningStage, StructuredQueryRetrievalStage},
};
use super::effective_technical_literal_intent;

/// Runs the document-chunk lane and reports its bundle and degradations.
async fn run_document_lane(
    request: DocumentChunkRetrievalRequest<'_>,
) -> anyhow::Result<(RetrievalBundle, Vec<super::super::types::RetrievalDegradation>)> {
    let outcome = Box::pin(retrieve_document_chunks(request)).await?;
    Ok((
        RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks: outcome.chunks },
        outcome.degradations,
    ))
}

/// Which graph retrieval lane a planned mode runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphRetrievalLane {
    Local,
    Global,
    Mixed,
}

/// Runs one graph retrieval lane and folds its vector degradation into the
/// reasons the caller reports.
async fn run_graph_lane(
    lane: GraphRetrievalLane,
    request: GraphRetrievalRequest<'_>,
    mut reasons: Vec<super::super::types::RetrievalDegradation>,
) -> anyhow::Result<(RetrievalBundle, Vec<super::super::types::RetrievalDegradation>)> {
    let outcome = match lane {
        GraphRetrievalLane::Local => retrieve_local_bundle(request).await?,
        GraphRetrievalLane::Global => retrieve_global_bundle(request).await?,
        GraphRetrievalLane::Mixed => retrieve_mixed_graph_bundle(request).await?,
    };
    add_graph_vector_degradation(&mut reasons, outcome.vector_unavailable);
    Ok((outcome.bundle, reasons))
}

/// Reads the deferred document-chunk lane for the modes that keep it.
async fn document_lane_future(
    context: &StructuredRetrievalContext<'_>,
    needs_document_chunks: bool,
) -> anyhow::Result<Option<super::super::retrieve::DocumentChunkRetrievalOutcome>> {
    if !needs_document_chunks {
        return Ok(None);
    }
    let planning = context.planning;
    Box::pin(retrieve_document_chunks(DocumentChunkRetrievalRequest {
        state: context.state,
        library_id: context.library_id,
        question: context.question,
        forced_target_document_ids: context.locked_target_document_ids,
        plan: &planning.plan,
        limit: planning.candidate_limit,
        question_embedding: planning
            .hyde_embedding
            .as_deref()
            .unwrap_or(&planning.question_embedding),
        vector_readiness: planning.vector_readiness,
        vector_search_context: planning.vector_search_context.as_ref(),
        document_index: &planning.document_index,
        query_ir: context.query_ir,
    }))
    .await
    .map(Some)
}

/// Folds the deferred document-chunk lane into the graph bundle, keeping its
/// degradations distinct from the graph lane's.
fn merge_document_lane_outcome(
    bundle: &mut RetrievalBundle,
    degradations: &mut Vec<super::super::types::RetrievalDegradation>,
    document_chunks: Option<super::super::retrieve::DocumentChunkRetrievalOutcome>,
) {
    let Some(document_chunks) = document_chunks else {
        return;
    };
    for reason in document_chunks.degradations {
        if !degradations.contains(&reason) {
            degradations.push(reason);
        }
    }
    bundle.chunks = document_chunks.chunks;
}

/// Reports the absent graph evidence store as a degradation rather than an
/// error.
fn mark_graph_evidence_unavailable(
    degradations: &mut Vec<super::super::types::RetrievalDegradation>,
) {
    degradations.push(super::super::types::RetrievalDegradation::GraphStore);
    tracing::warn!(
        stage = "retrieval.graph_evidence",
        retrieval_degraded = true,
        "graph evidence store unavailable"
    );
}

/// The read-only inputs a planned-mode retrieval carries through its lanes. The
/// planning stage is held whole so its plan and index types stay owned there.
struct StructuredRetrievalContext<'a> {
    state: &'a AppState,
    library_id: Uuid,
    question: &'a str,
    planning: &'a StructuredQueryPlanningStage,
    query_ir: Option<&'a crate::domains::query_ir::QueryIR>,
    target_entity_profiles: &'a [super::super::graph_retrieval::GraphTargetEntityProfile],
    locked_target_document_ids: Option<&'a std::collections::BTreeSet<Uuid>>,
}

impl StructuredRetrievalContext<'_> {
    /// The graph lane request every graph-backed mode shares.
    fn graph_request(&self) -> GraphRetrievalRequest<'_> {
        let planning = self.planning;
        GraphRetrievalRequest {
            state: self.state,
            library_id: self.library_id,
            plan: &planning.plan,
            query_ir: self.query_ir,
            target_entity_profiles: self.target_entity_profiles,
            limit: planning.candidate_limit,
            question_embedding: planning
                .hyde_embedding
                .as_deref()
                .unwrap_or(&planning.question_embedding),
            vector_search_context: planning.vector_search_context.as_ref(),
            graph_index: &planning.graph_index,
        }
    }

    /// Runs the lane the planned mode selects and reports whether the
    /// document-chunk leg is still deferred.
    async fn retrieve_mode_bundle(
        &self,
    ) -> anyhow::Result<(RetrievalBundle, bool, Vec<super::super::types::RetrievalDegradation>)>
    {
        let planning = self.planning;
        match planning.plan.planned_mode {
            RuntimeQueryMode::Document => {
                let (bundle, degradations) = run_document_lane(DocumentChunkRetrievalRequest {
                    state: self.state,
                    library_id: self.library_id,
                    question: self.question,
                    forced_target_document_ids: self.locked_target_document_ids,
                    plan: &planning.plan,
                    limit: planning.candidate_limit,
                    question_embedding: planning
                        .hyde_embedding
                        .as_deref()
                        .unwrap_or(&planning.question_embedding),
                    vector_readiness: planning.vector_readiness,
                    vector_search_context: planning.vector_search_context.as_ref(),
                    document_index: &planning.document_index,
                    query_ir: self.query_ir,
                })
                .await?;
                Ok((bundle, false, degradations))
            }
            RuntimeQueryMode::Local => {
                let (bundle, reasons) = run_graph_lane(
                    GraphRetrievalLane::Local,
                    self.graph_request(),
                    graph_primary_degradations(planning.vector_readiness),
                )
                .await?;
                Ok((bundle, false, reasons))
            }
            RuntimeQueryMode::Global => {
                let (bundle, reasons) = run_graph_lane(
                    GraphRetrievalLane::Global,
                    self.graph_request(),
                    graph_primary_degradations(planning.vector_readiness),
                )
                .await?;
                Ok((bundle, false, reasons))
            }
            RuntimeQueryMode::Hybrid | RuntimeQueryMode::Mix
                if planning.graph_store_unavailable =>
            {
                Ok((
                    RetrievalBundle {
                        entities: Vec::new(),
                        relationships: Vec::new(),
                        chunks: Vec::new(),
                    },
                    true,
                    Vec::new(),
                ))
            }
            RuntimeQueryMode::Hybrid => {
                let (bundle, reasons) =
                    run_graph_lane(GraphRetrievalLane::Local, self.graph_request(), Vec::new())
                        .await?;
                Ok((bundle, true, reasons))
            }
            RuntimeQueryMode::Mix => {
                let (bundle, reasons) =
                    run_graph_lane(GraphRetrievalLane::Mixed, self.graph_request(), Vec::new())
                        .await?;
                Ok((bundle, true, reasons))
            }
        }
    }
}

pub(crate) async fn retrieve_structured_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    mut planning: StructuredQueryPlanningStage,
    query_ir: Option<&crate::domains::query_ir::QueryIR>,
) -> anyhow::Result<StructuredQueryRetrievalStage> {
    let technical_literal_intent =
        effective_technical_literal_intent(question, query_ir, planning.technical_literal_intent);
    planning.technical_literal_intent = technical_literal_intent;
    planning.candidate_limit = planning
        .candidate_limit
        .max(technical_literal_candidate_limit(technical_literal_intent, planning.plan.top_k));

    let plan = &planning.plan;
    let graph_index = &planning.graph_index;
    let document_index = &planning.document_index;
    let target_profile_started = std::time::Instant::now();
    let target_entity_profiles = graph_target_entity_profiles(query_ir, graph_index);
    log_graph_target_profiles(
        target_entity_profiles.len(),
        target_profile_started.elapsed().as_millis(),
    );
    let candidate_limit = planning.candidate_limit;
    let document_filter_ids =
        resolve_scoped_target_document_ids(question, query_ir, document_index);
    let locked_target_document_ids =
        (!document_filter_ids.is_empty()).then_some(&document_filter_ids);
    ensure_required_graph_projection(plan.planned_mode, planning.graph_store_unavailable)?;

    let bundle_retrieval_started = std::time::Instant::now();
    // For Hybrid/Mix the graph-only bundle (entities/relationships) is
    // built here, but the document-chunk leg is deferred so it can run
    // concurrently with graph-evidence hydration below — both depend
    // only on the graph bundle plus shared read-only inputs, never on
    // each other.
    let retrieval_context = StructuredRetrievalContext {
        state,
        library_id,
        question,
        planning: &planning,
        query_ir,
        target_entity_profiles: &target_entity_profiles,
        locked_target_document_ids,
    };
    let (mut bundle, needs_document_chunks, mut degradations) =
        retrieval_context.retrieve_mode_bundle().await?;
    log_bundle_retrieved(
        library_id,
        plan.planned_mode,
        bundle_retrieval_started.elapsed().as_millis(),
        bundle.chunks.len(),
        bundle.entities.len(),
        bundle.relationships.len(),
    );

    // Document-chunk retrieval (Hybrid/Mix) and graph-evidence hydration
    // are independent reads off the same graph bundle; overlap them so
    // the turn pays the slower of the two, not their sum. Output is
    // identical: `merge_graph_evidence_chunks` runs only after both
    // complete and consumes both result sets explicitly.
    let graph_evidence_started = std::time::Instant::now();
    let document_chunks_future = document_lane_future(&retrieval_context, needs_document_chunks);
    let graph_evidence_future = async {
        if planning.graph_store_unavailable {
            return Err(super::super::graph_retrieval_error::GraphStoreUnavailable.into());
        }
        load_graph_evidence_chunks_for_bundle(
            GraphEvidenceScope {
                state,
                library_id,
                question,
                entities: &bundle.entities,
                relationships: &bundle.relationships,
                plan,
                query_ir,
                target_entity_profiles: &target_entity_profiles,
                graph_index,
            },
            document_index,
            &document_filter_ids,
            &plan.keywords,
        )
        .await
    };
    let (document_chunks, graph_evidence_result) =
        tokio::join!(document_chunks_future, graph_evidence_future);
    merge_document_lane_outcome(&mut bundle, &mut degradations, document_chunks?);
    let graph_evidence = resolve_optional_graph_evidence(plan.planned_mode, graph_evidence_result)?;
    let (graph_evidence_context_lines, graph_evidence_source_document_ids) =
        if let Some(graph_evidence) = graph_evidence {
            log_graph_evidence_loaded(
                library_id,
                graph_evidence_started.elapsed().as_millis(),
                graph_evidence.chunks.len(),
                bundle.chunks.len(),
            );
            let promote_graph_evidence = query_ir.is_some_and(query_ir_promotes_graph_evidence);
            if !graph_evidence.chunks.is_empty() && promote_graph_evidence {
                bundle.chunks = merge_graph_evidence_chunks(
                    std::mem::take(&mut bundle.chunks),
                    graph_evidence.chunks,
                    graph_evidence_context_top_k(candidate_limit),
                );
            } else if !graph_evidence.chunks.is_empty() {
                log_graph_evidence_demoted(library_id, graph_evidence.chunks.len());
            }
            (graph_evidence.context_lines, graph_evidence.source_document_ids)
        } else {
            mark_graph_evidence_unavailable(&mut degradations);
            (Vec::new(), Vec::new())
        };
    let stale_chunk_count = retain_current_document_head_chunks(&mut bundle.chunks, document_index);
    if stale_chunk_count > 0 {
        log_canonical_head_filtered(library_id, stale_chunk_count);
    }
    ensure_degraded_retrieval_has_evidence(&degradations, &bundle, &graph_evidence_context_lines)?;

    Ok(StructuredQueryRetrievalStage {
        planning,
        bundle,
        degradations,
        graph_evidence_context_lines,
        graph_evidence_source_document_ids,
    })
}

fn ensure_required_graph_projection(
    mode: RuntimeQueryMode,
    unavailable: bool,
) -> anyhow::Result<()> {
    if unavailable && matches!(mode, RuntimeQueryMode::Local | RuntimeQueryMode::Global) {
        return Err(crate::services::knowledge::runtime_read::RuntimeGraphStoreUnavailable.into());
    }
    Ok(())
}

fn ensure_degraded_retrieval_has_evidence(
    degradations: &[super::super::types::RetrievalDegradation],
    bundle: &RetrievalBundle,
    graph_context: &[super::super::types::SelectedGraphContextLine],
) -> anyhow::Result<()> {
    if !degradations.is_empty()
        && bundle.chunks.is_empty()
        && bundle.entities.is_empty()
        && bundle.relationships.is_empty()
        && graph_context.is_empty()
    {
        anyhow::bail!("retrieval degraded and returned no evidence");
    }
    Ok(())
}

fn resolve_optional_graph_evidence<T>(
    mode: RuntimeQueryMode,
    result: anyhow::Result<T>,
) -> anyhow::Result<Option<T>> {
    match result {
        Ok(evidence) => Ok(Some(evidence)),
        Err(error)
            if error
                .downcast_ref::<super::super::graph_retrieval_error::GraphStoreUnavailable>()
                .is_some()
                && !matches!(mode, RuntimeQueryMode::Local | RuntimeQueryMode::Global) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn graph_primary_degradations(
    readiness: super::super::types::QueryVectorReadiness,
) -> Vec<super::super::types::RetrievalDegradation> {
    if readiness == super::super::types::QueryVectorReadiness::ProviderUnavailable {
        vec![super::super::types::RetrievalDegradation::VectorProvider]
    } else {
        Vec::new()
    }
}

fn add_graph_vector_degradation(
    degradations: &mut Vec<super::super::types::RetrievalDegradation>,
    unavailable: bool,
) {
    let reason = super::super::types::RetrievalDegradation::VectorStore;
    if unavailable && !degradations.contains(&reason) {
        degradations.push(reason);
    }
}

fn log_graph_target_profiles(target_profile_count: usize, elapsed_ms: u128) {
    tracing::info!(
        stage = "retrieval.graph_target_profiles",
        target_profile_count,
        elapsed_ms,
        "graph target entity profiles prepared for query execution",
    );
}

fn log_bundle_retrieved(
    library_id: Uuid,
    planned_mode: RuntimeQueryMode,
    elapsed_ms: u128,
    chunk_count: usize,
    entity_count: usize,
    relationship_count: usize,
) {
    tracing::info!(
        stage = "retrieval.bundle",
        library_id = %library_id,
        planned_mode = ?planned_mode,
        elapsed_ms,
        chunk_count,
        entity_count,
        relationship_count,
        "bundle retrieval complete"
    );
}

fn log_graph_evidence_loaded(
    library_id: Uuid,
    elapsed_ms: u128,
    graph_chunk_count: usize,
    document_chunk_count: usize,
) {
    tracing::info!(
        stage = "retrieval.graph_evidence",
        library_id = %library_id,
        elapsed_ms,
        graph_chunk_count,
        document_chunk_count,
        "graph evidence + document chunks loaded concurrently"
    );
}

fn log_graph_evidence_demoted(library_id: Uuid, graph_chunk_count: usize) {
    tracing::info!(
        stage = "retrieval.graph_evidence_demoted",
        library_id = %library_id,
        graph_chunk_count,
        "graph evidence chunks omitted from answer-driving context for non-graph QueryIR"
    );
}

fn log_canonical_head_filtered(library_id: Uuid, stale_chunk_count: usize) {
    tracing::info!(
        stage = "retrieval.\u{63}anonical_head_filter",
        library_id = %library_id,
        stale_chunk_count,
        "removed non-head revision chunks from retrieval bundle"
    );
}

#[cfg(test)]
mod graph_degradation_tests {
    use super::*;
    use crate::services::knowledge::error::KnowledgeServiceError;

    #[test]
    fn graph_ann_failure_is_reported_once_in_retrieval_reasons() {
        let mut reasons = Vec::new();
        add_graph_vector_degradation(&mut reasons, true);
        add_graph_vector_degradation(&mut reasons, true);
        assert_eq!(reasons, vec![super::super::super::types::RetrievalDegradation::VectorStore]);
    }

    #[test]
    fn optional_graph_store_failure_is_typed_and_graph_primary_refuses_it() {
        for mode in [RuntimeQueryMode::Document, RuntimeQueryMode::Hybrid, RuntimeQueryMode::Mix] {
            let result = resolve_optional_graph_evidence::<()>(
                mode,
                Err(super::super::super::graph_retrieval_error::GraphStoreUnavailable.into()),
            )
            .expect("optional graph evidence may degrade");
            assert!(result.is_none());
        }
        for mode in [RuntimeQueryMode::Local, RuntimeQueryMode::Global] {
            let error = resolve_optional_graph_evidence::<()>(
                mode,
                Err(super::super::super::graph_retrieval_error::GraphStoreUnavailable.into()),
            )
            .expect_err("graph-primary retrieval requires graph evidence");
            assert!(
                error
                    .downcast_ref::<super::super::super::graph_retrieval_error::GraphStoreUnavailable>()
                    .is_some()
            );
        }
    }

    #[test]
    fn unavailable_projection_is_optional_only_for_document_capable_modes() {
        for mode in [RuntimeQueryMode::Document, RuntimeQueryMode::Hybrid, RuntimeQueryMode::Mix] {
            assert!(ensure_required_graph_projection(mode, true).is_ok());
        }
        for mode in [RuntimeQueryMode::Local, RuntimeQueryMode::Global] {
            let error = ensure_required_graph_projection(mode, true).expect_err("graph required");
            assert!(
                error
                    .downcast_ref::<crate::services::knowledge::runtime_read::RuntimeGraphStoreUnavailable>()
                    .is_some()
            );
            assert!(ensure_required_graph_projection(mode, false).is_ok());
        }
    }

    #[test]
    fn optional_graph_never_hides_not_ready_or_projection_corruption() {
        for error in [
            KnowledgeServiceError::GraphNotReady { message: "stale sources".into() },
            KnowledgeServiceError::GraphIntegrity { message: "invalid projection".into() },
        ] {
            assert!(
                resolve_optional_graph_evidence::<()>(
                    RuntimeQueryMode::Document,
                    Err(anyhow::Error::new(error)),
                )
                .is_err()
            );
        }
        assert_eq!(
            resolve_optional_graph_evidence(RuntimeQueryMode::Document, Ok(0))
                .expect("empty graph evidence is a valid result"),
            Some(0)
        );
    }

    #[test]
    fn graph_degradation_requires_remaining_authorized_evidence() {
        use super::super::super::types::{
            RetrievalDegradation, RuntimeChunkScoreKind, RuntimeMatchedChunk,
        };
        let mut bundle =
            RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks: Vec::new() };
        let reasons = [RetrievalDegradation::GraphStore];
        assert!(ensure_degraded_retrieval_has_evidence(&reasons, &bundle, &[]).is_err());
        assert!(ensure_degraded_retrieval_has_evidence(&[], &bundle, &[]).is_ok());
        bundle.chunks.push(RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_label: "document".into(),
            excerpt: "lexical evidence".into(),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: None,
            source_text: "lexical evidence".into(),
        });
        assert!(ensure_degraded_retrieval_has_evidence(&reasons, &bundle, &[]).is_ok());
    }
}
