use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use sha2::{Digest, Sha256};

use crate::shared::structured_document::{
    StructuredBlockData, StructuredBlockKind, StructuredChunkWindow,
};

#[derive(Debug, Clone, Copy)]
pub struct StructuredChunkingProfile {
    pub max_chars: usize,
    pub overlap_chars: usize,
}

impl Default for StructuredChunkingProfile {
    fn default() -> Self {
        Self { max_chars: 2_800, overlap_chars: 280 }
    }
}

#[must_use]
pub fn build_structured_chunk_windows(
    blocks: &[StructuredBlockData],
    profile: StructuredChunkingProfile,
) -> Vec<StructuredChunkWindow> {
    let filtered_blocks: Vec<StructuredBlockData> =
        blocks.iter().filter(|block| !block.is_boilerplate).cloned().collect();

    // Pre-split large code blocks at semantic boundaries
    let filtered_blocks = presplit_code_blocks(&filtered_blocks, profile.max_chars);
    let blocks = &filtered_blocks;

    let mut chunks = Vec::<StructuredChunkWindow>::new();
    let mut window_start = 0_usize;
    let mut current_char_count = 0_usize;

    for (index, block) in blocks.iter().enumerate() {
        let block_len = chunk_block_len(block);
        let projected = if current_char_count == 0 {
            block_len
        } else {
            current_char_count.saturating_add(2).saturating_add(block_len)
        };

        // If adding this block exceeds the limit, flush the current window.
        if window_start < index && projected > profile.max_chars {
            // Heading-aware: if this block is a Heading, it naturally starts the next window.
            // Otherwise, flush up to (not including) this block.
            push_structured_chunk_window(&mut chunks, &blocks[window_start..index]);

            // Overlap: rewind to include trailing blocks from the previous window
            // that fit within overlap_chars budget.
            window_start =
                compute_overlap_start(blocks, window_start, index, profile.overlap_chars);
            current_char_count = blocks[window_start..=index]
                .iter()
                .map(chunk_block_len)
                .enumerate()
                .fold(0_usize, |acc, (i, len)| {
                    if i == 0 { len } else { acc.saturating_add(2).saturating_add(len) }
                });
            continue;
        }

        // Heading-aware: if a heading appears mid-window and the window already has
        // substantial content, start a new window so the heading leads the next chunk.
        if block.block_kind == StructuredBlockKind::Heading
            && window_start < index
            && current_char_count >= profile.max_chars / 3
        {
            push_structured_chunk_window(&mut chunks, &blocks[window_start..index]);
            window_start = index;
            current_char_count = block_len;
            continue;
        }

        if window_start == index {
            current_char_count = block_len;
        } else {
            current_char_count = projected;
        }
    }

    if window_start < blocks.len() {
        push_structured_chunk_window(&mut chunks, &blocks[window_start..]);
    }

    // Near-duplicate detection pass
    mark_near_duplicates(&mut chunks);

    chunks
}

/// Compute the overlap start index: walk backward from `flush_end` toward `window_start`,
/// accumulating block lengths until the overlap budget is exceeded.
fn compute_overlap_start(
    blocks: &[StructuredBlockData],
    window_start: usize,
    flush_end: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 || flush_end == 0 {
        return flush_end;
    }

    let mut overlap_used = 0_usize;
    let mut overlap_start = flush_end;

    for i in (window_start..flush_end).rev() {
        let block_len = chunk_block_len(&blocks[i]);
        let projected = if overlap_used == 0 {
            block_len
        } else {
            overlap_used.saturating_add(2).saturating_add(block_len)
        };
        if projected > overlap_chars {
            break;
        }
        overlap_used = projected;
        overlap_start = i;
    }

    overlap_start
}

fn char_count(input: &str) -> usize {
    input.chars().count()
}

