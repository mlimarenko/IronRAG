use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::domains::query_ir::{QueryIR, QueryScope};
use crate::services::query::effective_query::current_question_segment;
use crate::services::query::planner::strip_leading_question_marker;
use crate::services::query::text_match::related_prefix_token_match;

use super::retrieve::score_value;
use super::types::RuntimeMatchedChunk;

/// Extracts focus keywords for technical chunk ranking.
///
/// When `ir` carries literal constraints, tokens from those constraints are
/// emitted first because they are the strongest focus signal. The remaining
/// structural tokens from the question are still retained afterwards: exact
/// technical answers often require the surrounding verb, endpoint role, or
/// setting purpose to disambiguate between nearby literal blocks.
///
/// When `ir` is `None` (retrieval runs in parallel with IR compilation, so
/// the lexical query builder cannot see the IR yet) or carries no literal
/// constraints (Describe / ConfigureHow / Enumerate questions), every
/// ≥4-char token from the question is kept. Downstream ranking already
/// weighs tokens by their presence in document text, so tokens that do not
/// appear in candidate chunks contribute nothing without needing a
/// hard-coded stop list.
pub(super) fn technical_literal_focus_keywords(
    question: &str,
    ir: Option<&QueryIR>,
) -> Vec<String> {
    let mut keywords = Vec::new();
    let mut seen = HashSet::new();
    if let Some(ir) = ir {
        for literal in &ir.literal_constraints {
            for token in technical_literal_question_tokens(&literal.text) {
                if seen.insert(token.clone()) {
                    keywords.push(token);
                }
            }
        }
    }
    let current_question = current_question_segment(question);
    for token in strip_leading_question_marker(current_question)
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .map(str::trim)
        .filter(|token| token.chars().count() >= 4)
        .map(str::to_lowercase)
    {
        if seen.insert(token.clone()) {
            keywords.push(token.clone());
        }
    }
    keywords
}

fn technical_literal_question_tokens(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .map(str::trim)
        .filter(|token| token.chars().count() >= 4)
        .map(str::to_lowercase)
}

fn technical_keyword_stem(keyword: &str) -> Option<String> {
    let stem = keyword.chars().take(5).collect::<String>();
    (stem.chars().count() >= 4).then_some(stem)
}

pub(super) fn technical_keyword_present(lowered_text: &str, keyword: &str) -> bool {
    lowered_text.contains(keyword)
        || technical_keyword_stem(keyword).is_some_and(|stem| lowered_text.contains(stem.as_str()))
        || technical_keyword_related_prefix_present(lowered_text, keyword)
}

pub(super) fn technical_keyword_weight(lowered_text: &str, keyword: &str) -> usize {
    if lowered_text.contains(keyword) {
        return keyword.chars().count().min(24);
    }
    if technical_keyword_stem(keyword).is_some_and(|stem| lowered_text.contains(stem.as_str())) {
        return 4;
    }
    if technical_keyword_related_prefix_present(lowered_text, keyword) {
        return 3;
    }
    0
}

fn technical_keyword_related_prefix_present(lowered_text: &str, keyword: &str) -> bool {
    keyword.chars().count() >= 5
        && lowered_text
            .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .any(|token| related_prefix_token_match(keyword, token))
}

pub(super) fn technical_literal_focus_keyword_segments(
    question: &str,
    ir: Option<&QueryIR>,
) -> Vec<Vec<String>> {
    if let Some(ir) = ir
        && matches!(ir.scope, QueryScope::MultiDocument)
    {
        let literal_segments = ir
            .literal_constraints
            .iter()
            .map(|literal| technical_literal_question_tokens(&literal.text).collect::<Vec<_>>())
            .filter(|keywords| !keywords.is_empty())
            .collect::<Vec<_>>();
        if !literal_segments.is_empty() {
            return literal_segments;
        }
    }

    let current_question = current_question_segment(question);
    let segments = current_question
        .split([';', ',', '\n'])
        .map(|segment| technical_literal_focus_keywords(&segment, ir))
        .filter(|keywords| !keywords.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        let fallback = technical_literal_focus_keywords(current_question, ir);
        if fallback.is_empty() { Vec::new() } else { vec![fallback] }
    } else {
        segments
    }
}

