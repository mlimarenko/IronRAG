use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use sha2::{Digest, Sha256};

use crate::shared::extraction::structured_document::{
    StructuredBlockData, StructuredBlockKind, StructuredChunkWindow,
};
use crate::shared::extraction::table_summary::is_table_summary_text;
use crate::shared::extraction::text_quality::text_quality_score;

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
    let filtered_blocks = filter_chunkable_blocks(blocks);
    let mut chunks = StructuredChunkWindowBuilder::new(&filtered_blocks, profile).build();

    mark_near_duplicates(&mut chunks);
    compute_window_text_pass(&mut chunks);

    chunks
}

fn filter_chunkable_blocks(blocks: &[StructuredBlockData]) -> Vec<StructuredBlockData> {
    let table_parent_ids_with_rows = blocks
        .iter()
        .filter(|block| block.block_kind == StructuredBlockKind::TableRow)
        .filter_map(|block| block.parent_block_id)
        .collect::<HashSet<_>>();

    blocks
        .iter()
        .filter(|block| !block.is_boilerplate)
        .filter(|block| {
            block.block_kind != StructuredBlockKind::Table
                || !table_parent_ids_with_rows.contains(&block.block_id)
        })
        .cloned()
        .collect()
}

struct StructuredChunkWindowBuilder<'block> {
    blocks: &'block [StructuredBlockData],
    profile: StructuredChunkingProfile,
    chunks: Vec<StructuredChunkWindow>,
    window_start: usize,
    current_char_count: usize,
}

impl<'block> StructuredChunkWindowBuilder<'block> {
    fn new(blocks: &'block [StructuredBlockData], profile: StructuredChunkingProfile) -> Self {
        Self { blocks, profile, chunks: Vec::new(), window_start: 0, current_char_count: 0 }
    }

    fn build(mut self) -> Vec<StructuredChunkWindow> {
        for (index, block) in self.blocks.iter().enumerate() {
            self.push_block(index, block);
        }
        self.flush_trailing_window();
        self.chunks
    }

    fn push_block(&mut self, index: usize, block: &StructuredBlockData) {
        if is_standalone_chunk_block(block) {
            self.push_standalone_block(index, block);
            return;
        }

        let block_len = chunk_block_len(block);
        let projected = projected_window_len(self.current_char_count, block_len);
        if self.window_start < index && projected > self.profile.max_chars {
            self.flush_overflowing_window(index);
            return;
        }
        if self.should_start_heading_window(index, block) {
            self.flush_window(self.window_start, index);
            self.window_start = index;
            self.current_char_count = block_len;
            return;
        }

        self.current_char_count = if self.window_start == index { block_len } else { projected };
    }

    fn push_standalone_block(&mut self, index: usize, block: &StructuredBlockData) {
        self.flush_window(self.window_start, index);
        push_structured_chunk_window(&mut self.chunks, std::slice::from_ref(block));
        self.window_start = index.saturating_add(1);
        self.current_char_count = 0;
    }

    fn flush_overflowing_window(&mut self, index: usize) {
        self.flush_window(self.window_start, index);
        self.window_start = compute_overlap_start(
            self.blocks,
            self.window_start,
            index,
            self.profile.overlap_chars,
        );
        self.current_char_count = count_window_chars(&self.blocks[self.window_start..=index]);
    }

    fn should_start_heading_window(&self, index: usize, block: &StructuredBlockData) -> bool {
        block.block_kind == StructuredBlockKind::Heading
            && self.window_start < index
            && self.current_char_count >= self.profile.max_chars / 3
    }

    fn flush_trailing_window(&mut self) {
        self.flush_window(self.window_start, self.blocks.len());
    }

    fn flush_window(&mut self, start: usize, end: usize) {
        if start < end {
            push_structured_chunk_window(&mut self.chunks, &self.blocks[start..end]);
        }
    }
}

fn is_standalone_chunk_block(block: &StructuredBlockData) -> bool {
    matches!(
        block.block_kind,
        StructuredBlockKind::TableRow
            | StructuredBlockKind::SourceProfile
            | StructuredBlockKind::SourceUnit
    ) || (block.block_kind == StructuredBlockKind::MetadataBlock
        && is_table_summary_text(&block.normalized_text))
}

