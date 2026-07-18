#[cfg(test)]
use uuid::Uuid;

mod answer;
mod answer_kind;
mod answer_pipeline;
mod associative_graph_retrieval;
mod canonical_answer_context;
mod chunk_support;
mod command_shape;
mod consolidation;
mod context;
mod document_target;
mod embed;
mod endpoint_answer;
#[cfg(test)]
mod endpoint_chunk_answer;
mod fact_lookup;
mod focused_document_answer;
mod fusion;
mod graph_retrieval;
mod graph_retrieval_error;
mod hyde;
mod output_boundary;
mod port_answer;
mod preflight;
mod preflight_context;
pub(crate) mod question_intent;
mod rerank;
mod retrieval_plan;
#[cfg(test)]
mod retrieval_plan_tests;
mod retrieve;
mod semantic_rerank;
mod source_context;
mod source_excerpt;
mod source_profile;
mod structured_query_pipeline;
mod table_retrieval;
mod table_row_answer;
mod table_summary_answer;
mod technical_answer;
mod technical_literal_context;
mod technical_literal_extractors;
mod technical_literal_focus;
mod technical_literals;
mod technical_parameter_answer;
mod technical_url_answer;
#[cfg(test)]
mod tests;
mod tuning;
mod types;
mod vector_retrieval;
mod verification;
mod verification_claims;
mod verification_policy;
mod verification_support;