pub(super) fn document_local_focus_keywords(
    question: &str,
    ir: Option<&QueryIR>,
    chunks: &[&RuntimeMatchedChunk],
    question_keywords: &[String],
) -> Vec<String> {
    if question_keywords.is_empty() {
        return Vec::new();
    }

    let document_text = chunks
        .iter()
        .map(|chunk| format!("{} {}", chunk.excerpt, chunk.source_text))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let best_segment = technical_literal_focus_keyword_segments(question, ir)
        .into_iter()
        .map(|segment_keywords| {
            let score = segment_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&document_text, keyword))
                .sum::<usize>();
            (score, segment_keywords)
        })
        .max_by_key(|(score, _)| *score)
        .filter(|(score, _)| *score > 0)
        .map(|(_, segment_keywords)| segment_keywords);
    if let Some(segment_keywords) = best_segment {
        let local_segment_keywords = segment_keywords
            .iter()
            .filter(|keyword| technical_keyword_present(&document_text, keyword))
            .cloned()
            .collect::<Vec<_>>();
        if !local_segment_keywords.is_empty() {
            return local_segment_keywords;
        }
        return segment_keywords;
    }
    let local_keywords = question_keywords
        .iter()
        .filter(|keyword| technical_keyword_present(&document_text, keyword))
        .cloned()
        .collect::<Vec<_>>();
    if local_keywords.is_empty() { question_keywords.to_vec() } else { local_keywords }
}

pub(super) fn technical_chunk_selection_score(
    text: &str,
    keywords: &[String],
    _pagination_requested: bool,
) -> isize {
    let lowered = text.to_lowercase();
    let keyword_count = keywords.len();
    keywords
        .iter()
        .enumerate()
        .map(|(index, keyword)| {
            let priority = keyword_count.saturating_sub(index).max(1) as isize;
            (technical_keyword_weight(&lowered, keyword) as isize) * priority
        })
        .sum::<isize>()
}

pub(super) fn select_document_balanced_chunks<'a>(
    question: &str,
    ir: Option<&QueryIR>,
    chunks: &'a [RuntimeMatchedChunk],
    keywords: &[String],
    pagination_requested: bool,
    max_total_chunks: usize,
    max_chunks_per_document: usize,
) -> Vec<&'a RuntimeMatchedChunk> {
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();

    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    for document_chunks in per_document_chunks.values_mut() {
        let local_keywords = document_local_focus_keywords(question, ir, document_chunks, keywords);
        let score_by_chunk_id = document_chunks
            .iter()
            .map(|chunk| {
                let match_score = technical_chunk_selection_score(
                    &format!("{} {}", chunk.excerpt, chunk.source_text),
                    &local_keywords,
                    pagination_requested,
                );
                (chunk.chunk_id, (match_score, score_value(chunk.score)))
            })
            .collect::<HashMap<_, _>>();
        document_chunks.sort_by(|left, right| {
            let (left_match, left_score) =
                score_by_chunk_id.get(&left.chunk_id).copied().unwrap_or_default();
            let (right_match, right_score) =
                score_by_chunk_id.get(&right.chunk_id).copied().unwrap_or_default();
            right_match.cmp(&left_match).then_with(|| right_score.total_cmp(&left_score))
        });
    }

    let mut selected = Vec::new();
    for target_document_slot in 0..max_chunks_per_document {
        for document_id in &ordered_document_ids {
            if selected.len() >= max_total_chunks {
                return selected;
            }
            if let Some(chunk) = per_document_chunks
                .get(document_id)
                .and_then(|document_chunks| document_chunks.get(target_document_slot))
            {
                selected.push(*chunk);
            }
        }
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn technical_keyword_weight_accepts_longer_related_prefix_token() {
        assert_eq!(technical_keyword_weight("acmealpha payment configuration", "acmew"), 3);
    }

    #[test]
    fn technical_keyword_weight_rejects_short_prefix_target_tokens() {
        assert_eq!(technical_keyword_weight("acmealpha payment configuration", "acmr"), 0);
    }
}
