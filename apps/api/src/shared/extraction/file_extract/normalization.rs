use crate::shared::extraction::{
    build_text_layout_from_content, text_quality::assess_text_quality,
    text_render::normalize_for_structured_preparation,
};

use super::{
    EXTRACTION_QUALITY_KEY, ExtractionNormalizationStatus, ExtractionStructureHints,
    RecognitionCapability, RecognitionEngine, RecognitionProfile, UploadFileKind,
    recognition_engine_ocr_source, with_recognition_source_map,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NormalizedExtractedContent {
    pub(super) source_text: String,
    pub(super) normalized_text: String,
    pub(super) normalization_status: ExtractionNormalizationStatus,
    pub(super) normalization_profile: String,
    pub(super) ocr_source: Option<String>,
    pub(super) structure_hints: ExtractionStructureHints,
}

pub(super) fn normalize_extracted_content(
    file_kind: UploadFileKind,
    recognition_engine: RecognitionEngine,
    content_text: &str,
    structure_hints: &ExtractionStructureHints,
) -> NormalizedExtractedContent {
    let scaffold_normalization = strip_docling_image_scaffold_lines(content_text);
    let source_text = if should_normalize_markdown_transport_escapes(file_kind, recognition_engine)
    {
        normalize_markdown_transport_escapes(&scaffold_normalization.text)
    } else {
        scaffold_normalization.text
    };
    let source_text = match file_kind {
        UploadFileKind::Image => normalize_image_ocr_text(&source_text),
        _ => source_text,
    };
    let source_changed = source_text.trim() != content_text.trim();
    let normalized_structure_hints = if source_changed {
        build_text_layout_from_content(&source_text).structure_hints
    } else {
        structure_hints.clone()
    };
    let pre_structuring =
        normalize_for_structured_preparation(&source_text, Some(&normalized_structure_hints));
    let normalized_text = pre_structuring.normalized_text;
    let normalization_status = if normalized_text.trim() == content_text.trim() {
        ExtractionNormalizationStatus::Verbatim
    } else {
        ExtractionNormalizationStatus::Normalized
    };
    let normalization_profile = if normalization_status == ExtractionNormalizationStatus::Verbatim {
        "verbatim_v1".to_string()
    } else if file_kind == UploadFileKind::Image {
        "image_ocr_pre_structuring_v1".to_string()
    } else if scaffold_normalization.applied
        && pre_structuring.normalization_profile == "pre_structuring_verbatim_v1"
    {
        "docling_image_scaffold_strip_v1".to_string()
    } else if scaffold_normalization.applied {
        format!("docling_image_scaffold_strip_{}", pre_structuring.normalization_profile)
    } else {
        pre_structuring.normalization_profile
    };

    NormalizedExtractedContent {
        source_text,
        normalized_text,
        normalization_status,
        normalization_profile,
        ocr_source: None,
        structure_hints: pre_structuring.structure_hints,
    }
}

pub(super) fn with_extraction_quality_markers(
    source_map: serde_json::Value,
    normalized: &NormalizedExtractedContent,
    warning_count: usize,
    provider_kind: Option<&str>,
    recognition_profile: RecognitionProfile,
) -> serde_json::Value {
    let mut source_map = match source_map {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let recognition_ocr_source = (recognition_profile.capability
        == RecognitionCapability::ImageOcr)
        .then(|| recognition_engine_ocr_source(recognition_profile.engine.as_str()));
    let text_quality = assess_text_quality(&normalized.normalized_text);
    source_map.insert(
        EXTRACTION_QUALITY_KEY.to_string(),
        serde_json::json!({
            "normalization_status": normalized.normalization_status.as_str(),
            "normalization_profile": normalized.normalization_profile,
            "ocr_source": normalized
                .ocr_source
                .as_deref()
                .or(recognition_ocr_source.as_deref())
                .or_else(|| provider_kind.map(|_| "vision_llm")),
            "warning_count": warning_count,
            "recognition_engine": recognition_profile.engine.as_str(),
            "recognition_capability": recognition_profile.capability.as_str(),
            "structure_tier": recognition_profile.structure_tier.as_str(),
            "text_score": text_quality.score,
            "text_low_confidence": text_quality.low_confidence,
            "text_reasons": text_quality.reasons,
        }),
    );
    with_recognition_source_map(serde_json::Value::Object(source_map), recognition_profile)
}

struct DoclingImageScaffoldNormalization {
    text: String,
    applied: bool,
}

fn strip_docling_image_scaffold_lines(content_text: &str) -> DoclingImageScaffoldNormalization {
    let normalized_newlines = content_text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = Vec::<String>::new();
    let mut applied = false;
    for line in normalized_newlines.lines() {
        let trimmed = line.trim();
        if trimmed == "<!-- image -->" {
            applied = true;
            continue;
        }
        if let Some(ocr_text) = strip_docling_image_ocr_prefix(trimmed) {
            applied = true;
            if !ocr_text.is_empty() {
                lines.push(ocr_text);
            }
            continue;
        }
        lines.push(line.trim_end().to_string());
    }

    if applied {
        DoclingImageScaffoldNormalization {
            text: collapse_excess_blank_lines(lines).trim().to_string(),
            applied,
        }
    } else {
        DoclingImageScaffoldNormalization { text: content_text.to_string(), applied }
    }
}

fn strip_docling_image_ocr_prefix(line: &str) -> Option<String> {
    let prefix = "> Image OCR:";
    if !line.starts_with(prefix) {
        return None;
    }
    let ocr_text = line[prefix.len()..].trim();
    if is_useful_inline_image_ocr_text(ocr_text) {
        Some(ocr_text.to_string())
    } else {
        Some(String::new())
    }
}

fn is_useful_inline_image_ocr_text(text: &str) -> bool {
    text.chars().filter(|ch| ch.is_alphanumeric()).count() >= 8
        || text.split_whitespace().count() >= 3
}

fn collapse_excess_blank_lines(lines: Vec<String>) -> String {
    let mut collapsed = Vec::<String>::new();
    let mut blank_run = 0usize;
    for line in lines {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                collapsed.push(String::new());
            }
        } else {
            blank_run = 0;
            collapsed.push(line);
        }
    }
    collapsed.join("\n")
}

