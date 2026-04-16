use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::retrieve::score_value;
use super::types::RuntimeMatchedChunk;

pub(super) fn technical_literal_focus_keywords(question: &str) -> Vec<String> {
    let ignored_keywords = [
        "если",
        "агенту",
        "ему",
        "какой",
        "какие",
        "какая",
        "какого",
        "какому",
        "endpoint",
        "url",
        "port",
        "порт",
        "path",
        "путь",
        "пути",
        "метод",
        "method",
        "использует",
        "используют",
        "used",
        "uses",
        "возвращает",
        "получить",
        "нужно",
        "нужен",
        "нужны",
        "отдельно",
    ]
    .into_iter()
    .collect::<HashSet<_>>();
    let mut keywords = Vec::new();
    let mut seen = HashSet::new();
    for token in question
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .map(str::trim)
        .filter(|token| token.chars().count() >= 4)
        .map(str::to_lowercase)
    {
        if ignored_keywords.contains(token.as_str()) {
            continue;
        }
        if seen.insert(token.clone()) {
            keywords.push(token.clone());
        }
    }
    keywords
}

fn technical_keyword_stem(keyword: &str) -> Option<String> {
    let stem = keyword.chars().take(5).collect::<String>();
    (stem.chars().count() >= 4).then_some(stem)
}

pub(super) fn technical_keyword_present(lowered_text: &str, keyword: &str) -> bool {
    lowered_text.contains(keyword)
        || technical_keyword_stem(keyword).is_some_and(|stem| lowered_text.contains(stem.as_str()))
}

pub(super) fn technical_keyword_weight(lowered_text: &str, keyword: &str) -> usize {
    if lowered_text.contains(keyword) {
        return keyword.chars().count().min(24);
    }
    if technical_keyword_stem(keyword).is_some_and(|stem| lowered_text.contains(stem.as_str())) {
        return 4;
    }
    0
}

pub(super) fn question_mentions_pagination(question: &str) -> bool {
    let lowered = question.to_lowercase();
    ["bypage", "page", "pagesize", "pagenumber", "пейдж", "постранич", "страниц", "пагинац"]
        .iter()
        .any(|marker| lowered.contains(marker))
}

pub(super) fn question_mentions_protocol(question: &str) -> bool {
    let lowered = question.to_lowercase();
    lowered.contains("protocol") || lowered.contains("протокол")
}

pub(super) fn technical_literal_focus_segments_text(question: &str) -> Vec<String> {
    question
        .to_lowercase()
        .replace(" и отдельно ", " | ")
        .replace(" отдельно ", " | ")
        .replace(" and then ", " | ")
        .replace(" then ", " | ")
        .replace(" and ", " | ")
        .replace([';', ','], "|")
        .split('|')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
}

pub(super) fn technical_literal_focus_keyword_segments(question: &str) -> Vec<Vec<String>> {
    let segments = technical_literal_focus_segments_text(question)
        .into_iter()
        .map(|segment| technical_literal_focus_keywords(&segment))
        .filter(|keywords| !keywords.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        let fallback = technical_literal_focus_keywords(question);
        if fallback.is_empty() { Vec::new() } else { vec![fallback] }
    } else {
        segments
    }
}

pub(super) fn document_local_focus_keywords(
    question: &str,
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
    let best_segment = technical_literal_focus_keyword_segments(question)
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
    pagination_requested: bool,
) -> isize {
    let lowered = text.to_lowercase();
    let keyword_count = keywords.len();
    let mut score = keywords
        .iter()
        .enumerate()
        .map(|(index, keyword)| {
            let priority = keyword_count.saturating_sub(index).max(1) as isize;
            (technical_keyword_weight(&lowered, keyword) as isize) * priority
        })
        .sum::<isize>();
    let has_pagination_marker = ["bypage", "pagesize", "pagenumber", "number_starting"]
        .iter()
        .any(|marker| lowered.contains(marker));
    if has_pagination_marker {
        score += if pagination_requested { 12 } else { -40 };
    }
    score
}

pub(super) fn select_document_balanced_chunks<'a>(
    question: &str,
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
        let local_keywords = document_local_focus_keywords(question, document_chunks, keywords);
        document_chunks.sort_by(|left, right| {
            let left_match = technical_chunk_selection_score(
                &format!("{} {}", left.excerpt, left.source_text),
                &local_keywords,
                pagination_requested,
            );
            let right_match = technical_chunk_selection_score(
                &format!("{} {}", right.excerpt, right.source_text),
                &local_keywords,
                pagination_requested,
            );
            right_match
                .cmp(&left_match)
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
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
