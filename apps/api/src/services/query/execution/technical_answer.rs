use uuid::Uuid;

use crate::domains::query_ir::QueryIR;

#[cfg(test)]
use super::technical_literals::technical_chunk_selection_score;
use super::technical_parameter_answer::build_exact_parameter_answer;
use super::technical_url_answer::build_exact_url_answer;
use super::{CanonicalAnswerEvidence, RuntimeMatchedChunk};

pub(super) fn build_exact_technical_literal_answer(
    question: &str,
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    build_exact_parameter_answer(question, query_ir, evidence, chunks)
        .or_else(|| build_exact_url_answer(question, query_ir, evidence, chunks))
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