fn should_normalize_markdown_transport_escapes(
    file_kind: UploadFileKind,
    recognition_engine: RecognitionEngine,
) -> bool {
    recognition_engine == RecognitionEngine::Docling
        && matches!(
            file_kind,
            UploadFileKind::Pdf
                | UploadFileKind::Docx
                | UploadFileKind::Pptx
                | UploadFileKind::Image
        )
}

fn normalize_markdown_transport_escapes(content_text: &str) -> String {
    let unescaped = unescape_markdown_punctuation(content_text);
    decode_common_text_entities(&unescaped)
}

fn unescape_markdown_punctuation(content_text: &str) -> String {
    let mut output = String::with_capacity(content_text.len());
    let mut chars = content_text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(next) = chars.peek().copied()
            && is_markdown_escaped_punctuation(next)
        {
            output.push(next);
            let _ = chars.next();
            continue;
        }
        output.push(ch);
    }
    output
}

const fn is_markdown_escaped_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '\\' | '`'
            | '*'
            | '_'
            | '{'
            | '}'
            | '['
            | ']'
            | '('
            | ')'
            | '#'
            | '+'
            | '-'
            | '.'
            | '!'
            | '|'
            | '<'
            | '>'
    )
}

fn decode_common_text_entities(content_text: &str) -> String {
    content_text
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn normalize_image_ocr_text(content_text: &str) -> String {
    let normalized_newlines = content_text.replace("\r\n", "\n").replace('\r', "\n");
    let lines = normalized_newlines.lines().map(|line| line.trim_end().to_string()).collect();
    collapse_excess_blank_lines(lines).trim().to_string()
}

#[cfg(test)]
mod tests {
    use crate::domains::recognition::RecognitionEngine;
    use crate::shared::extraction::{
        build_text_layout_from_content,
        file_extract::{UploadFileKind, normalization::normalize_extracted_content},
    };

    #[test]
    fn pdf_normalization_strips_docling_image_scaffold_from_text_and_hints() {
        let content = "<!-- image -->\n\n> Image OCR: AL\n\n# Product Manual\n\nGET /v1/status";
        let hints = build_text_layout_from_content(content).structure_hints;

        let normalized = normalize_extracted_content(
            UploadFileKind::Pdf,
            RecognitionEngine::Docling,
            content,
            &hints,
        );

        assert!(!normalized.source_text.contains("<!-- image -->"));
        assert!(!normalized.source_text.contains("Image OCR:"));
        assert!(normalized.normalized_text.contains("# Product Manual"));
        assert!(
            normalized
                .structure_hints
                .lines
                .iter()
                .all(|line| !line.text.contains("<!-- image -->")
                    && !line.text.contains("Image OCR:")),
            "structure hints must be rebuilt from the cleaned source text"
        );
    }

    #[test]
    fn pdf_normalization_removes_markdown_transport_escapes_from_identifiers() {
        let content =
            "Config: ALPHA\\_TIMEOUT\\_MS=4500\nPath: NODE\\_ALPHA-42 -&gt; CFG\\_TIMEOUT\\_MS";
        let hints = build_text_layout_from_content(content).structure_hints;

        let normalized = normalize_extracted_content(
            UploadFileKind::Pdf,
            RecognitionEngine::Docling,
            content,
            &hints,
        );

        assert_eq!(
            normalized.source_text,
            "Config: ALPHA_TIMEOUT_MS=4500\nPath: NODE_ALPHA-42 -> CFG_TIMEOUT_MS"
        );
        assert!(normalized.normalized_text.contains("ALPHA_TIMEOUT_MS=4500"));
        assert!(normalized.normalized_text.contains("NODE_ALPHA-42 -> CFG_TIMEOUT_MS"));
    }

    #[test]
    fn image_normalization_preserves_untyped_ocr_wrapper_text() {
        let content = "OCR text:\n\nVisible marker 42 ALPHA\\_TIMEOUT\\_MS\n\n<!-- image -->";
        let hints = build_text_layout_from_content(content).structure_hints;

        let normalized = normalize_extracted_content(
            UploadFileKind::Image,
            RecognitionEngine::Docling,
            content,
            &hints,
        );

        assert_eq!(normalized.source_text, "OCR text:\n\nVisible marker 42 ALPHA_TIMEOUT_MS");
        assert_eq!(normalized.normalized_text, "OCR text:\n\nVisible marker 42 ALPHA_TIMEOUT_MS");
        assert_eq!(normalized.structure_hints.lines.len(), 3);
        assert_eq!(normalized.structure_hints.lines[0].text, "OCR text:");
        assert_eq!(normalized.structure_hints.lines[2].text, "Visible marker 42 ALPHA_TIMEOUT_MS");
    }

    #[test]
    fn native_text_normalization_preserves_literal_backslashes_and_entities() {
        let content = r"Regex: \d{3}\.\w+; literal entity: &gt; and path C:\\Temp\\file";
        let hints = build_text_layout_from_content(content).structure_hints;

        let normalized = normalize_extracted_content(
            UploadFileKind::TextLike,
            RecognitionEngine::Native,
            content,
            &hints,
        );

        assert_eq!(normalized.source_text, content);
        assert_eq!(normalized.normalized_text, content);
    }
}