fn projected_window_len(current_char_count: usize, block_len: usize) -> usize {
    if current_char_count == 0 {
        block_len
    } else {
        current_char_count.saturating_add(2).saturating_add(block_len)
    }
}

fn count_window_chars(blocks: &[StructuredBlockData]) -> usize {
    blocks.iter().map(chunk_block_len).fold(0, projected_window_len)
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
        window_text: None,
    });
}

// ---------------------------------------------------------------------------
// Sentence-window retrieval
// ---------------------------------------------------------------------------

/// Maximum token budget for `window_text` (approx. whitespace-split words).
const WINDOW_TEXT_MAX_TOKENS: usize = 1_500;

/// Number of sentence-radius neighbours to include on each side of a chunk.
const WINDOW_SENTENCE_RADIUS: usize = 2;

/// Post-pass: fills `window_text` on every chunk in `chunks` by extracting
/// ±`WINDOW_SENTENCE_RADIUS` sentences from the neighbouring chunks' content.
/// The result is capped at `WINDOW_TEXT_MAX_TOKENS` tokens.
pub(crate) fn compute_window_text_pass(chunks: &mut [StructuredChunkWindow]) {
    let sentences_per_chunk =
        chunks.iter().map(|chunk| split_into_sentences(&chunk.content_text)).collect::<Vec<_>>();

    for (index, chunk) in chunks.iter_mut().enumerate() {
        let before = collect_preceding_sentences(&sentences_per_chunk, index);
        let after = collect_following_sentences(&sentences_per_chunk, index);
        chunk.window_text = build_window_text(&chunk.content_text, &before, &after);
    }
}

fn collect_preceding_sentences(sentences_per_chunk: &[Vec<String>], index: usize) -> Vec<&str> {
    let mut before = Vec::new();
    let mut remaining = WINDOW_SENTENCE_RADIUS;
    let mut chunk_index = index;

    loop {
        if chunk_index == 0 {
            break;
        }
        chunk_index -= 1;
        let sentences = &sentences_per_chunk[chunk_index];
        let take = remaining.min(sentences.len());
        before.extend(sentences[sentences.len() - take..].iter().map(String::as_str));
        remaining = remaining.saturating_sub(take);
        if remaining == 0 {
            break;
        }
    }
    before.reverse();
    before
}

fn collect_following_sentences(sentences_per_chunk: &[Vec<String>], index: usize) -> Vec<&str> {
    let mut after = Vec::new();
    let mut remaining = WINDOW_SENTENCE_RADIUS;
    let mut chunk_index = index;

    loop {
        chunk_index += 1;
        let Some(sentences) = sentences_per_chunk.get(chunk_index) else {
            break;
        };
        let take = remaining.min(sentences.len());
        after.extend(sentences[..take].iter().map(String::as_str));
        remaining = remaining.saturating_sub(take);
        if remaining == 0 {
            break;
        }
    }
    after
}

fn build_window_text(core: &str, before: &[&str], after: &[&str]) -> Option<String> {
    let prefix =
        if before.is_empty() { String::new() } else { format!("{}\n\n", before.join(" ")) };
    let suffix = if after.is_empty() { String::new() } else { format!("\n\n{}", after.join(" ")) };
    let trimmed = trim_to_tokens(&format!("{prefix}{core}{suffix}"), WINDOW_TEXT_MAX_TOKENS);
    (trimmed.len() > core.len()).then_some(trimmed)
}

/// Naively splits text into sentences at `.`, `!`, `?` boundaries.
fn split_into_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                sentences.push(trimmed);
            }
            current.clear();
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }
    sentences
}

/// Truncates `text` to at most `max_tokens` whitespace-split words.
fn trim_to_tokens(text: &str, max_tokens: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= max_tokens {
        return text.to_string();
    }
    words[..max_tokens].join(" ")
}

