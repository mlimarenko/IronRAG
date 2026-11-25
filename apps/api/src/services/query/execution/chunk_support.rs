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

/// Largest single evidence source handed to generation or verification.
///
/// The unit is UTF-8 bytes on purpose. The verifier aggregates evidence in
/// bytes (`MAX_EVIDENCE_UTF8_BYTES`), so a character cap here let one Cyrillic
/// chunk exceed the whole verifier budget on its own.
pub(crate) const MAX_EVIDENCE_SOURCE_UTF8_BYTES: usize = 16_000;

/// Longest prefix of `text` that fits `max_bytes` without splitting a code
/// point. Callers that need a closing marker add it themselves.
pub(crate) fn bounded_utf8_prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Canonical bounded evidence text for one selected source. Generation and the
/// semantic verifier both read exactly this value.
pub(crate) fn bounded_evidence_text(text: &str) -> String {
    bounded_utf8_prefix(text.trim(), MAX_EVIDENCE_SOURCE_UTF8_BYTES).trim_end().to_string()
}

/// Maps a stored knowledge chunk into answer evidence.
pub(crate) fn map_chunk_hit(
    chunk: KnowledgeChunkRow,
    score: f32,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    keywords: &[String],
) -> Option<RuntimeMatchedChunk> {
    let document = document_index.get(&chunk.document_id)?;
    if !document_has_exact_readable_revision(document, chunk.revision_id) {
        return None;
    }
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

pub(crate) fn current_document_revision_id(document: &KnowledgeDocumentRow) -> Option<Uuid> {
    document.readable_revision_id.or(document.active_revision_id)
}

/// Returns whether a revision is the document's currently query-readable head.
pub(super) fn document_has_exact_readable_revision(
    document: &KnowledgeDocumentRow,
    revision_id: Uuid,
) -> bool {
    document.is_query_readable() && document.readable_revision_id == Some(revision_id)
}

/// The chunk's own text, shared by both answer generation and semantic
/// verification so the generator never cites neighboring `window_text` that
/// the verifier cannot attribute to this chunk id.
pub(super) fn chunk_own_source_text(chunk: &KnowledgeChunkRow) -> String {
    chunk_answer_source_text(chunk)
}

pub(super) fn chunk_answer_source_text(chunk: &KnowledgeChunkRow) -> String {
    if chunk.chunk_kind.as_deref() == Some("table_row") {
        return bounded_evidence_text(&repair_technical_layout_noise(&chunk.normalized_text));
    }
    if chunk.chunk_kind.as_deref() == Some("source_unit") {
        if !chunk.content_text.trim().is_empty() {
            return bounded_evidence_text(&repair_technical_layout_noise(&chunk.content_text));
        }
        if !chunk.normalized_text.trim().is_empty() {
            return bounded_evidence_text(&repair_technical_layout_noise(&chunk.normalized_text));
        }
    }
    let content_text = (!chunk.content_text.trim().is_empty())
        .then(|| repair_technical_layout_noise(&chunk.content_text));
    let normalized_text = (!chunk.normalized_text.trim().is_empty())
        .then(|| repair_technical_layout_noise(&chunk.normalized_text));
    let fallback = content_text
        .or(normalized_text)
        .or_else(|| chunk.window_text.as_deref().map(repair_technical_layout_noise))
        .unwrap_or_default();
    bounded_evidence_text(
        &chunk_literal_preserving_source_text(chunk, &fallback).unwrap_or(fallback),
    )
}

fn chunk_literal_preserving_source_text(
    chunk: &KnowledgeChunkRow,
    fallback: &str,
) -> Option<String> {
    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    for value in [Some(chunk.content_text.as_str()), Some(chunk.normalized_text.as_str())]
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
    if candidate.len() <= MAX_EVIDENCE_SOURCE_UTF8_BYTES {
        return Some(candidate);
    }
    Some(bounded_utf8_prefix(&candidate, MAX_EVIDENCE_SOURCE_UTF8_BYTES).to_string())
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

/// The omitted-text marker belongs to the display, not to the source passage.
pub(super) struct ContextExcerpt<'a> {
    pub(super) source: &'a str,
    truncated: bool,
}

impl<'a> ContextExcerpt<'a> {
    pub(super) fn new(content: &'a str, max_chars: usize) -> Self {
        let text = content.trim();
        match text.char_indices().nth(max_chars) {
            Some((end, _)) => Self { source: &text[..end], truncated: true },
            None => Self { source: text, truncated: false },
        }
    }
}

impl std::fmt::Display for ContextExcerpt<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.source)?;
        if self.truncated { f.write_str("...") } else { Ok(()) }
    }
}

