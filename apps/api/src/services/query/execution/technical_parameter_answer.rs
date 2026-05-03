use uuid::Uuid;

use crate::domains::query_ir::QueryIR;
use crate::shared::extraction::technical_facts::TechnicalFactKind;
use crate::shared::extraction::text_render::repair_technical_layout_noise;

use super::{
    CanonicalAnswerEvidence, RuntimeMatchedChunk,
    fact_lookup::{best_matching_fact, build_document_labels},
    focused_answer_document_id,
    question_intent::{QuestionIntent, classify_question_or_ir_intents, has_question_intent},
    question_requests_multi_document_scope,
    technical_answer::document_focus_preference,
    technical_literals::{extract_parameter_literals, technical_literal_focus_keywords},
};

pub(super) fn build_exact_parameter_answer(
    question: &str,
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let intents = classify_question_or_ir_intents(question, query_ir);
    if !has_question_intent(&intents, QuestionIntent::Parameter) {
        return None;
    }
    if question_requests_multi_document_scope(question, Some(query_ir)) {
        return None;
    }

    let explicit_parameters = extract_parameter_literals(question, 6);
    if explicit_parameters.is_empty() {
        return None;
    }

    let target_parameter =
        select_exact_parameter_literal(question, query_ir, evidence, chunks, &explicit_parameters)?;
    let parameter_meaning =
        extract_parameter_meaning(question, &target_parameter, evidence, chunks);

    Some(match parameter_meaning {
        Some(meaning) => format!("Parameter `{target_parameter}`: {meaning}."),
        None => format!("Parameter `{target_parameter}`."),
    })
}

fn select_exact_parameter_literal(
    question: &str,
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    explicit_parameters: &[String],
) -> Option<String> {
    let focused_document_id = focused_answer_document_id(question, chunks);
    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let document_labels = build_document_labels(chunks);

    best_matching_fact(
        evidence,
        &document_labels,
        TechnicalFactKind::ParameterName,
        |fact| {
            explicit_parameters.iter().any(|parameter| {
                fact.display_value.eq_ignore_ascii_case(parameter)
                    || fact.canonical_value_exact.eq_ignore_ascii_case(parameter)
                    || fact.canonical_value_text.eq_ignore_ascii_case(parameter)
            })
        },
        |fact, document_label| {
            parameter_candidate_score(
                &fact.display_value,
                document_label.unwrap_or_default(),
                fact.document_id,
                focused_document_id,
                &question_keywords,
            )
        },
    )
    .map(|matched| matched.fact.display_value.clone())
}

fn parameter_candidate_score(
    parameter: &str,
    document_label: &str,
    candidate_document_id: Uuid,
    focused_document_id: Option<Uuid>,
    question_keywords: &[String],
) -> usize {
    let lowered_parameter = parameter.to_lowercase();
    let lowered_label = document_label.to_lowercase();
    usize::try_from(document_focus_preference(candidate_document_id, focused_document_id))
        .unwrap_or_default()
        + question_keywords
            .iter()
            .map(|keyword| {
                usize::from(lowered_label.contains(keyword)) * 20
                    + usize::from(lowered_parameter.contains(keyword)) * 8
            })
            .sum::<usize>()
}

fn extract_parameter_meaning(
    question: &str,
    parameter: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let focused_document_id = focused_answer_document_id(question, chunks);
    let structured_block_match = evidence
        .structured_blocks
        .iter()
        .filter_map(|block| {
            let meaning = extract_parameter_meaning_from_text(
                parameter,
                &repair_technical_layout_noise(&block.normalized_text),
            )
            .or_else(|| {
                extract_parameter_meaning_from_text(
                    parameter,
                    &repair_technical_layout_noise(&block.text),
                )
            })?;
            Some((block.document_id, meaning))
        })
        .max_by_key(|(document_id, _)| document_focus_preference(*document_id, focused_document_id))
        .map(|(_, meaning)| meaning);
    if structured_block_match.is_some() {
        return structured_block_match;
    }

    chunks
        .iter()
        .filter_map(|chunk| {
            let meaning = extract_parameter_meaning_from_text(parameter, &chunk.source_text)
                .or_else(|| extract_parameter_meaning_from_text(parameter, &chunk.excerpt))?;
            Some((chunk.document_id, meaning))
        })
        .max_by_key(|(document_id, _)| document_focus_preference(*document_id, focused_document_id))
        .map(|(_, meaning)| meaning)
}

fn extract_parameter_meaning_from_text(parameter: &str, text: &str) -> Option<String> {
    let normalized_parameter = parameter.to_lowercase();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if !line.to_lowercase().contains(&normalized_parameter) {
            continue;
        }
        if line.contains('|') {
            let cells = line
                .split('|')
                .map(str::trim)
                .filter(|cell| !cell.is_empty() && *cell != "---")
                .collect::<Vec<_>>();
            if cells.len() >= 2
                && cells[0].trim_matches('`').eq_ignore_ascii_case(parameter)
                && !cells[1].is_empty()
            {
                return Some(clean_parameter_meaning(cells[1]));
            }
        }
        for separator in [":", " - ", " — "] {
            if let Some((name, meaning)) = line.split_once(separator)
                && name.trim_matches('`').eq_ignore_ascii_case(parameter)
                && !meaning.trim().is_empty()
            {
                return Some(clean_parameter_meaning(meaning));
            }
        }
    }
    None
}

fn clean_parameter_meaning(raw: &str) -> String {
    raw.trim().trim_matches('`').trim_end_matches('.').trim().to_string()
}