fn dominant_chunk_kind(blocks: &[StructuredBlockData]) -> StructuredBlockKind {
    blocks
        .iter()
        .find_map(|block| match block.block_kind {
            StructuredBlockKind::EndpointBlock
            | StructuredBlockKind::CodeBlock
            | StructuredBlockKind::Table
            | StructuredBlockKind::TableRow
            | StructuredBlockKind::SourceProfile
            | StructuredBlockKind::SourceUnit => Some(block.block_kind),
            _ => None,
        })
        .unwrap_or_else(|| blocks[0].block_kind)
}

fn chunk_block_len(block: &StructuredBlockData) -> usize {
    char_count(match block.block_kind {
        StructuredBlockKind::TableRow => block.normalized_text.trim(),
        _ => block.normalized_text.trim(),
    })
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
    const DETECTORS: &[fn(&str) -> Option<&'static str>] = &[
        detect_rust,
        detect_python,
        detect_go,
        detect_javascript_family,
        detect_class_based_language,
        detect_php,
        detect_ruby,
        detect_swift,
        detect_kotlin,
        detect_elixir,
        detect_dockerfile,
        detect_yaml,
        detect_json,
        detect_terraform,
        detect_sql,
    ];
    DETECTORS.iter().find_map(|detect| detect(&sample)).unwrap_or("")
}

fn detect_rust(sample: &str) -> Option<&'static str> {
    (sample.contains("fn ")
        && (sample.contains("-> ") || sample.contains("pub ") || sample.contains("let ")))
    .then_some("rust")
}

fn detect_python(sample: &str) -> Option<&'static str> {
    (sample.contains("def ") && sample.contains(':') && !sample.contains('{')).then_some("python")
}

fn detect_go(sample: &str) -> Option<&'static str> {
    (sample.contains("func ") && sample.contains("package ")).then_some("go")
}

fn detect_javascript_family(sample: &str) -> Option<&'static str> {
    (sample.contains("function ") || sample.contains("const ") || sample.contains("=> {")).then(
        || {
            if sample.contains(": string")
                || sample.contains(": number")
                || sample.contains("interface ")
            {
                "typescript"
            } else {
                "javascript"
            }
        },
    )
}

fn detect_class_based_language(sample: &str) -> Option<&'static str> {
    if !sample.contains("class ")
        || !sample.contains('{')
        || (!sample.contains("public ") && !sample.contains("private "))
    {
        return None;
    }
    if sample.contains("package ") && sample.contains("import java") {
        return Some("java");
    }
    if sample.contains("#include") || sample.contains("std::") {
        return Some("cpp");
    }
    if sample.contains("using ") && sample.contains("namespace ") {
        return Some("csharp");
    }
    None
}

fn detect_php(sample: &str) -> Option<&'static str> {
    (sample.contains("<?php") || (sample.contains("function ") && sample.contains('$')))
        .then_some("php")
}

fn detect_ruby(sample: &str) -> Option<&'static str> {
    (sample.contains("require ") && sample.contains("end\n")).then_some("ruby")
}

fn detect_swift(sample: &str) -> Option<&'static str> {
    (sample.contains("import Swift")
        || (sample.contains("func ") && sample.contains("->") && !sample.contains("pub ")))
    .then_some("swift")
}

fn detect_kotlin(sample: &str) -> Option<&'static str> {
    (sample.contains("fun ") && sample.contains("val ")).then_some("kotlin")
}

fn detect_elixir(sample: &str) -> Option<&'static str> {
    (sample.contains("defmodule ") || (sample.contains("def ") && sample.contains("do\n")))
        .then_some("elixir")
}

fn detect_dockerfile(sample: &str) -> Option<&'static str> {
    (sample.contains("FROM ") && sample.contains("RUN ")).then_some("dockerfile")
}

fn detect_yaml(sample: &str) -> Option<&'static str> {
    (sample.contains("---") && sample.contains(": ") && !sample.contains('{')).then_some("yaml")
}

fn detect_json(sample: &str) -> Option<&'static str> {
    (sample.starts_with('{') || sample.contains("\":\n")).then_some("json")
}

fn detect_terraform(sample: &str) -> Option<&'static str> {
    (sample.contains("resource ") && sample.contains("provider ")).then_some("terraform")
}

