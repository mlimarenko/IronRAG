use std::io::{Cursor, Read};

use anyhow::{Context, Result};
use roxmltree::Document;
use zip::ZipArchive;

use crate::shared::extraction::ExtractionOutput;

pub fn extract_docx(file_bytes: &[u8]) -> Result<ExtractionOutput> {
    let reader = Cursor::new(file_bytes);
    let mut archive = ZipArchive::new(reader).context("failed to open docx archive")?;
    let mut document_xml =
        archive.by_name("word/document.xml").context("missing word/document.xml in docx")?;
    let mut xml_content = String::new();
    document_xml.read_to_string(&mut xml_content).context("failed to read docx xml")?;
    let xml = Document::parse(&xml_content).context("failed to parse docx xml")?;
    let mut paragraphs = Vec::new();
    for node in xml.descendants().filter(|node| node.tag_name().name() == "p") {
        let text = node
            .descendants()
            .filter(|child| child.tag_name().name() == "t")
            .filter_map(|child| child.text())
            .collect::<String>();
        if !text.trim().is_empty() {
            paragraphs.push(text.trim().to_string());
        }
    }

    Ok(ExtractionOutput {
        extraction_kind: "docx_text".into(),
        content_text: paragraphs.join("\n\n"),
        page_count: None,
        warnings: Vec::new(),
        source_map: serde_json::json!({
            "paragraph_count": paragraphs.len(),
        }),
        provider_kind: None,
        model_name: None,
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    use super::*;

    fn build_minimal_docx_bytes() -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer.start_file("word/document.xml", options).expect("start docx xml");
        writer
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
                <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                  <w:body>
                    <w:p><w:r><w:t>Entity extraction</w:t></w:r></w:p>
                    <w:p><w:r><w:t>Knowledge graph merge</w:t></w:r></w:p>
                  </w:body>
                </w:document>"#,
            )
            .expect("write docx xml");
        writer.finish().expect("finish docx").into_inner()
    }

    #[test]
    fn extracts_ordered_paragraphs_from_docx() {
        let output = extract_docx(&build_minimal_docx_bytes()).expect("docx extraction");

        assert_eq!(output.extraction_kind, "docx_text");
        assert_eq!(output.content_text, "Entity extraction\n\nKnowledge graph merge");
        assert_eq!(output.source_map["paragraph_count"], serde_json::json!(2));
    }
}
