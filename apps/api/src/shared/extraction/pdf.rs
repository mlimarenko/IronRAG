use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, anyhow};
use lopdf::Document;

use crate::shared::extraction::{
    ExtractionOutput, ExtractionSourceMetadata, RawExtractionPage, build_text_layout,
};

#[derive(Debug)]
struct PopplerPdfExtraction {
    pages: Vec<RawExtractionPage>,
    page_count: Option<u32>,
}

pub fn extract_pdf(file_bytes: &[u8]) -> Result<ExtractionOutput> {
    let mut warnings = Vec::new();
    let (pages, page_numbers, page_count) = match Document::load_mem(file_bytes) {
        Ok(document) => {
            let pages = document.get_pages();
            let page_numbers = pages.keys().copied().collect::<Vec<_>>();
            let page_count = Some(u32::try_from(page_numbers.len()).unwrap_or(u32::MAX));
            if page_numbers.is_empty() {
                (Vec::new(), Vec::new(), page_count)
            } else {
                match extract_pdf_pages_with_lopdf(&document, &page_numbers) {
                    Ok(extracted_pages) => (extracted_pages, page_numbers, page_count),
                    Err(primary_error) => {
                        let fallback = extract_pdf_with_poppler(file_bytes).with_context(|| {
                            format!(
                                "failed to extract pdf text with lopdf and poppler fallback: {primary_error:#}",
                            )
                        })?;
                        warnings.push(format!(
                            "lopdf extraction failed; used pdftotext fallback ({primary_error})"
                        ));
                        (fallback.pages, page_numbers, fallback.page_count.or(page_count))
                    }
                }
            }
        }
        Err(load_error) => {
            let fallback = extract_pdf_with_poppler(file_bytes).with_context(|| {
                format!(
                    "failed to extract pdf text with lopdf and poppler fallback: {load_error:#}"
                )
            })?;
            let page_numbers = fallback
                .page_count
                .map(|count| (1..=count).collect::<Vec<_>>())
                .unwrap_or_default();
            warnings.push(format!(
                "lopdf could not parse the pdf structure; used pdftotext fallback ({load_error})"
            ));
            (fallback.pages, page_numbers, fallback.page_count)
        }
    };
    let layout = build_text_layout(&pages);

    Ok(ExtractionOutput {
        extraction_kind: "pdf_text".into(),
        content_text: layout.content_text,
        page_count,
        warnings,
        source_metadata: ExtractionSourceMetadata {
            source_format: "pdf".to_string(),
            page_count,
            line_count: i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX),
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({
            "pages": page_numbers,
        }),
        provider_kind: None,
        model_name: None,
    })
}

fn extract_pdf_pages_with_lopdf(
    document: &Document,
    page_numbers: &[u32],
) -> Result<Vec<RawExtractionPage>> {
    page_numbers
        .iter()
        .copied()
        .map(|page_number| {
            let page_text = document
                .extract_text(&[page_number])
                .with_context(|| format!("failed to extract pdf page {page_number}"))?;
            Ok(RawExtractionPage {
                page_number: Some(i32::try_from(page_number).unwrap_or(i32::MAX)),
                lines: split_pdf_page_lines(&page_text),
            })
        })
        .collect()
}

fn extract_pdf_with_poppler(file_bytes: &[u8]) -> Result<PopplerPdfExtraction> {
    let tempdir = tempfile::tempdir().context("failed to create tempdir for pdftotext")?;
    let pdf_path = tempdir.path().join("document.pdf");
    fs::write(&pdf_path, file_bytes).context("failed to write temp pdf for pdftotext")?;
    let page_count = extract_pdf_page_count_with_pdfinfo(&pdf_path);
    let pages = extract_pdf_pages_with_poppler(&pdf_path, page_count)?;

    Ok(PopplerPdfExtraction { pages, page_count })
}

fn extract_pdf_pages_with_poppler(
    pdf_path: &Path,
    page_count: Option<u32>,
) -> Result<Vec<RawExtractionPage>> {
    if let Some(page_count) = page_count {
        let mut pages = Vec::with_capacity(usize::try_from(page_count).unwrap_or(0));
        for page_number in 1..=page_count {
            let output = Command::new("pdftotext")
                .arg("-layout")
                .arg("-f")
                .arg(page_number.to_string())
                .arg("-l")
                .arg(page_number.to_string())
                .arg(pdf_path)
                .arg("-")
                .output()
                .with_context(|| format!("failed to spawn pdftotext for page {page_number}"))?;
            if !output.status.success() {
                return Err(anyhow!(
                    "pdftotext exited with status {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            pages.push(RawExtractionPage {
                page_number: Some(i32::try_from(page_number).unwrap_or(i32::MAX)),
                lines: split_pdf_page_lines(&String::from_utf8_lossy(&output.stdout)),
            });
        }
        return Ok(pages);
    }

    let output = Command::new("pdftotext")
        .arg("-layout")
        .arg(pdf_path)
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
    Ok(vec![RawExtractionPage {
        page_number: Some(1),
        lines: split_pdf_page_lines(&String::from_utf8_lossy(&output.stdout)),
    }])
}

fn extract_pdf_page_count_with_pdfinfo(pdf_path: &Path) -> Option<u32> {
    let output = Command::new("pdfinfo").arg(pdf_path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).lines().find_map(|line| {
        let (label, value) = line.split_once(':')?;
        (label.trim() == "Pages").then(|| value.trim().parse::<u32>().ok()).flatten()
    })
}

fn split_pdf_page_lines(content: &str) -> Vec<String> {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| line.trim_end().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use lopdf::{
        Document, Object, Stream,
        content::{Content, Operation},
        dictionary,
    };

    use super::*;

    const POPPLER_FALLBACK_PDF: &[u8] = br"%PDF-1.4
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj
3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>
endobj
4 0 obj
<< /Length 67 >>
stream
BT
/F1 18 Tf
72 720 Td
(Runtime PDF upload check) Tj
0 -24 Td
(Quarterly graph report) Tj
ET
endstream
endobj
5 0 obj
<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>
endobj
xref
0 6
0000000000 65535 f 
0000000009 00000 n 
0000000058 00000 n 
0000000115 00000 n 
0000000241 00000 n 
0000000359 00000 n 
trailer
<< /Size 6 /Root 1 0 R >>
startxref
429
%%EOF
";

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
        assert!(output.structure_hints.lines.iter().any(|line| line.page_number == Some(1)));
    }

    #[test]
    fn falls_back_to_poppler_for_readable_non_lopdf_pdf() {
        let output =
            extract_pdf(POPPLER_FALLBACK_PDF).expect("pdf extraction with poppler fallback");

        assert_eq!(output.page_count, Some(1));
        assert!(output.content_text.contains("Runtime PDF upload check"));
        assert!(output.warnings.iter().any(|warning| warning.contains("pdftotext fallback")));
        assert_eq!(output.source_map["pages"], serde_json::json!([1]));
    }
}
