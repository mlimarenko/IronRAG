use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::{
    shared::extraction::technical_facts::{TechnicalFactKind, TechnicalFactQualifier},
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::{
    CanonicalAnswerEvidence, concise_document_subject_label,
    document_target::{focused_answer_document_id, question_requests_multi_document_scope},
    fact_lookup::{best_matching_fact, build_document_labels},
    question_intent::{
        QuestionIntent, classify_question_intents, has_question_intent,
        question_blocks_endpoint_lookup,
    },
    technical_answer::document_focus_preference,
    technical_literals::{
        question_mentions_pagination, technical_chunk_selection_score, technical_keyword_weight,
        technical_literal_focus_keyword_segments, technical_literal_focus_keywords,
    },
    types::RuntimeMatchedChunk,
};

pub(crate) fn build_multi_document_endpoint_answer_from_facts(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let intents = classify_question_intents(question);
    if !has_question_intent(&intents, QuestionIntent::Endpoint) {
        return None;
    }
    if !question_requests_multi_document_scope(question, None) {
        return None;
    }
    if question_blocks_endpoint_lookup(question) {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question, None);
    if question_keywords.is_empty() {
        return None;
    }

    let per_document_chunks = chunks_by_document(chunks);
    let mut ordered_document_ids = Vec::<Uuid>::new();
    for chunk in chunks {
        if !ordered_document_ids.contains(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
    }
    let document_labels = build_document_labels(chunks);
    let scoped_document_ids =
        select_multi_document_scope_ids(question, &ordered_document_ids, &per_document_chunks);

    let mut lines = Vec::new();
    for document_id in scoped_document_ids {
        let document_label =
            document_labels.get(&document_id).map(String::as_str).unwrap_or_default();
        let endpoint_match = best_matching_fact(
            evidence,
            &document_labels,
            TechnicalFactKind::EndpointPath,
            |fact| fact.document_id == document_id,
            |fact, document_label| {
                endpoint_fact_score(
                    &fact.display_value,
                    document_label.unwrap_or_default(),
                    fact.document_id,
                    Some(document_id),
                    &question_keywords,
                )
            },
        );
        let Some(endpoint_match) = endpoint_match else {
            continue;
        };
        let subject = concise_document_subject_label(document_label);
        let method = endpoint_method_literal(endpoint_match.fact, evidence);
        let endpoint = endpoint_match.fact.display_value.as_str();
        let literal = method
            .map(|method| format!("`{method} {endpoint}`"))
            .unwrap_or_else(|| format!("`{endpoint}`"));
        lines.push(format!("- для {subject} — {literal}"));
    }

    (lines.len() >= 2).then(|| format!("Нужны два endpoint'а:\n\n{}", lines.join("\n")))
}

pub(crate) fn build_single_endpoint_answer_from_facts(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let intents = classify_question_intents(question);
    if !has_question_intent(&intents, QuestionIntent::Endpoint) {
        return None;
    }
    if question_requests_multi_document_scope(question, None)
        || question_blocks_endpoint_lookup(question)
    {
        return None;
    }

    let focused_document_id = focused_answer_document_id(question, chunks);
    let question_keywords = technical_literal_focus_keywords(question, None);
    if question_keywords.is_empty() {
        return None;
    }

    let document_labels = build_document_labels(chunks);
    let endpoint_match = best_matching_fact(
        evidence,
        &document_labels,
        TechnicalFactKind::EndpointPath,
        |_| true,
        |fact, document_label| {
            endpoint_fact_score(
                &fact.display_value,
                document_label.unwrap_or_default(),
                fact.document_id,
                focused_document_id,
                &question_keywords,
            )
        },
    )?;

    let method = endpoint_method_literal(endpoint_match.fact, evidence);
    let endpoint = endpoint_match.fact.display_value.as_str();
    let literal = method
        .map(|method| format!("`{method} {endpoint}`"))
        .unwrap_or_else(|| format!("`{endpoint}`"));

    Some(if super::question_prefers_russian(question) {
        format!("Нужен endpoint {literal}.")
    } else {
        format!("The endpoint is {literal}.")
    })
}

fn endpoint_fact_score(
    endpoint: &str,
    document_label: &str,
    candidate_document_id: Uuid,
    focused_document_id: Option<Uuid>,
    question_keywords: &[String],
) -> usize {
    let lowered_endpoint = endpoint.to_lowercase();
    let lowered_label = document_label.to_lowercase();
    usize::try_from(document_focus_preference(candidate_document_id, focused_document_id))
        .unwrap_or_default()
        + question_keywords
            .iter()
            .map(|keyword| {
                usize::from(lowered_label.contains(keyword)) * 20
                    + usize::from(lowered_endpoint.contains(keyword)) * 8
            })
            .sum::<usize>()
}

fn endpoint_method_literal(
    endpoint_fact: &crate::infra::arangodb::document_store::KnowledgeTechnicalFactRow,
    evidence: &CanonicalAnswerEvidence,
) -> Option<String> {
    let qualifiers = serde_json::from_value::<Vec<TechnicalFactQualifier>>(
        endpoint_fact.qualifiers_json.clone(),
    )
    .unwrap_or_default();
    if let Some(method) = qualifiers
        .iter()
        .find(|qualifier| qualifier.key == "method")
        .map(|qualifier| qualifier.value.clone())
    {
        return Some(method);
    }

    let methods = evidence
        .technical_facts
        .iter()
        .filter(|fact| {
            fact.document_id == endpoint_fact.document_id
                && fact.fact_kind.parse::<TechnicalFactKind>().ok()
                    == Some(TechnicalFactKind::HttpMethod)
        })
        .map(|fact| repair_technical_layout_noise(&fact.display_value).to_ascii_uppercase())
        .collect::<HashSet<_>>();

    (methods.len() == 1).then(|| methods.into_iter().next()).flatten()
}

fn chunks_by_document(chunks: &[RuntimeMatchedChunk]) -> HashMap<Uuid, Vec<&RuntimeMatchedChunk>> {
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }
    per_document_chunks
}

pub(super) fn select_multi_document_scope_ids(
    question: &str,
    ordered_document_ids: &[Uuid],
    per_document_chunks: &HashMap<Uuid, Vec<&RuntimeMatchedChunk>>,
) -> Vec<Uuid> {
    let pagination_requested = question_mentions_pagination(question);
    let focus_segments = technical_literal_focus_keyword_segments(question, None);
    if focus_segments.is_empty() {
        return ordered_document_ids.to_vec();
    }

    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for segment_keywords in focus_segments {
        let best_document = ordered_document_ids
            .iter()
            .filter_map(|document_id| {
                let document_chunks = per_document_chunks.get(document_id)?;
                let best_chunk_score = document_chunks
                    .iter()
                    .map(|chunk| {
                        technical_chunk_selection_score(
                            &format!("{} {}", chunk.excerpt, chunk.source_text),
                            &segment_keywords,
                            pagination_requested,
                        )
                    })
                    .max()
                    .unwrap_or_default();
                let document_text = document_chunks
                    .iter()
                    .map(|chunk| format!("{} {}", chunk.excerpt, chunk.source_text))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .to_lowercase();
                let document_keyword_score = segment_keywords
                    .iter()
                    .map(|keyword| technical_keyword_weight(&document_text, keyword) as isize)
                    .sum::<isize>();
                let score = best_chunk_score.max(document_keyword_score);
                (score > 0).then_some((score, *document_id))
            })
            .max_by(|left, right| {
                left.0.cmp(&right.0).then_with(|| {
                    let left_index = ordered_document_ids
                        .iter()
                        .position(|document_id| document_id == &left.1)
                        .unwrap_or(usize::MAX);
                    let right_index = ordered_document_ids
                        .iter()
                        .position(|document_id| document_id == &right.1)
                        .unwrap_or(usize::MAX);
                    right_index.cmp(&left_index)
                })
            });
        if let Some((_, document_id)) = best_document
            && seen.insert(document_id)
        {
            selected.push(document_id);
        }
    }

    if selected.is_empty() { ordered_document_ids.to_vec() } else { selected }
}
