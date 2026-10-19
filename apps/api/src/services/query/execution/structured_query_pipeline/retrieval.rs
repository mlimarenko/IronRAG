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
    let vector_search_embedding =
        planning.hyde_embedding.as_deref().unwrap_or(&planning.question_embedding);
    let question_embedding = vector_search_embedding;
    let vector_search_context = planning.vector_search_context.as_ref();
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

    let bundle_retrieval_started = std::time::Instant::now();
    // For Hybrid/Mix the graph-only bundle (entities/relationships) is
    // built here, but the document-chunk leg is deferred so it can run
    // concurrently with graph-evidence hydration below — both depend
    // only on the graph bundle plus shared read-only inputs, never on
    // each other.
    let (mut bundle, needs_document_chunks) = match plan.planned_mode {
        RuntimeQueryMode::Document => {
            let chunks = Box::pin(retrieve_document_chunks(DocumentChunkRetrievalRequest {
                state,
                library_id,
                question,
                forced_target_document_ids: locked_target_document_ids,
                plan,
                limit: candidate_limit,
                question_embedding,
                vector_search_context,
                document_index,
                query_ir,
            }))
            .await?;
            (RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks }, false)
        }
        RuntimeQueryMode::Local => (
            retrieve_local_bundle(GraphRetrievalRequest {
                state,
                library_id,
                plan,
                query_ir,
                target_entity_profiles: &target_entity_profiles,
                limit: candidate_limit,
                question_embedding,
                vector_search_context,
                graph_index,
            })
            .await?,
            false,
        ),
        RuntimeQueryMode::Global => (
            retrieve_global_bundle(GraphRetrievalRequest {
                state,
                library_id,
                plan,
                query_ir,
                target_entity_profiles: &target_entity_profiles,
                limit: candidate_limit,
                question_embedding,
                vector_search_context,
                graph_index,
            })
            .await?,
            false,
        ),
        RuntimeQueryMode::Hybrid => (
            retrieve_local_bundle(GraphRetrievalRequest {
                state,
                library_id,
                plan,
                query_ir,
                target_entity_profiles: &target_entity_profiles,
                limit: candidate_limit,
                question_embedding,
                vector_search_context,
                graph_index,
            })
            .await?,
            true,
        ),
        RuntimeQueryMode::Mix => (
            retrieve_mixed_graph_bundle(GraphRetrievalRequest {
                state,
                library_id,
                plan,
                query_ir,
                target_entity_profiles: &target_entity_profiles,
                limit: candidate_limit,
                question_embedding,
                vector_search_context,
                graph_index,
            })
            .await?,
            true,
        ),
    };
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
    let document_chunks_future = async {
        if needs_document_chunks {
            Box::pin(retrieve_document_chunks(DocumentChunkRetrievalRequest {
                state,
                library_id,
                question,
                forced_target_document_ids: locked_target_document_ids,
                plan,
                limit: candidate_limit,
                question_embedding,
                vector_search_context,
                document_index,
                query_ir,
            }))
            .await
            .map(Some)
        } else {
            Ok(None)
        }
    };
    let graph_evidence_future = load_graph_evidence_chunks_for_bundle(
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
    );
    let (document_chunks, graph_evidence) =
        tokio::try_join!(document_chunks_future, graph_evidence_future)?;
    if let Some(document_chunks) = document_chunks {
        bundle.chunks = document_chunks;
    }
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
    let stale_chunk_count = retain_current_document_head_chunks(&mut bundle.chunks, document_index);
    if stale_chunk_count > 0 {
        log_canonical_head_filtered(library_id, stale_chunk_count);
    }

    Ok(StructuredQueryRetrievalStage {
        planning,
        bundle,
        graph_evidence_context_lines: graph_evidence.context_lines,
        graph_evidence_source_document_ids: graph_evidence.source_document_ids,
    })
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
