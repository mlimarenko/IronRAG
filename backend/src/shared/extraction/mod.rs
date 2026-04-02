use serde::{Deserialize, Serialize};

pub mod docx;
pub mod html_main_content;
pub mod image;
pub mod pdf;
pub mod pptx;
pub mod text_like;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionLineSignal {
    Heading,
    ListItem,
    CodeFence,
    CodeLine,
    TableRow,
    EndpointCandidate,
    MetadataCandidate,
    Quote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionLineHint {
    pub ordinal: i32,
    pub source_ordinals: Vec<i32>,
    pub page_number: Option<i32>,
    pub text: String,
    pub start_offset: Option<i32>,
    pub end_offset: Option<i32>,
    pub signals: Vec<ExtractionLineSignal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionStructureHints {
    pub lines: Vec<ExtractionLineHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionSourceMetadata {
    pub source_format: String,
    pub page_count: Option<u32>,
    pub line_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionOutput {
    pub extraction_kind: String,
    pub content_text: String,
    pub page_count: Option<u32>,
    pub warnings: Vec<String>,
    pub source_metadata: ExtractionSourceMetadata,
    pub structure_hints: ExtractionStructureHints,
    pub source_map: serde_json::Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RawExtractionPage {
    pub page_number: Option<i32>,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExtractionTextLayout {
    pub content_text: String,
    pub structure_hints: ExtractionStructureHints,
}

#[must_use]
pub fn build_text_layout(pages: &[RawExtractionPage]) -> ExtractionTextLayout {
    let mut content_text = String::new();
    let mut line_hints = Vec::<ExtractionLineHint>::new();
    let mut ordinal = 0_i32;
    let mut offset = 0_i32;

    for (page_index, page) in pages.iter().enumerate() {
        if page_index > 0 && !content_text.is_empty() {
            content_text.push('\n');
            content_text.push('\n');
            offset = offset.saturating_add(2);
        }

        for (line_index, line) in page.lines.iter().enumerate() {
            if line_index > 0 {
                content_text.push('\n');
                offset = offset.saturating_add(1);
            }
            let start_offset = offset;
            content_text.push_str(line);
            offset = offset.saturating_add(i32::try_from(line.chars().count()).unwrap_or(i32::MAX));
            line_hints.push(ExtractionLineHint {
                ordinal,
                source_ordinals: vec![ordinal],
                page_number: page.page_number,
                text: line.clone(),
                start_offset: Some(start_offset),
                end_offset: Some(offset),
                signals: infer_line_signals(line),
            });
            ordinal = ordinal.saturating_add(1);
        }
    }

    ExtractionTextLayout {
        content_text,
        structure_hints: ExtractionStructureHints { lines: line_hints },
    }
}

#[must_use]
pub fn build_text_layout_from_content(content_text: &str) -> ExtractionTextLayout {
    let pages = vec![RawExtractionPage {
        page_number: None,
        lines: content_text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .split('\n')
            .map(str::to_string)
            .collect(),
    }];
    build_text_layout(&pages)
}

#[must_use]
pub fn infer_line_signals(line: &str) -> Vec<ExtractionLineSignal> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut signals = Vec::<ExtractionLineSignal>::new();
    if is_heading_line(trimmed) {
        signals.push(ExtractionLineSignal::Heading);
    }
    if is_list_item_line(trimmed) {
        signals.push(ExtractionLineSignal::ListItem);
    }
    if is_code_fence_line(trimmed) {
        signals.push(ExtractionLineSignal::CodeFence);
    }
    if is_code_content_line(line, trimmed) {
        signals.push(ExtractionLineSignal::CodeLine);
    }
    if is_table_row_line(trimmed) {
        signals.push(ExtractionLineSignal::TableRow);
    }
    if is_endpoint_candidate_line(trimmed) {
        signals.push(ExtractionLineSignal::EndpointCandidate);
    }
    if is_metadata_candidate_line(trimmed) {
        signals.push(ExtractionLineSignal::MetadataCandidate);
    }
    if is_quote_line(trimmed) {
        signals.push(ExtractionLineSignal::Quote);
    }
    signals
}

fn is_heading_line(line: &str) -> bool {
    if line.starts_with('#') {
        return true;
    }
    let word_count = line.split_whitespace().count();
    word_count > 0
        && word_count <= 12
        && !line.ends_with('.')
        && line
            .chars()
            .all(|ch| !ch.is_alphabetic() || ch.is_uppercase() || ch.is_numeric() || ch == ' ')
}

fn is_list_item_line(line: &str) -> bool {
    line.starts_with("- ")
        || line.starts_with("* ")
        || line.starts_with("+ ")
        || line.split_once(' ').is_some_and(|(prefix, _)| {
            prefix.ends_with('.')
                && prefix[..prefix.len().saturating_sub(1)].chars().all(|ch| ch.is_ascii_digit())
        })
}

fn is_code_fence_line(line: &str) -> bool {
    line.starts_with("```")
}

fn is_code_content_line(original: &str, trimmed: &str) -> bool {
    original.starts_with("    ")
        || original.starts_with('\t')
        || trimmed.contains(" = ")
        || trimmed.contains(" => ")
        || trimmed.contains("::")
        || (trimmed.contains('{') && trimmed.contains('}'))
        || trimmed.ends_with(';')
}

fn is_table_row_line(line: &str) -> bool {
    line.split('|').filter(|segment| !segment.trim().is_empty()).count() >= 2
}

fn is_endpoint_candidate_line(line: &str) -> bool {
    const METHODS: [&str; 8] =
        ["GET ", "POST ", "PUT ", "PATCH ", "DELETE ", "HEAD ", "OPTIONS ", "CONNECT "];
    METHODS.iter().any(|prefix| line.starts_with(prefix))
        || line.contains("http://")
        || line.contains("https://")
        || (line.starts_with('/') && line.contains('/'))
}

fn is_metadata_candidate_line(line: &str) -> bool {
    line.split_once(": ").is_some_and(|(label, value)| {
        !label.trim().is_empty()
            && label.trim().chars().count() <= 40
            && !value.trim().is_empty()
            && !label.contains("://")
    })
}

fn is_quote_line(line: &str) -> bool {
    line.starts_with('>')
        || ((line.starts_with('"') || line.starts_with('«'))
            && (line.ends_with('"') || line.ends_with('»')))
}
