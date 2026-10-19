use anyhow::Context;

use super::{
    AppState, BTreeSet, ChunkRetrievalLaneOutcome, CompanionRetrievalPlan, DateTime,
    FOCUS_BROADEN_MIN_CHUNKS, HashMap, KnowledgeDocumentRow, LaneResolution, QueryIR, QueryScope,
    RetrievalLane, RuntimeMatchedChunk, RuntimeQueryPlan, RuntimeVectorSearchContext, Utc, Uuid,
    repositories, resolve_lane_result, resolve_scoped_target_document_ids,
    resolve_text_search_config, retrieve_document_chunks_with_targets,
};

/// Wrap a companion chunk-loader future so it emits a `lane`-kind turn span
/// (elapsed + returned chunk count) into the active inspector sink. The spans
/// surface which retrieval companion lane was heavy on a given turn without
/// changing the loader's own return contract.
pub(super) async fn timed_lane<Fut>(
    name: &'static str,
    fut: Fut,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>>
where
    Fut: std::future::Future<Output = anyhow::Result<Vec<RuntimeMatchedChunk>>>,
{
    let started = std::time::Instant::now();
    let result = fut.await;
    crate::services::query::turn_spans::record_span(
        name,
        "lane",
        started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        None,
        result.as_ref().ok().map(|chunks| chunks.len() as u64),
    );
    result
}

/// Wrap any future so it emits a `stage`-kind turn span (elapsed_ms) into the
/// active inspector sink. Complements `timed_lane` for retrieve phases that do
/// not return `Vec<RuntimeMatchedChunk>` (merges, graph evidence, index loads).
pub(super) async fn timed_stage<Fut>(name: &'static str, fut: Fut) -> Fut::Output
where
    Fut: std::future::Future,
{
    let started = std::time::Instant::now();
    let output = fut.await;
    crate::services::query::turn_spans::record_span(
        name,
        "stage",
        started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        None,
        None,
    );
    output
}

/// Record a `stage`-kind span for a synchronous block that already measured its
/// own elapsed time.
pub(super) fn record_stage_elapsed(name: &'static str, started: std::time::Instant) {
    crate::services::query::turn_spans::record_span(
        name,
        "stage",
        started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        None,
        None,
    );
}

pub(super) struct RetrievalExecutionRequest<'a> {
    pub(super) state: &'a AppState,
    pub(super) library_id: Uuid,
    pub(super) question: &'a str,
    pub(super) targeted_document_ids: BTreeSet<Uuid>,
    pub(super) runtime_plan: &'a RuntimeQueryPlan,
    pub(super) limit: usize,
    pub(super) question_embedding: &'a [f32],
    pub(super) vector_search_context: Option<&'a RuntimeVectorSearchContext>,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) query_ir: Option<&'a QueryIR>,
    pub(super) allow_broaden: bool,
    pub(super) text_search_config: &'a str,
    pub(super) reusable_primary: Option<ChunkRetrievalLaneOutcome>,
}

pub(super) struct InitialCompanionRequest<'a> {
    pub(super) state: &'a AppState,
    pub(super) library_id: Uuid,
    pub(super) question: &'a str,
    pub(super) targeted_document_ids: &'a BTreeSet<Uuid>,
    pub(super) plan_keywords: &'a [String],
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) query_ir: Option<&'a QueryIR>,
    pub(super) query_ir_focus_queries: &'a [String],
    pub(super) temporal_start: Option<DateTime<Utc>>,
    pub(super) temporal_end: Option<DateTime<Utc>>,
    pub(super) primary_evidence_available: bool,
}

pub(super) struct InitialCompanionChunks {
    pub(super) retrieval_plan: CompanionRetrievalPlan,
    pub(super) latest_version_document_ids: BTreeSet<Uuid>,
    pub(super) document_identity: Vec<RuntimeMatchedChunk>,
    pub(super) latest_version: Vec<RuntimeMatchedChunk>,
    pub(super) latest_version_semantic: Vec<RuntimeMatchedChunk>,
    pub(super) entity_bio: Vec<RuntimeMatchedChunk>,
    pub(super) query_ir_focus: Vec<RuntimeMatchedChunk>,
    pub(super) content_anchor: Vec<RuntimeMatchedChunk>,
    pub(super) document_evidence_anchor: Vec<RuntimeMatchedChunk>,
    pub(super) versioned_update_procedure: Vec<RuntimeMatchedChunk>,
    pub(super) setup_focus_document: Vec<RuntimeMatchedChunk>,
    pub(super) setup_variant_document: Vec<RuntimeMatchedChunk>,
}