#[cfg(test)]
use crate::domains::query::QueryVerificationState;
#[cfg(test)]
use crate::domains::query::QueryVerificationWarning;
#[cfg(test)]
use crate::domains::query::RuntimeQueryMode;
#[cfg(test)]
use crate::domains::query::{
    QueryAnswerCandidate, QueryAnswerCandidateProvenance, QueryAnswerDisposition,
    QueryClarification,
};
#[cfg(test)]
pub(crate) use answer::build_answer_prompt;
pub(crate) use answer::{
    build_deterministic_grounded_answer, build_exact_version_change_summary_answer,
    build_missing_explicit_document_answer, build_ordered_source_slice_answer,
    render_canonical_chunk_section, render_canonical_technical_fact_section,
    render_prepared_segment_section, render_targeted_evidence_chunk_section,
};
pub(crate) use answer_pipeline::{
    generate_answer_query, literal_revision_targets, prepare_answer_query,
};
#[cfg(test)]
pub(crate) use canonical_answer_context::{
    apply_runtime_chunk_overlays, merge_runtime_context_chunks,
    selected_fact_ids_for_canonical_evidence,
};
pub(crate) use canonical_answer_context::{
    build_canonical_answer_context, focus_token_overlap_count, load_canonical_answer_chunks,
    load_canonical_answer_evidence, load_direct_targeted_table_answer,
    query_ir_document_focus_tokens,
};
pub(crate) use chunk_support::{focused_excerpt_for, map_chunk_hit};
pub(crate) use consolidation::{focused_document_consolidation, prune_non_topical_document_tail};
pub(crate) use context::{
    apply_query_execution_library_summary, apply_query_execution_warning, assemble_answer_context,
    load_query_execution_library_context, should_prioritize_retrieved_context_for_query,
};
#[cfg(test)]
pub(crate) use context::{
    assemble_bounded_context, build_references, build_structured_query_diagnostics,
};
#[cfg(test)]
pub(crate) use document_target::document_focus_marker_hits;
pub(crate) use document_target::{
    concise_document_subject_label, explicit_document_reference_literal_is_present,
    explicit_document_reference_literals, explicit_target_document_ids_from_values,
    focused_answer_document_id, normalized_document_target_candidates,
    question_requests_multi_document_scope, resolve_scoped_target_document_ids,
};
#[cfg(test)]
pub(crate) use endpoint_chunk_answer::{
    build_multi_document_endpoint_answer_from_chunks, build_single_endpoint_answer_from_chunks,
};
#[cfg(test)]
pub(crate) use focused_document_answer::build_focused_document_answer;
pub(crate) use graph_retrieval::{
    GraphTargetEntityCoverageField, GraphTargetEntityCoverageFieldKind, GraphTargetEntityProfile,
    associative_edges_for_entities, graph_target_entity_coverage_score,
    query_relevant_graph_evidence_target_hits,
};
use preflight::prepare_canonical_answer_preflight;
#[cfg(test)]
use preflight::{
    build_canonical_preflight_answer, build_preflight_answer_chunks,
    build_preflight_canonical_evidence, build_preflight_graph_evidence_context_lines,
};
#[cfg(test)]
pub(crate) use rerank::apply_rerank_outcome;
#[cfg(test)]
pub(crate) use retrieve::{
    build_graph_evidence_text_queries, build_lexical_queries, graph_evidence_db_text_queries,
    query_graph_status, query_ir_focus_search_queries, query_ir_lexical_focus_queries,
    truncate_bundle,
};
pub(crate) use retrieve::{
    canonical_document_revision_id, load_document_index, merge_chunks,
    retain_canonical_document_head_chunks, score_desc_chunks, score_value,
};
#[cfg(test)]
pub(crate) use source_context::source_slice_context_top_k;
pub(crate) use source_context::{
    SOURCE_UNIT_CHUNK_KIND, augment_structured_source_context, source_anchor_window,
    source_slice_requested_count,
};
pub(crate) use structured_query_pipeline::{
    finalize_structured_query, plan_structured_query, refresh_query_plan_for_compiled_ir,
    replan_for_resolved_retrieval_query, rerank_structured_query, retrieve_structured_query,
};
#[cfg(test)]
pub(crate) use table_retrieval::is_table_analytics_chunk;
pub(crate) use table_retrieval::{
    load_initial_table_rows_for_documents, load_table_rows_for_documents,
    load_table_section_sibling_chunks, load_table_summary_chunks_for_documents,
    merge_canonical_table_aggregation_chunks, query_ir_requests_table_section_siblings,
    requested_initial_table_row_count,
};
#[cfg(test)]
pub(crate) use table_row_answer::parse_table_row_chunk;
pub(crate) use table_row_answer::{
    build_table_row_grounded_answer, question_asks_table_value_inventory,
};
pub(crate) use table_summary_answer::{
    build_table_summary_grounded_answer, question_asks_table_aggregation,
    render_table_summary_chunk_section,
};
#[cfg(test)]
use technical_literal_context::build_exact_technical_literals_section;
#[cfg(test)]
use technical_literals::detect_technical_literal_intent;
#[cfg(test)]
use technical_literals::technical_literal_focus_keyword_segments;
#[cfg(test)]
use technical_literals::technical_literal_focus_keywords;
pub(crate) use types::{
    AnswerGenerationStage, AnswerVerificationStage, CanonicalAnswerEvidence,
    PreparedAnswerQueryResult, QueryChunkReferenceSnapshot, QueryGraphIndex, RetrievalBundle,
    RuntimeAnswerQueryFailure, RuntimeAnswerQueryResult, RuntimeAnswerVerification,
    RuntimeChunkScoreKind, RuntimeMatchedChunk, RuntimeMatchedEntity, RuntimeMatchedRelationship,
    RuntimeStructuredQueryResult, SemanticRerankExecutionContext,
};
#[cfg(test)]
pub(crate) use types::{
    RuntimeQueryLibrarySummary, RuntimeQueryWarning, RuntimeRetrievedDocumentBrief,
    RuntimeStructuredQueryDiagnostics, RuntimeStructuredQueryReferenceCounts, sample_chunk_row,
    sample_structured_block_row, sample_technical_fact_row,
};
#[cfg(test)]
pub(crate) use verification::{
    attach_query_answer_outcome, enrich_query_assembly_diagnostics, enrich_query_candidate_summary,
};
pub(crate) use verification::{
    persist_query_verification, persisted_query_answer_outcome,
    verify_answer_against_canonical_evidence,
};
pub(crate) use verification_policy::{AnswerVisibilityKind, finalize_answer_visibility};

/// HyDE passage generation timeout. Increase for slow LLM providers.
const HYDE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// HyDE generation temperature. Lower = more factual, higher = more creative.
const HYDE_TEMPERATURE: f64 = 0.3;
/// Maximum structured blocks included per answer assembly pass.
const MAX_ANSWER_BLOCKS: usize = 16;
/// Maximum chunks selected per document in balanced chunk selection.
const MAX_CHUNKS_PER_DOCUMENT: usize = 8;
/// Minimum chunks selected per document in balanced chunk selection.
const MIN_CHUNKS_PER_DOCUMENT: usize = 2;
