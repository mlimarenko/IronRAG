use anyhow::{Result, anyhow};

use crate::shared::extraction::{
    ExtractionOutput, ExtractionSourceMetadata, build_text_layout_from_content,
};

pub fn extract_text_like(file_bytes: &[u8]) -> Result<ExtractionOutput> {
    let raw_text = String::from_utf8(file_bytes.to_vec())
        .map_err(|_| anyhow!("invalid utf-8 text payload"))?;
    let layout = build_text_layout_from_content(&raw_text);
    Ok(ExtractionOutput {
        extraction_kind: "text_like".into(),
        content_text: layout.content_text,
        page_count: None,
        warnings: Vec::new(),
        source_metadata: ExtractionSourceMetadata {
            source_format: "text_like".to_string(),
            page_count: None,
            line_count: i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX),
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({}),
        provider_kind: None,
        model_name: None,
    })
}

#[cfg(test)]
mod tests {
    use crate::shared::extraction::ExtractionLineSignal;

    use super::*;

    #[test]
    fn extracts_utf8_text_without_mutation() {
        let output =
            extract_text_like("Graph-ready plain text".as_bytes()).expect("text extraction");

        assert_eq!(output.extraction_kind, "text_like");
        assert_eq!(output.content_text, "Graph-ready plain text");
        assert_eq!(output.page_count, None);
        assert!(output.warnings.is_empty());
    }

    #[test]
    fn emits_heading_list_and_code_hints_for_text_like_content() {
        let output = extract_text_like("# API\n- first item\n```json\n{\n".as_bytes())
            .expect("text extraction");

        assert!(
            output
                .structure_hints
                .lines
                .iter()
                .any(|line| line.signals.contains(&ExtractionLineSignal::Heading))
        );
        assert!(
            output
                .structure_hints
                .lines
                .iter()
                .any(|line| line.signals.contains(&ExtractionLineSignal::ListItem))
        );
        assert!(
            output
                .structure_hints
                .lines
                .iter()
                .any(|line| line.signals.contains(&ExtractionLineSignal::CodeFence))
        );
    }
}