fn push_structured_chunk_window(
    out: &mut Vec<StructuredChunkWindow>,
    blocks: &[StructuredBlockData],
) {
    if blocks.is_empty() {
        return;
    }

    let content_text =
        blocks.iter().map(|block| block.text.trim()).collect::<Vec<_>>().join("\n\n");
    let normalized_text =
        blocks.iter().map(|block| block.normalized_text.trim()).collect::<Vec<_>>().join("\n\n");
    let literal_digest =
        format!("sha256:{}", hex::encode(Sha256::digest(normalized_text.as_bytes())));
    let support_block_ids = blocks.iter().map(|block| block.block_id).collect::<Vec<_>>();
    let heading_trail = blocks
        .iter()
        .rev()
        .find(|block| !block.heading_trail.is_empty())
        .map(|block| block.heading_trail.clone())
        .unwrap_or_default();
    let section_path = blocks
        .iter()
        .rev()
        .find(|block| !block.section_path.is_empty())
        .map(|block| block.section_path.clone())
        .unwrap_or_default();
    let token_count = i32::try_from(normalized_text.split_whitespace().count()).ok();

    let quality_score = compute_chunk_quality_score(blocks);
    let simhash_fingerprint = Some(compute_simhash(&normalized_text));

    out.push(StructuredChunkWindow {
        chunk_index: i32::try_from(out.len()).unwrap_or(i32::MAX),
        chunk_kind: dominant_chunk_kind(blocks),
        support_block_ids,
        content_text,
        normalized_text,
        heading_trail,
        section_path,
        token_count,
        literal_digest: Some(literal_digest),
        quality_score,
        simhash_fingerprint,
        is_near_duplicate: false,
    });
}

fn dominant_chunk_kind(blocks: &[StructuredBlockData]) -> StructuredBlockKind {
    blocks
        .iter()
        .find_map(|block| match block.block_kind {
            StructuredBlockKind::EndpointBlock
            | StructuredBlockKind::CodeBlock
            | StructuredBlockKind::Table
            | StructuredBlockKind::TableRow => Some(block.block_kind),
            _ => None,
        })
        .unwrap_or_else(|| blocks[0].block_kind)
}

fn chunk_block_len(block: &StructuredBlockData) -> usize {
    char_count(block.normalized_text.trim())
}

// ---------------------------------------------------------------------------
// Code-aware semantic splitting
// ---------------------------------------------------------------------------

/// Detects line indices where new logical code units begin (functions, classes, etc.)
/// Returns 0-based line numbers that are good split points.
fn detect_code_boundaries(text: &str, language: Option<&str>) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let mut boundaries = Vec::new();

    let lang = language.unwrap_or("");
    let effective_lang = if lang.is_empty() { guess_language(&lines) } else { lang };

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if is_code_boundary(trimmed, line, effective_lang, i, &lines) {
            boundaries.push(i);
        }
    }

    boundaries
}

fn guess_language(lines: &[&str]) -> &'static str {
    let sample = lines.iter().take(50).copied().collect::<Vec<_>>().join("\n");
    if sample.contains("fn ")
        && (sample.contains("-> ") || sample.contains("pub ") || sample.contains("let "))
    {
        return "rust";
    }
    if sample.contains("def ") && sample.contains(':') && !sample.contains('{') {
        return "python";
    }
    if sample.contains("func ") && sample.contains("package ") {
        return "go";
    }
    if sample.contains("function ") || sample.contains("const ") || sample.contains("=> {") {
        if sample.contains(": string")
            || sample.contains(": number")
            || sample.contains("interface ")
        {
            return "typescript";
        }
        return "javascript";
    }
    if sample.contains("class ")
        && sample.contains('{')
        && (sample.contains("public ") || sample.contains("private "))
    {
        if sample.contains("package ") && sample.contains("import java") {
            return "java";
        }
        if sample.contains("#include") || sample.contains("std::") {
            return "cpp";
        }
        if sample.contains("using ") && sample.contains("namespace ") {
            return "csharp";
        }
    }
    if sample.contains("<?php") || (sample.contains("function ") && sample.contains('$')) {
        return "php";
    }
    if sample.contains("require ") && sample.contains("end\n") {
        return "ruby";
    }
    if sample.contains("import Swift")
        || (sample.contains("func ") && sample.contains("->") && !sample.contains("pub "))
    {
        return "swift";
    }
    if sample.contains("fun ") && sample.contains("val ") {
        return "kotlin";
    }
    if sample.contains("defmodule ") || (sample.contains("def ") && sample.contains("do\n")) {
        return "elixir";
    }
    if sample.contains("FROM ") && sample.contains("RUN ") {
        return "dockerfile";
    }
    if sample.contains("---") && sample.contains(": ") && !sample.contains('{') {
        return "yaml";
    }
    if sample.starts_with('{') || sample.contains("\":\n") {
        return "json";
    }
    if sample.contains("resource ") && sample.contains("provider ") {
        return "terraform";
    }
    if sample.contains("CREATE TABLE") || sample.contains("SELECT ") {
        return "sql";
    }
    ""
}

