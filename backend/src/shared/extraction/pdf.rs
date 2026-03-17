use std::{fs, process::Command};

use anyhow::{Context, Result, anyhow};
use lopdf::Document;

use crate::shared::extraction::ExtractionOutput;

pub fn extract_pdf(file_bytes: &[u8]) -> Result<ExtractionOutput> {
    let document = Document::load_mem(file_bytes).context("failed to load pdf bytes")?;
    let pages = document.get_pages();
    let page_numbers = pages.keys().copied().collect::<Vec<_>>();
    let mut warnings = Vec::new();
    let content_text = if page_numbers.is_empty() {
        String::new()
    } else {
        match document.extract_text(&page_numbers) {
            Ok(content_text) => content_text,
            Err(primary_error) => {
                let fallback_text =
                    extract_pdf_text_with_pdftotext(file_bytes).with_context(|| {
                        format!(
                            "failed to extract pdf text with lopdf and pdftotext fallback: {primary_error:#}",
                        )
                    })?;
                warnings.push(format!(
                    "lopdf extraction failed; used pdftotext fallback ({primary_error})"
                ));
                fallback_text
            }
        }
    };

    Ok(ExtractionOutput {
        extraction_kind: "pdf_text".into(),
        content_text,
        page_count: Some(u32::try_from(page_numbers.len()).unwrap_or(u32::MAX)),
        warnings,
        source_map: serde_json::json!({
            "pages": page_numbers,
        }),
        provider_kind: None,
        model_name: None,
    })
}

fn extract_pdf_text_with_pdftotext(file_bytes: &[u8]) -> Result<String> {
    let tempdir = tempfile::tempdir().context("failed to create tempdir for pdftotext")?;
    let pdf_path = tempdir.path().join("document.pdf");
    fs::write(&pdf_path, file_bytes).context("failed to write temp pdf for pdftotext")?;

    let output = Command::new("pdftotext")
        .arg("-layout")
        .arg("-nopgbrk")
        .arg(&pdf_path)
        .arg("-")
        .output()
        .context("failed to spawn pdftotext")?;

    if !output.status.success() {
        return Err(anyhow!(
            "pdftotext exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use lopdf::{
        Document, Object, Stream,
        content::{Content, Operation},
        dictionary,
    };

    use super::*;

    fn build_minimal_pdf_bytes() -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let single_page_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), Object::Integer(14)]),
                Operation::new("Td", vec![Object::Integer(72), Object::Integer(720)]),
                Operation::new("Tj", vec![Object::string_literal("Quarterly graph report")]),
                Operation::new("ET", vec![]),
            ],
        };
        let encoded = content.encode().expect("encode pdf stream");
        let content_id = document.add_object(Stream::new(dictionary! {}, encoded));
        document.objects.insert(
            single_page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![single_page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).expect("save pdf");
        bytes
    }

    #[test]
    fn extracts_text_and_page_map_from_minimal_pdf() {
        let output = extract_pdf(&build_minimal_pdf_bytes()).expect("pdf extraction");

        assert_eq!(output.extraction_kind, "pdf_text");
        assert_eq!(output.page_count, Some(1));
        assert!(output.content_text.contains("Quarterly graph report"));
        assert_eq!(output.source_map["pages"], serde_json::json!([1]));
    }
}
