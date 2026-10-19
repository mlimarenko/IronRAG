use std::collections::{BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        query::{
            QueryAnswerCandidate, QueryAnswerCandidateProvenance, QueryAnswerDisposition,
            QueryClarification, QueryVerificationState, QueryVerificationWarning,
        },
        query_ir::{EntityRole, QueryAct, QueryIR, QueryLanguage, QueryScope, QueryTargetKind},
    },
    infra::{knowledge_plane::LexicalQueryOrigin, knowledge_rows::KnowledgeDocumentRow},
    integrations::llm::ChatMessage,
    interfaces::http::router_support::ApiError,
    services::query::{
        assistant_grounding::AssistantGroundingEvidence,
        compiler::{
            CompileHistoryTurn, CompileQueryCommand, QueryCompilerService,
            provider_free_fallback_query_ir,
        },
        i18n::deterministic_query_messages,
        latest_versions::query_requests_latest_versions,
        service::ExternalConversationTurn,
    },
};

use super::answer_kind::AnswerKind;
use super::output_boundary::strip_trailing_media_source_token;
use super::question_intent::{
    query_ir_has_focused_document_answer_intent, query_ir_requires_remediation_synthesis,
};
use super::technical_literals::{
    TechnicalLiteralIntent, detect_explicit_technical_literal_intent_from_query_ir,
    detect_technical_literal_intent_from_query_ir, extract_config_assignment_literals,
    extract_config_section_literals, extract_explicit_path_literals, extract_http_methods,
    extract_package_command_literals, extract_parameter_literals, extract_prefix_literals,
    extract_url_literals, technical_literal_focus_keywords,
};
use super::tuning::{
    CLARIFY_MAX_VARIANTS, SINGLE_SHOT_CONFIDENT_ANSWER_CHARS, SINGLE_SHOT_MIN_ANSWER_CHARS,
    SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS,
};
use super::types::{
    RuntimeAnswerVerification, RuntimeChunkScoreKind, SemanticRerankExecutionContext,
};
use super::{
    AnswerGenerationStage, AnswerVerificationRequest, AnswerVerificationStage,
    PreparedAnswerQueryResult, QueryChunkReferenceSnapshot, QueryVerificationPersistence,
    RuntimeAnswerQueryFailure, RuntimeAnswerQueryResult, RuntimeMatchedChunk,
    apply_query_execution_library_summary, apply_query_execution_warning, assemble_answer_context,
    load_query_execution_library_context, render_targeted_evidence_chunk_section,
    should_prioritize_retrieved_context_for_query, verify_answer_against_authoritative_evidence,
};

mod answer_snapshot;
mod clarification;
mod context_probes;
mod coverage;
mod deterministic;
mod disposition;
mod document_statement;
mod literal_coverage;
mod literal_revision;
mod output_boundary;
mod preflight;
mod single_shot;
mod variant_selection;
mod verification;

pub(crate) use deterministic::generate_answer_query;
pub(crate) use literal_coverage::literal_revision_targets;
pub(crate) use preflight::{PrepareAnswerQueryRequest, prepare_answer_query};
pub(crate) use single_shot::AnswerQueryRequest;
#[cfg(test)]
pub(crate) use verification::apply_enumeration_coverage_warning;

#[cfg(test)]
use crate::{
    domains::{
        query::{GroupedReference, GroupedReferenceKind},
        query_ir::{
            ClarificationReason, ClarificationSpec, ComparisonSpec, ConversationRefKind,
            DocumentHint, EntityMention, LiteralKind, LiteralSpan, UnresolvedRef,
        },
    },
    services::query::execution::types::RuntimeRetrievedDocumentBrief,
};
#[cfg(test)]
use clarification::{
    ClarificationPromptKind, clarification_not_answer_verification,
    deterministic_clarification_question, disposition_clarification,
    render_typed_clarification_answer,
};
#[cfg(test)]
use context_probes::{
    collect_technical_focus_probe_terms, extract_focus_aligned_structural_literals,
};
#[cfg(test)]
use coverage::{
    EvidenceCoverage, compare_operands_covered_by_context,
    focused_configuration_inventory_waits_for_preflight, query_requires_single_shot_focus_support,
    should_use_single_shot_answer, single_shot_answer_is_acceptable,
    single_shot_lacks_query_focus_support,
};
#[cfg(test)]
use disposition::{
    AnswerDisposition, classify_answer_disposition, classify_answer_disposition_from_evidence,
    classify_answer_disposition_from_groups,
};
#[cfg(test)]
use literal_coverage::{
    answer_generation_question, answer_omits_expected_technical_literals,
    answer_question_for_disposition, collect_focused_context_parameter_literals,
    collect_intended_technical_literals, literal_revision_can_replace_answer,
    requires_no_evidence_candidate,
};
#[cfg(test)]
use literal_revision::{
    CLARIFICATION_LABEL_MAX_CHARS, declared_subject_literal_revision_targets,
    token_has_internal_uppercase,
};
#[cfg(test)]
use output_boundary::{
    build_update_procedure_answer_with_source_context_fallback, selected_runtime_answer_chunks,
    selected_runtime_grounding_evidence, strip_trailing_inventory_meta_paragraph,
};
#[cfg(test)]
use preflight::TECHNICAL_FOCUS_PROBE_TERM_LIMIT;
#[cfg(test)]
use test_support::{
    prepared_for_single_shot, procedure_chunk, retrieved_doc, sample_groups, sample_ir,
    sample_ir_with_act, sample_ir_with_two_target_entities, setup_anchor_chunk,
};
#[cfg(test)]
use variant_selection::{clarify_variant_dedup_key, extract_query_specific_variants};
#[cfg(test)]
use verification::{
    answer_omits_structural_context_coverage, apply_structural_coverage_warning,
    collect_structural_coverage_anchors, normalize_structural_coverage_anchor,
    structural_answer_contains_anchor, structural_coverage_focus_tokens,
};
#[cfg(test)]
mod answer_snapshot_tests;
#[cfg(test)]
mod clarification_tests;
#[cfg(test)]
mod context_probes_tests;
#[cfg(test)]
mod coverage_tests;
#[cfg(test)]
mod disposition_tests;
#[cfg(test)]
mod literal_revision_tests;
#[cfg(test)]
mod output_boundary_tests;
#[cfg(test)]
mod single_shot_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod variant_selection_tests;