#[allow(clippy::too_many_lines)]
fn is_code_boundary(
    trimmed: &str,
    raw_line: &str,
    lang: &str,
    line_idx: usize,
    lines: &[&str],
) -> bool {
    match lang {
        "rust" | "rs" => {
            trimmed.starts_with("pub fn ")
                || trimmed.starts_with("fn ")
                || trimmed.starts_with("pub async fn ")
                || trimmed.starts_with("async fn ")
                || trimmed.starts_with("pub struct ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("pub enum ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("pub trait ")
                || trimmed.starts_with("trait ")
                || trimmed.starts_with("impl ")
                || trimmed.starts_with("pub impl ")
                || trimmed.starts_with("mod ")
                || trimmed.starts_with("pub mod ")
                || trimmed.starts_with("pub const ")
                || trimmed.starts_with("pub static ")
                || trimmed.starts_with("#[test]")
                || trimmed.starts_with("#[cfg(test)]")
                || (trimmed.starts_with("/// ") && line_idx + 1 < lines.len() && {
                    let next = lines[line_idx + 1].trim();
                    next.starts_with("pub fn ")
                        || next.starts_with("fn ")
                        || next.starts_with("pub struct ")
                })
        }
        "python" | "py" => {
            trimmed.starts_with("def ")
                || trimmed.starts_with("async def ")
                || trimmed.starts_with("class ")
                || (trimmed.starts_with('@')
                    && !trimmed.starts_with("@property")
                    && line_idx + 1 < lines.len()
                    && {
                        let next = lines[line_idx + 1].trim();
                        next.starts_with("def ")
                            || next.starts_with("class ")
                            || next.starts_with("async def ")
                    })
        }
        "go" => {
            trimmed.starts_with("func ")
                || trimmed.starts_with("func(")
                || (trimmed.starts_with("type ")
                    && (trimmed.contains(" struct ") || trimmed.contains(" interface ")))
                || trimmed.starts_with("package ")
        }
        "typescript" | "ts" | "tsx" | "javascript" | "js" | "jsx" => {
            trimmed.starts_with("function ")
                || trimmed.starts_with("async function ")
                || trimmed.starts_with("export function ")
                || trimmed.starts_with("export async function ")
                || trimmed.starts_with("export default function ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("export class ")
                || trimmed.starts_with("interface ")
                || trimmed.starts_with("export interface ")
                || (trimmed.starts_with("type ") && trimmed.contains(" = "))
                || trimmed.starts_with("export type ")
                || (trimmed.starts_with("export const ")
                    && (trimmed.contains(" = (") || trimmed.contains(" = async (")))
                || (trimmed.starts_with("const ")
                    && trimmed.contains(" = (")
                    && raw_line.starts_with("const "))
        }
        "java" => {
            ((trimmed.starts_with("public ")
                || trimmed.starts_with("private ")
                || trimmed.starts_with("protected "))
                && (trimmed.contains(" class ")
                    || trimmed.contains(" interface ")
                    || trimmed.contains(" enum ")
                    || (trimmed.contains('(') && trimmed.contains(')') && !trimmed.contains('='))))
                || trimmed.starts_with("@Override")
                || trimmed.starts_with("@Test")
                || trimmed.starts_with("package ")
                || trimmed.starts_with("import ")
        }
        "csharp" | "cs" => {
            ((trimmed.starts_with("public ")
                || trimmed.starts_with("private ")
                || trimmed.starts_with("protected ")
                || trimmed.starts_with("internal "))
                && (trimmed.contains(" class ")
                    || trimmed.contains(" interface ")
                    || trimmed.contains(" struct ")
                    || trimmed.contains(" enum ")
                    || (trimmed.contains('(') && !trimmed.contains('='))))
                || trimmed.starts_with("namespace ")
                || trimmed.starts_with("using ")
                || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        }
        "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" => {
            (raw_line.starts_with(|c: char| c.is_alphabetic() || c == '_')
                && trimmed.contains('(')
                && !trimmed.starts_with("//")
                && !trimmed.starts_with('#')
                && !trimmed.starts_with("if ")
                && !trimmed.starts_with("for ")
                && !trimmed.starts_with("while ")
                && !trimmed.starts_with("return ")
                && !trimmed.starts_with("case "))
                || trimmed.starts_with("class ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("namespace ")
                || trimmed.starts_with("template")
                || trimmed.starts_with("#include ")
                || trimmed.starts_with("#define ")
        }
        "php" => {
            trimmed.starts_with("function ")
                || trimmed.starts_with("public function ")
                || trimmed.starts_with("private function ")
                || trimmed.starts_with("protected function ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("interface ")
                || trimmed.starts_with("trait ")
                || trimmed.starts_with("namespace ")
        }
        "ruby" | "rb" => {
            trimmed.starts_with("def ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("module ")
                || trimmed.starts_with("describe ")
                || trimmed.starts_with("it ")
                || trimmed.starts_with("context ")
        }
        "swift" => {
            trimmed.starts_with("func ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("protocol ")
                || trimmed.starts_with("extension ")
                || trimmed.starts_with("import ")
        }
        "kotlin" | "kt" => {
            trimmed.starts_with("fun ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("data class ")
                || trimmed.starts_with("object ")
                || trimmed.starts_with("interface ")
                || trimmed.starts_with("sealed class ")
                || trimmed.starts_with("suspend fun ")
                || trimmed.starts_with("override fun ")
                || trimmed.starts_with("package ")
                || trimmed.starts_with("import ")
        }
        "scala" => {
            trimmed.starts_with("def ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("object ")
                || trimmed.starts_with("trait ")
                || trimmed.starts_with("case class ")
                || trimmed.starts_with("val ")
                || trimmed.starts_with("package ")
                || trimmed.starts_with("import ")
        }
        "elixir" | "ex" | "exs" => {
            trimmed.starts_with("def ")
                || trimmed.starts_with("defp ")
                || trimmed.starts_with("defmodule ")
                || trimmed.starts_with("defmacro ")
                || trimmed.starts_with("test ")
                || trimmed.starts_with("describe ")
        }
        "dart" => {
            trimmed.starts_with("class ")
                || trimmed.starts_with("abstract class ")
                || trimmed.starts_with("void ")
                || trimmed.starts_with("Future<")
                || trimmed.starts_with("import ")
                || trimmed.starts_with("enum ")
        }
        "lua" => trimmed.starts_with("function ") || trimmed.starts_with("local function "),
        "r" => trimmed.contains("<- function(") || trimmed.contains("= function("),
        "sh" | "bash" | "shell" | "zsh" => {
            trimmed.starts_with("function ")
                || (!trimmed.starts_with('#')
                    && (trimmed.ends_with("()") || trimmed.ends_with("() {")))
        }
        "sql" => {
            let upper = trimmed.to_ascii_uppercase();
            upper.starts_with("CREATE TABLE")
                || upper.starts_with("CREATE INDEX")
                || upper.starts_with("CREATE VIEW")
                || upper.starts_with("CREATE FUNCTION")
                || upper.starts_with("ALTER TABLE")
                || upper.starts_with("INSERT INTO")
                || upper.starts_with("-- ===")
                || upper.starts_with("-- ---")
        }
        "terraform" | "tf" | "hcl" => {
            trimmed.starts_with("resource ")
                || trimmed.starts_with("data ")
                || trimmed.starts_with("variable ")
                || trimmed.starts_with("output ")
                || trimmed.starts_with("module ")
                || trimmed.starts_with("provider ")
                || trimmed.starts_with("locals {")
        }
        "dockerfile" => {
            trimmed.starts_with("FROM ")
                || trimmed.starts_with("RUN ")
                || trimmed.starts_with("COPY ")
                || trimmed.starts_with("ENTRYPOINT ")
                || trimmed.starts_with("CMD ")
                || trimmed.starts_with("EXPOSE ")
                || trimmed.starts_with("ENV ")
                || trimmed.starts_with("WORKDIR ")
        }
        "yaml" | "yml" => {
            !raw_line.starts_with(' ')
                && !raw_line.starts_with('\t')
                && trimmed.ends_with(':')
                && !trimmed.starts_with('#')
                && !trimmed.starts_with('-')
        }
        "toml" => trimmed.starts_with('[') && trimmed.ends_with(']'),
        _ => false,
    }
}

/// Pre-splits large code blocks at language-aware boundaries.
fn presplit_code_blocks(
    blocks: &[StructuredBlockData],
    max_chars: usize,
) -> Vec<StructuredBlockData> {
    let mut result = Vec::new();
    for block in blocks {
        if block.block_kind != StructuredBlockKind::CodeBlock || chunk_block_len(block) <= max_chars
        {
            result.push(block.clone());
            continue;
        }
        let boundaries = detect_code_boundaries(&block.text, block.code_language.as_deref());
        if boundaries.len() < 2 {
            result.push(block.clone());
            continue;
        }
        let lines: Vec<&str> = block.text.lines().collect();
        let norm_lines: Vec<&str> = block.normalized_text.lines().collect();

        let mut split_points = vec![0];
        split_points.extend(&boundaries);
        split_points.push(lines.len());
        split_points.dedup();

        for window in split_points.windows(2) {
            let start = window[0];
            let end = window[1];
            if start >= end || start >= lines.len() {
                continue;
            }
            let end = end.min(lines.len());
            let text = lines[start..end].join("\n");
            let normalized_text = if start < norm_lines.len() {
                norm_lines[start..end.min(norm_lines.len())].join("\n")
            } else {
                text.clone()
            };
            if text.trim().is_empty() {
                continue;
            }
            result.push(StructuredBlockData {
                block_id: uuid::Uuid::now_v7(),
                ordinal: 0,
                block_kind: StructuredBlockKind::CodeBlock,
                text,
                normalized_text,
                heading_trail: block.heading_trail.clone(),
                section_path: block.section_path.clone(),
                page_number: block.page_number,
                source_span: None,
                parent_block_id: Some(block.block_id),
                table_coordinates: None,
                code_language: block.code_language.clone(),
                is_boilerplate: false,
            });
        }
    }
    for (i, block) in result.iter_mut().enumerate() {
        block.ordinal = i32::try_from(i).unwrap_or(i32::MAX);
    }
    result
}

/// Computes a quality score for a chunk window based on its constituent blocks.
fn compute_chunk_quality_score(blocks: &[StructuredBlockData]) -> f32 {
    if blocks.is_empty() {
        return 0.0;
    }

    if blocks.iter().all(|b| b.is_boilerplate) {
        return 0.0;
    }

    let mut score: f32 = 1.0;

    // Bonus for code or endpoint blocks
    if blocks.iter().any(|b| {
        matches!(b.block_kind, StructuredBlockKind::CodeBlock | StructuredBlockKind::EndpointBlock)
    }) {
        score += 0.1;
    }

    // Bonus for headings
    if blocks.iter().any(|b| matches!(b.block_kind, StructuredBlockKind::Heading)) {
        score += 0.1;
    }

    // Bonus for table content
    if blocks
        .iter()
        .any(|b| matches!(b.block_kind, StructuredBlockKind::Table | StructuredBlockKind::TableRow))
    {
        score += 0.1;
    }

    // Penalty for very short text
    let total_chars: usize = blocks.iter().map(|b| b.normalized_text.len()).sum();
    if total_chars < 100 {
        score -= 0.2;
    }

    // Penalty for low unique word ratio
    let words: Vec<&str> =
        blocks.iter().flat_map(|b| b.normalized_text.split_whitespace()).collect();
    if !words.is_empty() {
        let unique: HashSet<&str> = words.iter().copied().collect();
        let ratio = unique.len() as f32 / words.len() as f32;
        if ratio < 0.3 {
            score -= 0.1;
        }
    }

    score.clamp(0.0, 1.0)
}

/// Computes a 64-bit SimHash fingerprint from text using 3-gram word shingles.
fn compute_simhash(text: &str) -> u64 {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 3 {
        // For very short text, hash the whole thing
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        return hasher.finish();
    }

    let mut bit_counts = [0_i64; 64];

    for window in words.windows(3) {
        let mut hasher = DefaultHasher::new();
        window.hash(&mut hasher);
        let hash = hasher.finish();

        for (bit, count) in bit_counts.iter_mut().enumerate() {
            if (hash >> bit) & 1 == 1 {
                *count += 1;
            } else {
                *count -= 1;
            }
        }
    }

    let mut fingerprint: u64 = 0;
    for (bit, count) in bit_counts.iter().enumerate() {
        if *count > 0 {
            fingerprint |= 1 << bit;
        }
    }
    fingerprint
}

/// Marks near-duplicate chunks: if two chunks share the same simhash fingerprint
/// but have different literal digests, the later one is marked as a near-duplicate.
fn mark_near_duplicates(chunks: &mut [StructuredChunkWindow]) {
    let mut seen_digests: std::collections::HashMap<u64, String> = std::collections::HashMap::new();

    for chunk in chunks.iter_mut() {
        let Some(fingerprint) = chunk.simhash_fingerprint else {
            continue;
        };

        if let Some(prev_digest) = seen_digests.get(&fingerprint) {
            let current_digest = chunk.literal_digest.as_deref().unwrap_or("");
            if current_digest != prev_digest {
                chunk.is_near_duplicate = true;
            }
        } else if let Some(digest) = &chunk.literal_digest {
            seen_digests.insert(fingerprint, digest.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{
        StructuredChunkingProfile, build_structured_chunk_windows, compute_simhash,
        detect_code_boundaries, guess_language, mark_near_duplicates, presplit_code_blocks,
    };
    use crate::shared::structured_document::{
        StructuredBlockData, StructuredBlockKind, StructuredChunkWindow,
    };

    fn make_block(
        ordinal: i32,
        kind: StructuredBlockKind,
        text: &str,
        is_boilerplate: bool,
    ) -> StructuredBlockData {
        StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal,
            block_kind: kind,
            text: text.to_string(),
            normalized_text: text.to_string(),
            heading_trail: Vec::new(),
            section_path: Vec::new(),
            page_number: None,
            source_span: None,
            parent_block_id: None,
            table_coordinates: None,
            code_language: None,
            is_boilerplate,
        }
    }

    fn make_paragraph(ordinal: i32, char_count: usize) -> StructuredBlockData {
        let text: String =
            "abcdefghij ".repeat(char_count / 11 + 1).chars().take(char_count).collect();
        make_block(ordinal, StructuredBlockKind::Paragraph, &text, false)
    }

    #[test]
    fn overlap_produces_shared_blocks_between_chunks() {
        // Use 10 blocks of ~200 chars each (total ~2000+). With max_chars=1200, we get 2+ chunks.
        // Each block is 200 chars, so overlap_chars=300 can include at least one trailing block.
        let blocks: Vec<StructuredBlockData> = (0..10).map(|i| make_paragraph(i, 200)).collect();

        let chunks = build_structured_chunk_windows(
            &blocks,
            StructuredChunkingProfile { max_chars: 1200, overlap_chars: 300 },
        );

        assert!(chunks.len() >= 2, "expected at least 2 chunks, got {}", chunks.len());

        let first_ids: std::collections::HashSet<Uuid> =
            chunks[0].support_block_ids.iter().copied().collect();
        let second_ids: std::collections::HashSet<Uuid> =
            chunks[1].support_block_ids.iter().copied().collect();
        let shared: Vec<_> = first_ids.intersection(&second_ids).collect();
        assert!(
            !shared.is_empty(),
            "overlap should produce at least one shared block between chunks"
        );
    }

    #[test]
    fn heading_starts_new_chunk_when_window_has_content() {
        let blocks = vec![
            make_paragraph(0, 500),
            make_paragraph(1, 500),
            make_block(2, StructuredBlockKind::Heading, "Section 2", false),
            make_paragraph(3, 500),
        ];

        let chunks = build_structured_chunk_windows(
            &blocks,
            StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
        );

        assert_eq!(chunks.len(), 2, "heading should force a split: got {} chunks", chunks.len());
        assert_eq!(
            chunks[1].support_block_ids[0], blocks[2].block_id,
            "second chunk should start with the heading block"
        );
    }

    #[test]
    fn boilerplate_blocks_are_filtered_from_chunks() {
        let blocks = vec![
            make_block(0, StructuredBlockKind::Paragraph, "Normal paragraph text here.", false),
            make_block(1, StructuredBlockKind::Paragraph, "This is boilerplate content.", true),
            make_block(2, StructuredBlockKind::Paragraph, "Another normal paragraph.", false),
        ];

        let boilerplate_id = blocks[1].block_id;
        let chunks = build_structured_chunk_windows(
            &blocks,
            StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
        );

        for chunk in &chunks {
            assert!(
                !chunk.support_block_ids.contains(&boilerplate_id),
                "boilerplate block_id must not appear in any chunk's support_block_ids"
            );
        }
    }

    #[test]
    fn quality_score_rewards_code_and_headings() {
        let blocks = vec![
            make_block(
                0,
                StructuredBlockKind::CodeBlock,
                "fn main() { println!(\"hello world\"); } // some extra padding text to reach minimum length requirement for quality scoring",
                false,
            ),
            make_block(1, StructuredBlockKind::Heading, "Getting Started Guide", false),
        ];

        let chunks = build_structured_chunk_windows(
            &blocks,
            StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
        );

        assert_eq!(chunks.len(), 1);
        // The quality function adds +0.1 for code and +0.1 for heading, but clamps to 1.0 max.
        // So with code + heading the score should be exactly 1.0 (the clamped maximum).
        assert!(
            chunks[0].quality_score >= 1.0,
            "code + heading should give quality_score >= 1.0, got {}",
            chunks[0].quality_score
        );
    }

    #[test]
    fn quality_score_penalizes_short_content() {
        let blocks = vec![make_block(0, StructuredBlockKind::Paragraph, "Very short text.", false)];

        let chunks = build_structured_chunk_windows(
            &blocks,
            StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
        );

        assert_eq!(chunks.len(), 1);
        assert!(
            chunks[0].quality_score < 1.0,
            "short content should give quality_score < 1.0, got {}",
            chunks[0].quality_score
        );
    }

    #[test]
    fn simhash_fingerprint_is_computed() {
        let blocks = vec![make_paragraph(0, 200), make_paragraph(1, 200)];

        let chunks = build_structured_chunk_windows(
            &blocks,
            StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
        );

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].simhash_fingerprint.is_some(), "simhash_fingerprint should be Some");
        assert_ne!(
            chunks[0].simhash_fingerprint.unwrap(),
            0,
            "simhash_fingerprint should be non-zero"
        );
    }

    #[test]
    fn near_duplicate_marking_works() {
        // Directly test mark_near_duplicates: two chunks with the same simhash but
        // different literal_digest should result in the later one being marked as near-duplicate.
        let shared_text = "This is a comprehensive paragraph with enough words to produce meaningful simhash shingles for near duplicate detection testing purposes in the chunking system";
        let fingerprint = compute_simhash(shared_text);

        let mut chunks = vec![
            StructuredChunkWindow {
                chunk_index: 0,
                chunk_kind: StructuredBlockKind::Paragraph,
                support_block_ids: vec![Uuid::now_v7()],
                content_text: shared_text.to_string(),
                normalized_text: shared_text.to_string(),
                heading_trail: Vec::new(),
                section_path: Vec::new(),
                token_count: Some(20),
                literal_digest: Some("sha256:aaa".to_string()),
                quality_score: 1.0,
                simhash_fingerprint: Some(fingerprint),
                is_near_duplicate: false,
            },
            StructuredChunkWindow {
                chunk_index: 1,
                chunk_kind: StructuredBlockKind::Paragraph,
                support_block_ids: vec![Uuid::now_v7()],
                content_text: shared_text.to_string(),
                normalized_text: shared_text.to_string(),
                heading_trail: Vec::new(),
                section_path: Vec::new(),
                token_count: Some(20),
                literal_digest: Some("sha256:bbb".to_string()),
                quality_score: 1.0,
                simhash_fingerprint: Some(fingerprint),
                is_near_duplicate: false,
            },
        ];

        mark_near_duplicates(&mut chunks);

        assert!(
            chunks[1].is_near_duplicate,
            "second chunk with same simhash but different digest should be marked as near_duplicate"
        );
    }

    #[test]
    fn zero_overlap_produces_no_shared_blocks() {
        // 6 blocks of ~600 chars each to produce 2 chunks
        let blocks: Vec<StructuredBlockData> = (0..6).map(|i| make_paragraph(i, 600)).collect();

        let chunks = build_structured_chunk_windows(
            &blocks,
            StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
        );

        assert!(chunks.len() >= 2, "expected at least 2 chunks");

        for i in 0..chunks.len() {
            for j in (i + 1)..chunks.len() {
                let ids_i: std::collections::HashSet<Uuid> =
                    chunks[i].support_block_ids.iter().copied().collect();
                let ids_j: std::collections::HashSet<Uuid> =
                    chunks[j].support_block_ids.iter().copied().collect();
                let shared: Vec<_> = ids_i.intersection(&ids_j).collect();
                assert!(
                    shared.is_empty(),
                    "with zero overlap, no block_id should appear in multiple chunks"
                );
            }
        }
    }

    #[test]
    fn detects_rust_function_boundaries() {
        let code = "use std::io;\n\npub fn main() {\n    println!(\"hello\");\n}\n\nfn helper() -> bool {\n    true\n}\n";
        let bounds = detect_code_boundaries(code, Some("rust"));
        assert!(bounds.contains(&2), "should detect pub fn main at line 2, got {bounds:?}");
        assert!(bounds.contains(&6), "should detect fn helper at line 6, got {bounds:?}");
    }

    #[test]
    fn detects_python_class_and_def_boundaries() {
        let code = "import os\n\nclass MyClass:\n    def __init__(self):\n        pass\n\n    def method(self):\n        pass\n\ndef standalone():\n    pass\n";
        let bounds = detect_code_boundaries(code, Some("python"));
        assert!(bounds.contains(&2), "should detect class MyClass at line 2, got {bounds:?}");
        assert!(bounds.contains(&9), "should detect def standalone at line 9, got {bounds:?}");
    }

    #[test]
    fn auto_detects_language_from_content() {
        let rust_code = "pub fn main() {\n    let x = 5;\n    println!(\"{}\", x);\n}\n";
        let lang = guess_language(&rust_code.lines().collect::<Vec<_>>());
        assert_eq!(lang, "rust");
    }

    #[test]
    fn presplits_large_code_block() {
        let mut lines = Vec::new();
        for i in 0..10 {
            lines.push(format!("fn func_{i}() {{"));
            for j in 0..30 {
                lines.push(format!("    let x_{j} = {j};"));
            }
            lines.push("}".to_string());
            lines.push(String::new());
        }
        let big_code = lines.join("\n");
        let block = StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal: 0,
            block_kind: StructuredBlockKind::CodeBlock,
            text: big_code.clone(),
            normalized_text: big_code,
            heading_trail: Vec::new(),
            section_path: Vec::new(),
            page_number: None,
            source_span: None,
            parent_block_id: None,
            table_coordinates: None,
            code_language: Some("rust".to_string()),
            is_boilerplate: false,
        };
        let result = presplit_code_blocks(&[block], 500);
        assert!(
            result.len() > 1,
            "should split large code block into multiple sub-blocks, got {}",
            result.len()
        );
    }

    #[test]
    fn builds_structured_chunk_windows_from_semantic_blocks() {
        let blocks = vec![
            StructuredBlockData {
                block_id: Uuid::now_v7(),
                ordinal: 0,
                block_kind: StructuredBlockKind::Heading,
                text: "API".to_string(),
                normalized_text: "API".to_string(),
                heading_trail: vec!["API".to_string()],
                section_path: vec!["api".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: None,
                table_coordinates: None,
                code_language: None,
                is_boilerplate: false,
            },
            StructuredBlockData {
                block_id: Uuid::now_v7(),
                ordinal: 1,
                block_kind: StructuredBlockKind::EndpointBlock,
                text: "GET /v1/accounts".to_string(),
                normalized_text: "GET /v1/accounts".to_string(),
                heading_trail: vec!["API".to_string()],
                section_path: vec!["api".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: None,
                table_coordinates: None,
                code_language: None,
                is_boilerplate: false,
            },
        ];

        let chunks = build_structured_chunk_windows(
            &blocks,
            StructuredChunkingProfile { max_chars: 80, overlap_chars: 0 },
        );

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_kind, StructuredBlockKind::EndpointBlock);
        assert_eq!(chunks[0].support_block_ids.len(), 2);
        assert_eq!(chunks[0].heading_trail, vec!["API".to_string()]);
        assert!(
            chunks[0].literal_digest.as_deref().is_some_and(|value| value.starts_with("sha256:"))
        );
    }
}
