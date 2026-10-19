use std::collections::{HashMap, HashSet};

use uuid::Uuid;

#[path = "preflight_setup_structured_stage.rs"]
pub(super) mod setup_structured_stage;

mod answer_construction;
pub(super) mod document_presence;
mod document_scope;
mod setup_literals;
#[cfg(test)]
mod tests;

use setup_structured_stage::{
    SetupPreflightStructuredRequest, augment_setup_preflight_structured_blocks,
};

#[cfg(test)]
pub(super) use answer_construction::build_preflight_answer_chunks;
pub(super) use answer_construction::build_primary_preflight_answer_override;
pub(super) use answer_construction::{
    TechnicalLiteralSelection, build_evidence_preflight_answer,
    build_preflight_graph_evidence_context_lines, query_ir_requests_low_confidence_setup_preflight,
    query_ir_requests_setup_literal_context, select_technical_literal_chunks,
};
use answer_construction::{
    broad_procedure_preflight_requires_synthesis, build_setup_configuration_anchor_answer_override,
    prepend_preflight_source_title_inventory, query_ir_requests_structured_inventory_preflight,
};
#[cfg(test)]
pub(super) use document_scope::build_preflight_authoritative_evidence;
use document_scope::{
    build_preflight_answer_chunks_for_scope, build_preflight_authoritative_evidence_for_scope,
    preflight_allows_empty_scope_fallback,
};
pub(super) use document_scope::{
    preflight_exact_literal_document_scope, query_ir_requests_broad_procedure_variant_coverage,
};
#[cfg(test)]
pub(super) use document_scope::{
    question_prefers_single_exact_literal_scope, select_preflight_literal_document_id,
};
use setup_literals::{
    select_setup_literal_document_id, select_setup_literal_document_ids,
    setup_literal_assignment_count, setup_literal_configuration_path_count,
    setup_literal_section_count,
};

use super::answer::{
    augment_deterministic_grounded_answer_with_evidence,
    build_setup_configuration_anchor_candidate, build_update_procedure_sequence_answer,
};
use super::answer_kind::AnswerKind;
pub(super) use super::preflight_context::{
    extend_setup_preflight_chunks_from_setup_lanes,
    extend_setup_preflight_chunks_from_structured_context,
};
use crate::{
    app::state::AppState,
    domains::query_ir::{
        QueryAct, QueryIR, QueryScope, QueryTargetKind, literal_text_is_identifier_shaped,
    },
    infra::knowledge_rows::KnowledgeDocumentRow,
    services::query::{
        effective_query::{current_question_segment, structured_current_question_segment},
        planner::QueryIntentProfile,
        text_match::label_terms,
    },
};

use super::{
    AuthoritativeAnswerEvidence, PreparedAnswerQueryResult, RuntimeChunkScoreKind,
    RuntimeMatchedChunk, build_authoritative_answer_context, build_deterministic_grounded_answer,
    load_authoritative_answer_evidence, load_direct_targeted_table_answer, load_document_index,
    load_primary_answer_chunks,
    question_intent::{QuestionIntent, classify_query_ir_intents, has_question_intent},
    question_intent::{
        query_ir_has_focused_document_answer_intent,
        query_ir_targets_graph_entities_or_relationships,
    },
    question_requests_multi_document_scope,
    retrieve::{merge_chunks, score_value},
    technical_literals::{
        DocumentBalancedChunkSelection, TechnicalLiteralIntent, document_local_focus_keywords,
        extract_explicit_path_literals, extract_package_command_literals,
        extract_parameter_literals, select_document_balanced_chunks,
        technical_chunk_selection_score, technical_keyword_weight,
        technical_literal_candidate_limit, technical_literal_focus_keywords,
    },
};

#[derive(Debug, Clone)]
pub(super) struct AnswerEvidencePreflight {
    pub(super) primary_answer_chunks: Vec<RuntimeMatchedChunk>,
    pub(super) authoritative_evidence: AuthoritativeAnswerEvidence,
    pub(super) prompt_context: String,
    pub(super) answer_override: Option<DeterministicAnswerOverride>,
}

#[derive(Debug, Clone)]
pub(super) struct DeterministicAnswerOverride {
    pub(super) answer: String,
    pub(super) answer_kind: AnswerKind,
}

