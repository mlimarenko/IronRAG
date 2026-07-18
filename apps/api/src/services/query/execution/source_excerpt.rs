use std::collections::BTreeSet;

use crate::shared::extraction::text_render::repair_technical_layout_noise;

use super::{
    retrieve::excerpt_for,
    technical_literals::{
        extract_config_assignment_literals, extract_config_section_literals,
        extract_explicit_path_literals, extract_package_command_literals,
        extract_parameter_literals,
    },
};

pub(super) fn structured_literal_excerpt_for(
    content: &str,
    keywords: &[String],
    max_chars: usize,
) -> Option<String> {
    let repaired = repair_technical_layout_noise(content);
    let lines = excerpt_source_lines(&repaired, max_chars)?;
    let normalized_keywords = normalized_excerpt_keywords(keywords);
    let scored = score_structured_excerpt_lines(&lines, &normalized_keywords);
    let selected = select_excerpt_line_indexes(&lines, scored, max_chars, 24, |line| {
        structured_literal_line_score(line) > 0
            || line_is_short_context_label(line, &normalized_keywords)
    });
    render_selected_excerpt(&lines, selected, max_chars)
}

fn excerpt_source_lines(content: &str, max_chars: usize) -> Option<Vec<&str>> {
    if max_chars == 0 {
        return None;
    }
    let lines: Vec<_> =
        content.trim().lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    (!lines.is_empty()).then_some(lines)
}

fn score_structured_excerpt_lines(
    lines: &[&str],
    normalized_keywords: &[String],
) -> Vec<(usize, usize)> {
    let mut scored = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let literal_score = structured_literal_line_score(line);
            if literal_score == 0 {
                return None;
            }
            let lowered = line.to_lowercase();
            let keyword_score = normalized_keywords
                .iter()
                .filter(|keyword| lowered.contains(keyword.as_str()))
                .map(|keyword| keyword.chars().count().min(24))
                .sum::<usize>();
            Some((literal_score.saturating_mul(100).saturating_add(keyword_score), index))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left_index), (right_score, right_index)| {
        right_score.cmp(left_score).then_with(|| left_index.cmp(right_index))
    });
    scored
}

fn select_excerpt_line_indexes(
    lines: &[&str],
    scored: Vec<(usize, usize)>,
    max_chars: usize,
    candidate_limit: usize,
    neighbor_is_relevant: impl Fn(&str) -> bool,
) -> BTreeSet<usize> {
    let mut selected = BTreeSet::new();
    let mut selected_chars = 0usize;
    for (_, index) in scored.into_iter().take(candidate_limit) {
        try_select_excerpt_line(lines, index, max_chars, &mut selected_chars, &mut selected);
        for neighbor in adjacent_line_indexes(index, lines.len()) {
            if neighbor_is_relevant(lines[neighbor]) {
                try_select_excerpt_line(
                    lines,
                    neighbor,
                    max_chars,
                    &mut selected_chars,
                    &mut selected,
                );
            }
        }
    }
    selected
}

fn adjacent_line_indexes(index: usize, line_count: usize) -> impl Iterator<Item = usize> {
    [index.checked_sub(1), index.checked_add(1).filter(|neighbor| *neighbor < line_count)]
        .into_iter()
        .flatten()
}

fn try_select_excerpt_line(
    lines: &[&str],
    index: usize,
    max_chars: usize,
    selected_chars: &mut usize,
    selected: &mut BTreeSet<usize>,
) {
    if selected.contains(&index) {
        return;
    }
    let line_chars = lines[index].chars().count().saturating_add(1);
    if selected_chars.saturating_add(line_chars) > max_chars && !selected.is_empty() {
        return;
    }
    selected.insert(index);
    *selected_chars = selected_chars.saturating_add(line_chars);
}

