use crate::shared::extraction::{
    text_quality::assess_text_quality, text_render::normalize_for_structured_preparation,
};

use super::*;

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
    content_text: &str,
    structure_hints: &ExtractionStructureHints,
) -> NormalizedExtractedContent {
    let source_text = match file_kind {
        UploadFileKind::Image => normalize_image_ocr_text(content_text),
        _ => content_text.to_string(),
    };
    let pre_structuring = normalize_for_structured_preparation(&source_text, Some(structure_hints));
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

fn normalize_image_ocr_text(content_text: &str) -> String {
    let normalized_newlines = content_text.replace("\r\n", "\n").replace('\r', "\n");
    let lines = normalized_newlines.lines().map(str::trim).collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }

    let mut start = 0usize;
    while start < lines.len() {
        let line = lines[start];
        if line.is_empty() {
            start += 1;
            continue;
        }
        if is_ocr_wrapper_line(line) {
            start += 1;
            continue;
        }
        break;
    }

    let cleaned = lines[start..]
        .iter()
        .map(|line| strip_wrapper_label_prefix(line))
        .collect::<Vec<_>>()
        .join("\n");
    let cleaned = cleaned.trim().trim_matches('`').trim().to_string();
    if cleaned.is_empty() { content_text.trim().to_string() } else { cleaned }
}

fn is_ocr_wrapper_line(line: &str) -> bool {
    let normalized = line.trim().trim_matches(':').to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "transcription"
            | "ocr"
            | "ocr text"
            | "recognized text"
            | "recognized text from the image"
            | "extracted text"
            | "extracted text from the image"
            | "text from the image"
            | "visible text"
    ) || (normalized.contains("image")
        && (normalized.contains("extracted")
            || normalized.contains("transcription")
            || normalized.contains("recognized")
            || normalized.contains("visible text")
            || normalized.contains("readable text")
            || normalized.contains("ocr")))
}

fn strip_wrapper_label_prefix(line: &str) -> String {
    let trimmed = line.trim();
    let lowercase = trimmed.to_ascii_lowercase();
    for prefix in [
        "transcription:",
        "ocr:",
        "ocr text:",
        "recognized text:",
        "recognized text from the image:",
        "extracted text:",
        "extracted text from the image:",
        "text from the image:",
        "visible text:",
    ] {
        if lowercase.starts_with(prefix) {
            return trimmed[prefix.len()..].trim().to_string();
        }
    }
    trimmed.to_string()
}