pub(super) async fn prepare_answer_preflight(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    execution_id: Uuid,
    question: &str,
    prepared: &PreparedAnswerQueryResult,
) -> anyhow::Result<AnswerEvidencePreflight> {
    // The targeted-table and primary-chunk loads both read the document index,
    // so they stay a chain. The bundle-scoped authoritative evidence is keyed
    // by `execution_id` alone and shares no input with that chain, so overlap
    // the two and pay the slower branch instead of their sum. Output is
    // identical: every consumer below runs only after both branches complete.
    // The chain is listed first so a same-poll double failure still surfaces
    // the chain's error, and `try_join!` keeps the fail-fast drop of the
    // sibling that the retrieval stage already relies on.
    let document_index_chain = Box::pin(async {
        let document_index = load_document_index(state, library_id).await?;
        let direct_targeted_table_answer = load_direct_targeted_table_answer(
            state,
            question,
            Some(&prepared.query_ir),
            &document_index,
        )
        .await?;
        let primary_answer_chunks = load_primary_answer_chunks(
            state,
            execution_id,
            question,
            &prepared.query_ir,
            &prepared.structured.context_chunks,
            &document_index,
        )
        .await?;
        anyhow::Ok((document_index, direct_targeted_table_answer, primary_answer_chunks))
    });
    let authoritative_evidence_load = load_authoritative_answer_evidence(state, execution_id);
    let (
        (document_index, direct_targeted_table_answer, primary_answer_chunks),
        authoritative_evidence,
    ) = tokio::try_join!(document_index_chain, authoritative_evidence_load)?;
    let scoped_document_ids = preflight_exact_literal_document_scope(
        question,
        &prepared.query_ir,
        &prepared.structured.intent_profile,
        &prepared.structured.technical_literal_chunks,
    );
    let allow_empty_scope_fallback =
        preflight_allows_empty_scope_fallback(question, &prepared.query_ir);
    let mut preflight_answer_chunks = build_preflight_answer_chunks_for_scope(
        &primary_answer_chunks,
        &prepared.structured.technical_literal_chunks,
        scoped_document_ids.as_ref(),
        allow_empty_scope_fallback,
    );
    if query_ir_requests_setup_literal_context(&prepared.query_ir) {
        extend_setup_preflight_chunks_from_structured_context(
            &mut preflight_answer_chunks,
            &prepared.structured.context_chunks,
            scoped_document_ids.as_ref(),
        );
    } else if query_ir_requests_low_confidence_setup_preflight(
        &prepared.query_ir,
        &prepared.structured.context_chunks,
    ) || query_ir_requests_structured_inventory_preflight(
        question,
        &prepared.query_ir,
        &prepared.structured.context_chunks,
    ) {
        extend_setup_preflight_chunks_from_structured_context(
            &mut preflight_answer_chunks,
            &prepared.structured.context_chunks,
            None,
        );
    } else if matches!(prepared.query_ir.act, QueryAct::ConfigureHow)
        && preflight_answer_chunks.is_empty()
    {
        extend_setup_preflight_chunks_from_setup_lanes(
            &mut preflight_answer_chunks,
            &prepared.structured.context_chunks,
        );
    }
    let mut preflight_evidence = build_preflight_authoritative_evidence_for_scope(
        &authoritative_evidence,
        scoped_document_ids.as_ref(),
        allow_empty_scope_fallback,
    );
    augment_setup_preflight_structured_blocks(
        SetupPreflightStructuredRequest {
            state,
            question,
            query_ir: &prepared.query_ir,
            document_index: &document_index,
            preflight_answer_chunks: &preflight_answer_chunks,
            scoped_document_ids: scoped_document_ids.as_ref(),
        },
        &mut preflight_evidence,
    )
    .await?;
    let graph_evidence_context_lines = build_preflight_graph_evidence_context_lines(
        &prepared.structured.graph_evidence_context_lines,
    );
    let prompt_context = build_authoritative_answer_context(
        question,
        &prepared.query_ir,
        prepared.structured.technical_literals_text.as_deref(),
        &preflight_evidence,
        &preflight_answer_chunks,
        &graph_evidence_context_lines,
    );
    let prompt_context = prepend_preflight_source_title_inventory(
        &document_index,
        &preflight_evidence,
        &preflight_answer_chunks,
        prompt_context,
    );
    let primary_answer_override = build_primary_preflight_answer_override(
        document_presence::resolve_explicit_document_presence_override(
            state,
            workspace_id,
            library_id,
            question,
            &document_index,
            prepared.query_ir.language,
        )
        .await?,
        direct_targeted_table_answer.as_deref(),
    );
    let broad_procedure_requires_synthesis = broad_procedure_preflight_requires_synthesis(
        question,
        &prepared.query_ir,
        &preflight_answer_chunks,
    );
    let answer_override = primary_answer_override.or_else(|| {
        if broad_procedure_requires_synthesis {
            return None;
        }
        build_update_procedure_sequence_answer(&prepared.query_ir, &preflight_answer_chunks)
            .map(|answer| {
                let answer = augment_deterministic_grounded_answer_with_evidence(
                    answer,
                    question,
                    &prepared.query_ir,
                    &preflight_answer_chunks,
                );
                DeterministicAnswerOverride {
                    answer,
                    answer_kind: AnswerKind::UpdateProcedureSequence,
                }
            })
            .or_else(|| {
                build_setup_configuration_anchor_answer_override(
                    question,
                    &prepared.query_ir,
                    &preflight_answer_chunks,
                )
                .map(|answer| DeterministicAnswerOverride {
                    answer,
                    answer_kind: AnswerKind::SetupConfigurationAnchor,
                })
            })
            .or_else(|| {
                build_evidence_preflight_answer(
                    question,
                    &prepared.query_ir,
                    &prepared.structured.intent_profile,
                    direct_targeted_table_answer,
                    &preflight_evidence,
                    &preflight_answer_chunks,
                )
            })
    });
    Ok(AnswerEvidencePreflight {
        primary_answer_chunks: preflight_answer_chunks,
        authoritative_evidence: preflight_evidence,
        prompt_context,
        answer_override,
    })
}
