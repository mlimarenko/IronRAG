use std::collections::HashSet;

use crate::domains::query_ir::QueryIR;

use super::question_intent::{QuestionIntent, classify_query_ir_intents};
use super::{
    concise_document_subject_label, focused_answer_document_id, types::RuntimeMatchedChunk,
};

pub(crate) fn build_focused_document_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let document_chunks = focused_or_single_document_chunks(question, chunks)?;
    let intents = classify_query_ir_intents(query_ir);
    let intent = [
        QuestionIntent::FocusedFormatsUnderTest,
        QuestionIntent::FocusedSecondaryHeading,
        QuestionIntent::FocusedPrimaryHeading,
    ]
    .into_iter()
    .find(|intent| intents.contains(intent))?;
    match intent {
        QuestionIntent::FocusedFormatsUnderTest => {
            extract_formats_under_test_answer(&document_chunks)
        }
        QuestionIntent::FocusedSecondaryHeading => {
            extract_secondary_document_heading(&document_chunks)
        }
        QuestionIntent::FocusedPrimaryHeading => extract_primary_document_heading(&document_chunks),
        _ => None,
    }
}

fn focused_or_single_document_chunks<'a>(
    question: &str,
    chunks: &'a [RuntimeMatchedChunk],
) -> Option<Vec<&'a RuntimeMatchedChunk>> {
    if chunks.is_empty() {
        return None;
    }

    if let Some(document_id) = focused_answer_document_id(question, chunks) {
        let focused =
            chunks.iter().filter(|chunk| chunk.document_id == document_id).collect::<Vec<_>>();
        if !focused.is_empty() {
            return Some(focused);
        }
    }

    let unique_document_ids = chunks.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>();
    (unique_document_ids.len() == 1).then(|| chunks.iter().collect::<Vec<_>>())
}

fn extract_formats_under_test_answer(document_chunks: &[&RuntimeMatchedChunk]) -> Option<String> {
    for chunk in document_chunks {
        for line in chunk.source_text.lines().map(str::trim) {
            let lowered = line.to_lowercase();
            if !lowered.contains("formats under test") {
                continue;
            }
            let Some((_, remainder)) = line.split_once(':') else {
                continue;
            };
            let formats = remainder
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            if !formats.is_empty() {
                return Some(formats.join(", "));
            }
        }
    }
    None
}

fn extract_primary_document_heading(document_chunks: &[&RuntimeMatchedChunk]) -> Option<String> {
    document_heading_lines(document_chunks).into_iter().next()
}

fn extract_secondary_document_heading(document_chunks: &[&RuntimeMatchedChunk]) -> Option<String> {
    let headings = document_heading_lines(document_chunks);
    headings
        .get(1)
        .cloned()
        .or_else(|| extract_inline_secondary_heading(document_chunks))
        .or_else(|| headings.first().cloned())
}

fn extract_inline_secondary_heading(document_chunks: &[&RuntimeMatchedChunk]) -> Option<String> {
    for chunk in document_chunks {
        let subjects = inline_secondary_heading_subjects(&chunk.excerpt, &chunk.document_label);
        for line in chunk.source_text.lines().map(str::trim) {
            for subject in &subjects {
                let Some(prefix_len) = ascii_case_insensitive_prefix_len(line, subject) else {
                    continue;
                };
                let remainder = line[prefix_len..]
                    .trim_start_matches(|character: char| {
                        character.is_whitespace() || matches!(character, ':' | '-' | '|')
                    })
                    .trim();
                if normalize_heading_line(remainder).is_some() {
                    return Some(remainder.to_string());
                }
            }
        }
    }
    None
}

fn inline_secondary_heading_subjects(excerpt: &str, document_label: &str) -> Vec<String> {
    let mut seen = HashSet::<String>::new();
    let mut subjects = Vec::new();
    let candidates =
        vec![excerpt.trim().to_string(), concise_document_subject_label(document_label)];
    for candidate in candidates {
        let candidate = candidate.trim();
        if candidate.is_empty() || !seen.insert(candidate.to_lowercase()) {
            continue;
        }
        subjects.push(candidate.to_string());
    }
    subjects
}

fn ascii_case_insensitive_prefix_len(line: &str, prefix: &str) -> Option<usize> {
    let mut cursor = 0;
    for prefix_character in prefix.chars() {
        let line_character = line[cursor..].chars().next()?;
        if !line_character.eq_ignore_ascii_case(&prefix_character) {
            return None;
        }
        cursor += line_character.len_utf8();
    }
    Some(cursor)
}

fn document_heading_lines(document_chunks: &[&RuntimeMatchedChunk]) -> Vec<String> {
    let mut headings = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for chunk in document_chunks {
        for line in chunk.source_text.lines() {
            let Some(candidate) = normalize_heading_line(line) else {
                continue;
            };
            if seen.insert(candidate.clone()) {
                headings.push(candidate);
                if headings.len() >= 6 {
                    return headings;
                }
            }
        }
    }
    headings
}

fn normalize_heading_line(line: &str) -> Option<String> {
    let candidate = line.trim().trim_start_matches('#').trim();
    if candidate.is_empty()
        || candidate.len() > 120
        || candidate.starts_with("Source:")
        || candidate.starts_with("Source type:")
        || candidate.starts_with("http://")
        || candidate.starts_with("https://")
        || candidate.starts_with('/')
        || matches!(candidate, "GET" | "POST" | "PUT" | "PATCH" | "DELETE")
    {
        return None;
    }
    Some(candidate.to_string())
}