pub(super) struct MergedCompanionChunks {
    pub(super) chunks: Vec<RuntimeMatchedChunk>,
    pub(super) retrieval_plan: CompanionRetrievalPlan,
    pub(super) latest_version_document_ids: BTreeSet<Uuid>,
    pub(super) protected_document_ids: BTreeSet<Uuid>,
}

pub(super) struct PostMergeCompanionRequest<'a> {
    pub(super) state: &'a AppState,
    pub(super) library_id: Uuid,
    pub(super) question: &'a str,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) plan_keywords: &'a [String],
    pub(super) query_ir: Option<&'a QueryIR>,
    pub(super) temporal_start: Option<DateTime<Utc>>,
    pub(super) temporal_end: Option<DateTime<Utc>>,
    pub(super) limit: usize,
    pub(super) primary_evidence_available: bool,
}

pub(super) struct FinalChunkShapingRequest<'a> {
    pub(super) state: &'a AppState,
    pub(super) targeted_document_ids: &'a BTreeSet<Uuid>,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) plan_keywords: &'a [String],
    pub(super) query_ir: Option<&'a QueryIR>,
    pub(super) temporal_start: Option<DateTime<Utc>>,
    pub(super) temporal_end: Option<DateTime<Utc>>,
    pub(super) limit: usize,
    pub(super) initial_table_row_count: Option<usize>,
    pub(super) targeted_table_aggregation: bool,
}

/// Storage and plan context shared by every companion retrieval lane: the
/// application state, the question, the document index and plan keywords a lane
/// filters by, and the resolved temporal bounds.
#[derive(Clone, Copy)]
pub(super) struct RetrievalLaneContext<'a> {
    pub(super) state: &'a AppState,
    pub(super) question: &'a str,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) plan_keywords: &'a [String],
    pub(super) temporal_start: Option<DateTime<Utc>>,
    pub(super) temporal_end: Option<DateTime<Utc>>,
}

/// Inputs of one plain document-chunk retrieval pass: the query turn, its plan
/// limits and the embedding/vector context the primary lanes search with.
pub(crate) struct DocumentChunkRetrievalRequest<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) library_id: Uuid,
    pub(crate) question: &'a str,
    pub(crate) forced_target_document_ids: Option<&'a BTreeSet<Uuid>>,
    pub(crate) plan: &'a RuntimeQueryPlan,
    pub(crate) limit: usize,
    pub(crate) question_embedding: &'a [f32],
    pub(crate) vector_search_context: Option<&'a RuntimeVectorSearchContext>,
    pub(crate) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(crate) query_ir: Option<&'a QueryIR>,
}

pub(super) enum PlannedLaneRun<T> {
    Skipped,
    Launched { spec: super::super::retrieval_plan::LaneSpec, result: anyhow::Result<T> },
}

pub(super) async fn run_planned_chunk_lane<Fut>(
    plan: &CompanionRetrievalPlan,
    lane: RetrievalLane,
    future: Fut,
) -> PlannedLaneRun<Vec<RuntimeMatchedChunk>>
where
    Fut: std::future::Future<Output = anyhow::Result<Vec<RuntimeMatchedChunk>>>,
{
    let Some(spec) = plan.spec(lane) else {
        return PlannedLaneRun::Skipped;
    };
    tracing::debug!(
        stage = "retrieval.companion_lane_launched",
        lane = lane.name(),
        criticality = ?spec.criticality,
        "launched planned companion retrieval lane"
    );
    PlannedLaneRun::Launched { spec, result: timed_lane(lane.span_name(), future).await }
}

