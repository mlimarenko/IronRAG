use crate::domains::query_ir::QueryIR;
use crate::shared::extraction::technical_facts::TechnicalFactKind;

use super::{
    CanonicalAnswerEvidence, RuntimeMatchedChunk, concise_document_subject_label,
    fact_lookup::{best_matching_fact, build_document_labels},
    focused_answer_document_id,
    question_intent::{
        ExactUrlLookupKind, classify_exact_url_lookup, classify_question_or_ir_intents,
    },
    question_requests_multi_document_scope,
    technical_answer::document_focus_preference,
    technical_literals::technical_literal_focus_keywords,
};

#[derive(Debug, Clone)]
struct ExactUrlLiteralMatch {
    value: String,
    document_label: Option<String>,
}

pub(super) fn build_exact_url_answer(
    question: &str,
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let intents = classify_question_or_ir_intents(question, query_ir);
    let url_lookup_kind = classify_exact_url_lookup(query_ir, &intents)?;
    if question_requests_multi_document_scope(question, Some(query_ir)) {
        return None;
    }
    let asks_wsdl = matches!(url_lookup_kind, ExactUrlLookupKind::Wsdl);

    let url_match = select_exact_url_literal(question, query_ir, evidence, chunks, asks_wsdl)?;
    let subject = url_match
        .document_label
        .as_deref()
        .map(concise_document_subject_label)
        .filter(|value| !value.is_empty());

    Some(subject.map_or_else(
        || format!("`{}`.", url_match.value),
        |subject| format!("{subject}: `{}`.", url_match.value),
    ))
}

fn select_exact_url_literal(
    question: &str,
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    wants_wsdl: bool,
) -> Option<ExactUrlLiteralMatch> {
    let focused_document_id = focused_answer_document_id(question, chunks);
    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let document_labels = build_document_labels(chunks);

    best_matching_fact(
        evidence,
        &document_labels,
        TechnicalFactKind::Url,
        |fact| !wants_wsdl || fact.display_value.to_lowercase().contains("wsdl"),
        |fact, document_label| {
            exact_url_candidate_score(
                &fact.display_value,
                document_label.unwrap_or_default(),
                focused_document_id,
                Some(fact.document_id),
                wants_wsdl,
                &question_keywords,
            )
        },
    )
    .map(|matched| ExactUrlLiteralMatch {
        value: matched.fact.display_value.clone(),
        document_label: matched.document_label.map(str::to_string),
    })
}

fn exact_url_candidate_score(
    value: &str,
    document_label: &str,
    focused_document_id: Option<uuid::Uuid>,
    candidate_document_id: Option<uuid::Uuid>,
    wants_wsdl: bool,
    question_keywords: &[String],
) -> usize {
    let lowered_value = value.to_lowercase();
    let lowered_label = document_label.to_lowercase();
    let mut score = 0;
    if wants_wsdl && lowered_value.contains("wsdl") {
        score += 200;
    }
    score += usize::try_from(
        candidate_document_id
            .map(|document_id| document_focus_preference(document_id, focused_document_id))
            .unwrap_or_default(),
    )
    .unwrap_or_default();
    score
        + question_keywords
            .iter()
            .map(|keyword| {
                usize::from(lowered_label.contains(keyword)) * 20
                    + usize::from(lowered_value.contains(keyword)) * 8
            })
            .sum::<usize>()
}
