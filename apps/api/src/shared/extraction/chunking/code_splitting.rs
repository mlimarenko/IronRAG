use super::code_boundaries::detect_code_boundaries;
use super::{StructuredBlockData, StructuredBlockKind, char_count, chunk_block_len};

/// Splits large code blocks at language-aware boundaries before chunk windows are built.
///
/// The returned blocks are the normalized structured blocks that downstream chunk
/// support ids may reference. Chunking must not mint hidden block ids because
/// those ids cannot be persisted, cited, or used by graph evidence.
#[must_use]
pub fn split_large_code_blocks(
    blocks: &[StructuredBlockData],
    max_chars: usize,
) -> Vec<StructuredBlockData> {
    let max_chars = max_chars.max(1);
    let mut result = Vec::new();
    for block in blocks {
        if block.block_kind != StructuredBlockKind::CodeBlock || chunk_block_len(block) <= max_chars
        {
            result.push(block.clone());
            continue;
        }
        let segments = split_code_block_segments(
            &block.text,
            &block.normalized_text,
            block.code_language.as_deref(),
            max_chars,
        );
        if segments.len() < 2 {
            result.push(block.clone());
            continue;
        }
        for segment in segments {
            if segment.text.trim().is_empty() && segment.normalized_text.trim().is_empty() {
                continue;
            }
            result.push(StructuredBlockData {
                block_id: uuid::Uuid::now_v7(),
                ordinal: 0,
                block_kind: StructuredBlockKind::CodeBlock,
                text: segment.text,
                normalized_text: segment.normalized_text,
                heading_trail: block.heading_trail.clone(),
                section_path: block.section_path.clone(),
                page_number: block.page_number,
                source_span: None,
                parent_block_id: block.parent_block_id,
                table_coordinates: None,
                code_language: block.code_language.clone(),
                is_boilerplate: block.is_boilerplate,
            });
        }
    }
    for (i, block) in result.iter_mut().enumerate() {
        block.ordinal = i32::try_from(i).unwrap_or(i32::MAX);
    }
    result
}

struct CodeBlockSegment {
    text: String,
    normalized_text: String,
}

fn split_code_block_segments(
    text: &str,
    normalized_text: &str,
    language: Option<&str>,
    max_chars: usize,
) -> Vec<CodeBlockSegment> {
    let text_lines = text.lines().collect::<Vec<_>>();
    if text_lines.is_empty() {
        return Vec::new();
    }
    let norm_lines = normalized_text.lines().collect::<Vec<_>>();
    let mut ranges = Vec::<(usize, usize)>::new();
    let boundaries = detect_code_boundaries(text, language);

    if boundaries.len() >= 2 {
        let mut split_points = vec![0];
        split_points.extend(boundaries.into_iter().filter(|point| *point <= text_lines.len()));
        split_points.push(text_lines.len());
        split_points.sort_unstable();
        split_points.dedup();
        for window in split_points.windows(2) {
            let start = window[0];
            let end = window[1].min(text_lines.len());
            if start < end {
                ranges.push((start, end));
            }
        }
    } else {
        ranges.push((0, text_lines.len()));
    }

    let mut segments = Vec::new();
    for (start, end) in ranges {
        push_line_budgeted_segments(&mut segments, &text_lines, &norm_lines, start, end, max_chars);
    }
    segments
}

fn push_line_budgeted_segments(
    out: &mut Vec<CodeBlockSegment>,
    text_lines: &[&str],
    norm_lines: &[&str],
    start: usize,
    end: usize,
    max_chars: usize,
) {
    let mut current_text = Vec::<String>::new();
    let mut current_norm = Vec::<String>::new();
    let mut current_chars = 0_usize;

    let capped_end = end.min(text_lines.len());
    for (index, text_line) in
        text_lines[start..capped_end].iter().enumerate().map(|(i, s)| (start + i, *s))
    {
        let norm_line = norm_lines.get(index).copied().unwrap_or(text_line);
        let line_chars = char_count(norm_line);
        if line_chars > max_chars {
            flush_code_segment(out, &mut current_text, &mut current_norm, &mut current_chars);
            push_long_line_segments(out, text_line, norm_line, max_chars);
            continue;
        }

        let projected = if current_chars == 0 {
            line_chars
        } else {
            current_chars.saturating_add(1).saturating_add(line_chars)
        };
        if !current_text.is_empty() && projected > max_chars {
            flush_code_segment(out, &mut current_text, &mut current_norm, &mut current_chars);
        }
        current_text.push(text_line.to_string());
        current_norm.push(norm_line.to_string());
        current_chars = if current_chars == 0 {
            line_chars
        } else {
            current_chars.saturating_add(1).saturating_add(line_chars)
        };
    }

    flush_code_segment(out, &mut current_text, &mut current_norm, &mut current_chars);
}

fn flush_code_segment(
    out: &mut Vec<CodeBlockSegment>,
    text_lines: &mut Vec<String>,
    norm_lines: &mut Vec<String>,
    current_chars: &mut usize,
) {
    if text_lines.is_empty() && norm_lines.is_empty() {
        return;
    }
    out.push(CodeBlockSegment {
        text: std::mem::take(text_lines).join("\n"),
        normalized_text: std::mem::take(norm_lines).join("\n"),
    });
    *current_chars = 0;
}

fn push_long_line_segments(
    out: &mut Vec<CodeBlockSegment>,
    text_line: &str,
    norm_line: &str,
    max_chars: usize,
) {
    let text_parts = split_text_by_char_budget(text_line, max_chars);
    let norm_parts = split_text_by_char_budget(norm_line, max_chars);
    let part_count = text_parts.len().max(norm_parts.len());
    for index in 0..part_count {
        let text = text_parts.get(index).cloned().unwrap_or_default();
        let normalized_text = norm_parts.get(index).cloned().unwrap_or_else(|| text.clone());
        out.push(CodeBlockSegment { text, normalized_text });
    }
}

fn split_text_by_char_budget(text: &str, max_chars: usize) -> Vec<String> {
    let mut parts = Vec::<String>::new();
    let mut current = String::new();
    let mut current_chars = 0_usize;
    for character in text.chars() {
        if current_chars >= max_chars {
            parts.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current.push(character);
        current_chars = current_chars.saturating_add(1);
    }
    if !current.is_empty() || parts.is_empty() {
        parts.push(current);
    }
    parts
}
