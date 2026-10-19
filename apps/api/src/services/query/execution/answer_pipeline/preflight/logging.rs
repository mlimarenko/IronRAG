use uuid::Uuid;

use crate::domains::query_ir::QueryIR;

use super::super::super::consolidation::TopicalPruneDiagnostics;
use super::super::super::source_context::StructuredSourceContextDiagnostics;
use super::super::super::types::StructuredQueryPlanningStage;
use super::super::context_probes::{CompareContextProbeOutcome, TechnicalFocusProbeOutcome};
use super::FinalizedBundle;
pub(super) fn log_canonical_head_filter(library_id: Uuid, stale_chunk_count: usize, after: &str) {
    if stale_chunk_count == 0 {
        return;
    }
    tracing::info!(
        stage = "retrieval.\u{63}anonical_head_filter",
        library_id = %library_id,
        stale_chunk_count,
        "removed non-head revision chunks after {}",
        after
    );
}

pub(super) fn log_structured_source_context(
    library_id: Uuid,
    source_context: &StructuredSourceContextDiagnostics,
) {
    let has_companions = source_context.source_profile_count > 0
        || source_context.neighbor_count > 0
        || source_context.focused_match_count > 0
        || source_context.procedural_structured_sibling_count > 0
        || source_context.source_slice_count > 0;
    if !has_companions {
        return;
    }
    tracing::info!(
        stage = "retrieval.structured_source_context",
        library_id = %library_id,
        eligible_document_count = source_context.eligible_document_count,
        source_profile_count = source_context.source_profile_count,
        neighbor_count = source_context.neighbor_count,
        focused_match_count = source_context.focused_match_count,
        procedural_structured_sibling_count = source_context.procedural_structured_sibling_count,
        library_profile_count = source_context.library_profile_count,
        source_slice_count = source_context.source_slice_count,
        "structured source context companions added after consolidation"
    );
}

pub(super) fn log_topical_prune(library_id: Uuid, topical_prune: &TopicalPruneDiagnostics) {
    if topical_prune.removed_chunk_count == 0 {
        return;
    }
    tracing::info!(
        stage = "answer.topical_prune",
        library_id = %library_id,
        removed_chunk_count = topical_prune.removed_chunk_count,
        kept_chunk_count = topical_prune.kept_chunk_count,
        topical_token_count = topical_prune.topical_token_count,
        "pruned non-topical retrieval tail before answer context assembly"
    );
}

pub(super) fn log_plan_done(
    library_id: Uuid,
    plan_elapsed_ms: u128,
    planning_stage: &StructuredQueryPlanningStage,
) {
    tracing::info!(
        stage = "answer.plan_done",
        library_id = %library_id,
        elapsed_ms = plan_elapsed_ms,
        planned_mode = ?planning_stage.plan.planned_mode,
        "query compile+plan parallel step done"
    );
}

pub(super) fn log_retrieve_done(library_id: Uuid, elapsed_ms: u128) {
    tracing::info!(
        stage = "answer.retrieve_done",
        library_id = %library_id,
        elapsed_ms,
        "structured retrieval done"
    );
}

pub(super) fn log_rerank_done(library_id: Uuid, elapsed_ms: u128) {
    tracing::info!(
        stage = "answer.rerank_done",
        library_id = %library_id,
        elapsed_ms,
        "rerank done"
    );
}

pub(super) fn log_prepare_stages(
    library_id: Uuid,
    finalized: &FinalizedBundle,
    answer_context: &str,
    query_ir: &QueryIR,
    stage_1_elapsed_ms: u128,
    stage_2_elapsed_ms: u128,
) {
    tracing::info!(
        stage = "answer.prepare",
        library_id = %library_id,
        stage_1_compile_retrieval_ms = stage_1_elapsed_ms,
        stage_2_library_ms = stage_2_elapsed_ms,
        consolidation_ms = finalized.consolidation_elapsed_ms,
        consolidation_reason = finalized.consolidation_reason,
        consolidation_winner_chunks = finalized.consolidation_winner_chunks,
        consolidation_tangential_chunks = finalized.consolidation_tangential_chunks,
        topical_pruned_chunks = finalized.topical_pruned_chunks,
        retrieved_document_count = finalized.structured.retrieved_documents.len(),
        answer_context_chars = answer_context.chars().count(),
        query_ir_confidence = query_ir.confidence,
        query_ir_act = ?query_ir.act,
        "prepare_answer_query stages"
    );
}

pub(super) fn log_library_context_skipped(library_id: Uuid, error: &anyhow::Error) {
    tracing::warn!(
        error = %error,
        library_id = %library_id,
        "skipping non-critical query library context enrichment"
    );
}

pub(super) fn log_compare_context_probe(library_id: Uuid, outcome: &CompareContextProbeOutcome) {
    tracing::info!(
        stage = "answer.compare_context_probe",
        library_id = %library_id,
        missing_operand_count = outcome.missing_operand_count,
        added_chunk_count = outcome.added_chunk_count,
        unresolved_operand_count = outcome.unresolved_operand_count,
        "partial compare evidence probe completed"
    );
}

pub(super) fn log_technical_focus_probe(library_id: Uuid, outcome: &TechnicalFocusProbeOutcome) {
    tracing::info!(
        stage = "answer.technical_focus_probe",
        library_id = %library_id,
        probe_term_count = outcome.probe_term_count,
        missing_term_count = outcome.missing_term_count,
        added_chunk_count = outcome.added_chunk_count,
        "technical focus evidence probe completed"
    );
}

pub(super) fn log_compile_provider_fallback(library_id: Uuid, fallback_ir: &QueryIR) {
    tracing::error!(
        %library_id,
        fallback_act = ?fallback_ir.act,
        fallback_scope = ?fallback_ir.scope,
        "query compile provider failed; continuing with provider-free fallback IR"
    );
}
