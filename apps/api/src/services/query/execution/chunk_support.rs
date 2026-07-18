//! Neutral chunk mapping and evidence-text policy shared by retrieval and answer stages.

use std::collections::{BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use crate::infra::knowledge_rows::{KnowledgeChunkRow, KnowledgeDocumentRow};
use crate::shared::extraction::{
    record_jsonl::focused_record_unit_excerpt, text_render::repair_technical_layout_noise,
};

use super::command_shape::content_is_command_dense;
use super::technical_literals::{
    extract_config_assignment_literals, extract_config_section_literals,
    extract_explicit_path_literals, extract_package_command_literals, extract_parameter_literals,
};
use super::types::{RuntimeChunkScoreKind, RuntimeMatchedChunk};

const CONFIG_PATH_EXTENSIONS: [&str; 8] =
    [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"];

/// Maps a canonical knowledge chunk into answer evidence.
pub(crate) fn map_chunk_hit(
    chunk: KnowledgeChunkRow,
    score: f32,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    keywords: &[String],
) -> Option<RuntimeMatchedChunk> {
    if chunk.raptor_level.is_some() {
        return None;
    }
    let document = document_index.get(&chunk.document_id)?;
    canonical_document_revision_id(document)?;
    let source_text = chunk_answer_source_text(&chunk);
    let excerpt = if chunk.chunk_kind.as_deref() == Some("source_unit") {
        focused_record_unit_excerpt(&source_text, keywords, 280)
            .unwrap_or_else(|| focused_excerpt_for(&source_text, keywords, 280))
    } else {
        focused_excerpt_for(&source_text, keywords, 280)
    };
    Some(RuntimeMatchedChunk {
        chunk_id: chunk.chunk_id,
        document_id: chunk.document_id,
        revision_id: chunk.revision_id,
        chunk_index: chunk.chunk_index,
        chunk_kind: chunk.chunk_kind.clone(),
        document_label: document
            .title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| document.external_key.clone()),
        excerpt,
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text,
    })
}

pub(crate) fn canonical_document_revision_id(document: &KnowledgeDocumentRow) -> Option<Uuid> {
    document.readable_revision_id.or(document.active_revision_id)
}

pub(super) fn chunk_answer_source_text(chunk: &KnowledgeChunkRow) -> String {
    if chunk.chunk_kind.as_deref() == Some("table_row") {
        return repair_technical_layout_noise(&chunk.normalized_text);
    }
    if chunk.chunk_kind.as_deref() == Some("source_unit") {
        if !chunk.content_text.trim().is_empty() {
            return repair_technical_layout_noise(&chunk.content_text);
        }
        if !chunk.normalized_text.trim().is_empty() {
            return repair_technical_layout_noise(&chunk.normalized_text);
        }
    }
    let content_text = (!chunk.content_text.trim().is_empty())
        .then(|| repair_technical_layout_noise(&chunk.content_text));
    let window_text = chunk
        .window_text
        .as_deref()
        .filter(|window| !window.trim().is_empty())
        .map(repair_technical_layout_noise);
    let normalized_text = (!chunk.normalized_text.trim().is_empty())
        .then(|| repair_technical_layout_noise(&chunk.normalized_text));
    let fallback = if content_text.is_some() {
        merge_chunk_source_text_variants([content_text.as_deref(), window_text.as_deref()])
    } else {
        merge_chunk_source_text_variants([window_text.as_deref(), normalized_text.as_deref()])
    };
    chunk_literal_preserving_source_text(chunk, &fallback).unwrap_or(fallback)
}

fn merge_chunk_source_text_variants<const N: usize>(values: [Option<&str>; N]) -> String {
    const MAX_MERGED_CHUNK_SOURCE_CHARS: usize = 16_000;

    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    for value in values.into_iter().flatten() {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        if seen.iter().any(|seen_value: &String| {
            seen_value.contains(&normalized) || normalized.contains(seen_value)
        }) {
            continue;
        }
        if seen.insert(normalized) {
            parts.push(trimmed.to_string());
        }
    }
    let merged = parts.join("\n");
    if merged.chars().count() <= MAX_MERGED_CHUNK_SOURCE_CHARS {
        merged
    } else {
        excerpt_for(&merged, MAX_MERGED_CHUNK_SOURCE_CHARS)
    }
}

fn chunk_literal_preserving_source_text(
    chunk: &KnowledgeChunkRow,
    fallback: &str,
) -> Option<String> {
    const MAX_LITERAL_PRESERVING_SOURCE_CHARS: usize = 16_000;

    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    for value in [
        chunk.window_text.as_deref(),
        Some(chunk.content_text.as_str()),
        Some(chunk.normalized_text.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        let repaired = repair_technical_layout_noise(value);
        let trimmed = repaired.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        if seen.insert(normalized) {
            parts.push(trimmed.to_string());
        }
    }
    if parts.len() <= 1 {
        return None;
    }
    let candidate = parts.join("\n");
    let candidate_score = chunk_structured_literal_text_score(&candidate);
    if candidate_score == 0 {
        return None;
    }
    let fallback_score = chunk_structured_literal_text_score(fallback);
    if candidate_score <= fallback_score
        && !chunk_text_preserves_missing_structured_literals(&candidate, fallback)
    {
        return None;
    }
    if candidate.chars().count() <= MAX_LITERAL_PRESERVING_SOURCE_CHARS {
        return Some(candidate);
    }
    Some(excerpt_for(&candidate, MAX_LITERAL_PRESERVING_SOURCE_CHARS))
}

fn chunk_structured_literal_text_score(text: &str) -> usize {
    text.lines().map(str::trim).map(chunk_structured_literal_line_score).sum()
}

fn chunk_structured_literal_line_score(line: &str) -> usize {
    extract_config_assignment_literals(line, 4).len().saturating_mul(8)
        + extract_config_section_literals(line, 4).len().saturating_mul(6)
        + extract_explicit_path_literals(line, 4).len().saturating_mul(5)
        + extract_package_command_literals(line, 2).len().saturating_mul(5)
        + extract_parameter_literals(line, 8).len().saturating_mul(3)
        + usize::from(chunk_line_has_key_value_literal_surface(line)).saturating_mul(3)
        + usize::from(chunk_line_has_table_like_literal_surface(line)).saturating_mul(2)
}

fn chunk_line_has_key_value_literal_surface(line: &str) -> bool {
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    let key = key.trim().trim_start_matches(['-', '*']).trim();
    let value = value.trim();
    key.chars().any(char::is_alphabetic)
        && key.chars().filter(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '-').count() >= 2
        && !value.is_empty()
        && !key.contains("==")
}

fn chunk_line_has_table_like_literal_surface(line: &str) -> bool {
    let alphanumeric_count = line.chars().filter(|ch| ch.is_alphanumeric()).count();
    alphanumeric_count >= 3
        && (line.matches('|').count() >= 2
            || line.split('\t').filter(|cell| !cell.trim().is_empty()).count() >= 3)
}

fn chunk_text_preserves_missing_structured_literals(candidate: &str, fallback: &str) -> bool {
    chunk_structured_literals(candidate, 32).iter().any(|literal| !fallback.contains(literal))
}

fn chunk_structured_literals(text: &str, limit: usize) -> Vec<String> {
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for value in extract_config_assignment_literals(text, limit) {
        push_unique_chunk_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_config_section_literals(text, limit) {
        push_unique_chunk_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_explicit_path_literals(text, limit) {
        push_unique_chunk_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_package_command_literals(text, limit) {
        push_unique_chunk_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_parameter_literals(text, limit) {
        push_unique_chunk_literal(&mut values, &mut seen, value, limit);
    }
    values
}

fn push_unique_chunk_literal(
    values: &mut Vec<String>,
    seen: &mut HashSet<String>,
    value: String,
    limit: usize,
) {
    if values.len() >= limit {
        return;
    }
    if seen.insert(value.to_lowercase()) {
        values.push(value);
    }
}

/// Returns true when a chunk contains both an executable package command and
/// an explicit configuration-file path.
pub(crate) fn chunk_is_setup_focus_command_path_anchor(chunk: &RuntimeMatchedChunk) -> bool {
    !extract_package_command_literals(&chunk.source_text, 1).is_empty()
        && extract_explicit_path_literals(&chunk.source_text, 8).into_iter().any(|path| {
            let path = path.to_ascii_lowercase();
            CONFIG_PATH_EXTENSIONS.iter().any(|extension| path.ends_with(extension))
        })
}

/// Returns a bounded, trimmed excerpt.
pub(crate) fn excerpt_for(content: &str, max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let excerpt = trimmed.chars().take(max_chars).collect::<String>();
    format!("{excerpt}...")
}

/// Selects a bounded line window centered on the strongest keyword match.
pub(crate) fn focused_excerpt_for(content: &str, keywords: &[String], max_chars: usize) -> String {
    let trimmed = content.trim();
    let lines = trimmed.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }
    let normalized_keywords = keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    let Some(center_index) = strongest_keyword_line(&lines, &normalized_keywords) else {
        return excerpt_for(trimmed, max_chars);
    };
    focused_line_window(&lines, center_index, max_chars)
}

fn strongest_keyword_line(lines: &[&str], keywords: &[String]) -> Option<usize> {
    if keywords.is_empty() {
        return None;
    }
    lines
        .iter()
        .enumerate()
        .fold(None, |strongest, (index, line)| {
            let score = keyword_line_score(line, keywords);
            match strongest {
                Some((_, strongest_score)) if score <= strongest_score => strongest,
                _ if score > 0 => Some((index, score)),
                _ => strongest,
            }
        })
        .map(|(index, _)| index)
}

fn keyword_line_score(line: &str, keywords: &[String]) -> usize {
    let lowered = line.to_lowercase();
    keywords
        .iter()
        .filter(|keyword| lowered.contains(keyword.as_str()))
        .map(|keyword| keyword.chars().count().min(24))
        .sum()
}

fn focused_line_window(lines: &[&str], center_index: usize, max_chars: usize) -> String {
    let mut selected = BTreeSet::from([center_index]);
    let mut radius = 1usize;
    loop {
        let excerpt = selected.iter().map(|&index| lines[index]).collect::<Vec<_>>().join(" ");
        if excerpt.chars().count() >= max_chars
            || selected.len() >= 5
            || selected.len() == lines.len()
        {
            return excerpt_for(&excerpt, max_chars);
        }
        if !expand_focused_line_window(&mut selected, center_index, radius, lines.len()) {
            return excerpt_for(&excerpt, max_chars);
        }
        radius += 1;
    }
}

fn expand_focused_line_window(
    selected: &mut BTreeSet<usize>,
    center_index: usize,
    radius: usize,
    line_count: usize,
) -> bool {
    let mut expanded = false;
    if center_index >= radius {
        expanded |= selected.insert(center_index - radius);
    }
    if center_index + radius < line_count {
        expanded |= selected.insert(center_index + radius);
    }
    expanded
}

/// Builds a bounded command-dense excerpt through the shared evidence policy.
pub(crate) fn command_dense_excerpt_for(content: &str, max_chars: usize) -> Option<String> {
    if !content_is_command_dense(content) {
        return None;
    }
    let repaired = repair_technical_layout_noise(content);
    let excerpt = excerpt_for(&repaired, max_chars);
    (!excerpt.trim().is_empty()).then_some(excerpt)
}
