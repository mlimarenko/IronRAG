use std::io::{Cursor, Read};

use anyhow::{Context, Result};
use roxmltree::Document;
use zip::ZipArchive;

use crate::shared::extraction::ExtractionOutput;

pub fn extract_pptx(file_bytes: &[u8]) -> Result<ExtractionOutput> {
    let reader = Cursor::new(file_bytes);
    let mut archive = ZipArchive::new(reader).context("failed to open pptx archive")?;
    let mut slide_names = archive
        .file_names()
        .filter(|name| {
            name.starts_with("ppt/slides/slide")
                && name.ends_with(".xml")
                && !name.contains("/_rels/")
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    slide_names.sort_by_key(|name| slide_sort_key(name));

    let mut slides = Vec::new();
    for slide_name in &slide_names {
        let mut slide_xml =
            archive.by_name(slide_name).with_context(|| format!("missing {slide_name} in pptx"))?;
        let mut xml_content = String::new();
        slide_xml
            .read_to_string(&mut xml_content)
            .with_context(|| format!("failed to read {slide_name} from pptx"))?;
        let xml = Document::parse(&xml_content)
            .with_context(|| format!("failed to parse {slide_name} from pptx"))?;
        let paragraphs = xml
            .descendants()
            .filter(|node| node.tag_name().name() == "p")
            .filter_map(|node| extract_paragraph_text(&node))
            .collect::<Vec<_>>();
        if !paragraphs.is_empty() {
            slides.push(paragraphs.join("\n"));
        }
    }

    Ok(ExtractionOutput {
        extraction_kind: "pptx_text".into(),
        content_text: slides.join("\n\n"),
        page_count: Some(u32::try_from(slide_names.len()).unwrap_or(u32::MAX)),
        warnings: Vec::new(),
        source_map: serde_json::json!({
            "slide_count": slide_names.len(),
            "slides": slide_names,
        }),
        provider_kind: None,
        model_name: None,
    })
}

fn extract_paragraph_text(node: &roxmltree::Node<'_, '_>) -> Option<String> {
    let text = node
        .descendants()
        .filter(|child| child.tag_name().name() == "t")
        .filter_map(|child| child.text())
        .collect::<String>();
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn slide_sort_key(path: &str) -> u32 {
    path.rsplit_once("slide")
        .and_then(|(_, suffix)| suffix.strip_suffix(".xml"))
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    use super::*;

    fn build_minimal_pptx_bytes() -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        writer.start_file("ppt/slides/slide1.xml", options).expect("start slide1");
        writer
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
                <p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
                  <p:cSld>
                    <p:spTree>
                      <p:sp>
                        <p:txBody>
                          <a:p><a:r><a:t>Quarterly pipeline audit</a:t></a:r></a:p>
                          <a:p><a:r><a:t>Upload regressions closed</a:t></a:r></a:p>
                        </p:txBody>
                      </p:sp>
                    </p:spTree>
                  </p:cSld>
                </p:sld>"#,
            )
            .expect("write slide1");

        writer.start_file("ppt/slides/slide2.xml", options).expect("start slide2");
        writer
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
                <p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
                  <p:cSld>
                    <p:spTree>
                      <p:sp>
                        <p:txBody>
                          <a:p><a:r><a:t>Knowledge graph overview</a:t></a:r></a:p>
                        </p:txBody>
                      </p:sp>
                    </p:spTree>
                  </p:cSld>
                </p:sld>"#,
            )
            .expect("write slide2");

        writer.finish().expect("finish pptx").into_inner()
    }

    #[test]
    fn extracts_text_and_slide_count_from_pptx() {
        let output = extract_pptx(&build_minimal_pptx_bytes()).expect("pptx extraction");

        assert_eq!(output.extraction_kind, "pptx_text");
        assert_eq!(
            output.content_text,
            "Quarterly pipeline audit\nUpload regressions closed\n\nKnowledge graph overview",
        );
        assert_eq!(output.page_count, Some(2));
        assert_eq!(output.source_map["slide_count"], serde_json::json!(2));
    }
}