fn render_selected_excerpt(
    lines: &[&str],
    selected: BTreeSet<usize>,
    max_chars: usize,
) -> Option<String> {
    if selected.is_empty() {
        return None;
    }
    let mut excerpt_lines = Vec::new();
    let mut previous = None;
    for index in selected {
        if previous.is_some_and(|previous| previous + 1 < index) {
            excerpt_lines.push("...");
        }
        excerpt_lines.push(lines[index]);
        previous = Some(index);
    }
    let excerpt = excerpt_for(&excerpt_lines.join("\n"), max_chars);
    (!excerpt.trim().is_empty()).then_some(excerpt)
}

pub(super) fn salient_source_excerpt_for(
    content: &str,
    keywords: &[String],
    max_chars: usize,
) -> Option<String> {
    let repaired = repair_technical_layout_noise(content);
    let lines = excerpt_source_lines(&repaired, max_chars)?;
    let normalized_keywords = normalized_excerpt_keywords(keywords);
    let scored = score_salient_excerpt_lines(&lines, &normalized_keywords);
    let selected = select_excerpt_line_indexes(&lines, scored, max_chars, 32, |line| {
        source_local_evidence_line_score(line, &normalized_keywords) > 0
    });
    render_selected_excerpt(&lines, selected, max_chars)
}

fn score_salient_excerpt_lines(
    lines: &[&str],
    normalized_keywords: &[String],
) -> Vec<(usize, usize)> {
    let mut scored = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let score = source_local_evidence_line_score(line, normalized_keywords);
            (score > 0).then_some((score, index))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left_index), (right_score, right_index)| {
        right_score.cmp(left_score).then_with(|| left_index.cmp(right_index))
    });
    scored
}

pub(super) fn source_local_evidence_line_score(line: &str, keywords: &[String]) -> usize {
    let line = line.trim();
    if line.is_empty() {
        return 0;
    }
    let lowered = line.to_lowercase();
    let keyword_score = keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 3)
        .filter(|keyword| lowered.contains(*keyword))
        .map(|keyword| keyword.chars().count().min(24))
        .sum::<usize>();
    let structural_score = structured_literal_line_score(line);
    let surface_score = source_local_salience_score(line);
    if structural_score == 0 && surface_score == 0 && keyword_score == 0 {
        return 0;
    }
    structural_score
        .saturating_mul(100)
        .saturating_add(surface_score.saturating_mul(10))
        .saturating_add(keyword_score)
}

fn normalized_excerpt_keywords(keywords: &[String]) -> Vec<String> {
    keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 3)
        .map(|keyword| keyword.to_lowercase())
        .collect()
}

pub(super) fn structured_literal_line_score(line: &str) -> usize {
    extract_config_assignment_literals(line, 4).len().saturating_mul(8)
        + extract_config_section_literals(line, 4).len().saturating_mul(6)
        + extract_explicit_path_literals(line, 4).len().saturating_mul(5)
        + extract_package_command_literals(line, 2).len().saturating_mul(5)
        + extract_parameter_literals(line, 8).len().saturating_mul(3)
        + usize::from(line_has_key_value_literal_surface(line)).saturating_mul(3)
        + usize::from(line_has_table_like_literal_surface(line)).saturating_mul(2)
}

fn line_has_table_like_literal_surface(line: &str) -> bool {
    let alphanumeric_count = line.chars().filter(|ch| ch.is_alphanumeric()).count();
    alphanumeric_count >= 3
        && (line.matches('|').count() >= 2
            || line.split('\t').filter(|cell| !cell.trim().is_empty()).count() >= 3)
}

fn source_local_salience_score(line: &str) -> usize {
    let line = line.trim();
    let alphanumeric_count = line.chars().filter(|ch| ch.is_alphanumeric()).count();
    if alphanumeric_count < 3 {
        return 0;
    }
    usize::from(line_has_list_marker_surface(line)).saturating_mul(4)
        + usize::from(line_has_label_value_surface(line)).saturating_mul(4)
        + usize::from(line_has_identifier_token_surface(line)).saturating_mul(3)
        + usize::from(line_has_bracket_or_quote_surface(line)).saturating_mul(2)
        + usize::from(line_has_numeric_marker_surface(line)).saturating_mul(2)
        + usize::from(line_has_compact_fact_surface(line)).saturating_mul(1)
}

