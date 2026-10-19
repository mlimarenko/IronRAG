use std::{collections::BTreeSet, sync::LazyLock};

use uuid::Uuid;

use crate::domains::query_ir::{QueryAct, QueryIR, QueryTargetKind};
use crate::infra::knowledge_rows::KnowledgeStructuredBlockRow;
use crate::shared::extraction::technical_facts::TechnicalFactKind;

use super::question_intent::{
    QuestionIntent, classify_question_or_ir_intents, has_question_intent,
};
use super::technical_answer_identity::transport_assignment_source_addresses_question_focus;
#[cfg(test)]
use super::technical_literals::technical_chunk_selection_score;
use super::technical_literals::{
    extract_config_section_literals, extract_explicit_path_literals,
    extract_package_command_literals,
};
use super::technical_parameter_answer::build_exact_parameter_answer;
use super::technical_url_answer::build_exact_url_answer;
use super::{AuthoritativeAnswerEvidence, RuntimeMatchedChunk};

mod error_code_mapping;
mod module_configuration;
mod transport_config;

use error_code_mapping::build_error_code_mapping_answer;
use module_configuration::{
    collect_configuration_paths_with_structured_blocks,
    collect_configuration_sections_with_structured_blocks,
    collect_module_packages_with_structured_blocks,
    collect_visible_parameter_rows_with_structured_blocks, module_configuration_inventory_question,
    module_configuration_scope_chunks, query_ir_requests_module_configuration_setup,
    render_module_configuration_answer, select_module_configuration_path_from_paths,
};
use transport_config::build_transport_config_assignment_answer;

pub(super) fn build_exact_technical_literal_answer(
    question: &str,
    query_ir: &QueryIR,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !query_ir_allows_exact_technical_literal_answer(query_ir) {
        return None;
    }
    build_module_configuration_setup_answer(question, query_ir, evidence, chunks)
        .or_else(|| build_transport_config_assignment_answer(question, query_ir, evidence, chunks))
        .or_else(|| build_error_code_mapping_answer(question, query_ir, chunks))
        .or_else(|| build_exact_parameter_answer(question, query_ir, evidence, chunks))
        .or_else(|| build_exact_url_answer(question, query_ir, evidence, chunks))
}

fn query_ir_allows_exact_technical_literal_answer(query_ir: &QueryIR) -> bool {
    !classify_question_or_ir_intents("", query_ir).is_empty()
        || query_ir_requests_module_configuration_setup(query_ir)
}

pub(super) fn build_module_configuration_setup_answer(
    question: &str,
    query_ir: &QueryIR,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if module_configuration_inventory_question(question, query_ir) {
        return None;
    }
    if !query_ir_requests_module_configuration_setup(query_ir) {
        return None;
    }
    let scoped_chunks = module_configuration_scope_chunks(question, chunks);
    let candidate_chunks = if scoped_chunks.is_empty() { chunks } else { scoped_chunks.as_slice() };
    let packages = collect_module_packages_with_structured_blocks(
        candidate_chunks,
        &evidence.structured_blocks,
        4,
    );
    let config_paths = collect_configuration_paths_with_structured_blocks(
        candidate_chunks,
        &evidence.structured_blocks,
        16,
    );
    let config_path = select_module_configuration_path_from_paths(&config_paths, &packages)?;
    let config_sections = collect_configuration_sections_with_structured_blocks(
        candidate_chunks,
        &evidence.structured_blocks,
        8,
    );
    let parameter_focus_terms = query_ir
        .literal_constraints
        .iter()
        .map(|literal| literal.text.as_str())
        .collect::<Vec<_>>();
    let parameters = collect_visible_parameter_rows_with_structured_blocks(
        candidate_chunks,
        &evidence.structured_blocks,
        &parameter_focus_terms,
        32,
    );
    if packages.is_empty() && parameters.is_empty() {
        return None;
    }
    let document_label = candidate_chunks
        .iter()
        .map(|chunk| chunk.document_label.trim())
        .find(|label| !label.is_empty())
        .unwrap_or("source");
    // Language-neutral deterministic structure. The grounded values — source
    // document, package commands, configuration paths, parameter rows — carry
    // the meaning and are rendered verbatim from the evidence (whatever the
    // source language is). There are NO hardcoded natural-language section
    // labels here: phrasing and language are the LLM's responsibility, never a
    // code-side per-language dictionary. Markdown grouping (a code-span header
    // plus blank-line-separated bullet blocks) keeps the sections distinct
    // without embedding prose in any language.
    Some(render_module_configuration_answer(
        document_label,
        packages,
        &config_path,
        &config_paths,
        config_sections,
        parameters,
    ))
}

#[cfg(test)]
pub(super) fn prioritized_technical_chunk_score(
    text: &str,
    candidate_document_id: Uuid,
    keywords: &[String],
    pagination_requested: bool,
    focused_document_id: Option<Uuid>,
) -> isize {
    technical_chunk_selection_score(text, keywords, pagination_requested)
        + document_focus_preference(candidate_document_id, focused_document_id)
}

pub(super) fn document_focus_preference(
    candidate_document_id: Uuid,
    focused_document_id: Option<Uuid>,
) -> isize {
    if focused_document_id == Some(candidate_document_id) { 24 } else { 0 }
}

#[cfg(test)]
#[path = "technical_answer_tests.rs"]
mod tests;
