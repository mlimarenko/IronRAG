use std::collections::HashSet;
use std::io::{Cursor, Read};

use anyhow::{Context, Result};
use roxmltree::Document;
use zip::ZipArchive;

use crate::shared::extraction::{
    ExtractedImage, ExtractionOutput, ExtractionSourceMetadata, RawExtractionPage,
    build_text_layout,
    table_markdown::{render_markdown_table_from_rows, render_plain_table_rows},
};

pub fn extract_docx(file_bytes: &[u8]) -> Result<ExtractionOutput> {
    let reader = Cursor::new(file_bytes);
    let mut archive = ZipArchive::new(reader).context("failed to open docx archive")?;
    let mut document_xml =
        archive.by_name("word/document.xml").context("missing word/document.xml in docx")?;
    let mut xml_content = String::new();
    document_xml.read_to_string(&mut xml_content).context("failed to read docx xml")?;
    drop(document_xml);

    let xml = Document::parse(&xml_content).context("failed to parse docx xml")?;

    let mut content_blocks = Vec::new();
    let mut image_rel_ids = Vec::new();
    let mut tables_found: usize = 0;

    for node in xml.descendants() {
        match node.tag_name().name() {
            "tbl" => {
                // Only handle top-level tables (not nested inside other tables)
                if node.ancestors().skip(1).any(|a| a.tag_name().name() == "tbl") {
                    continue;
                }
                let table_md = extract_table_as_markdown(&node);
                if !table_md.is_empty() {
                    tables_found += 1;
                    content_blocks.push(table_md);
                }
            }
            "p" => {
                // Skip paragraphs inside tables (handled by table extraction)
                if node.ancestors().any(|a| a.tag_name().name() == "tbl") {
                    continue;
                }

                // Collect image references from <w:drawing> elements
                for drawing in node.descendants().filter(|n| n.tag_name().name() == "drawing") {
                    if let Some(rel_id) = extract_drawing_image_rel_id(&drawing) {
                        image_rel_ids.push(rel_id);
                    }
                }

                let text = node
                    .descendants()
                    .filter(|child| child.tag_name().name() == "t")
                    .filter_map(|child| child.text())
                    .collect::<String>();
                if !text.trim().is_empty() {
                    content_blocks.push(text.trim().to_string());
                }
            }
            _ => {}
        }
    }

    // Resolve image relationships and extract image files
    let rels_xml = read_archive_entry(&mut archive, "word/_rels/document.xml.rels");
    let image_targets = resolve_image_targets(&rels_xml, &image_rel_ids);
    let extracted_images = extract_docx_images(&mut archive, &image_targets);

    tracing::info!(
        stage = "docx_images",
        images_extracted = extracted_images.len(),
        "DOCX image extraction complete"
    );
    tracing::info!(
        stage = "docx_tables",
        tables_found = tables_found,
        "DOCX table extraction complete"
    );

    let mut paragraph_lines = Vec::new();
    for (index, block) in content_blocks.iter().enumerate() {
        if index > 0 {
            paragraph_lines.push(String::new());
        }
        for line in block.lines() {
            paragraph_lines.push(line.to_string());
        }
    }
    let layout =
        build_text_layout(&[RawExtractionPage { page_number: None, lines: paragraph_lines }]);

    Ok(ExtractionOutput {
        extraction_kind: "docx_text".into(),
        content_text: layout.content_text,
        page_count: None,
        warnings: Vec::new(),
        source_metadata: ExtractionSourceMetadata {
            source_format: "docx".to_string(),
            page_count: None,
            line_count: i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX),
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({
            "paragraph_count": content_blocks.len(),
            "extracted_image_count": extracted_images.len(),
        }),
        provider_kind: None,
        model_name: None,
        usage_json: serde_json::json!({}),
        extracted_images,
    })
}

fn extract_table_as_markdown(table_node: &roxmltree::Node<'_, '_>) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();

    for row_node in table_node.children().filter(|n| n.tag_name().name() == "tr") {
        let mut cells = Vec::new();
        for cell_node in row_node.children().filter(|n| n.tag_name().name() == "tc") {
            let cell_text = cell_node
                .descendants()
                .filter(|child| child.tag_name().name() == "t")
                .filter_map(|child| child.text())
                .collect::<String>();
            cells.push(cell_text.trim().to_string());
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
    }

    render_markdown_table_from_rows(&rows)
        .unwrap_or_else(|| render_plain_table_rows(&rows, " ").join("\n"))
}

