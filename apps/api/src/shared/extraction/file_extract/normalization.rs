use crate::shared::extraction::{
    RawExtractionPage, build_text_layout, build_text_layout_from_content, image,
    text_quality::assess_text_quality, text_render::normalize_for_structured_preparation,
};

use super::{
    EXTRACTION_QUALITY_KEY, ExtractionNormalizationStatus, ExtractionStructureHints,
    RecognitionCapability, RecognitionProfile, UploadFileKind, recognition_engine_ocr_source,
    with_recognition_source_map,
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
    content_text: &str,
    structure_hints: &ExtractionStructureHints,
) -> NormalizedExtractedContent {
    let source_text = match file_kind {
        UploadFileKind::Image => normalize_image_ocr_text(content_text),
        _ => content_text.to_owned(),
    };
    let source_changed = source_text.trim() != content_text.trim();
    let normalized_structure_hints = if source_changed && !structure_hints.lines.is_empty() {
        let mut pages: Vec<RawExtractionPage> = Vec::new();
        for line in &structure_hints.lines {
            let page_number = line.page_number;
            let text = if file_kind == UploadFileKind::Image {
                normalize_image_ocr_text(&line.text)
            } else {
                line.text.clone()
            };
            let lines = text.split('\n').map(str::to_owned).collect::<Vec<_>>();
            match pages.last_mut() {
                Some(page) if page.page_number == page_number => page.lines.extend(lines),
                _ => pages.push(RawExtractionPage { page_number, lines }),
            }
        }
        build_text_layout(&pages).structure_hints
    } else if source_changed {
        // Without producer line associations there is no page identity to preserve.
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
    let evidence_limitation = evidence_limitation_marker(&source_map);
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
            "evidence_limitation": evidence_limitation,
        }),
    );
    with_recognition_source_map(serde_json::Value::Object(source_map), recognition_profile)
}

/// One explicit marker when any unit carries an evidence limitation, so a
/// consumer of the quality record does not have to walk the per-unit list.
fn evidence_limitation_marker(
    source_map: &serde_json::Map<String, serde_json::Value>,
) -> Option<&'static str> {
    let units = source_map.get("unit_evidence")?.as_array()?;
    units
        .iter()
        .any(|unit| unit.get("evidence_limitation").and_then(serde_json::Value::as_str).is_some())
        .then_some(image::LOW_CONFIDENCE_WITHOUT_VISUAL_READING)
}

fn normalize_image_ocr_text(content_text: &str) -> String {
    let normalized_newlines = content_text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = Vec::new();
    let mut previous_blank = false;
    for line in normalized_newlines.lines().map(str::trim_end) {
        let blank = line.is_empty();
        if !blank || !previous_blank {
            lines.push(line);
        }
        previous_blank = blank;
    }
    lines.join("\n").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use crate::shared::extraction::{
        build_text_layout_from_content,
        file_extract::{UploadFileKind, normalization::normalize_extracted_content},
    };

    #[test]
    fn pdf_normalization_preserves_literal_source_comments_and_ocr_labels() {
        let content = "<!-- image -->\n\n> Image OCR: AL\n\n# Product Manual\n\nGET /v1/status";
        let hints = build_text_layout_from_content(content).structure_hints;

        let normalized = normalize_extracted_content(UploadFileKind::Pdf, content, &hints);

        assert!(normalized.source_text.contains("<!-- image -->"));
        assert!(normalized.source_text.contains("Image OCR:"));
        assert!(normalized.normalized_text.contains("# Product Manual"));
        assert!(
            normalized.structure_hints.lines.iter().any(|line| line.text.contains("Image OCR:")),
            "literal source labels are not a retired producer's scaffold"
        );
    }

    #[test]
    fn document_normalization_preserves_literal_backslashes_and_entities() {
        let content =
            "Config: ALPHA\\_TIMEOUT\\_MS=4500\nPath: NODE\\_ALPHA-42 -&gt; CFG\\_TIMEOUT\\_MS";
        let hints = build_text_layout_from_content(content).structure_hints;

        let normalized = normalize_extracted_content(UploadFileKind::Pdf, content, &hints);

        assert_eq!(normalized.source_text, content);
        assert_eq!(normalized.normalized_text, content);
    }

    #[test]
    fn image_normalization_preserves_untyped_ocr_wrapper_text() {
        let content = "OCR text:\n\nVisible marker 42 ALPHA\\_TIMEOUT\\_MS\n\n<!-- image -->";
        let hints = build_text_layout_from_content(content).structure_hints;

        let normalized = normalize_extracted_content(UploadFileKind::Image, content, &hints);

        assert_eq!(normalized.source_text, content);
    }

    #[test]
    fn native_text_normalization_preserves_literal_backslashes_and_entities() {
        let content = r"Regex: \d{3}\.\w+; literal entity: &gt; and path C:\\Temp\\file";
        let hints = build_text_layout_from_content(content).structure_hints;

        let normalized = normalize_extracted_content(UploadFileKind::TextLike, content, &hints);

        assert_eq!(normalized.source_text, content);
        assert_eq!(normalized.normalized_text, content);
    }

    fn recognition_profile() -> crate::domains::recognition::RecognitionProfile {
        use crate::domains::recognition::{
            RecognitionCapability, RecognitionEngine, RecognitionProfile, RecognitionStructureTier,
        };
        RecognitionProfile {
            capability: RecognitionCapability::DocumentLayout,
            engine: RecognitionEngine::Xberg,
            structure_tier: RecognitionStructureTier::Layout,
        }
    }

    fn normalized_pdf(content: &str) -> super::NormalizedExtractedContent {
        let hints = build_text_layout_from_content(content).structure_hints;
        normalize_extracted_content(UploadFileKind::Pdf, content, &hints)
    }

    #[test]
    fn content_quality_carries_a_unit_evidence_limitation() {
        let normalized = normalized_pdf("Native layer text");
        let source_map = serde_json::json!({
            "unit_evidence": [
                {
                    "ordinal": 1,
                    "text_score": 0.2,
                    "text_low_confidence": true,
                    "text_reasons": ["dense_unstable_tokens"],
                    "native_text_low_confidence": true,
                    "visual_coverage": "native_text",
                    "evidence_limitation": "low_confidence_native_text_without_visual_reading"
                },
                {
                    "ordinal": 2,
                    "text_score": 1.0,
                    "text_low_confidence": false,
                    "text_reasons": [],
                    "native_text_low_confidence": false,
                    "visual_coverage": "native_text"
                }
            ]
        });

        let markers = super::with_extraction_quality_markers(
            source_map,
            &normalized,
            0,
            None,
            recognition_profile(),
        );

        assert_eq!(
            markers["content_quality"]["evidence_limitation"],
            serde_json::json!("low_confidence_native_text_without_visual_reading")
        );
    }

    #[test]
    fn content_quality_has_no_limitation_without_a_unit_evidence_marker() {
        let normalized = normalized_pdf("Native layer text");
        let source_map = serde_json::json!({
            "unit_evidence": [{
                "ordinal": 1,
                "text_score": 1.0,
                "text_low_confidence": false,
                "text_reasons": [],
                "native_text_low_confidence": false,
                "visual_coverage": "native_text"
            }]
        });

        let markers = super::with_extraction_quality_markers(
            source_map,
            &normalized,
            0,
            None,
            recognition_profile(),
        );

        assert_eq!(markers["content_quality"]["evidence_limitation"], serde_json::Value::Null);
    }
}