pub(super) fn resolve_planned_chunk_lane(
    run: PlannedLaneRun<Vec<RuntimeMatchedChunk>>,
    primary_evidence_available: bool,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let PlannedLaneRun::Launched { spec, result } = run else {
        return Ok(Vec::new());
    };
    match resolve_lane_result(spec, result, primary_evidence_available)? {
        LaneResolution::Ready(chunks) => Ok(chunks),
        LaneResolution::Degraded(error) => {
            tracing::warn!(
                stage = "retrieval.companion_lane_degraded",
                lane = spec.lane.name(),
                criticality = ?spec.criticality,
                error = %format!("{error:#}"),
                retrieval_degraded = true,
                primary_evidence_available,
                "optional companion retrieval lane failed; retaining primary evidence"
            );
            Ok(Vec::new())
        }
    }
}

pub(crate) async fn retrieve_document_chunks(
    request: DocumentChunkRetrievalRequest<'_>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let DocumentChunkRetrievalRequest {
        state,
        library_id,
        question,
        forced_target_document_ids,
        plan,
        limit,
        question_embedding,
        vector_search_context,
        document_index,
        query_ir,
    } = request;
    // Load the library's retrieval config to derive the lexical text-search
    // A retrieval pass requires an existing library with persisted retrieval
    // configuration; missing rows/configuration are integrity errors.
    let stored_retrieval_config = repositories::catalog_repository::get_library_by_id(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .context("failed to load library retrieval config for chunk search")?
    .ok_or_else(|| {
        anyhow::Error::new(crate::services::query::error::QueryServiceError::LibraryNotFound {
            library_id,
        })
    })?
    .retrieval_config;
    let text_search_config = resolve_text_search_config(Some(stored_retrieval_config))
        .context("failed to plan lexical retrieval from stored library configuration")?;
    let forced_pin = forced_target_document_ids.is_some_and(|ids| !ids.is_empty());
    let targeted_document_ids = forced_target_document_ids
        .filter(|ids| !ids.is_empty())
        .cloned()
        .unwrap_or_else(|| resolve_scoped_target_document_ids(question, query_ir, document_index));
    // P3 coverage fallback is armed only for a *compiler-inferred*
    // single-document focus pin. An explicit document reference (forced
    // target, or a title/filename the user literally named) is an intent
    // signal we must honour even when the doc is thin — broadening it would
    // answer "tell me about doc X" from the whole library. An inferred pin,
    // by contrast, can hard-lock onto a thin same-titled stub and exclude
    // the real procedure docs; if it comes back near-empty we re-broaden.
    let pin_is_inferred = !forced_pin
        && !targeted_document_ids.is_empty()
        && query_ir.is_some_and(query_ir_focus_pin_is_inferred);
    Box::pin(retrieve_document_chunks_with_targets(RetrievalExecutionRequest {
        state,
        library_id,
        question,
        targeted_document_ids,
        runtime_plan: plan,
        limit,
        question_embedding,
        vector_search_context,
        document_index,
        query_ir,
        allow_broaden: pin_is_inferred,
        text_search_config: &text_search_config,
        reusable_primary: None,
    }))
    .await
}

/// `true` when the single-document focus pin was derived from a
/// compiler-inferred `document_focus` hint rather than from an explicit
/// document reference the user named verbatim. Only inferred pins are
/// eligible for the P3 coverage-broaden fallback.
pub(super) fn query_ir_focus_pin_is_inferred(ir: &QueryIR) -> bool {
    matches!(ir.scope, QueryScope::SingleDocument)
        && ir.document_focus.is_some()
        && !ir.is_follow_up()
}

/// Decide whether a focus-locked retrieval came back too thin to trust and
/// the pin should be dropped for a single broad replan.
///
/// Fires only when the pin was compiler-inferred AND the narrowed retrieval
/// is at or below a small coverage floor — a genuinely thin stub, not merely
/// a small-but-sufficient document. Bounding it to near-empty results keeps
/// the fallback to at most one companion-lane replan inside the tool-call SLO.
/// The lexical/vector primary snapshot is reused and never searched twice.
pub(crate) fn should_broaden_focus(pin_is_inferred: bool, post_filter_chunk_count: usize) -> bool {
    pin_is_inferred && post_filter_chunk_count <= FOCUS_BROADEN_MIN_CHUNKS
}

#[cfg(test)]
#[path = "lane_execution_tests.rs"]
mod tests;