fn detect_sql(sample: &str) -> Option<&'static str> {
    (sample.contains("CREATE TABLE") || sample.contains("SELECT ")).then_some("sql")
}

fn is_code_boundary(
    trimmed: &str,
    raw_line: &str,
    lang: &str,
    line_idx: usize,
    lines: &[&str],
) -> bool {
    match lang {
        "rust" | "rs" => is_rust_boundary(trimmed, line_idx, lines),
        "python" | "py" => is_python_boundary(trimmed, line_idx, lines),
        "go" => is_go_boundary(trimmed),
        "typescript" | "ts" | "tsx" | "javascript" | "js" | "jsx" => {
            is_javascript_boundary(trimmed, raw_line)
        }
        "java" => is_java_boundary(trimmed),
        "csharp" | "cs" => is_csharp_boundary(trimmed),
        "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" => is_c_family_boundary(trimmed, raw_line),
        "php" => is_php_boundary(trimmed),
        "ruby" | "rb" => is_ruby_boundary(trimmed),
        "swift" => is_swift_boundary(trimmed),
        "kotlin" | "kt" => is_kotlin_boundary(trimmed),
        "scala" => is_scala_boundary(trimmed),
        "elixir" | "ex" | "exs" => is_elixir_boundary(trimmed),
        "dart" => is_dart_boundary(trimmed),
        "lua" => starts_with_any(trimmed, &["function ", "local function "]),
        "r" => contains_any(trimmed, &["<- function(", "= function("]),
        "sh" | "bash" | "shell" | "zsh" => is_shell_boundary(trimmed),
        "sql" => is_sql_boundary(trimmed),
        "terraform" | "tf" | "hcl" => is_terraform_boundary(trimmed),
        "dockerfile" => is_dockerfile_boundary(trimmed),
        "yaml" | "yml" => is_yaml_boundary(trimmed, raw_line),
        "toml" => trimmed.starts_with('[') && trimmed.ends_with(']'),
        _ => false,
    }
}

fn starts_with_any(value: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| value.starts_with(prefix))
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn next_trimmed_line<'line>(line_idx: usize, lines: &'line [&str]) -> Option<&'line str> {
    lines.get(line_idx.saturating_add(1)).map(|line| line.trim())
}

fn is_rust_boundary(trimmed: &str, line_idx: usize, lines: &[&str]) -> bool {
    const PREFIXES: &[&str] = &[
        "pub fn ",
        "fn ",
        "pub async fn ",
        "async fn ",
        "pub struct ",
        "struct ",
        "pub enum ",
        "enum ",
        "pub trait ",
        "trait ",
        "impl ",
        "pub impl ",
        "mod ",
        "pub mod ",
        "pub const ",
        "pub static ",
        "#[test]",
        "#[cfg(test)]",
    ];
    starts_with_any(trimmed, PREFIXES)
        || (trimmed.starts_with("/// ")
            && next_trimmed_line(line_idx, lines)
                .is_some_and(|next| starts_with_any(next, &["pub fn ", "fn ", "pub struct "])))
}

fn is_python_boundary(trimmed: &str, line_idx: usize, lines: &[&str]) -> bool {
    starts_with_any(trimmed, &["def ", "async def ", "class "])
        || (trimmed.starts_with('@')
            && !trimmed.starts_with("@property")
            && next_trimmed_line(line_idx, lines)
                .is_some_and(|next| starts_with_any(next, &["def ", "class ", "async def "])))
}

fn is_go_boundary(trimmed: &str) -> bool {
    starts_with_any(trimmed, &["func ", "func(", "package "])
        || (trimmed.starts_with("type ") && contains_any(trimmed, &[" struct ", " interface "]))
}

fn is_javascript_boundary(trimmed: &str, raw_line: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "function ",
        "async function ",
        "export function ",
        "export async function ",
        "export default function ",
        "class ",
        "export class ",
        "interface ",
        "export interface ",
        "export type ",
    ];
    starts_with_any(trimmed, PREFIXES)
        || (trimmed.starts_with("type ") && trimmed.contains(" = "))
        || (trimmed.starts_with("export const ") && contains_any(trimmed, &[" = (", " = async ("]))
        || (trimmed.starts_with("const ")
            && trimmed.contains(" = (")
            && raw_line.starts_with("const "))
}

