use uuid::Uuid;

use crate::shared::extraction::technical_facts::TechnicalFactKind;
use crate::shared::extraction::text_render::repair_technical_layout_noise;

use super::{
    CanonicalAnswerEvidence, RuntimeMatchedChunk,
    fact_lookup::{best_matching_fact, build_document_labels},
    focused_answer_document_id,
    question_intent::{QuestionIntent, classify_question_intents, has_question_intent},
    question_prefers_russian, question_requests_multi_document_scope,
    technical_answer::document_focus_preference,
    technical_literals::{extract_parameter_literals, technical_literal_focus_keywords},
};

pub(super) fn build_exact_parameter_answer(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let intents = classify_question_intents(question);
    if !has_question_intent(&intents, QuestionIntent::Parameter) {
        return None;
    }
    if question_requests_multi_document_scope(question)
        || has_question_intent(&intents, QuestionIntent::Endpoint)
    {
        return None;
    }

    let explicit_parameters = extract_parameter_literals(question, 6);
    if explicit_parameters.is_empty() {
        return None;
    }

    let target_parameter =
        select_exact_parameter_literal(question, evidence, chunks, &explicit_parameters)?;
    let parameter_meaning =
        extract_parameter_meaning(question, &target_parameter, evidence, chunks);

    let is_existence_question = parameter_existence_question(question);
    let is_name_question = parameter_name_question(question);

    Some(if question_prefers_russian(question) {
        match (is_existence_question, parameter_meaning) {
            (true, Some(meaning)) => format!("Да, есть параметр `{target_parameter}` — {meaning}."),
            (true, None) => format!("Да, есть параметр `{target_parameter}`."),
            (false, Some(meaning)) if is_name_question => {
                format!("Параметр называется `{target_parameter}` — {meaning}.")
            }
            (false, Some(meaning)) => format!("Параметр `{target_parameter}` — {meaning}."),
            (false, None) if is_name_question => {
                format!("Параметр называется `{target_parameter}`.")
            }
            (false, None) => format!("Параметр `{target_parameter}`."),
        }
    } else {
        match (is_existence_question, parameter_meaning) {
            (true, Some(meaning)) => {
                format!("Yes. The parameter is `{target_parameter}` — {meaning}.")
            }
            (true, None) => format!("Yes, the parameter `{target_parameter}` is present."),
            (false, Some(meaning)) if is_name_question => {
                format!("The parameter is named `{target_parameter}` — {meaning}.")
            }
            (false, Some(meaning)) => {
                format!("The parameter `{target_parameter}` means {meaning}.")
            }
            (false, None) if is_name_question => {
                format!("The parameter is named `{target_parameter}`.")
            }
            (false, None) => format!("The parameter is `{target_parameter}`."),
        }
    })
}

fn select_exact_parameter_literal(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    explicit_parameters: &[String],
) -> Option<String> {
    let focused_document_id = focused_answer_document_id(question, chunks);
    let question_keywords = technical_literal_focus_keywords(question);
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

fn parameter_existence_question(question: &str) -> bool {
    let lowered = question.to_lowercase();
    lowered.contains("есть ли")
        || lowered.contains("существует ли")
        || lowered.contains("is there")
        || lowered.contains("does ")
}

fn parameter_name_question(question: &str) -> bool {
    let lowered = question.to_lowercase();
    lowered.contains("как называется")
        || lowered.contains("what is the name")
        || lowered.contains("name of")
}