fn extract_drawing_image_rel_id(drawing_node: &roxmltree::Node<'_, '_>) -> Option<String> {
    drawing_node.descendants().find(|n| n.tag_name().name() == "blip").and_then(|blip| {
        // r:embed attribute contains the relationship ID for the image
        blip.attribute("embed")
            .or_else(|| {
                // Try namespace-prefixed lookup
                blip.attributes().find(|a| a.name() == "embed").map(|a| a.value())
            })
            .map(str::to_string)
    })
}

fn read_archive_entry(archive: &mut ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut entry = archive.by_name(name).ok()?;
    let mut content = String::new();
    entry.read_to_string(&mut content).ok()?;
    Some(content)
}

fn resolve_image_targets(rels_xml: &Option<String>, rel_ids: &[String]) -> Vec<String> {
    let Some(rels_content) = rels_xml else {
        return Vec::new();
    };
    let Ok(doc) = Document::parse(rels_content) else {
        return Vec::new();
    };

    let mut targets = Vec::new();
    for rel_id in rel_ids {
        for node in doc.descendants().filter(|n| n.tag_name().name() == "Relationship") {
            if node.attribute("Id") == Some(rel_id) {
                if let Some(target) = node.attribute("Target") {
                    let full_path = if target.starts_with('/') {
                        target.trim_start_matches('/').to_string()
                    } else {
                        format!("word/{target}")
                    };
                    targets.push(full_path);
                }
            }
        }
    }
    targets
}

fn extract_docx_images(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    targets: &[String],
) -> Vec<ExtractedImage> {
    let mut images = Vec::new();

    // Also collect images from word/media/ not explicitly referenced
    let media_files: Vec<String> = archive
        .file_names()
        .filter(|name| name.starts_with("word/media/") && is_image_file(name))
        .map(str::to_string)
        .collect();

    let all_targets: Vec<String> = {
        let mut seen: HashSet<String> = targets.iter().cloned().collect();
        let mut combined = targets.to_vec();
        for media_file in media_files {
            if seen.insert(media_file.clone()) {
                combined.push(media_file);
            }
        }
        combined
    };

    for target in &all_targets {
        let mut entry = match archive.by_name(target) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_err() || bytes.is_empty() {
            continue;
        }

        let mime_type = guess_image_mime_type(target, &bytes);
        let (width, height) = image_dimensions(&bytes).unwrap_or((0, 0));
        if width == 0 || height == 0 {
            continue;
        }

        images.push(ExtractedImage { page: 1, image_bytes: bytes, mime_type, width, height });
    }

    images
}

fn is_image_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".bmp")
        || lower.ends_with(".tiff")
        || lower.ends_with(".tif")
        || lower.ends_with(".webp")
        || lower.ends_with(".emf")
        || lower.ends_with(".wmf")
}

fn guess_image_mime_type(path: &str, bytes: &[u8]) -> String {
    if bytes.len() >= 8 {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return "image/png".to_string();
        }
        if bytes.starts_with(b"\xff\xd8\xff") {
            return "image/jpeg".to_string();
        }
        if bytes.starts_with(b"GIF8") {
            return "image/gif".to_string();
        }
        if bytes.starts_with(b"RIFF") && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
            return "image/webp".to_string();
        }
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".bmp") {
        "image/bmp"
    } else {
        "application/octet-stream"
    }
    .to_string()
}

fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::load_from_memory(bytes).ok().map(|img| (img.width(), img.height()))
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

    fn build_docx_with_table() -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer.start_file("word/document.xml", options).expect("start docx xml");
        writer
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
                <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
                  <w:body>
                    <w:p><w:r><w:t>Before table</w:t></w:r></w:p>
                    <w:tbl>
                      <w:tr>
                        <w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc>
                        <w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc>
                      </w:tr>
                      <w:tr>
                        <w:tc><w:p><w:r><w:t>Alpha</w:t></w:r></w:p></w:tc>
                        <w:tc><w:p><w:r><w:t>42</w:t></w:r></w:p></w:tc>
                      </w:tr>
                    </w:tbl>
                    <w:p><w:r><w:t>After table</w:t></w:r></w:p>
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

    #[test]
    fn extracts_table_as_markdown() {
        let output = extract_docx(&build_docx_with_table()).expect("docx extraction with table");

        assert!(output.content_text.contains("| Name | Value |"));
        assert!(output.content_text.contains("| --- | --- |"));
        assert!(output.content_text.contains("| Alpha | 42 |"));
        assert!(output.content_text.contains("Before table"));
        assert!(output.content_text.contains("After table"));
    }
}