fn is_java_boundary(trimmed: &str) -> bool {
    const MODIFIERS: &[&str] = &["public ", "private ", "protected "];
    const DECLARATIONS: &[&str] = &[" class ", " interface ", " enum "];
    (starts_with_any(trimmed, MODIFIERS)
        && (contains_any(trimmed, DECLARATIONS)
            || (trimmed.contains('(') && trimmed.contains(')') && !trimmed.contains('='))))
        || starts_with_any(trimmed, &["@Override", "@Test", "package ", "import "])
}

fn is_csharp_boundary(trimmed: &str) -> bool {
    const MODIFIERS: &[&str] = &["public ", "private ", "protected ", "internal "];
    const DECLARATIONS: &[&str] = &[" class ", " interface ", " struct ", " enum "];
    (starts_with_any(trimmed, MODIFIERS)
        && (contains_any(trimmed, DECLARATIONS)
            || (trimmed.contains('(') && !trimmed.contains('='))))
        || starts_with_any(trimmed, &["namespace ", "using "])
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
}

fn is_c_family_boundary(trimmed: &str, raw_line: &str) -> bool {
    let is_top_level_declaration = raw_line
        .starts_with(|character: char| character.is_alphabetic() || character == '_')
        && trimmed.contains('(')
        && !starts_with_any(trimmed, &["//", "#", "if ", "for ", "while ", "return ", "case "]);
    is_top_level_declaration
        || starts_with_any(
            trimmed,
            &["class ", "struct ", "namespace ", "template", "#include ", "#define "],
        )
}

fn is_php_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &[
            "function ",
            "public function ",
            "private function ",
            "protected function ",
            "class ",
            "interface ",
            "trait ",
            "namespace ",
        ],
    )
}

fn is_ruby_boundary(trimmed: &str) -> bool {
    starts_with_any(trimmed, &["def ", "class ", "module ", "describe ", "it ", "context "])
}

fn is_swift_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &["func ", "class ", "struct ", "enum ", "protocol ", "extension ", "import "],
    )
}

fn is_kotlin_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &[
            "fun ",
            "class ",
            "data class ",
            "object ",
            "interface ",
            "sealed class ",
            "suspend fun ",
            "override fun ",
            "package ",
            "import ",
        ],
    )
}

fn is_scala_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &["def ", "class ", "object ", "trait ", "case class ", "val ", "package ", "import "],
    )
}

fn is_elixir_boundary(trimmed: &str) -> bool {
    starts_with_any(trimmed, &["def ", "defp ", "defmodule ", "defmacro ", "test ", "describe "])
}

fn is_dart_boundary(trimmed: &str) -> bool {
    starts_with_any(trimmed, &["class ", "abstract class ", "void ", "Future<", "import ", "enum "])
}

fn is_shell_boundary(trimmed: &str) -> bool {
    trimmed.starts_with("function ")
        || (!trimmed.starts_with('#') && (trimmed.ends_with("()") || trimmed.ends_with("() {")))
}

fn is_sql_boundary(trimmed: &str) -> bool {
    let upper = trimmed.to_ascii_uppercase();
    starts_with_any(
        &upper,
        &[
            "CREATE TABLE",
            "CREATE INDEX",
            "CREATE VIEW",
            "CREATE FUNCTION",
            "ALTER TABLE",
            "INSERT INTO",
            "-- ===",
            "-- ---",
        ],
    )
}

fn is_terraform_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &["resource ", "data ", "variable ", "output ", "module ", "provider ", "locals {"],
    )
}

fn is_dockerfile_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &["FROM ", "RUN ", "COPY ", "ENTRYPOINT ", "CMD ", "EXPOSE ", "ENV ", "WORKDIR "],
    )
}

fn is_yaml_boundary(trimmed: &str, raw_line: &str) -> bool {
    !raw_line.starts_with(' ')
        && !raw_line.starts_with('\t')
        && trimmed.ends_with(':')
        && !trimmed.starts_with('#')
        && !trimmed.starts_with('-')
}