pub(crate) fn excerpt_for(content: &str, max_chars: usize) -> String {
    ContextExcerpt::new(content, max_chars).to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk_row(kind: &str, content: &str, normalized: &str) -> KnowledgeChunkRow {
        KnowledgeChunkRow {
            chunk_id: Uuid::from_u128(1),
            workspace_id: Uuid::from_u128(2),
            library_id: Uuid::from_u128(3),
            document_id: Uuid::from_u128(4),
            revision_id: Uuid::from_u128(5),
            chunk_index: 0,
            chunk_kind: Some(kind.to_string()),
            content_text: content.to_string(),
            normalized_text: normalized.to_string(),
            span_start: None,
            span_end: None,
            token_count: None,
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
            quality_score: Some(1.0),
            window_text: None,
            occurred_at: None,
            occurred_until: None,
        }
    }

    fn assert_within_bound(text: &str) {
        assert!(
            text.len() <= MAX_EVIDENCE_SOURCE_UTF8_BYTES,
            "evidence text must stay within the UTF-8 byte bound"
        );
        assert!(text.is_char_boundary(text.len()));
        assert!(std::str::from_utf8(text.as_bytes()).is_ok());
    }

    #[test]
    fn long_russian_source_is_bounded_by_utf8_bytes_not_characters() {
        let russian = "Привет, мир! Это длинный русский источник. ".repeat(800);
        assert!(russian.len() > MAX_EVIDENCE_SOURCE_UTF8_BYTES * 2);
        let text = chunk_answer_source_text(&chunk_row("paragraph", &russian, &russian));
        assert_within_bound(&text);
        assert!(!text.is_empty());
        // A character cap would leave 16,000 chars = 32,000 bytes here.
        assert!(text.chars().count() < MAX_EVIDENCE_SOURCE_UTF8_BYTES);
    }

    #[test]
    fn long_ascii_source_is_bounded_by_utf8_bytes() {
        let ascii = "a".repeat(MAX_EVIDENCE_SOURCE_UTF8_BYTES * 2);
        let text = chunk_answer_source_text(&chunk_row("paragraph", &ascii, &ascii));
        assert_within_bound(&text);
        assert_eq!(text.len(), MAX_EVIDENCE_SOURCE_UTF8_BYTES);
    }

    #[test]
    fn source_unit_and_table_row_cannot_bypass_the_evidence_bound() {
        let russian = "Строка таблицы: значение параметра. ".repeat(1_000);
        for kind in ["source_unit", "table_row"] {
            let text = chunk_answer_source_text(&chunk_row(kind, &russian, &russian));
            assert_within_bound(&text);
            assert!(!text.is_empty(), "{kind} source must not be dropped by the bound");
        }
        // The table_row path reads normalized_text even when content is empty.
        let table = chunk_answer_source_text(&chunk_row("table_row", "", &russian));
        assert_within_bound(&table);
        assert!(!table.is_empty());
    }

    #[test]
    fn bounded_prefix_never_splits_a_code_point() {
        let text = "ё".repeat(10_000);
        let prefix = bounded_utf8_prefix(&text, MAX_EVIDENCE_SOURCE_UTF8_BYTES);
        assert!(prefix.len() <= MAX_EVIDENCE_SOURCE_UTF8_BYTES);
        assert!(text.is_char_boundary(prefix.len()));
        assert_eq!(prefix.len() % 2, 0);
    }
}
