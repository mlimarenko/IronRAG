use anyhow::{Result, anyhow};

use crate::shared::extraction::ExtractionOutput;

pub fn extract_text_like(file_bytes: &[u8]) -> Result<ExtractionOutput> {
    let content_text = String::from_utf8(file_bytes.to_vec())
        .map_err(|_| anyhow!("invalid utf-8 text payload"))?;
    Ok(ExtractionOutput {
        extraction_kind: "text_like".into(),
        content_text,
        page_count: None,
        warnings: Vec::new(),
        source_map: serde_json::json!({}),
        provider_kind: None,
        model_name: None,
    })
}

#[cfg(test)]
mod tests {
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
}