/// Splits large code blocks at language-aware boundaries before chunk windows are built.
///
/// The returned blocks are the canonical structured blocks that downstream chunk
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

    let combined_text =
        blocks.iter().map(|b| b.normalized_text.as_str()).collect::<Vec<_>>().join("\n");
    score *= text_quality_score(&combined_text);

    score.clamp(0.0, 1.0)
}

/// Computes a 64-bit `SimHash` fingerprint from text using 3-gram word shingles.
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
        compute_window_text_pass, detect_code_boundaries, guess_language, mark_near_duplicates,
        split_large_code_blocks,
    };
    use crate::shared::extraction::structured_document::{
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

    fn make_chunk(chunk_index: i32, content_text: &str) -> StructuredChunkWindow {
        StructuredChunkWindow {
            chunk_index,
            chunk_kind: StructuredBlockKind::Paragraph,
            support_block_ids: vec![Uuid::now_v7()],
            content_text: content_text.to_string(),
            normalized_text: content_text.to_string(),
            heading_trail: Vec::new(),
            section_path: Vec::new(),
            token_count: None,
            literal_digest: None,
            quality_score: 1.0,
            simhash_fingerprint: None,
            is_near_duplicate: false,
            window_text: None,
        }
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
                window_text: None,
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
                window_text: None,
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
    fn computes_sentence_context_without_reordering_existing_semantics() {
        let mut chunks = vec![
            make_chunk(0, "First alpha. First beta."),
            make_chunk(1, "Core sentence."),
            make_chunk(2, "Last alpha. Last beta."),
        ];

        compute_window_text_pass(&mut chunks);

        assert_eq!(
            chunks[1].window_text.as_deref(),
            Some("First beta. First alpha.\n\nCore sentence.\n\nLast alpha. Last beta.")
        );
    }

    #[test]
    fn detects_language_boundary_aliases_and_lookahead_rules() {
        let rust_code = "/// Boundary docs\npub struct Item;\n";
        assert_eq!(detect_code_boundaries(rust_code, Some("rs")), vec![0, 1]);

        let python_code = "@decorator\ndef item():\n    pass\n@property\ndef value():\n    pass\n";
        assert_eq!(detect_code_boundaries(python_code, Some("py")), vec![0, 1, 4]);

        let javascript_code = "const top = () => true;\n    const nested = () => false;\nexport type Item = string;\n";
        assert_eq!(detect_code_boundaries(javascript_code, Some("js")), vec![0, 2]);
    }

    #[test]
    fn detects_raw_line_sensitive_and_case_normalized_boundaries() {
        let c_code = "int run() {\n    if (ready) {\n}\nreturn value;\n";
        assert_eq!(detect_code_boundaries(c_code, Some("cpp")), vec![0]);

        let sql_code = "create table items (id int);\nselect * from items;\n";
        assert_eq!(detect_code_boundaries(sql_code, Some("sql")), vec![0]);

        let yaml = "root:\n  nested:\n- list:\nsecond:\n";
        assert_eq!(detect_code_boundaries(yaml, Some("yaml")), vec![0, 3]);
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
    fn splits_large_code_block_before_chunking() {
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
        let result = split_large_code_blocks(&[block], 500);
        assert!(
            result.len() > 1,
            "should split large code block into multiple sub-blocks, got {}",
            result.len()
        );
        assert!(
            result.iter().all(|split| split.parent_block_id.is_none()),
            "split code blocks must not reference a discarded parent block"
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

    #[test]
    fn table_rows_become_independent_chunks() {
        let table_id = Uuid::now_v7();
        let blocks = vec![
            StructuredBlockData {
                block_id: table_id,
                ordinal: 0,
                block_kind: StructuredBlockKind::Table,
                text: "| Name | Value |\n| --- | --- |\n| Alice | 42 |".to_string(),
                normalized_text: "| Name | Value |\n| --- | --- |\n| Alice | 42 |".to_string(),
                heading_trail: vec!["people".to_string()],
                section_path: vec!["people".to_string()],
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
                block_kind: StructuredBlockKind::TableRow,
                text: "| Alice | 42 |".to_string(),
                normalized_text: "Sheet: people | Row 1 | Name: Alice | Value: 42".to_string(),
                heading_trail: vec!["people".to_string()],
                section_path: vec!["people".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: Some(table_id),
                table_coordinates: None,
                code_language: None,
                is_boilerplate: false,
            },
            StructuredBlockData {
                block_id: Uuid::now_v7(),
                ordinal: 2,
                block_kind: StructuredBlockKind::TableRow,
                text: "| Bob | 7 |".to_string(),
                normalized_text: "Sheet: people | Row 2 | Name: Bob | Value: 7".to_string(),
                heading_trail: vec!["people".to_string()],
                section_path: vec!["people".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: Some(table_id),
                table_coordinates: None,
                code_language: None,
                is_boilerplate: false,
            },
        ];

        let chunks = build_structured_chunk_windows(
            &blocks,
            StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
        );

        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|chunk| chunk.chunk_kind == StructuredBlockKind::TableRow));
        assert_eq!(chunks[0].content_text, "| Alice | 42 |");
        assert_eq!(chunks[1].content_text, "| Bob | 7 |");
    }

    #[test]
    fn table_summaries_become_independent_chunks() {
        let blocks = vec![
            StructuredBlockData {
                block_id: Uuid::now_v7(),
                ordinal: 0,
                block_kind: StructuredBlockKind::Heading,
                text: "organizations".to_string(),
                normalized_text: "organizations".to_string(),
                heading_trail: vec!["organizations".to_string()],
                section_path: vec!["organizations".to_string()],
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
                block_kind: StructuredBlockKind::MetadataBlock,
                text: String::new(),
                normalized_text: "Table Summary | Sheet: organizations | Column: Country | Value Kind: categorical | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 2 | Most Frequent Count: 2 | Most Frequent Tie Count: 1 | Most Frequent Values: Sweden".to_string(),
                heading_trail: vec!["organizations".to_string()],
                section_path: vec!["organizations".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: Some(Uuid::now_v7()),
                table_coordinates: None,
                code_language: None,
                is_boilerplate: false,
            },
            StructuredBlockData {
                block_id: Uuid::now_v7(),
                ordinal: 2,
                block_kind: StructuredBlockKind::MetadataBlock,
                text: String::new(),
                normalized_text: "Table Summary | Sheet: organizations | Column: Employees | Value Kind: numeric | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 3 | Average: 20 | Min: 10 | Max: 30".to_string(),
                heading_trail: vec!["organizations".to_string()],
                section_path: vec!["organizations".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: Some(Uuid::now_v7()),
                table_coordinates: None,
                code_language: None,
                is_boilerplate: false,
            },
        ];

        let chunks = build_structured_chunk_windows(
            &blocks,
            StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
        );

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chunk_kind, StructuredBlockKind::Heading);
        assert_eq!(chunks[1].chunk_kind, StructuredBlockKind::MetadataBlock);
        assert_eq!(chunks[2].chunk_kind, StructuredBlockKind::MetadataBlock);
        assert!(chunks[1].normalized_text.starts_with("Table Summary |"));
        assert!(chunks[2].normalized_text.starts_with("Table Summary |"));
    }

    #[test]
    fn source_units_become_independent_chunks() {
        let blocks = vec![
            make_block(
                0,
                StructuredBlockKind::SourceProfile,
                "[source_profile unit_count=2]",
                false,
            ),
            make_block(1, StructuredBlockKind::SourceUnit, "[unit_id=one] first detail", false),
            make_block(2, StructuredBlockKind::SourceUnit, "[unit_id=two] second detail", false),
        ];

        let chunks = build_structured_chunk_windows(
            &blocks,
            StructuredChunkingProfile { max_chars: 2800, overlap_chars: 280 },
        );

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chunk_kind, StructuredBlockKind::SourceProfile);
        assert_eq!(chunks[1].chunk_kind, StructuredBlockKind::SourceUnit);
        assert_eq!(chunks[2].chunk_kind, StructuredBlockKind::SourceUnit);
        assert_eq!(chunks[1].content_text, "[unit_id=one] first detail");
        assert_eq!(chunks[2].content_text, "[unit_id=two] second detail");
    }
}
