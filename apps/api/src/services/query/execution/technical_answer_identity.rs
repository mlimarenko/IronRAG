use std::collections::BTreeSet;

use uuid::Uuid;

use crate::{domains::query_ir::QueryIR, services::query::text_match::normalized_alnum_tokens};

use super::RuntimeMatchedChunk;

/// Reports whether the selected assignment source carries the question's
/// most discriminative typed focus. Evidence from another document cannot
/// make an unrelated assignment-shaped chunk eligible.
pub(super) fn transport_assignment_source_addresses_question_focus(
    query_ir: &QueryIR,
    candidate_document_id: Uuid,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    let focus_terms = transport_assignment_focus_terms(query_ir);
    let focus_terms = least_document_frequency_terms(&focus_terms, chunks);
    !focus_terms.is_empty()
        && chunks.iter().filter(|chunk| chunk.document_id == candidate_document_id).any(|chunk| {
            focus_terms.iter().any(|term| {
                chunk_fields(chunk).any(|field| has_complete_token_or_structural_match(field, term))
            })
        })
}

fn transport_assignment_focus_terms(query_ir: &QueryIR) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
        extend_focus_terms(&mut terms, retrieval_query, 3);
    }
    for entity in &query_ir.target_entities {
        extend_focus_terms(&mut terms, &entity.label, 2);
    }
    for literal in &query_ir.literal_constraints {
        extend_focus_terms(&mut terms, &literal.text, 2);
    }
    terms
}

pub(super) fn extend_focus_terms(terms: &mut BTreeSet<String>, value: &str, minimum_chars: usize) {
    terms.extend(normalized_alnum_tokens(value, minimum_chars));
    for fragment in value.split_whitespace() {
        let token = fragment.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '_' | '-' | '.' | '/' | ':' | '@')
        });
        if token.chars().count() >= minimum_chars
            && token.chars().any(|character| matches!(character, '_' | '-' | '.' | '/' | ':' | '@'))
        {
            terms.insert(token.to_lowercase());
        }
    }
}

fn least_document_frequency_terms(
    focus_terms: &BTreeSet<String>,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<String> {
    let mut frequencies = focus_terms
        .iter()
        .filter_map(|term| {
            let document_count = chunks
                .iter()
                .filter(|chunk| {
                    chunk_fields(chunk)
                        .any(|field| has_complete_token_or_structural_match(field, term))
                })
                .map(|chunk| chunk.document_id)
                .collect::<BTreeSet<_>>()
                .len();
            (document_count > 0).then(|| (document_count, term.clone()))
        })
        .collect::<Vec<_>>();
    let minimum_frequency = frequencies.iter().map(|(count, _)| *count).min();
    frequencies.retain(|(count, _)| Some(*count) == minimum_frequency);
    frequencies.into_iter().map(|(_, term)| term).collect()
}

fn chunk_fields(chunk: &RuntimeMatchedChunk) -> impl Iterator<Item = &str> {
    [chunk.document_label.as_str(), chunk.source_text.as_str(), chunk.excerpt.as_str()].into_iter()
}

pub(super) fn has_complete_token_or_structural_match(haystack: &str, term: &str) -> bool {
    let term = term.trim();
    if haystack.is_empty() || term.is_empty() {
        return false;
    }
    if term.chars().all(char::is_alphanumeric) {
        return normalized_alnum_tokens(haystack, 1).contains(&term.to_lowercase());
    }
    let haystack = haystack.to_lowercase();
    let term = term.to_lowercase();
    haystack.match_indices(&term).any(|(start, matched)| {
        let end = start + matched.len();
        let before = haystack[..start].chars().next_back();
        let after = haystack[end..].chars().next();
        !before.is_some_and(is_identity_character) && !after.is_some_and(is_identity_character)
    })
}

fn is_identity_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

#[cfg(test)]
#[path = "technical_answer_identity_tests.rs"]
mod tests;