fn line_has_list_marker_surface(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }
    let mut chars = trimmed.chars().peekable();
    let mut digits = 0usize;
    while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
        digits = digits.saturating_add(1);
        chars.next();
    }
    digits > 0 && chars.next().is_some_and(|ch| matches!(ch, '.' | ')' | ':'))
}

fn line_has_label_value_surface(line: &str) -> bool {
    let Some((left, right)) =
        line.split_once(':').or_else(|| line.split_once(" - ")).or_else(|| line.split_once(" | "))
    else {
        return false;
    };
    let left = left.trim().trim_start_matches(['-', '*', '+']).trim();
    let right = right.trim();
    !left.is_empty()
        && !right.is_empty()
        && left.chars().count() <= 120
        && left.chars().any(|ch| ch.is_alphanumeric())
        && right.chars().any(|ch| ch.is_alphanumeric())
}

fn line_has_identifier_token_surface(line: &str) -> bool {
    line.split_whitespace().any(token_has_identifier_surface)
}

fn token_has_identifier_surface(token: &str) -> bool {
    let token = token.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(ch, ',' | ';' | '"' | '\'' | '`' | '(' | ')' | '{' | '}' | '<' | '>')
    });
    let mut alphanumeric_count = 0usize;
    let mut has_alpha = false;
    let mut has_digit = false;
    let mut has_connector = false;
    for ch in token.chars() {
        if ch.is_alphanumeric() {
            alphanumeric_count = alphanumeric_count.saturating_add(1);
            has_alpha |= ch.is_alphabetic();
            has_digit |= ch.is_ascii_digit();
        } else if matches!(ch, '_' | '-' | '.' | '/' | ':' | '#' | '@' | '[' | ']') {
            has_connector = true;
        }
    }
    alphanumeric_count >= 3 && (has_connector || (has_alpha && has_digit))
}

fn line_has_bracket_or_quote_surface(line: &str) -> bool {
    (line.contains('[') && line.contains(']'))
        || (line.contains('(') && line.contains(')'))
        || (line.contains('"') && line.matches('"').count() >= 2)
        || (line.contains('`') && line.matches('`').count() >= 2)
}

fn line_has_numeric_marker_surface(line: &str) -> bool {
    let has_digit = line.chars().any(|ch| ch.is_ascii_digit());
    has_digit && line.chars().any(|ch| matches!(ch, ':' | '.' | '-' | '/' | '#'))
}

fn line_has_compact_fact_surface(line: &str) -> bool {
    let char_count = line.chars().count();
    if !(24..=240).contains(&char_count) {
        return false;
    }
    let alphanumeric_count = line.chars().filter(|ch| ch.is_alphanumeric()).count();
    alphanumeric_count >= 12
        && line.chars().any(|ch| matches!(ch, ':' | ';' | '.' | '!' | '?' | '|' | ')' | ']'))
}

fn line_is_short_context_label(line: &str, normalized_keywords: &[String]) -> bool {
    let char_count = line.chars().count();
    char_count <= 160
        && (line.ends_with(':')
            || line.starts_with('#')
            || normalized_keywords
                .iter()
                .any(|keyword| line.to_lowercase().contains(keyword.as_str())))
}

pub(super) fn line_has_key_value_literal_surface(line: &str) -> bool {
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    let key = key.trim().trim_start_matches(['-', '*']).trim();
    let value = value.trim();
    key.chars().any(|ch| ch.is_alphabetic())
        && key.chars().filter(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '-').count() >= 2
        && !value.is_empty()
        && !key.contains("==")
}
